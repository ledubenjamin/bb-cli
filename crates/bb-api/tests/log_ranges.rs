use bb_api::{ByteRange, Client};
use std::error::Error;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn client(server: &MockServer) -> Result<Client> {
    Ok(Client::builder()
        .base_url(format!("{}/2.0/", server.uri()))
        .token("test-secret")
        .build()?)
}

#[tokio::test]
async fn suffix_range_preserves_offsets_and_uncompressed_source_bytes() -> Result {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/log"))
        .and(header("range", "bytes=-5"))
        .and(header("accept-encoding", "identity"))
        .and(header("authorization", "Bearer test-secret"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 95-99/100")
                .set_body_bytes(b"hello".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut response = client(&server)?
        .log_range("log", ByteRange::Suffix(5))
        .await?;
    assert_eq!(response.status(), 206);
    assert_eq!(response.range(), Some(95..100));
    assert_eq!(response.total(), Some(100));
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        bytes.extend(chunk);
    }
    assert_eq!(bytes, b"hello");
    Ok(())
}

#[tokio::test]
async fn full_response_is_identified_when_server_ignores_range() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/log"))
        .respond_with(ResponseTemplate::new(200).set_body_string("complete log"))
        .mount(&server)
        .await;
    let response = client(&server)?
        .log_range("log", ByteRange::Inclusive { start: 50, end: 70 })
        .await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.range(), None);
    assert_eq!(response.total(), Some(12));
    Ok(())
}

#[tokio::test]
async fn unchanged_and_unsatisfied_ranges_are_typed_responses() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/unchanged"))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;
    Mock::given(path("/2.0/eof"))
        .respond_with(ResponseTemplate::new(416).insert_header("content-range", "bytes */100"))
        .mount(&server)
        .await;
    Mock::given(path("/2.0/no-total"))
        .respond_with(ResponseTemplate::new(416).set_body_string("error body is not the log size"))
        .mount(&server)
        .await;
    let client = client(&server)?;
    let unchanged = client.log_range("unchanged", ByteRange::From(100)).await?;
    assert_eq!(unchanged.status(), 304);
    assert_eq!(unchanged.total(), None);
    let eof = client.log_range("eof", ByteRange::From(100)).await?;
    assert_eq!(eof.status(), 416);
    assert_eq!(eof.total(), Some(100));
    assert_eq!(eof.range(), None);
    assert_eq!(
        client
            .log_range("no-total", ByteRange::From(100))
            .await?
            .total(),
        None
    );
    Ok(())
}

#[tokio::test]
async fn invalid_partial_metadata_and_unsafe_redirects_are_rejected() -> Result {
    let server = MockServer::start().await;
    let foreign = MockServer::start().await;
    Mock::given(path("/2.0/missing"))
        .respond_with(ResponseTemplate::new(206).set_body_string("missing Content-Range"))
        .mount(&server)
        .await;
    Mock::given(path("/2.0/invalid"))
        .respond_with(ResponseTemplate::new(206).insert_header("content-range", "bytes 20-10/30"))
        .mount(&server)
        .await;
    Mock::given(path("/2.0/redirect"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/stolen", foreign.uri())),
        )
        .mount(&server)
        .await;
    let client = client(&server)?;
    for path in ["missing", "invalid", "redirect"] {
        client
            .log_range(path, ByteRange::Suffix(10))
            .await
            .err()
            .ok_or("unsafe or invalid response was accepted")?;
    }
    assert!(
        foreign
            .received_requests()
            .await
            .ok_or("missing request recorder")?
            .is_empty()
    );
    client
        .log_range("invalid-request", ByteRange::Suffix(0))
        .await
        .err()
        .ok_or("empty range was accepted")?;
    Ok(())
}

#[tokio::test]
async fn same_origin_redirect_preserves_only_the_requested_range() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/log"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/2.0/contents"))
        .mount(&server)
        .await;
    Mock::given(path("/2.0/contents"))
        .and(header("range", "bytes=2-4"))
        .and(header("accept-encoding", "identity"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 2-4/5")
                .set_body_string("abc"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let response = client(&server)?
        .log_range("log", ByteRange::Inclusive { start: 2, end: 4 })
        .await?;
    assert_eq!(response.range(), Some(2..5));
    Ok(())
}
