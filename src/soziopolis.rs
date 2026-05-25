use crate::app_paths;
use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::{StatusCode, blocking::Client};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::{
    collections::hash_map::DefaultHasher,
    collections::{HashMap, HashSet, VecDeque},
    fs,
    hash::{Hash, Hasher},
    sync::mpsc,
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

const BASE_URL: &str = "https://www.soziopolis.de";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BROWSE_SECTION_WORKERS: usize = 4;
const MAX_SECTION_PAGE_DEPTH: usize = 80;
const HTML_CACHE_TTL: Duration = Duration::from_secs(180);
const HTML_DISK_CACHE_TTL: Duration = Duration::from_secs(900);
const HTML_CACHE_CAPACITY: usize = 96;
const HTML_DISK_CACHE_FILE_CAPACITY: usize = 160;

#[path = "soziopolis/cache.rs"]
mod cache;
#[path = "soziopolis/parse.rs"]
mod parse;
#[path = "soziopolis/types.rs"]
mod types;

use types::{CachedHtml, ListingMetadata};

pub use types::{
    AllSectionsBrowseState, Article, ArticleMetadata, ArticleSummary, BrowseSectionResult,
    DiscoveryReport, DiscoverySourceKind, SECTIONS, Section, SectionBrowseState,
};

pub use cache::clear_browse_cache;
use cache::*;
pub(crate) use parse::build_clean_text;
pub use parse::normalize_article_date;
use parse::*;

static HTML_CACHE: OnceLock<Mutex<HashMap<String, CachedHtml>>> = OnceLock::new();
static SUMMARY_CACHE: OnceLock<Mutex<HashMap<String, Vec<ArticleSummary>>>> = OnceLock::new();

struct ArticleCollectionTarget<'a> {
    fallback_section: Option<&'a str>,
    source_url: &'a str,
    source_kind: DiscoverySourceKind,
    limit: usize,
    seen: &'a mut HashSet<String>,
    articles: &'a mut Vec<ArticleSummary>,
    report: &'a mut DiscoveryReport,
}

#[derive(Clone)]
pub struct SoziopolisClient {
    client: Client,
    article_url_re: Regex,
}

impl SoziopolisClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("failed to build HTTP client")?;
        let article_url_re =
            Regex::new(r"^https://www\.soziopolis\.de/.+\.html(?:\?.*)?$").context("bad regex")?;

        Ok(Self {
            client,
            article_url_re,
        })
    }

    pub fn sections(&self) -> &'static [Section] {
        SECTIONS
    }

    pub fn section_by_id(&self, id: &str) -> Option<&'static Section> {
        SECTIONS.iter().find(|section| section.id == id)
    }

    pub fn browse_section(&self, section: &Section, limit: usize) -> Result<Vec<ArticleSummary>> {
        Ok(self.browse_section_detailed(section, limit)?.articles)
    }

    pub fn start_section_browse(&self, section: &Section) -> Result<SectionBrowseState> {
        let first_page_url = section.url.to_owned();
        let first_page_html = self.fetch_html(&first_page_url)?;
        let first_page_document = Html::parse_document(&first_page_html);
        let mut articles = Vec::new();
        let mut seen_article_urls = HashSet::new();
        let mut report = DiscoveryReport::default();

        report.record_source_visit(DiscoverySourceKind::Section);
        self.collect_articles_from_document(
            &first_page_document,
            ArticleCollectionTarget {
                fallback_section: Some(section.label),
                source_url: &first_page_url,
                source_kind: DiscoverySourceKind::Section,
                limit: usize::MAX,
                seen: &mut seen_article_urls,
                articles: &mut articles,
                report: &mut report,
            },
        );

        let pending_page_urls =
            section_page_urls(section, &first_page_html, MAX_SECTION_PAGE_DEPTH)
                .into_iter()
                .collect::<VecDeque<_>>();
        let discovered_page_urls = pending_page_urls.iter().cloned().collect::<HashSet<_>>();
        let mut visited_page_urls = HashSet::new();
        visited_page_urls.insert(first_page_url);

        Ok(SectionBrowseState {
            articles,
            report,
            exhausted: pending_page_urls.is_empty(),
            section: *section,
            pending_page_urls,
            discovered_page_urls,
            seen_article_urls,
            visited_page_urls,
        })
    }

    pub fn grow_section_browse(
        &self,
        state: &mut SectionBrowseState,
        target_limit: usize,
    ) -> Result<()> {
        let target_limit = target_limit.max(state.articles.len());
        let max_pages = desired_section_page_count(target_limit);

        while state.articles.len() < target_limit {
            let Some(page_url) = state.pending_page_urls.pop_front() else {
                state.exhausted = true;
                break;
            };

            if state.visited_page_urls.len() >= max_pages {
                state.exhausted = state.pending_page_urls.is_empty();
                break;
            }
            if !state.visited_page_urls.insert(page_url.clone()) {
                continue;
            }

            let html = self.fetch_html(&page_url)?;
            let document = Html::parse_document(&html);
            state
                .report
                .record_source_visit(DiscoverySourceKind::Section);
            self.collect_articles_from_document(
                &document,
                ArticleCollectionTarget {
                    fallback_section: Some(state.section.label),
                    source_url: &page_url,
                    source_kind: DiscoverySourceKind::Section,
                    limit: target_limit,
                    seen: &mut state.seen_article_urls,
                    articles: &mut state.articles,
                    report: &mut state.report,
                },
            );

            for discovered_url in
                extract_paginated_section_urls(&state.section, &html, MAX_SECTION_PAGE_DEPTH)
            {
                if state.discovered_page_urls.insert(discovered_url.clone()) {
                    state.pending_page_urls.push_back(discovered_url);
                }
            }
        }

        if state.pending_page_urls.is_empty() {
            state.exhausted = true;
        }

        Ok(())
    }

    pub fn browse_section_detailed(
        &self,
        section: &Section,
        limit: usize,
    ) -> Result<BrowseSectionResult> {
        let mut state = self.start_section_browse(section)?;
        self.grow_section_browse(&mut state, limit)?;

        Ok(BrowseSectionResult {
            articles: state.articles,
            report: state.report,
            exhausted: state.exhausted,
        })
    }

    pub fn browse_all_sections_detailed(&self, total_limit: usize) -> Result<BrowseSectionResult> {
        let mut state = self.start_all_sections_browse()?;
        self.grow_all_sections_browse(&mut state, total_limit)
    }

    pub fn start_all_sections_browse(&self) -> Result<AllSectionsBrowseState> {
        let worker_count = browse_section_worker_count(self.sections().len().max(1));
        let mut ordered_states = Vec::new();

        for chunk in self.sections().chunks(worker_count) {
            let (tx, rx) = mpsc::channel();
            thread::scope(|scope| {
                for (offset, section) in chunk.iter().enumerate() {
                    let tx = tx.clone();
                    let scraper = self.clone();
                    let section = *section;
                    scope.spawn(move || {
                        let result = scraper.start_section_browse(&section);
                        let _ = tx.send((offset, result));
                    });
                }
            });
            drop(tx);

            let mut chunk_results = Vec::new();
            while let Ok((offset, result)) = rx.recv() {
                chunk_results.push((offset, result?));
            }
            chunk_results.sort_by_key(|(offset, _)| *offset);
            ordered_states.extend(chunk_results.into_iter().map(|(_, state)| state));
        }

        Ok(AllSectionsBrowseState {
            section_states: ordered_states,
        })
    }

    pub fn grow_all_sections_browse(
        &self,
        state: &mut AllSectionsBrowseState,
        total_limit: usize,
    ) -> Result<BrowseSectionResult> {
        let section_count = state.section_states.len().max(1);
        let per_section_limit = total_limit.div_ceil(section_count).max(8);
        let worker_count = browse_section_worker_count(section_count);

        for chunk in state
            .section_states
            .iter()
            .cloned()
            .enumerate()
            .collect::<Vec<_>>()
            .chunks(worker_count)
        {
            let (tx, rx) = mpsc::channel();
            thread::scope(|scope| {
                for (index, section_state) in chunk.iter().cloned() {
                    let tx = tx.clone();
                    let scraper = self.clone();
                    scope.spawn(move || {
                        let mut section_state = section_state;
                        let result = scraper
                            .grow_section_browse(&mut section_state, per_section_limit)
                            .map(|_| section_state);
                        let _ = tx.send((index, result));
                    });
                }
            });
            drop(tx);

            while let Ok((index, result)) = rx.recv() {
                state.section_states[index] = result?;
            }
        }

        Ok(merge_all_sections_states(
            &state.section_states,
            total_limit,
        ))
    }

    pub fn browse_url(
        &self,
        url: &str,
        fallback_section: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ArticleSummary>> {
        let html = self.fetch_html(url)?;
        let document = Html::parse_document(&html);
        let mut articles = Vec::new();
        let mut seen = HashSet::new();
        let mut report = DiscoveryReport::default();
        self.collect_articles_from_document(
            &document,
            ArticleCollectionTarget {
                fallback_section,
                source_url: url,
                source_kind: DiscoverySourceKind::Section,
                limit,
                seen: &mut seen,
                articles: &mut articles,
                report: &mut report,
            },
        );
        Ok(articles)
    }

    pub fn fetch_article(&self, url: &str) -> Result<Article> {
        let html = self.fetch_html(url)?;
        parse_article_html(url, &html)
    }

    pub fn fetch_article_metadata(&self, url: &str) -> Result<ArticleMetadata> {
        let article = self.fetch_article(url)?;
        Ok(ArticleMetadata {
            url: article.url,
            title: article.title,
            date: article.date,
            section: article.section,
        })
    }

    fn fetch_html(&self, url: &str) -> Result<String> {
        if let Some(cached) = lookup_cached_html(url) {
            crate::perf::record_browse_cache_hit();
            return Ok(cached);
        }
        crate::perf::record_browse_cache_miss();

        let mut last_error = None;

        for attempt in 1..=3 {
            match self.client.get(url).send() {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        let body = response
                            .text()
                            .with_context(|| format!("network: failed to read body for {url}"));
                        if let Ok(body) = body {
                            store_cached_html(url, &body);
                            store_disk_cached_html(url, &body);
                            return Ok(body);
                        }
                        return body;
                    }

                    let retryable = is_retryable_status(status);
                    last_error = Some(anyhow::anyhow!(
                        "network: non-success response {} for {}",
                        status,
                        url
                    ));
                    if !retryable || attempt == 3 {
                        break;
                    }
                }
                Err(err) => {
                    last_error = Some(anyhow::anyhow!("network: request failed for {url}: {err}"));
                    if attempt == 3 {
                        break;
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(450 * attempt as u64));
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("network: failed to fetch {url}")))
    }

    fn collect_articles_from_document(&self, document: &Html, target: ArticleCollectionTarget<'_>) {
        for summary in cached_article_summaries_for_source(
            self,
            document,
            target.fallback_section,
            target.source_url,
            target.source_kind,
        ) {
            if target.articles.len() >= target.limit {
                break;
            }
            if !target.seen.insert(summary.url.clone()) {
                target.report.deduped_articles += 1;
                continue;
            }
            target.articles.push(summary);
            target.report.record_article(target.source_kind);
        }
    }
}

fn browse_section_worker_count(section_count: usize) -> usize {
    section_count.clamp(1, MAX_BROWSE_SECTION_WORKERS)
}

fn merge_all_sections_states(
    section_states: &[SectionBrowseState],
    total_limit: usize,
) -> BrowseSectionResult {
    let mut merged_articles = Vec::new();
    let mut merged_report = DiscoveryReport::default();
    let mut seen = HashSet::new();

    for section_state in section_states {
        merged_report.merge(&section_state.report);
        for article in &section_state.articles {
            if merged_articles.len() >= total_limit {
                break;
            }
            if seen.insert(article.url.clone()) {
                merged_articles.push(article.clone());
            } else {
                merged_report.deduped_articles += 1;
            }
        }
        if merged_articles.len() >= total_limit {
            break;
        }
    }

    BrowseSectionResult {
        articles: merged_articles,
        report: merged_report,
        exhausted: section_states.iter().all(|state| state.exhausted),
    }
}

fn parse_article_html(url: &str, html: &str) -> Result<Article> {
    let document = Html::parse_document(html);

    let title = first_text(
        &document,
        &[
            "h1.article-title",
            "h1",
            "meta[property=\"og:title\"]",
            "title",
        ],
    )
    .map(|value| value.replace(" | Soziopolis", "").trim().to_owned())
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "Untitled".to_owned());

    let subtitle = first_text(
        &document,
        &["h2.article-subtitle", "meta[name=\"description\"]"],
    )
    .unwrap_or_default();

    let author = collect_authors(&document);
    let date = first_article_text(
        &document,
        &[
            ".article-date",
            "time",
            "meta[property=\"article:published_time\"]",
        ],
    )
    .or_else(|| extract_date_from_html(html))
    .unwrap_or_default();
    let section = extract_section(&document)
        .or_else(|| infer_section_from_url(url))
        .unwrap_or_else(|| "Soziopolis".to_owned());

    let body_text = extract_body(&document)?;
    let word_count = body_text.split_whitespace().count();
    if word_count < 80 {
        bail!("article extraction produced too little text for {url}");
    }

    let clean_text = build_clean_text(&title, &subtitle, &author, &date, &body_text);

    let published_at = normalize_article_date(&date).unwrap_or_default();

    Ok(Article {
        url: url.to_owned(),
        title,
        subtitle,
        teaser: String::new(),
        author,
        date,
        published_at,
        section,
        source_kind: "article".to_owned(),
        source_label: source_label(url),
        body_text,
        clean_text,
        word_count,
        fetched_at: iso_timestamp_now(),
    })
}

fn iso_timestamp_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_fixture_extracts_expected_fields() -> Result<()> {
        let html = include_str!("../tests/fixtures/soziopolis_article_fixture.html");
        let document = Html::parse_document(html);

        let title = first_text(
            &document,
            &[
                "h1.article-title",
                "h1",
                "meta[property=\"og:title\"]",
                "title",
            ],
        )
        .expect("fixture title");
        let subtitle = first_text(
            &document,
            &["h2.article-subtitle", "meta[name=\"description\"]"],
        )
        .expect("fixture subtitle");
        let author = collect_authors(&document);
        let section = extract_section(&document).expect("fixture section");
        let body = extract_body(&document)?;
        let clean_text = build_clean_text(&title, &subtitle, &author, "2026-02-19", &body);

        assert_eq!(title, "Im Strudel des Digitalen");
        assert_eq!(
            subtitle,
            "Rezension zu \"Der Stachel des Digitalen. Geisteswissenschaften und Digital Humanities\" von Sybille Kraemer"
        );
        assert_eq!(author, "Sybille Kraemer");
        assert_eq!(section, "Essay");
        assert!(body.contains("## Zwischen Daten und Deutung"));
        assert!(body.contains("Der Stachel des Digitalen"));
        assert!(!body.contains("Artikel lesen"));
        assert!(clean_text.contains("Von Sybille Kraemer"));
        Ok(())
    }

    #[test]
    fn article_cleanup_fixture_extracts_article_without_page_chrome() -> Result<()> {
        let html = include_str!("../tests/fixtures/soziopolis_article_cleanup_fixture.html");
        let document = Html::parse_document(html);

        let title = first_text(
            &document,
            &[
                "h1.article-title",
                "h1",
                "meta[property=\"og:title\"]",
                "title",
            ],
        )
        .map(|value| value.replace(" | Soziopolis", "").trim().to_owned())
        .expect("fixture title");
        let subtitle = first_text(
            &document,
            &["h2.article-subtitle", "meta[name=\"description\"]"],
        )
        .expect("fixture subtitle");
        let author = collect_authors(&document);
        let date = first_text(
            &document,
            &[
                ".article-date",
                "time",
                "meta[property=\"article:published_time\"]",
            ],
        )
        .expect("fixture date");
        let section = extract_section(&document).expect("fixture section");
        let body = extract_body(&document)?;
        let clean_text = build_clean_text(&title, &subtitle, &author, &date, &body);

        assert_eq!(title, "Der Alltag der Algorithmen");
        assert_eq!(
            subtitle,
            "Wie digitale Sortierungen soziale Routinen veraendern"
        );
        assert_eq!(author, "Mara Beispiel, Jens Muster");
        assert_eq!(date, "23.04.2026");
        assert_eq!(normalize_article_date(&date).as_deref(), Some("2026-04-23"));
        assert_eq!(section, "Essay");
        assert!(body.contains("Digitale Sortierungen treten selten"));
        assert!(body.contains("## Routinen und Reibungen"));
        assert!(body.contains("- Die beobachteten Routinen verbinden"));
        assert!(body.contains("Entscheidend ist nicht die einzelne Bewertung"));
        assert!(!body.contains("Artikel lesen"));
        assert!(!body.contains("Teilen auf Facebook"));
        assert!(!body.contains("ISSN 2509-5196"));
        assert!(!body.contains("Zum Seitenanfang"));
        assert!(!body.contains("Newsletter abonnieren"));
        assert!(clean_text.contains("Von Mara Beispiel, Jens Muster"));
        assert!(clean_text.contains("23.04.2026"));
        Ok(())
    }

    #[test]
    fn invalid_article_url_reports_network_failure_without_live_request() {
        let client = SoziopolisClient::new().expect("client should build");

        let error = client
            .fetch_article("not a url")
            .expect_err("invalid URL should fail before any live network request");
        let message = error.to_string();

        assert!(message.contains("network: request failed for not a url"));
        assert!(message.contains("builder error") || message.contains("relative URL"));
    }

    #[test]
    fn page_chrome_without_article_reports_missing_body() {
        let html = include_str!("../tests/fixtures/soziopolis_malformed_article_fixture.html");

        let error = parse_article_html("https://www.soziopolis.de/unvollstaendig.html", html)
            .expect_err("page chrome without article content should not produce an article");

        assert_eq!(error.to_string(), "could not extract article body");
    }

    #[test]
    fn missing_title_uses_existing_untitled_fallback_when_body_is_present() -> Result<()> {
        let html = include_str!("../tests/fixtures/soziopolis_missing_title_article_fixture.html");

        let article = parse_article_html("https://www.soziopolis.de/ohne-titel.html", html)?;

        assert_eq!(article.title, "Untitled");
        assert_eq!(article.author, "Test Autorin");
        assert_eq!(article.date, "02.05.2026");
        assert_eq!(article.published_at, "2026-05-02");
        assert_eq!(article.section, "Essay");
        assert!(article.word_count >= 80);
        assert!(article.clean_text.starts_with("Untitled\nVon Test Autorin"));
        Ok(())
    }

    #[test]
    fn german_month_date_is_preserved_without_blocking_body_extraction() -> Result<()> {
        let html = article_html(
            r#"
            <article>
              <p class="article-type">Essay</p>
              <p class="article-overline">
                <span class="author-name">Einzel Autor</span>
              </p>
              <p class="article-date">24. Mai 2026</p>
              <h1 class="article-title">Datum mit Monatsnamen</h1>
              <div class="article-content">
                <p>Der erste Absatz beschreibt eine getestete Metadatenvariante mit deutschem Monatsnamen und ausreichend langem Fliesstext fuer die Artikelverarbeitung.</p>
                <p>Der zweite Absatz hält fest, dass das Datum aktuell als rohe Anzeige erhalten bleibt und nicht zur Normalisierung gezwungen wird.</p>
                <p>Der dritte Absatz ergänzt Beobachtungen zu Redaktion, Quellenlage und technischer Extraktion, damit die Mindestlänge der Verarbeitung stabil bleibt.</p>
                <p>Der vierte Absatz macht die Fixture deterministisch und vermeidet reale Soziopolis-Texte oder externe Netzwerkzugriffe.</p>
                <p>Der fünfte Absatz liefert zusätzliche neutrale Wörter über Überschrift, Vorspann, Quellenhinweis und Lesefluss, ohne eine neue fachliche Behauptung einzuführen.</p>
              </div>
            </article>
            "#,
        );

        let article = parse_article_html("https://www.soziopolis.de/monatsname.html", &html)?;

        assert_eq!(article.author, "Einzel Autor");
        assert_eq!(article.date, "24. Mai 2026");
        assert_eq!(article.published_at, "");
        assert!(article.body_text.contains("deutschem Monatsnamen"));
        Ok(())
    }

    #[test]
    fn optional_author_and_bad_date_do_not_discard_valid_body() -> Result<()> {
        let html = article_html(
            r#"
            <article>
              <p class="article-type">Essay</p>
              <p class="article-date">demnaechst</p>
              <h1 class="article-title">Ohne Autor mit Rohdatum</h1>
              <div class="article-content">
                <p>Diese Seite hat absichtlich keine Autorzeile, aber einen ausreichend langen Artikeltext, der weiterhin extrahiert werden soll.</p>
                <p>Der zweite Absatz beschreibt, dass fehlende Autorinnen oder Autoren aktuell kein Fehlerfall sind und die lokale Speicherung nicht blockieren.</p>
                <p>Der dritte Absatz stabilisiert die Wortzahl mit weiterer Beschreibung zu Metadaten, redaktioneller Einordnung und Parserverhalten.</p>
                <p>Der vierte Absatz hält die Fixture synthetisch, klein und frei von kopiertem Artikeltext.</p>
                <p>Der fünfte Absatz ergänzt neutrale Wörter über Überschrift, Vorspann, Lesefluss und Extraktion, damit nur die Metadatenvariante getestet wird.</p>
              </div>
            </article>
            "#,
        );

        let article = parse_article_html("https://www.soziopolis.de/ohne-autor.html", &html)?;

        assert_eq!(article.author, "");
        assert_eq!(article.date, "demnaechst");
        assert_eq!(article.published_at, "");
        assert!(article.body_text.contains("keine Autorzeile"));
        Ok(())
    }

    #[test]
    fn multiple_authors_with_und_separator_are_preserved_as_text() -> Result<()> {
        let html = article_html(
            r#"
            <article>
              <p class="article-type">Essay</p>
              <p class="article-overline">
                <span class="author-name">Anna Beispiel und Bernd Muster</span>
              </p>
              <time>2026-05-24</time>
              <h1 class="article-title">Autoren mit Und</h1>
              <div class="article-content">
                <p>Der erste Absatz beschreibt eine synthetische Autorenvariante, bei der zwei Namen in einer gemeinsamen Autorzeile stehen.</p>
                <p>Der zweite Absatz stellt sicher, dass die Autorzeile als vorhandener Text erhalten bleibt und nicht neu interpretiert wird.</p>
                <p>Der dritte Absatz ergänzt genug Wörter über Soziopolis-nahe Metadaten und Extraktion, damit die bestehende Mindestlänge erreicht wird.</p>
                <p>Der vierte Absatz dokumentiert den aktuellen Parservertrag ohne eine neue Normalisierung fuer Autorennamen einzuführen.</p>
                <p>Der fünfte Absatz fügt neutrale Beschreibung zu Überschrift, Vorspann, Lesefluss und Quellenhinweis hinzu, ohne Verhalten oder Bedeutung zu ändern.</p>
              </div>
            </article>
            "#,
        );

        let article = parse_article_html("https://www.soziopolis.de/autor-und.html", &html)?;

        assert_eq!(article.author, "Anna Beispiel und Bernd Muster");
        assert_eq!(article.date, "2026-05-24");
        assert_eq!(article.published_at, "2026-05-24");
        Ok(())
    }

    #[test]
    fn sidebar_author_and_date_do_not_override_article_metadata() -> Result<()> {
        let html = article_html(
            r#"
            <aside>
              <p class="article-overline"><span class="author-name">Sidebar Name</span></p>
              <p class="article-date">01.01.2001</p>
            </aside>
            <article>
              <p class="article-type">Essay</p>
              <p class="article-overline">
                <span class="author-name">Artikel Name</span>
              </p>
              <p class="article-date">24.05.2026</p>
              <h1 class="article-title">Metadaten im Artikel</h1>
              <div class="article-content">
                <p>Der erste Absatz stellt sicher, dass navigierende oder seitliche Seitenelemente nicht als Artikelmetadaten übernommen werden.</p>
                <p>Der zweite Absatz beschreibt die eigentliche Artikelzone mit genug synthetischem Fliesstext fuer die Verarbeitung.</p>
                <p>Der dritte Absatz ergänzt weitere Beobachtungen zu Klassen, Autorenzeilen und Datumsfeldern im Parser.</p>
                <p>Der vierte Absatz bleibt künstlich und klein, damit der Test keine kopierten Inhalte enthält.</p>
                <p>Der fünfte Absatz liefert neutrale Wörter über Überschrift, Vorspann, Lesefluss und Extraktion, damit die Metadatenprüfung stabil bleibt.</p>
                <p>Der sechste Absatz beschreibt nochmals die Artikelzone, die bereinigte Textmenge und den synthetischen Charakter dieser Fixture.</p>
                <p>Der siebte Absatz verhindert, dass die Prüfung versehentlich nur an der Mindestlänge statt an der Metadatenquelle scheitert.</p>
              </div>
            </article>
            "#,
        );

        let article = parse_article_html("https://www.soziopolis.de/metadaten.html", &html)?;

        assert_eq!(article.author, "Artikel Name");
        assert_eq!(article.date, "24.05.2026");
        assert_eq!(article.published_at, "2026-05-24");
        assert!(!article.author.contains("Sidebar"));
        assert_ne!(article.date, "01.01.2001");
        Ok(())
    }

    #[test]
    fn unexpected_article_container_markup_reports_missing_body() {
        let html = r#"
            <!doctype html>
            <html lang="de">
            <body>
              <article>
                <h1 class="article-title">Neue Containerstruktur</h1>
                <p class="article-date">07.05.2026</p>
                <section class="article-body">
                  <p>Dieser Text steht absichtlich nicht in .article-content.</p>
                </section>
              </article>
            </body>
            </html>
        "#;

        let error = parse_article_html("https://www.soziopolis.de/neue-struktur.html", html)
            .expect_err("unexpected container markup should not produce an article");

        assert_eq!(error.to_string(), "could not extract article body");
    }

    #[test]
    fn inline_links_footnotes_and_emphasis_are_cleaned_in_body_blocks() -> Result<()> {
        let html = r#"
            <!doctype html>
            <html lang="de">
            <body>
              <article>
                <p class="article-type">Essay</p>
                <p class="article-overline"><span class="author-name">Inline Test</span></p>
                <p class="article-date">08.05.2026</p>
                <h1 class="article-title">Inline Marker im Artikeltext</h1>
                <h2 class="article-subtitle">Eine Prüfung vorhandener Bereinigung</h2>
                <div class="article-content">
                  <p>Der erste Absatz enthält <a href="/x">einen Link</a>, <em>betonte Begriffe</em> und eine Fussnote [1], die beim Bereinigen entfernt wird.</p>
                  <p>Der zweite Absatz stabilisiert den Umfang der Fixture mit Beobachtungen zu digitalen Routinen, institutionellen Entscheidungen und sozialwissenschaftlichen Deutungen.</p>
                  <p>Der dritte Absatz beschreibt, wie Formulare, Rankings und redaktionelle Hinweise in alltäglichen Arbeitsprozessen auftauchen und neue Erwartungen erzeugen.</p>
                  <p>Der vierte Absatz hält fest, dass die Extraktion Inline-Markup als Text liest und keine Navigations- oder Quellverweise daraus machen soll.</p>
                  <p>Der fünfte Absatz sorgt dafür, dass die bestehende Mindestlänge der Artikelverarbeitung überschritten wird, ohne neue Regeln einzuführen.</p>
                </div>
              </article>
            </body>
            </html>
        "#;

        let article = parse_article_html("https://www.soziopolis.de/inline-marker.html", html)?;

        assert_eq!(article.title, "Inline Marker im Artikeltext");
        assert!(article.body_text.contains("einen Link"));
        assert!(article.body_text.contains("betonte Begriffe"));
        assert!(!article.body_text.contains("[1]"));
        assert!(article.word_count >= 80);
        Ok(())
    }

    #[test]
    fn very_short_article_text_reports_too_little_text() {
        let html = r#"
            <!doctype html>
            <html lang="de">
            <body>
              <article>
                <h1 class="article-title">Kurzer Artikel</h1>
                <div class="article-content">
                  <p>Dieser Absatz ist lang genug fuer einen Block, aber zu kurz fuer einen Artikel.</p>
                </div>
              </article>
            </body>
            </html>
        "#;

        let error = parse_article_html("https://www.soziopolis.de/kurz.html", html)
            .expect_err("short extracted body should fail the current minimum length rule");

        assert_eq!(
            error.to_string(),
            "article extraction produced too little text for https://www.soziopolis.de/kurz.html"
        );
    }

    fn article_html(body: &str) -> String {
        format!(
            r#"<!doctype html>
            <html lang="de">
            <head><meta charset="utf-8"></head>
            <body>{body}</body>
            </html>"#
        )
    }

    #[test]
    fn section_fixture_discovers_unique_articles_and_teasers() -> Result<()> {
        let client = SoziopolisClient::new()?;
        let document = Html::parse_document(include_str!(
            "../tests/fixtures/soziopolis_section_fixture.html"
        ));

        let mut seen = HashSet::new();
        let mut articles = Vec::new();
        let mut report = DiscoveryReport::default();

        client.collect_articles_from_document(
            &document,
            ArticleCollectionTarget {
                fallback_section: Some("Essays"),
                source_url: "https://www.soziopolis.de/texte/essay.html",
                source_kind: DiscoverySourceKind::Section,
                limit: 10,
                seen: &mut seen,
                articles: &mut articles,
                report: &mut report,
            },
        );

        assert_eq!(articles.len(), 2);
        assert_eq!(report.section_articles, 2);
        assert_eq!(report.deduped_articles, 1);
        assert_eq!(articles[0].title, "Im Strudel des Digitalen");
        assert_eq!(articles[0].author, "Sybille Kraemer");
        assert_eq!(articles[0].date, "19.02.2026");
        assert!(articles[0].teaser.contains("Geisteswissenschaften"));
        assert_eq!(articles[1].title, "Mood Tracker");
        assert_eq!(articles[1].author, "Test Autorin");
        assert_eq!(articles[1].date, "17.02.2026");
        assert!(articles[1].teaser.contains("Selbstvermessung"));
        Ok(())
    }

    #[test]
    fn real_essay_listing_fixture_preserves_author_date_and_teaser() -> Result<()> {
        let client = SoziopolisClient::new()?;
        let document = Html::parse_document(include_str!(
            "../tests/fixtures/soziopolis_real_essay_listing_fixture.html"
        ));

        let mut seen = HashSet::new();
        let mut articles = Vec::new();
        let mut report = DiscoveryReport::default();

        client.collect_articles_from_document(
            &document,
            ArticleCollectionTarget {
                fallback_section: Some("Essays"),
                source_url: "https://www.soziopolis.de/texte/essay.html",
                source_kind: DiscoverySourceKind::Section,
                limit: 10,
                seen: &mut seen,
                articles: &mut articles,
                report: &mut report,
            },
        );

        assert!(articles.len() >= 2);
        assert_eq!(articles[0].title, "Buchempfehlungen zum Frühling");
        assert_eq!(articles[0].date, "10.04.2026");
        assert!(articles[0].author.contains("Stephanie Kappacher"));
        assert!(articles[0].teaser.contains("Lektüretipps"));
        Ok(())
    }

    #[test]
    fn real_interview_listing_fixture_preserves_article_metadata() -> Result<()> {
        let client = SoziopolisClient::new()?;
        let document = Html::parse_document(include_str!(
            "../tests/fixtures/soziopolis_real_interview_listing_fixture.html"
        ));

        let mut seen = HashSet::new();
        let mut articles = Vec::new();
        let mut report = DiscoveryReport::default();

        client.collect_articles_from_document(
            &document,
            ArticleCollectionTarget {
                fallback_section: Some("Interviews"),
                source_url: "https://www.soziopolis.de/texte/interview.html",
                source_kind: DiscoverySourceKind::Section,
                limit: 10,
                seen: &mut seen,
                articles: &mut articles,
                report: &mut report,
            },
        );

        assert!(articles.len() >= 2);
        assert_eq!(
            articles[0].title,
            "„Tierrechte sind juridische Bauchrednerei“"
        );
        assert_eq!(articles[0].date, "04.03.2026");
        assert_eq!(articles[0].section, "Interview");
        assert!(articles[0].author.contains("Gonzalo Haefner"));
        Ok(())
    }

    #[test]
    fn section_page_urls_expand_with_higher_limits() {
        let html = r#"
            <html><body>
                <a href="/texte/interview.html?listArticles13%5Bcontroller%5D=Search&amp;listArticles13%5Bpage%5D=1&amp;cHash=aaa">1</a>
                <a href="/texte/interview.html?listArticles13%5Bcontroller%5D=Search&amp;listArticles13%5Bpage%5D=2&amp;cHash=bbb">2</a>
                <a href="/texte/interview.html?listArticles13%5Bcontroller%5D=Search&amp;listArticles13%5Bpage%5D=3&amp;cHash=ccc">3</a>
                <a href="/texte/interview.html?listArticles13%5Bcontroller%5D=Search&amp;listArticles13%5Bpage%5D=4&amp;cHash=ddd">4</a>
                <a href="/texte/interview.html?listArticles13%5Bcontroller%5D=Search&amp;listArticles13%5Bpage%5D=5&amp;cHash=eee">5</a>
                <a href="/texte/interview.html?listArticles13%5Bcontroller%5D=Search&amp;listArticles13%5Bpage%5D=6&amp;cHash=fff">6</a>
                <a href="/texte/interview.html?listArticles13%5Bcontroller%5D=Search&amp;listArticles13%5Bpage%5D=7&amp;cHash=ggg">7</a>
            </body></html>
        "#;
        let urls = section_page_urls(&SECTIONS[3], html, 80);
        assert_eq!(urls.len(), 6);
        assert!(urls[0].contains("listArticles13%5Bpage%5D=2"));
        assert!(urls[0].contains("cHash="));
    }

    #[test]
    fn deeper_interview_fixture_discovers_later_pages() {
        let html = include_str!("../tests/fixtures/soziopolis_real_interview_page10_fixture.html");
        let urls = extract_paginated_section_urls(&SECTIONS[3], html, 20);
        assert!(urls.iter().any(|url| url.contains("page%5D=11")));
        assert!(urls.iter().any(|url| url.contains("page%5D=12")));
        assert!(urls.iter().any(|url| url.contains("page%5D=13")));
    }

    #[test]
    fn merge_all_sections_states_marks_exhausted_only_when_every_section_is_done() {
        let state_a = SectionBrowseState {
            articles: vec![ArticleSummary {
                url: "https://example.com/a".to_owned(),
                title: "A".to_owned(),
                teaser: String::new(),
                author: String::new(),
                date: String::new(),
                section: "Essay".to_owned(),
                source_kind: DiscoverySourceKind::Section,
                source_label: "Essays".to_owned(),
            }],
            report: DiscoveryReport::default(),
            exhausted: true,
            section: SECTIONS[1],
            pending_page_urls: VecDeque::new(),
            discovered_page_urls: HashSet::new(),
            seen_article_urls: HashSet::new(),
            visited_page_urls: HashSet::new(),
        };
        let state_b = SectionBrowseState {
            articles: vec![ArticleSummary {
                url: "https://example.com/b".to_owned(),
                title: "B".to_owned(),
                teaser: String::new(),
                author: String::new(),
                date: String::new(),
                section: "Interview".to_owned(),
                source_kind: DiscoverySourceKind::Section,
                source_label: "Interviews".to_owned(),
            }],
            report: DiscoveryReport::default(),
            exhausted: false,
            section: SECTIONS[3],
            pending_page_urls: VecDeque::from([String::from("https://example.com/page2")]),
            discovered_page_urls: HashSet::new(),
            seen_article_urls: HashSet::new(),
            visited_page_urls: HashSet::new(),
        };

        let merged = merge_all_sections_states(&[state_a.clone(), state_b.clone()], 20);
        assert_eq!(merged.articles.len(), 2);
        assert!(!merged.exhausted);

        let merged_exhausted = merge_all_sections_states(
            &[
                state_a,
                SectionBrowseState {
                    exhausted: true,
                    ..state_b
                },
            ],
            20,
        );
        assert!(merged_exhausted.exhausted);
    }

    #[test]
    fn browse_section_worker_count_is_bounded() {
        assert_eq!(browse_section_worker_count(0), 1);
        assert_eq!(browse_section_worker_count(1), 1);
        assert_eq!(browse_section_worker_count(3), 3);
        assert_eq!(browse_section_worker_count(20), 4);
    }

    #[test]
    fn desired_section_page_count_scales_but_stays_bounded() {
        assert_eq!(desired_section_page_count(0), 2);
        assert_eq!(desired_section_page_count(80), 8);
        assert_eq!(desired_section_page_count(160), 16);
        assert_eq!(desired_section_page_count(5000), 80);
    }
}
