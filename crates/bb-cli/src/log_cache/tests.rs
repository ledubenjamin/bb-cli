use super::*;
use crate::discovery::{CatalogItem, CatalogKind, CatalogQuery};

fn catalog_query(workspace: &str, text: &str) -> CatalogQuery {
    CatalogQuery {
        workspace: workspace.into(),
        kind: CatalogKind::Repositories,
        project: None,
        text: text.into(),
    }
}

fn catalog_repository(workspace: &str, slug: &str, name: &str) -> CatalogItem {
    CatalogItem::Repository(Box::new(bb_api::models::Repository {
        full_name: format!("{workspace}/{slug}"),
        slug: slug.into(),
        name: name.into(),
        project: Some(bb_api::models::Project {
            key: "APP".into(),
            name: "Applications".into(),
            ..Default::default()
        }),
        ..Default::default()
    }))
}

#[tokio::test]
async fn catalog_is_literal_unicode_aware_scoped_and_survives_log_clear() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("catalog.db");
    let cache = Cache::open(path.clone(), "account-a".into(), QUOTA).await?;
    cache
        .index_catalog(
            "acme".into(),
            vec![
                catalog_repository("acme", "service", "Équipe %_"),
                catalog_repository("acme", "worker", "Worker"),
            ],
            now(),
        )
        .await?;
    cache
        .index_catalog(
            "other".into(),
            vec![catalog_repository("other", "service", "Équipe")],
            now(),
        )
        .await?;
    for text in ["éQUIPE", "%_", "ACME/SERVICE"] {
        assert_eq!(
            cache
                .find_catalog(catalog_query("acme", text))
                .await?
                .values
                .len(),
            1
        );
    }
    assert!(
        cache
            .find_catalog(catalog_query("acme", "\" OR 1=1"))
            .await?
            .values
            .is_empty()
    );
    let mut query = catalog_query("acme", "applications");
    query.project = Some("APP".into());
    assert_eq!(cache.find_catalog(query.clone()).await?.values.len(), 2);
    query.project = Some("OTHER".into());
    assert!(cache.find_catalog(query).await?.values.is_empty());
    cache.clear("acme".into()).await?;
    assert_eq!(
        cache
            .find_catalog(catalog_query("acme", ""))
            .await?
            .values
            .len(),
        2
    );
    let other = Cache::open(path, "account-b".into(), QUOTA).await?;
    assert!(
        other
            .find_catalog(catalog_query("acme", ""))
            .await?
            .values
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn catalog_upgrades_old_pages_without_refreshing_retention() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("catalog.db");
    let cache = Cache::open(path.clone(), "account".into(), QUOTA).await?;
    let CatalogItem::Repository(repository) = catalog_repository("acme", "service", "Service")
    else {
        unreachable!()
    };
    cache
        .save(
            EntityKind::Repositories,
            "acme:APP".into(),
            &[*repository],
            None,
        )
        .await?;
    assert_eq!(
        cache
            .find_catalog(catalog_query("acme", "service"))
            .await?
            .values
            .len(),
        1
    );
    let old = now().saturating_sub(RETENTION + 10);
    cache
        .index_catalog(
            "acme".into(),
            vec![catalog_repository("acme", "expired", "Expired")],
            old,
        )
        .await?;
    assert!(
        cache
            .find_catalog(catalog_query("acme", "expired"))
            .await?
            .values
            .is_empty()
    );
    // A late legacy snapshot must not replace fresher metadata in the catalog.
    cache
        .index_catalog(
            "acme".into(),
            vec![catalog_repository("acme", "service", "Fresh")],
            now(),
        )
        .await?;
    cache
        .index_catalog(
            "acme".into(),
            vec![catalog_repository("acme", "service", "Stale")],
            old,
        )
        .await?;
    assert_eq!(
        cache
            .find_catalog(catalog_query("acme", "fresh"))
            .await?
            .values
            .len(),
        1
    );
    assert!(
        cache
            .find_catalog(catalog_query("acme", "stale"))
            .await?
            .values
            .is_empty()
    );
    Ok(())
}

fn identity() -> LogIdentity {
    LogIdentity {
        workspace: "acme".into(),
        repository: "acme/service".into(),
        root_run_uuid: "parent".into(),
        parent_run_uuid: Some("parent".into()),
        parent_run_attempt_uuid: None,
        parent_step_uuid: None,
        run_uuid: "child".into(),
        run_number: 178,
        run_attempt: 2,
        step_uuid: "compile".into(),
        step_name: "Compile".into(),
        step_attempt: 2,
        execution_path: vec!["#177".into(), "#178".into(), "Compile".into()],
    }
}

#[tokio::test]
async fn reassembles_overlaps_unicode_ansi_and_partial_lines() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let raw = "setup\n\u{1b}[31mÉchec de compilation\u{1b}[0m\nlast line\n".as_bytes();
    let total = raw.len() as u64;
    let split = "setup\n\u{1b}[31mÉ".len() - 1;
    // The first portion begins in the middle of a UTF-8 character.
    cache
        .put(identity(), split as u64, raw[split..].to_vec(), Some(total))
        .await?;
    assert!(!cache.window(identity(), None).await?.complete);
    cache
        .put(identity(), 0, raw[..split + 6].to_vec(), Some(total))
        .await?;
    cache
        .put(identity(), split as u64, raw[split..].to_vec(), Some(total))
        .await?;
    let window = cache.window(identity(), None).await?;
    assert!(window.complete);
    assert_eq!(window.ranges, vec![(0, total)]);
    assert_eq!(
        window
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        ["setup", "Échec de compilation", "last line"]
    );
    let matches = cache
        .search("acme".into(), None, "éCHEC".into(), 30)
        .await?;
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].byte_offset, 11);
    assert!(matches[0].log_complete);
    Ok(())
}

#[tokio::test]
async fn disjoint_ranges_never_invent_text_and_truncation_replaces_the_index() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    cache
        .put(identity(), 0, b"begin error\n".to_vec(), Some(100))
        .await?;
    cache
        .put(identity(), 90, b"end error\n".to_vec(), Some(100))
        .await?;
    assert_eq!(
        cache
            .search("acme".into(), None, "error".into(), 30)
            .await?
            .len(),
        2
    );
    assert!(!cache.window(identity(), None).await?.complete);
    cache
        .put(identity(), 0, b"new result\n".to_vec(), Some(11))
        .await?;
    assert!(
        cache
            .search("acme".into(), None, "error".into(), 30)
            .await?
            .is_empty()
    );
    assert!(cache.window(identity(), None).await?.complete);
    Ok(())
}

#[tokio::test]
async fn long_line_boundary_matches_are_searchable_once() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let mut raw = vec![b'x'; BLOCK_SIZE * 3];
    let needle = b"needle_unique";
    let at = BLOCK_SIZE - 4;
    raw[at..at + needle.len()].copy_from_slice(needle);
    let overlap = BLOCK_SIZE * 2 - 3000;
    raw[overlap..overlap + needle.len()].copy_from_slice(needle);
    for (index, chunk) in raw.chunks(BLOCK_SIZE).enumerate() {
        cache
            .put(
                identity(),
                (index * BLOCK_SIZE) as u64,
                chunk.to_vec(),
                Some(raw.len() as u64),
            )
            .await?;
    }
    let matches = cache
        .search("acme".into(), None, "NEEDLE_UNIQUE".into(), 30)
        .await?;
    assert_eq!(
        matches
            .iter()
            .map(|hit| hit.byte_offset)
            .collect::<Vec<_>>(),
        [at as u64, overlap as u64]
    );
    let window = cache.window(identity(), None).await?;
    assert_eq!(
        window
            .lines
            .iter()
            .map(|line| line.text.len())
            .sum::<usize>(),
        raw.len()
    );
    assert!(window.complete);
    Ok(())
}

#[tokio::test]
async fn accounts_workspaces_repositories_and_attempts_are_isolated() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("logs.db");
    let first = Cache::open(path.clone(), "account-a".into(), QUOTA).await?;
    let second = Cache::open(path, "account-b".into(), QUOTA).await?;
    first
        .put(identity(), 0, b"cached secret".to_vec(), Some(13))
        .await?;
    assert!(
        second
            .search("acme".into(), None, "secret".into(), 30)
            .await?
            .is_empty()
    );
    assert!(
        first
            .search("elsewhere".into(), None, "secret".into(), 30)
            .await?
            .is_empty()
    );
    assert!(
        first
            .search(
                "acme".into(),
                Some("acme/other".into()),
                "secret".into(),
                30
            )
            .await?
            .is_empty()
    );
    let mut attempt = identity();
    attempt.run_attempt = 3;
    first
        .put(attempt.clone(), 0, b"new attempt".to_vec(), Some(11))
        .await?;
    assert_eq!(
        first
            .search("acme".into(), None, "secret".into(), 30)
            .await?
            .len(),
        1
    );
    assert!(
        first
            .search_step("acme".into(), None, Some(attempt), "secret".into(), 30)
            .await?
            .is_empty()
    );
    assert_eq!(first.clear("acme".into()).await?, 2);
    assert!(
        first
            .search("acme".into(), None, "secret".into(), 30)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn expiration_is_based_on_download_and_quota_evicts_old_logs() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(
        directory.path().join("logs.db"),
        "account".into(),
        4 * 1024 * 1024,
    )
    .await?;
    cache
        .put(identity(), 0, b"obsolete entry".to_vec(), Some(14))
        .await?;
    cache
        .call(|db, _| {
            db.connection.execute("UPDATE logs SET updated=0", [])?;
            Ok(())
        })
        .await?;
    cache
        .save(EntityKind::Projects, "acme".into(), &["project"], None)
        .await?;
    assert!(cache.window(identity(), None).await?.lines.is_empty());
    for attempt in 1..=35 {
        let mut id = identity();
        id.step_attempt = attempt;
        cache
            .put(id, 0, vec![b'x'; BLOCK_SIZE], Some(BLOCK_SIZE as u64))
            .await?;
    }
    let retained: u32 = cache
        .call(|db, _| {
            Ok(db
                .connection
                .query_row("SELECT count(*) FROM logs", [], |row| row.get(0))?)
        })
        .await?;
    assert!(retained > 0 && retained < 35);
    Ok(())
}

#[test]
fn strips_terminal_commands_and_preserves_literal_search() -> Result<()> {
    assert_eq!(
        text::clean(b"safe\x1b]52;c;payload\x07\x1b[2J done\xff"),
        "safe done�"
    );
    assert_eq!(text::clean(b"\x1bPprivate\x1b\\text"), "text");
    validate_query("xy").err().context("short query accepted")?;
    validate_query("line\nbreak")
        .err()
        .context("multiline query accepted")?;
    validate_query("\" OR *")?;
    Ok(())
}

#[tokio::test]
async fn cancelling_a_long_query_does_not_interrupt_the_next_request() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let pending = cache.clone();
    let job = tokio::spawn(async move {
        pending.call(|db,_| {
            let total: i64 = db.connection.query_row("WITH RECURSIVE n(x) AS (SELECT 0 UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n",[],|row|row.get(0))?;
            Ok(total)
        }).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while *cache
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    job.abort();
    job.await.err().context("query was not cancelled")?;
    let results = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        cache.search("acme".into(), None, "needle".into(), 30),
    )
    .await??;
    assert!(results.is_empty());
    Ok(())
}

#[tokio::test]
async fn cancellation_before_sql_starts_does_not_block_the_next_request() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let pending = cache.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (proceed, wait) = std::sync::mpsc::channel();
    let job = tokio::spawn(async move {
        pending.call(move |db, _| {
            let _ = started.send(());
            // Hold the worker after accepting the job, but before any SQL runs.
            wait.recv_timeout(std::time::Duration::from_secs(5))?;
            let total: i64 = db.connection.query_row(
                "WITH RECURSIVE n(x) AS (SELECT 0 UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n",
                [], |row| row.get(0),
            )?;
            Ok(total)
        }).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), ready).await??;
    job.abort();
    job.await.err().context("query was not cancelled")?;
    proceed.send(())?;
    let results = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        cache.search("acme".into(), None, "needle".into(), 30),
    )
    .await??;
    assert!(results.is_empty());
    Ok(())
}

#[tokio::test]
async fn concurrent_connections_merge_portions_and_remain_within_disk_budget() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("logs.db");
    let quota = 8 * 1024 * 1024;
    let first = Cache::open(path.clone(), "account".into(), quota).await?;
    let second = Cache::open(path.clone(), "account".into(), quota).await?;
    for step in 0..20 {
        let mut id = identity();
        id.step_uuid = format!("step-{step}");
        let data: Vec<u8> = (0..BLOCK_SIZE)
            .map(|index| {
                b'a' + u8::try_from((index * 7919 + index / 7 + step) % 26).unwrap_or_default()
            })
            .collect();
        tokio::try_join!(
            first.put(id.clone(), 0, data.clone(), Some((BLOCK_SIZE * 2) as u64)),
            second.put(
                id.clone(),
                BLOCK_SIZE as u64,
                data,
                Some((BLOCK_SIZE * 2) as u64)
            )
        )?;
        assert!(first.window(id, None).await?.complete);
        let disk = std::fs::metadata(&path)?.len()
            + std::fs::metadata(path.with_file_name("logs.db-wal"))
                .map_or(0, |metadata| metadata.len());
        assert!(
            disk <= quota,
            "cache used {disk} bytes for a {quota}-byte budget"
        );
    }
    Ok(())
}

#[tokio::test]
async fn unicode_search_offsets_snippets_and_highlights_share_fts_semantics() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let cases = [
        ("ΑΒΣ", "αβς"),
        ("µmm", "μmm"),
        ("İABC", "ABC"),
        ("Straße", "straße"),
        ("Échec", "éCHEC"),
        ("a\"b", "a\"b"),
        ("e\u{301}chec", "e\u{301}chec"),
    ];
    for (source, query) in cases {
        let prefix = format!("{}\x1b[31m", "İ".repeat(100));
        let raw = format!("{prefix}{source}\x1b[0m then {source}\n");
        cache.restart(identity()).await?;
        cache
            .put(
                identity(),
                0,
                raw.as_bytes().to_vec(),
                Some(raw.len() as u64),
            )
            .await?;
        let hits = cache.search("acme".into(), None, query.into(), 30).await?;
        assert_eq!(hits.len(), 1, "{query}");
        let within = if source == "İABC" { "İ".len() } else { 0 };
        assert_eq!(
            hits[0].byte_offset,
            (prefix.len() + within) as u64,
            "{query}"
        );
        assert!(hits[0].text.contains(source));
        let window = cache
            .window_matching(identity(), None, Some(LogQuery::new(query)?))
            .await?;
        let line = &window.lines[0];
        assert_eq!(line.highlights.len(), 2, "{query}");
        assert_eq!(line.highlights[0].start, "İ".repeat(100).len() + within);
        for span in &line.highlights {
            assert_eq!(&line.text[span.clone()], &source[within..]);
        }
    }
    Ok(())
}

#[tokio::test]
async fn first_known_total_invalidates_older_unbounded_coverage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    cache
        .put(identity(), 0, b"new\nobsolete secret\n".to_vec(), None)
        .await?;
    cache.put(identity(), 0, b"new\n".to_vec(), Some(4)).await?;
    let window = cache.window(identity(), None).await?;
    assert_eq!(window.ranges, [(0, 4)]);
    assert!(window.complete);
    assert_eq!(
        cache
            .search("acme".into(), None, "obsolete".into(), 30)
            .await?
            .len(),
        0
    );
    Ok(())
}

#[tokio::test]
async fn highlighted_offsets_survive_invalid_utf8_ansi_and_long_line_overlap() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let mut raw = vec![b'x'; BLOCK_SIZE * 2];
    raw[..7].copy_from_slice(b"\xff\x1b[31mZ");
    let at = BLOCK_SIZE - 3;
    raw[at..at + 6].copy_from_slice(b"NEEDLE");
    for (i, chunk) in raw.chunks(BLOCK_SIZE).enumerate() {
        cache
            .put(
                identity(),
                (i * BLOCK_SIZE) as u64,
                chunk.to_vec(),
                Some(raw.len() as u64),
            )
            .await?;
    }
    let hits = cache
        .search("acme".into(), None, "needle".into(), 30)
        .await?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].byte_offset, at as u64);
    let window = cache
        .window_matching(identity(), None, Some(LogQuery::new("needle")?))
        .await?;
    let highlighted: String = window
        .lines
        .iter()
        .flat_map(|line| line.highlights.iter().map(|span| &line.text[span.clone()]))
        .collect();
    assert_eq!(highlighted, "NEEDLE");
    Ok(())
}

#[tokio::test]
async fn multibyte_character_at_fragment_boundary_is_rendered_and_highlighted_once() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let mut raw = vec![b'x'; BLOCK_SIZE - 1];
    raw.extend_from_slice("éNEEDLE".as_bytes());
    for (i, chunk) in raw.chunks(BLOCK_SIZE).enumerate() {
        cache
            .put(
                identity(),
                (i * BLOCK_SIZE) as u64,
                chunk.to_vec(),
                Some(raw.len() as u64),
            )
            .await?;
    }
    let hits = cache
        .search("acme".into(), None, "éneedle".into(), 30)
        .await?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].byte_offset, (BLOCK_SIZE - 1) as u64);
    let window = cache
        .window_matching(identity(), None, Some(LogQuery::new("éneedle")?))
        .await?;
    let rendered: String = window.lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(rendered.as_bytes(), raw);
    let highlighted: String = window
        .lines
        .iter()
        .flat_map(|line| line.highlights.iter().map(|span| &line.text[span.clone()]))
        .collect();
    assert_eq!(highlighted, "éNEEDLE");
    Ok(())
}

#[tokio::test]
async fn ansi_sequence_crossing_fragment_boundary_keeps_source_offsets() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let mut raw = vec![b'x'; BLOCK_SIZE - 2];
    raw.extend_from_slice(b"\x1b[31mNEEDLE\x1b[0m");
    for (i, chunk) in raw.chunks(BLOCK_SIZE).enumerate() {
        cache
            .put(
                identity(),
                (i * BLOCK_SIZE) as u64,
                chunk.to_vec(),
                Some(raw.len() as u64),
            )
            .await?;
    }
    let hits = cache
        .search("acme".into(), None, "needle".into(), 30)
        .await?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].byte_offset, (BLOCK_SIZE + 3) as u64);
    let window = cache
        .window_matching(identity(), None, Some(LogQuery::new("needle")?))
        .await?;
    let rendered: String = window.lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(rendered, format!("{}NEEDLE", "x".repeat(BLOCK_SIZE - 2)));
    let highlighted: String = window
        .lines
        .iter()
        .flat_map(|line| line.highlights.iter().map(|span| &line.text[span.clone()]))
        .collect();
    assert_eq!(highlighted, "NEEDLE");
    Ok(())
}

#[tokio::test]
async fn concurrent_rewrites_keep_window_text_and_highlights_consistent() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("logs.db");
    let writer = Cache::open(path.clone(), "account".into(), QUOTA).await?;
    let reader = Cache::open(path, "account".into(), QUOTA).await?;
    let writing = async {
        for iteration in 0..16 {
            let mut raw = if iteration % 2 == 0 {
                "é".repeat(BLOCK_SIZE).into_bytes()
            } else {
                vec![b'x'; BLOCK_SIZE * 2]
            };
            raw[BLOCK_SIZE + 100] = 0;
            raw.extend_from_slice(b"needle");
            for (i, bytes) in raw.chunks(BLOCK_SIZE).enumerate() {
                writer
                    .put(
                        identity(),
                        (i * BLOCK_SIZE) as u64,
                        bytes.to_vec(),
                        Some(raw.len() as u64),
                    )
                    .await?;
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let reading = async {
        for _ in 0..80 {
            let window = reader
                .window_matching(identity(), None, Some(LogQuery::new("needle")?))
                .await?;
            for line in window.lines {
                for span in line.highlights {
                    assert_eq!(line.text.get(span), Some("needle"));
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::try_join!(writing, reading)?;
    Ok(())
}

#[tokio::test]
#[ignore = "manual release-mode measurement; uses a temporary cache"]
async fn measure_highlighted_window() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let raw = format!("{} needle\n", "x".repeat(503))
        .repeat(2000)
        .into_bytes();
    for (i, chunk) in raw.chunks(BLOCK_SIZE).enumerate() {
        cache
            .put(
                identity(),
                (i * BLOCK_SIZE) as u64,
                chunk.to_vec(),
                Some(raw.len() as u64),
            )
            .await?;
    }
    for _ in 0..3 {
        let start = std::time::Instant::now();
        let window = cache
            .window_matching(identity(), None, Some(LogQuery::new("needle")?))
            .await?;
        eprintln!(
            "highlight_window lines={} elapsed_us={}",
            window.lines.len(),
            start.elapsed().as_micros()
        );
        assert_eq!(window.lines.len(), 2000);
        assert!(window.lines.iter().all(|line| line.highlights.len() == 1));
    }
    Ok(())
}
