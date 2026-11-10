use super::*;
use crate::log_cache::QUOTA;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, path, query_param},
};

fn identity() -> LogIdentity {
    LogIdentity {
        workspace: "acme".into(),
        repository: "acme/service".into(),
        root_run_uuid: "root".into(),
        parent_run_uuid: None,
        parent_run_attempt_uuid: None,
        parent_step_uuid: None,
        run_uuid: "root".into(),
        run_number: 1,
        run_attempt: 1,
        step_uuid: "build".into(),
        step_name: "Build".into(),
        step_attempt: 1,
        execution_path: vec!["#1".into(), "Build".into()],
    }
}

#[tokio::test]
async fn unknown_length_full_response_replaces_an_older_longer_log() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    cache
        .put(identity(), 0, b"obsolete content\n".to_vec(), Some(17))
        .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let client = Client::builder()
        .base_url(format!("http://{}/2.0/", listener.local_addr()?))
        .build()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_request(&mut socket).await?;
        socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nnew\r\n4\r\n log\r\n0\r\n\r\n").await?;
        Ok::<_, std::io::Error>(())
    });
    let (progress, _receiver) = mpsc::channel(8);
    assert!(
        !download(
            &cache,
            &client,
            &identity(),
            ByteRange::Suffix(65536),
            LogProgress {
                epoch: 0,
                ticket: 1,
                sender: &progress,
                query: None,
                from: None
            }
        )
        .await?
    );
    let window = cache.window(identity(), None).await?;
    assert!(window.complete);
    assert_eq!(window.total, Some(7));
    assert_eq!(window.lines[0].text, "new log");
    assert!(
        cache
            .search("acme".into(), None, "obsolete".into(), 30)
            .await?
            .is_empty()
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn truncated_log_reloads_tail_after_416() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    cache
        .put(identity(), 0, b"old longer log\n".to_vec(), Some(15))
        .await?;
    let server = MockServer::start().await;
    Mock::given(path(
        "/2.0/repositories/acme/service/pipelines/root/steps/build/log",
    ))
    .and(header("range", "bytes=15-"))
    .respond_with(ResponseTemplate::new(416).insert_header("content-range", "bytes */4"))
    .expect(1)
    .mount(&server)
    .await;
    Mock::given(path(
        "/2.0/repositories/acme/service/pipelines/root/steps/build/log",
    ))
    .and(header("range", "bytes=-65536"))
    .respond_with(
        ResponseTemplate::new(206)
            .insert_header("content-range", "bytes 0-3/4")
            .set_body_string("new\n"),
    )
    .expect(1)
    .mount(&server)
    .await;
    let client = Client::builder()
        .base_url(format!("{}/2.0/", server.uri()))
        .build()?;
    let (progress, _receiver) = mpsc::channel(8);
    assert!(
        download(
            &cache,
            &client,
            &identity(),
            ByteRange::From(15),
            LogProgress {
                epoch: 0,
                ticket: 1,
                sender: &progress,
                query: None,
                from: None
            }
        )
        .await?
    );
    let window = cache.window(identity(), None).await?;
    assert!(window.complete);
    assert_eq!(window.ranges, vec![(0, 4)]);
    assert_eq!(window.lines[0].text, "new");
    Ok(())
}

#[tokio::test]
async fn interrupted_body_never_claims_complete_coverage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let client = Client::builder()
        .base_url(format!("http://{}/2.0/", listener.local_addr()?))
        .build()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_request(&mut socket).await?;
        socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 100\r\nContent-Range: bytes 0-99/100\r\nConnection: close\r\n\r\npartial log\n").await?;
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        Ok::<_, std::io::Error>(())
    });
    let (progress, _receiver) = mpsc::channel(8);
    download(
        &cache,
        &client,
        &identity(),
        ByteRange::Suffix(65536),
        LogProgress {
            epoch: 0,
            ticket: 1,
            sender: &progress,
            query: None,
            from: None,
        },
    )
    .await
    .err()
    .context("short body accepted")?;
    assert!(!cache.window(identity(), None).await?.complete);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn public_workspace_endpoint_and_cached_fallback_are_used() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let server = MockServer::start().await;
    Mock::given(path("/2.0/user/workspaces")).and(query_param("pagelen","50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"values":[{"workspace":{"uuid":"workspace","slug":"acme","name":"Example"}}]}))).expect(1).mount(&server).await;
    let client = Client::builder()
        .base_url(format!("{}/2.0/", server.uri()))
        .build()?;
    let live = read::<WorkspaceAccess>(&cache, Some(&client), &Resource::Workspaces, None).await?;
    assert!(!live.cached);
    assert_eq!(live.page.values[0].workspace.slug, "acme");
    let offline = read::<WorkspaceAccess>(&cache, None, &Resource::Workspaces, None).await?;
    assert!(offline.cached);
    assert_eq!(offline.page.values.len(), 1);
    server.reset().await;
    let unavailable =
        read::<WorkspaceAccess>(&cache, Some(&client), &Resource::Workspaces, None).await?;
    assert!(unavailable.cached);
    assert!(unavailable.warning.is_some());
    Ok(())
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while request.len() < 16 * 1024 {
        let received = socket.read(&mut buffer).await?;
        if received == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request ended before headers",
            ));
        }
        request.extend_from_slice(&buffer[..received]);
        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            return Ok(());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "request headers too large",
    ))
}

#[tokio::test]
#[ignore = "manual release-mode measurement; uses a loopback server and temporary cache"]
async fn benchmark_log_ingestion() -> Result<()> {
    const SIZE: usize = 1024 * 1024;
    for chunk_size in [1024, 65536] {
        let directory = tempfile::tempdir()?;
        let cache =
            Cache::open(directory.path().join("logs.db"), "benchmark".into(), QUOTA).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let client = Client::builder()
            .base_url(format!("http://{}/2.0/", listener.local_addr()?))
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            read_request(&mut socket).await?;
            socket.write_all(format!("HTTP/1.1 206 Partial Content\r\nTransfer-Encoding: chunked\r\nContent-Range: bytes 0-{}/{SIZE}\r\nConnection: close\r\n\r\n", SIZE - 1).as_bytes()).await?;
            let mut chunk = vec![b'x'; chunk_size];
            for line in chunk.chunks_mut(64) {
                line[63] = b'\n';
            }
            for _ in 0..SIZE / chunk_size {
                socket
                    .write_all(format!("{chunk_size:x}\r\n").as_bytes())
                    .await?;
                socket.write_all(&chunk).await?;
                socket.write_all(b"\r\n").await?;
                tokio::task::yield_now().await;
            }
            socket.write_all(b"0\r\n\r\n").await?;
            Ok::<_, std::io::Error>(())
        });
        let (progress, mut receiver) = mpsc::channel(8);
        let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
        let started = std::time::Instant::now();
        download(
            &cache,
            &client,
            &identity(),
            ByteRange::From(0),
            LogProgress {
                epoch: 0,
                ticket: 1,
                sender: &progress,
                query: None,
                from: None,
            },
        )
        .await?;
        eprintln!(
            "log_ingestion bytes={SIZE} chunk_bytes={chunk_size} elapsed_ms={}",
            started.elapsed().as_millis()
        );
        let window = cache.window(identity(), None).await?;
        assert!(window.complete);
        assert_eq!(window.ranges, vec![(0, SIZE as u64)]);
        drop(progress);
        drain.await?;
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn small_partial_chunk_flushes_during_silence_without_repeated_snapshots() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(directory.path().join("logs.db"), "account".into(), QUOTA).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let client = Client::builder()
        .base_url(format!("http://{}/2.0/", listener.local_addr()?))
        .build()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_request(&mut socket).await?;
        socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-11/12\r\nContent-Length: 12\r\nConnection: close\r\n\r\nneedle").await?;
        std::future::pending::<()>().await;
        Ok::<_, std::io::Error>(())
    });
    let (sender, mut receiver) = mpsc::channel(8);
    let writer_cache = cache.clone();
    let writer = tokio::spawn(async move {
        download(
            &writer_cache,
            &client,
            &identity(),
            ByteRange::From(0),
            LogProgress {
                epoch: 0,
                ticket: 1,
                sender: &sender,
                query: Some(LogQuery::new("needle")?),
                from: None,
            },
        )
        .await
    });
    let progress = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
        .await?
        .context("missing progress")?;
    let Response::Log {
        window, finished, ..
    } = progress.response
    else {
        bail!("expected log snapshot");
    };
    assert!(!finished);
    assert!(!window.complete);
    assert_eq!(window.ranges, [(0, 6)]);
    assert_eq!(window.lines[0].highlights.len(), 1);
    assert_eq!(window.lines[0].highlights[0], 0..6);
    tokio::time::timeout(std::time::Duration::from_millis(250), receiver.recv())
        .await
        .err()
        .context("unchanged log snapshot repeated")?;
    writer.abort();
    assert!(
        writer
            .await
            .err()
            .context("writer was not cancelled")?
            .is_cancelled()
    );
    server.abort();
    assert!(
        server
            .await
            .err()
            .context("server was not cancelled")?
            .is_cancelled()
    );
    assert_eq!(cache.window(identity(), None).await?.ranges, [(0, 6)]);
    Ok(())
}
