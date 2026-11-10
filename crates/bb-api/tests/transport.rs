use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bb_api::{BranchName, Client, CommentId, Method, PullRequestId, RepoRef, TaskId, Workspace};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn client(server: &MockServer) -> Result<Client> {
    Ok(Client::builder()
        .base_url(format!("{}/2.0/", server.uri()))
        .token("test-secret")
        .build()?)
}

#[test]
fn repository_and_identifier_validation() -> Result {
    let expected: RepoRef = "acme/widgets".parse()?;
    for input in [
        "acme/widgets",
        "https://bitbucket.org/acme/widgets",
        "git@bitbucket.org:acme/widgets.git",
        "ssh://git@altssh.bitbucket.org:443/acme/widgets.git",
    ] {
        assert_eq!(input.parse::<RepoRef>()?, expected);
    }
    for input in [
        "a",
        "a/b/c",
        "a/..",
        "a/%2e%2e",
        "a/b?x=1",
        "https://evil.test/a/b",
        "https://token@bitbucket.org/a/b",
        "https://bitbucket.org/a/b#frag",
    ] {
        assert!(input.parse::<RepoRef>().is_err(), "{input}");
    }
    for input in [
        "../escape",
        "-option",
        "bad..name",
        "a/.hidden",
        "a.lock/b",
        "refs//x",
        "a@{1}",
        "a b",
    ] {
        assert!(input.parse::<BranchName>().is_err(), "{input}");
    }
    assert_eq!("feature/é".parse::<BranchName>()?.as_str(), "feature/é");
    "0".parse::<PullRequestId>()
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    "-1".parse::<CommentId>()
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    "18446744073709551616"
        .parse::<TaskId>()
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    serde_json::from_value::<RepoRef>(json!("../x"))
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    serde_json::from_value::<Workspace>(json!("bad/name"))
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    assert_eq!("42".parse::<PullRequestId>()?.get(), 42);
    Ok(())
}

#[tokio::test]
async fn pagination_follows_next_and_enforces_total_limit() -> Result {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/things"))
        .and(query_param("pagelen", "3"))
        .and(header("authorization", "Bearer test-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"values":[1,2],"next":format!("{}/2.0/things?cursor=opaque", server.uri())}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/2.0/things"))
        .and(query_param("cursor", "opaque"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"values":[3,4],"next":"https://untrusted.invalid/steal"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server)?.collection::<u32>("things", &[], 3).await?,
        vec![1, 2, 3]
    );
    Ok(())
}

#[tokio::test]
async fn foreign_pagination_and_redirects_never_receive_credentials() -> Result {
    let server = MockServer::start().await;
    let foreign = MockServer::start().await;
    Mock::given(path("/2.0/items"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"values":[],"next":format!("{}/2.0/stolen", foreign.uri())})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/2.0/redirect"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/2.0/stolen", foreign.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server)?;
    client
        .collection::<Value>("items", &[], 5)
        .await
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    client
        .raw(Method::Get, "redirect", &[], None, &[])
        .await
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    assert!(
        foreign
            .received_requests()
            .await
            .ok_or("request recording unavailable")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn same_origin_redirects_work_and_pagination_cycles_fail() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/redirect"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/2.0/final"))
        .mount(&server)
        .await;
    Mock::given(path("/2.0/final"))
        .and(header("authorization", "Bearer test-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
        .expect(1)
        .mount(&server)
        .await;
    let repeated = format!("{}/2.0/cycle?pagelen=5", server.uri());
    Mock::given(path("/2.0/cycle"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"values":[],"next":repeated})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server)?;
    assert_eq!(
        client.resource::<Value>("redirect").await?,
        json!({"ok":true})
    );
    client
        .collection::<Value>("cycle", &[], 5)
        .await
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    Ok(())
}

#[tokio::test]
async fn transient_reads_retry_but_writes_are_sent_once() -> Result {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    Mock::given(method("GET"))
        .and(path("/2.0/retry"))
        .respond_with(move |_: &wiremock::Request| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"ok":true}))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/2.0/retry"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server)?;
    assert_eq!(client.resource::<Value>("retry").await?["ok"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    client
        .mutate::<Value, _>(Method::Post, "retry", Some(&json!({"title":"once"})))
        .await
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    Ok(())
}

#[tokio::test]
async fn long_retry_after_is_reported_without_sleeping() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/limited"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server)?;
    let error = tokio::time::timeout(Duration::from_secs(2), client.resource::<Value>("limited"))
        .await?
        .err()
        .ok_or("expected rate limit error")?;
    assert!(error.is_rate_limited());
    Ok(())
}

#[tokio::test]
async fn unsafe_urls_headers_and_malformed_responses_are_rejected() -> Result {
    let server = MockServer::start().await;
    let client = client(&server)?;
    for path in [
        "https://example.org/2.0/x",
        "../outside",
        "/2.0/%2e%2e/outside",
        "https://user:pass@api.bitbucket.org/2.0/user",
        "user#fragment",
    ] {
        assert!(client.resolve_url(path).is_err(), "{path}");
    }
    for name in ["authorization", "cookie", "host", "proxy-authorization"] {
        client
            .raw(
                Method::Get,
                "user",
                &[],
                None,
                &[(name.into(), "secret".into())],
            )
            .await
            .err()
            .ok_or("operation unexpectedly succeeded")?;
    }
    assert!(
        server
            .received_requests()
            .await
            .ok_or("missing requests")?
            .is_empty()
    );
    Mock::given(path("/2.0/bad"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
        .mount(&server)
        .await;
    client
        .resource::<Value>("bad")
        .await
        .err()
        .ok_or("operation unexpectedly succeeded")?;
    Ok(())
}

#[tokio::test]
async fn errors_and_debug_output_redact_credentials() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/private"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"message":"invalid test-secret"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server)?;
    assert!(!format!("{client:?}").contains("test-secret"));
    let error = client
        .resource::<Value>("private")
        .await
        .err()
        .ok_or("expected authentication error")?;
    assert!(error.is_authentication());
    assert!(!error.to_string().contains("test-secret"));
    assert!(!format!("{error:?}").contains("test-secret"));
    Ok(())
}

#[tokio::test]
async fn streams_binary_bytes_without_utf8_conversion() -> Result {
    let server = MockServer::start().await;
    let bytes = vec![0, 255, 3, 254, 10];
    Mock::given(path("/2.0/diff"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut result = Vec::new();
    client(&server)?.download("diff", &mut result).await?;
    assert_eq!(result, bytes);
    Ok(())
}

#[test]
fn reviewers_validate_construction_and_preserve_wire_format() -> Result {
    use bb_api::requests::{CreatePullRequest, Reviewer};
    for invalid in ["", " ", "account id", "account\\n"] {
        let error = Reviewer::from_account_id(invalid.replace("\\n", "\n"))
            .err()
            .ok_or("invalid reviewer was accepted")?;
        assert!(error.is_invalid_input());
    }
    let uuid = uuid::Uuid::parse_str("12345678-1234-5678-1234-567812345678")?;
    let reviewer = Reviewer::from(uuid);
    assert_eq!(
        "{12345678-1234-5678-1234-567812345678}".parse::<Reviewer>()?,
        reviewer
    );
    let request = CreatePullRequest::new("A change", "feature/x".parse()?, "main".parse()?)?
        .reviewers(vec![reviewer, Reviewer::from_account_id("account-42")?]);
    let value = serde_json::to_value(request)?;
    assert_eq!(
        value["reviewers"],
        json!([
            {"uuid":"{12345678-1234-5678-1234-567812345678}"},
            {"account_id":"account-42"}
        ])
    );
    assert_eq!(value["source"]["branch"]["name"], "feature/x");
    Ok(())
}

#[tokio::test]
async fn cloned_client_remains_usable_after_original_is_dropped() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/user"))
        .and(header("authorization", "Bearer test-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name":"shared"})))
        .expect(1)
        .mount(&server)
        .await;
    let original = client(&server)?;
    let shared = original.clone();
    drop(original);
    assert_eq!(shared.resource::<Value>("user").await?["name"], "shared");
    assert!(!format!("{shared:?}").contains("test-secret"));
    Ok(())
}

#[tokio::test]
async fn json_body_presence_and_custom_headers_keep_their_wire_contract() -> Result {
    #[derive(serde::Serialize)]
    struct Payload<'a> {
        title: &'a str,
        reviewers: Vec<&'a str>,
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .expect(4)
        .mount(&server)
        .await;
    let client = client(&server)?;
    client
        .mutate::<Value, ()>(Method::Post, "absent", None)
        .await?;
    client
        .mutate::<Value, ()>(Method::Post, "null", Some(&()))
        .await?;
    client
        .raw(
            Method::Post,
            "custom",
            &[],
            Some(&json!({"title":"été"})),
            &[("content-type".into(), "application/custom+json".into())],
        )
        .await?;
    client
        .mutate::<Value, _>(
            Method::Post,
            "typed",
            Some(&Payload {
                title: "été",
                reviewers: vec!["a", "b"],
            }),
        )
        .await?;
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording unavailable")?;
    assert_eq!(requests[0].body, [] as [u8; 0]);
    assert!(!requests[0].headers.contains_key("content-type"));
    assert_eq!(requests[1].body, b"null");
    assert_eq!(requests[1].headers["content-type"], "application/json");
    assert_eq!(
        requests[2].headers["content-type"],
        "application/custom+json"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[3].body)?,
        json!({"title":"été","reviewers":["a","b"]})
    );
    Ok(())
}

#[tokio::test]
async fn encoding_failure_is_invalid_input_and_never_sends_a_request() -> Result {
    struct Invalid;
    impl serde::Serialize for Invalid {
        fn serialize<S: serde::Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("invalid request"))
        }
    }
    let server = MockServer::start().await;
    let error = client(&server)?
        .mutate::<Value, _>(Method::Post, "unused", Some(&Invalid))
        .await
        .err()
        .ok_or("invalid request was accepted")?;
    assert!(error.is_invalid_input());
    assert_eq!(
        server
            .received_requests()
            .await
            .ok_or("missing request recording")?
            .len(),
        0
    );
    Ok(())
}

#[tokio::test]
async fn log_debug_omits_url_and_streams_shared_bytes() -> Result {
    let server = MockServer::start().await;
    Mock::given(path("/2.0/log"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"log bytes"))
        .mount(&server)
        .await;
    let mut response = client(&server)?
        .log_range("log?signature=signed-secret", bb_api::ByteRange::From(0))
        .await?;
    let debug = format!("{response:?}");
    assert!(!debug.contains("signed-secret"));
    assert!(!debug.contains("signature"));
    let bytes: bytes::Bytes = response.chunk().await?.ok_or("missing chunk")?;
    assert_eq!(bytes.as_ref(), b"log bytes");
    Ok(())
}
