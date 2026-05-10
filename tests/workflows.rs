use soziopolis_lingq_tool::{
    app_ops,
    context::AppContext,
    database::SharedDatabase,
    jobs::{
        CompletedJob, FailedFetchItem, JobKind, QueueSnapshot, QueuedJob, QueuedJobRequest,
        UploadFailure,
    },
    repositories::JobRepository,
    soziopolis::{Article, ArticleSummary, DiscoverySourceKind},
};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_db_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}_{unique}.sqlite"))
}

fn temp_context(prefix: &str) -> (AppContext, PathBuf) {
    let path = temp_db_path(prefix);
    let db = SharedDatabase::open(&path).expect("shared database should open");
    (AppContext::new(db), path)
}

fn sample_article(url: &str, title: &str, section: &str, word_count: usize) -> Article {
    Article {
        url: url.to_owned(),
        title: title.to_owned(),
        subtitle: "Subtitle".to_owned(),
        teaser: "Short summary for previewing".to_owned(),
        author: "Workflow Test".to_owned(),
        date: "2026-04-23".to_owned(),
        published_at: "2026-04-23".to_owned(),
        section: section.to_owned(),
        source_kind: "section".to_owned(),
        source_label: section.to_owned(),
        body_text: "First paragraph.\n\nSecond paragraph.".to_owned(),
        clean_text: String::new(),
        word_count,
        fetched_at: "2026-04-23T12:00:00Z".to_owned(),
    }
}

#[test]
fn refresh_content_reports_saved_article_urls_and_stats() {
    let (ctx, path) = temp_context("soziopolis_workflow_refresh");
    ctx.db
        .with_db(|db| {
            db.save_article(&sample_article(
                "https://example.com/one",
                "One",
                "Essay",
                1200,
            ))
        })
        .expect("article should save");

    let refreshed = app_ops::refresh_content(&ctx).expect("refresh should succeed");
    let imported_urls = refreshed
        .imported_urls
        .expect("imported url set should be available");
    let library_articles = refreshed
        .library_articles
        .expect("library cards should be available");
    let stats = refreshed.library_stats.expect("stats should be available");

    assert!(imported_urls.contains("https://example.com/one"));
    assert_eq!(library_articles.len(), 1);
    assert_eq!(library_articles[0].title, "One");
    assert_eq!(stats.total_articles, 1);
    assert_eq!(stats.average_word_count, 1200);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
}

#[test]
fn delete_article_updates_follow_up_refresh() {
    let (ctx, path) = temp_context("soziopolis_workflow_delete");
    let article_id = ctx
        .db
        .with_db(|db| {
            db.save_article(&sample_article(
                "https://example.com/delete",
                "Delete Me",
                "Essay",
                950,
            ))
        })
        .expect("article should save");

    app_ops::delete_article(&ctx, article_id).expect("delete should succeed");
    let refreshed = app_ops::refresh_content(&ctx).expect("refresh should succeed");

    assert!(
        refreshed
            .imported_urls
            .expect("imported urls should load")
            .is_empty()
    );
    assert!(
        refreshed
            .library_articles
            .expect("library articles should load")
            .is_empty()
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
}

#[test]
fn queue_snapshot_and_history_round_trip_through_context() {
    let (ctx, path) = temp_context("soziopolis_workflow_queue");
    let snapshot = QueueSnapshot {
        next_job_id: 12,
        queue_paused: true,
        queued_jobs: vec![QueuedJob {
            id: 12,
            kind: JobKind::Import,
            label: "Import queued".to_owned(),
            total: 1,
            request: QueuedJobRequest::Import {
                articles: vec![ArticleSummary {
                    url: "https://example.com/import".to_owned(),
                    title: "Import Me".to_owned(),
                    teaser: "Teaser".to_owned(),
                    author: "Tester".to_owned(),
                    date: "23.04.2026".to_owned(),
                    section: "Essay".to_owned(),
                    source_kind: DiscoverySourceKind::Section,
                    source_label: "Essays".to_owned(),
                }],
            },
        }],
        completed_jobs: vec![],
        failed_fetches: vec![FailedFetchItem {
            url: "https://example.com/fail".to_owned(),
            title: "Failed".to_owned(),
            category: "network".to_owned(),
            message: "timed out".to_owned(),
        }],
        failed_uploads: vec![UploadFailure {
            article_id: 7,
            title: "Upload failed".to_owned(),
            message: "unauthorized".to_owned(),
        }],
    };

    ctx.db
        .with_db(|db| {
            let mut repository = JobRepository::new(db);
            repository.save_snapshot(&snapshot)
        })
        .expect("queue snapshot should save");

    ctx.db
        .with_db(|db| {
            let repository = JobRepository::new(db);
            repository.record_completed_job_history(&CompletedJob {
                id: 11,
                kind: JobKind::Upload,
                label: "Upload completed".to_owned(),
                summary: "Uploaded 1, failed 0".to_owned(),
                success: true,
                recorded_at: "1710000000".to_owned(),
            })
        })
        .expect("completed job history should save");

    let (restored_snapshot, history) = ctx
        .db
        .with_db(|db| {
            let repository = JobRepository::new(db);
            Ok((
                repository.load_snapshot()?,
                repository.list_completed_job_history(5)?,
            ))
        })
        .expect("queue snapshot and history should load");

    assert_eq!(restored_snapshot.next_job_id, 12);
    assert!(restored_snapshot.queue_paused);
    assert_eq!(restored_snapshot.queued_jobs.len(), 1);
    assert_eq!(
        restored_snapshot.failed_fetches[0].url,
        "https://example.com/fail"
    );
    assert_eq!(restored_snapshot.failed_fetches[0].title, "Failed");
    assert_eq!(restored_snapshot.failed_fetches[0].category, "network");
    assert_eq!(restored_snapshot.failed_fetches[0].message, "timed out");
    assert_eq!(restored_snapshot.failed_uploads.len(), 1);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].label, "Upload completed");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
}
