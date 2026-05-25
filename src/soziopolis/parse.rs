use super::*;

pub(super) fn extract_body(document: &Html) -> Result<String> {
    let article_selector = Selector::parse(".article-content").expect("selector");
    let block_selector = Selector::parse("p, h2, h3, li, blockquote").expect("selector");
    let mut best_blocks = Vec::new();

    for article in document.select(&article_selector) {
        let mut blocks = Vec::new();
        for node in article.select(&block_selector) {
            let name = node.value().name();
            let mut text = clean_whitespace(&collect_text(node));
            text = normalize_inline_markers(&text);
            if should_skip_block(&text) {
                continue;
            }
            match name {
                "h2" | "h3" if text.len() >= 4 => blocks.push(format!("## {text}")),
                "li" if text.len() >= 16 => blocks.push(format!("- {text}")),
                "blockquote" if text.len() >= 30 => blocks.push(text),
                "p" if text.len() >= 35 => blocks.push(text),
                _ => {}
            }
        }
        if blocks.len() > best_blocks.len() {
            best_blocks = blocks;
        }
    }

    dedupe_lines(&mut best_blocks);
    if best_blocks.is_empty() {
        bail!("could not extract article body");
    }
    Ok(best_blocks.join("\n\n"))
}

pub(super) fn should_skip_block(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let markers = [
        "Empfehlungen",
        "Artikel lesen",
        "Zur PDF-Datei dieses Artikels",
        "Social Science Open Access Repository",
        "ISSN 2509-5196",
        "Zum Seitenanfang",
    ];
    markers.iter().any(|marker| text.contains(marker))
}

pub(super) fn normalize_inline_markers(text: &str) -> String {
    let footnote_re = Regex::new(r"\[\d+\]").expect("footnote regex");
    clean_whitespace(&footnote_re.replace_all(text, " "))
}

pub(super) fn dedupe_lines(lines: &mut Vec<String>) {
    let mut seen = HashSet::new();
    lines.retain(|line| seen.insert(canonical_text(line)));
}

pub(super) fn collect_authors(document: &Html) -> String {
    let primary_selector = Selector::parse("p.article-overline .author-name").expect("selector");
    let primary = article_metadata_root(document)
        .map(|root| root.select(&primary_selector).collect::<Vec<_>>())
        .unwrap_or_else(|| document.select(&primary_selector).collect::<Vec<_>>())
        .into_iter()
        .map(collect_text)
        .map(|value| clean_whitespace(&value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if !primary.is_empty() {
        return primary.join(", ");
    }

    let fallback_selector =
        Selector::parse(".article-overline .author-name, .article-header .author-name")
            .expect("selector");
    let mut authors = Vec::new();
    let mut seen = HashSet::new();
    let fallback_nodes = article_metadata_root(document)
        .map(|root| root.select(&fallback_selector).collect::<Vec<_>>())
        .unwrap_or_else(|| document.select(&fallback_selector).collect::<Vec<_>>());
    for node in fallback_nodes {
        let value = clean_whitespace(&collect_text(node));
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        authors.push(value);
    }
    authors.join(", ")
}

pub(super) fn first_article_text(document: &Html, selectors: &[&str]) -> Option<String> {
    if let Some(root) = article_metadata_root(document) {
        for selector in selectors {
            let selector = Selector::parse(selector).ok()?;
            let value = root.select(&selector).find_map(|node| {
                let attr_content = node.value().attr("content").map(clean_whitespace);
                let text_content =
                    Some(clean_whitespace(&collect_text(node))).filter(|value| !value.is_empty());
                attr_content.or(text_content)
            });
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                return Some(value);
            }
        }
    }
    first_text(document, selectors)
}

fn article_metadata_root(document: &Html) -> Option<ElementRef<'_>> {
    let article_selector = Selector::parse("article").ok()?;
    let content_selector = Selector::parse(".article-content").ok()?;
    document
        .select(&article_selector)
        .find(|article| article.select(&content_selector).next().is_some())
}

pub(super) fn extract_section(document: &Html) -> Option<String> {
    if let Some(article_type) = first_text(document, &[".article-type"]) {
        let normalized = clean_whitespace(&article_type);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }

    let category_selector = Selector::parse("p.article-categories a").ok()?;
    for node in document.select(&category_selector) {
        let value = clean_whitespace(&collect_text(node));
        if !value.is_empty() {
            return Some(value);
        }
    }

    let keywords_selector = Selector::parse("meta[name=\"keywords\"]").ok()?;
    for node in document.select(&keywords_selector) {
        let value = node.value().attr("content").map(clean_whitespace)?;
        let first_keyword = value
            .split(',')
            .map(str::trim)
            .find(|part| !part.is_empty())
            .map(str::to_owned);
        if first_keyword.is_some() {
            return first_keyword;
        }
    }

    None
}

pub(super) fn extract_date_from_html(html: &str) -> Option<String> {
    let re = Regex::new(r#"article-date\">\s*([^<]+)\s*<"#).ok()?;
    re.captures(html)
        .and_then(|captures| captures.get(1))
        .map(|value| clean_whitespace(value.as_str()))
}

pub(super) fn extract_teaser_from_heading(link: ElementRef<'_>) -> String {
    let preferred_selectors = [
        "p.article-abstract",
        ".article-abstract",
        "p.article-subtitle",
        ".article-subtitle",
        ".list-text",
    ];
    let fallback_selector = Selector::parse("p").expect("selector");
    let link_text = collect_text(link);
    let mut parent = link.parent();
    for _ in 0..5 {
        let Some(node) = parent else {
            break;
        };
        if let Some(element) = ElementRef::wrap(node) {
            for selector in preferred_selectors {
                let selector = Selector::parse(selector).expect("selector");
                for candidate in element.select(&selector) {
                    let text = clean_whitespace(&collect_text(candidate));
                    if is_good_teaser_candidate(&text, &link_text) {
                        return trim_chars(&text, 240);
                    }
                }
            }

            for candidate in element.select(&fallback_selector) {
                let text = clean_whitespace(&collect_text(candidate));
                if is_good_teaser_candidate(&text, &link_text) {
                    return trim_chars(&text, 240);
                }
            }
        }
        parent = node.parent();
    }
    String::new()
}

pub(super) fn extract_listing_metadata(
    link: ElementRef<'_>,
    fallback_section: Option<&str>,
    source_url: &str,
) -> ListingMetadata {
    let container = nearest_listing_container(link);
    let mut metadata = ListingMetadata::default();

    if let Some(container) = container {
        let author_selector = Selector::parse(".author-name").expect("selector");
        let date_selector = Selector::parse(".article-date, time").expect("selector");
        let section_selector =
            Selector::parse(".article-type, .article-categories a").expect("selector");

        let mut authors = Vec::new();
        let mut seen_authors = HashSet::new();
        for candidate in container.select(&author_selector) {
            let value = clean_whitespace(&collect_text(candidate));
            if !value.is_empty() && seen_authors.insert(value.clone()) {
                authors.push(value);
            }
        }
        metadata.author = authors.join(", ");

        metadata.date = container
            .select(&date_selector)
            .map(collect_text)
            .map(|value| clean_whitespace(&value))
            .find(|value| !value.is_empty())
            .unwrap_or_default();

        metadata.section = container
            .select(&section_selector)
            .map(collect_text)
            .map(|value| clean_whitespace(&value))
            .find(|value| !value.is_empty() && !value.contains('|'))
            .unwrap_or_default();
    }

    if metadata.section.is_empty() {
        metadata.section = fallback_section
            .map(str::to_owned)
            .or_else(|| extract_context_section(link))
            .unwrap_or_else(|| source_label(source_url));
    }

    metadata
}

pub(super) fn is_good_teaser_candidate(text: &str, link_text: &str) -> bool {
    if text.len() < 18
        || same_enough(text, link_text)
        || text.contains("Artikel lesen")
        || text.contains('|')
    {
        return false;
    }

    let lower = text.to_lowercase();
    !lower.starts_with("von ")
        && !lower.contains("rezension |")
        && !lower.contains("interview |")
        && !lower.contains("essay |")
}

pub(super) fn extract_context_section(link: ElementRef<'_>) -> Option<String> {
    let mut parent = link.parent();
    let selector =
        Selector::parse(".article-type, .article-overline, .article-categories a").ok()?;
    for _ in 0..4 {
        let Some(node) = parent else {
            break;
        };
        if let Some(element) = ElementRef::wrap(node) {
            for candidate in element.select(&selector) {
                let value = clean_whitespace(&collect_text(candidate));
                if !value.is_empty() && !value.contains('|') {
                    return Some(value);
                }
            }
        }
        parent = node.parent();
    }
    None
}

pub(super) fn nearest_listing_container(link: ElementRef<'_>) -> Option<ElementRef<'_>> {
    let marker_selector = Selector::parse(
        ".article-overline, .article-abstract, .article-type, .article-date, .list-text",
    )
    .ok()?;

    link.ancestors()
        .filter_map(ElementRef::wrap)
        .take(6)
        .find(|element| element.select(&marker_selector).next().is_some())
}

pub(super) fn source_label(source_url: &str) -> String {
    SECTIONS
        .iter()
        .find(|section| section.url.trim_end_matches('/') == source_url.trim_end_matches('/'))
        .map(|section| section.label.to_owned())
        .unwrap_or_else(|| {
            infer_section_from_url(source_url).unwrap_or_else(|| "Soziopolis".to_owned())
        })
}

pub(super) fn infer_section_from_url(url: &str) -> Option<String> {
    let path = url.trim_start_matches(BASE_URL).trim_start_matches('/');
    let first = path.split('/').next()?;
    if first.is_empty() {
        return Some("Latest".to_owned());
    }
    Some(first.replace('-', " "))
}

pub(crate) fn build_clean_text(
    title: &str,
    subtitle: &str,
    author: &str,
    date: &str,
    body: &str,
) -> String {
    let normalized_subtitle = clean_whitespace(subtitle);
    let normalized_body = normalize_body_for_lingq(body, title, &normalized_subtitle);

    let mut pieces = vec![title.to_owned()];
    if !normalized_subtitle.is_empty() && !same_enough(&normalized_subtitle, title) {
        pieces.push(String::new());
        pieces.push(normalized_subtitle);
    }
    if !author.is_empty() {
        pieces.push(format!("Von {author}"));
    }
    if !date.is_empty() {
        pieces.push(date.to_owned());
    }
    pieces.push(String::new());
    pieces.push(normalized_body);
    pieces.join("\n")
}

pub fn normalize_article_date(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    for format in ["%d.%m.%Y", "%Y-%m-%d"] {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, format) {
            return Some(date.format("%Y-%m-%d").to_string());
        }
    }

    trimmed
        .get(..10)
        .and_then(|prefix| chrono::NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok())
        .map(|date| date.format("%Y-%m-%d").to_string())
}

pub(super) fn normalize_body_for_lingq(body: &str, title: &str, subtitle: &str) -> String {
    let mut cleaned_blocks = Vec::new();
    for raw_block in body.split("\n\n") {
        let block = clean_whitespace(raw_block);
        if block.is_empty() {
            continue;
        }
        let normalized_block = if let Some(heading) = block.strip_prefix("## ") {
            heading.trim().to_owned()
        } else {
            block
        };
        if same_enough(&normalized_block, title)
            || (!subtitle.is_empty() && same_enough(&normalized_block, subtitle))
        {
            continue;
        }
        cleaned_blocks.push(normalized_block);
    }
    dedupe_similar_blocks(&mut cleaned_blocks);
    cleaned_blocks.join("\n\n")
}

pub(super) fn dedupe_similar_blocks(blocks: &mut Vec<String>) {
    let mut seen = HashSet::new();
    blocks.retain(|block| seen.insert(canonical_text(block)));
}

pub(super) fn canonical_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric() || ch.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(super) fn same_enough(left: &str, right: &str) -> bool {
    let left = canonical_text(left);
    let right = canonical_text(right);
    !left.is_empty() && left == right
}

pub(super) fn first_text(document: &Html, selectors: &[&str]) -> Option<String> {
    for selector in selectors {
        let selector = Selector::parse(selector).ok()?;
        let value = document.select(&selector).find_map(|node| {
            let attr_content = node.value().attr("content").map(clean_whitespace);
            let text_content =
                Some(clean_whitespace(&collect_text(node))).filter(|value| !value.is_empty());
            attr_content.or(text_content)
        });
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

pub(super) fn absolute_url(raw_href: &str) -> String {
    if raw_href.starts_with("http://") || raw_href.starts_with("https://") {
        return raw_href.to_owned();
    }
    if raw_href.starts_with('/') {
        return format!("{BASE_URL}{raw_href}");
    }
    format!("{BASE_URL}/{raw_href}")
}

pub(super) fn looks_like_article_title(title: &str) -> bool {
    title.len() >= 10
        && title.len() <= 220
        && !title.eq_ignore_ascii_case("Artikel lesen")
        && !title.eq_ignore_ascii_case("Essays")
        && !title.eq_ignore_ascii_case("Besprechungen")
        && !title.eq_ignore_ascii_case("Interviews")
        && !title.eq_ignore_ascii_case("Dossiers")
}

pub(super) fn is_excluded_article_url(url: &str) -> bool {
    let path = url.trim_start_matches(BASE_URL).trim_start_matches('/');

    let exact = [
        "index.html",
        "suche.html",
        "newsletter.html",
        "veroeffentlichen.html",
        "kontakt.html",
        "partner.html",
        "ueber-uns.html",
        "rssfeed.xml",
        "texte/essay.html",
        "texte/interview.html",
        "texte/podcast-video.html",
        "besprechungen.html",
        "dossier.html",
        "soziales-leben.html",
        "gesellschaftstheorie-anthropologie.html",
        "politik-zeitgeschichte.html",
        "wirtschaft-recht.html",
        "kultur-medien.html",
        "wissenschaft-technik.html",
        "zeitschriftenschau.html",
    ];

    if exact.contains(&path) {
        return true;
    }

    [
        "autoren/",
        "ausschreibungen/",
        "buchforum/",
        "meta/",
        "fileadmin/",
        "dossier/",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}

pub(super) fn collect_text(node: ElementRef<'_>) -> String {
    node.text().collect::<Vec<_>>().join(" ")
}

pub(super) fn clean_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn trim_chars(input: &str, max: usize) -> String {
    input.chars().take(max).collect()
}
