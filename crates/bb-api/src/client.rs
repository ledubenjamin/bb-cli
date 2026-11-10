use crate::Error;
use crate::transport::{ResponseMode, Transport};
use crate::{ByteRange, LogResponse};
use futures_util::StreamExt;
use reqwest::redirect::Policy;
use secrecy::{ExposeSecret, SecretString};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use url::Url;

/// An HTTP request method supported by the API escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    /// Read a resource.
    Get,
    /// Read response headers.
    Head,
    /// Create or invoke a resource.
    Post,
    /// Replace or update a resource.
    Put,
    /// Partially update a resource.
    Patch,
    /// Delete a resource.
    Delete,
}
impl Method {
    /// Reports whether the method is safe to repeat automatically.
    #[must_use]
    pub fn is_read(self) -> bool {
        matches!(self, Self::Get | Self::Head)
    }
    pub(crate) fn http(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Head => reqwest::Method::HEAD,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Patch => reqwest::Method::PATCH,
            Self::Delete => reqwest::Method::DELETE,
        }
    }
}
impl std::str::FromStr for Method {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self::Get),
            "HEAD" => Ok(Self::Head),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "PATCH" => Ok(Self::Patch),
            "DELETE" => Ok(Self::Delete),
            _ => Err(Error::invalid(
                "method must be GET, HEAD, POST, PUT, PATCH, or DELETE",
            )),
        }
    }
}

/// A raw response, including headers and the unmodified body.
#[derive(Debug)]
pub struct RawResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers, normalized to lowercase names.
    pub headers: BTreeMap<String, String>,
    /// Response bytes.
    pub body: Vec<u8>,
}

/// A reusable asynchronous client with credentials scoped to one API origin.
///
/// Cloning shares the connection pool, configuration, and credential allocation.
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) inner: Arc<Transport>,
}

/// Configuration for a Bitbucket Cloud client.
#[derive(Debug)]
pub struct ClientBuilder {
    base: String,
    token: Option<SecretString>,
    timeout: Duration,
}
impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            base: "https://api.bitbucket.org/2.0/".into(),
            token: None,
            timeout: Duration::from_secs(30),
        }
    }
}
impl ClientBuilder {
    /// Sets a bearer credential without exposing it in debug output.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(SecretString::from(token.into()));
        self
    }
    /// Sets an explicit API base, including a version path, for private test servers.
    #[must_use]
    pub fn base_url(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }
    /// Sets the total timeout for each HTTP request, including its body.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Builds a client after validating all options.
    ///
    /// # Errors
    /// Returns an error for invalid URLs, credentials, timeouts, or TLS configuration.
    pub fn build(self) -> Result<Client, Error> {
        let base = Url::parse(&format!("{}/", self.base.trim_end_matches('/')))?;
        let loopback = matches!(base.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(base.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
        if (base.scheme() != "https" && !(base.scheme() == "http" && loopback))
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || self.timeout.is_zero()
            || self.timeout > Duration::from_hours(24)
        {
            return Err(Error::invalid(
                "API base must use HTTPS (or loopback HTTP for tests), without credentials or query",
            ));
        }
        if let Some(token) = &self.token
            && (token.expose_secret().is_empty()
                || token.expose_secret().chars().any(char::is_control))
        {
            return Err(Error::invalid(
                "token must not be empty or contain control characters",
            ));
        }
        let http = reqwest::Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(self.timeout)
            .user_agent(concat!("bb/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Client {
            inner: Arc::new(Transport {
                http,
                base,
                token: self.token,
                // Bound buffered responses; download_to streams larger artifacts instead.
                max_body: 64 * 1024 * 1024,
            }),
        })
    }
}

impl Client {
    /// Starts configuring a client.
    #[must_use]
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Resolves a path within this client's API origin and version prefix.
    ///
    /// # Errors
    /// Rejects invalid URLs, credentials, fragments, and foreign destinations.
    pub fn resolve_url(&self, path: &str) -> Result<Url, Error> {
        self.inner.resolve_url(path)
    }

    /// Makes an authenticated request to the configured API origin.
    ///
    /// # Errors
    /// Returns an error for unsafe URLs, HTTP failures, or bodies exceeding 64 MiB.
    pub async fn raw(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&serde_json::Value>,
        headers: &[(String, String)],
    ) -> Result<RawResponse, Error> {
        self.raw_bytes(method, path, query, encode_json(body)?, headers)
            .await
    }

    async fn raw_bytes(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<bytes::Bytes>,
        headers: &[(String, String)],
    ) -> Result<RawResponse, Error> {
        let response = self
            .inner
            .send(method, path, query, body, headers, ResponseMode::Api)
            .await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_owned())))
            .collect();
        let body = Transport::read_body(response, self.inner.max_body).await?;
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }

    /// Streams a resource to a writer without buffering the entire response.
    ///
    /// Signed HTTPS redirects to Amazon S3 or `CloudFront` are followed without
    /// authentication headers. Ordinary API and pagination requests remain origin-bound.
    ///
    /// # Errors
    /// Returns HTTP, transport, and output errors. A failed stream can have partial output.
    pub async fn download(
        &self,
        path: &str,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> Result<(), Error> {
        let response = self
            .inner
            .send(Method::Get, path, &[], None, &[], ResponseMode::Download)
            .await?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            output.write_all(&chunk?).await?;
        }
        output.flush().await?;
        Ok(())
    }

    /// Opens a log download, preserving byte ranges across signed storage redirects.
    ///
    /// # Errors
    /// Returns invalid-range, HTTP, redirect-policy, or transport errors.
    pub async fn log_range(&self, path: &str, range: ByteRange) -> Result<LogResponse, Error> {
        range.header()?;
        LogResponse::new(
            self.inner
                .send(Method::Get, path, &[], None, &[], ResponseMode::Log(range))
                .await?,
        )
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.json(Method::Get, path, None::<&()>).await
    }

    pub(crate) async fn json<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, Error> {
        let response = self
            .raw_bytes(method, path, &[], encode_json(body)?, &[])
            .await?;
        Ok(serde_json::from_slice(if response.body.is_empty() {
            b"null"
        } else {
            &response.body
        })?)
    }
}

/// Preserve absent bodies versus an explicitly serialized JSON null.
fn encode_json<B: Serialize>(body: Option<&B>) -> Result<Option<bytes::Bytes>, Error> {
    body.map(|body| {
        serde_json::to_vec(body)
            .map(bytes::Bytes::from)
            .map_err(Error::encode)
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    #[tokio::test]
    async fn retries_reuse_the_original_typed_serialization()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Counted<'a>(&'a AtomicUsize);
        impl serde::Serialize for Counted<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_u64(self.0.fetch_add(1, Ordering::SeqCst) as u64)
            }
        }
        let server = MockServer::start().await;
        let client = Client::builder()
            .base_url(format!("{}/2.0/", server.uri()))
            .build()?;
        let count = AtomicUsize::new(0);
        let calls = AtomicUsize::new(0);
        Mock::given(method("GET"))
            .respond_with(move |_: &wiremock::Request| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(429).insert_header("retry-after", "0")
                } else {
                    ResponseTemplate::new(200).set_body_json(json!(null))
                }
            })
            .expect(2)
            .mount(&server)
            .await;
        client
            .json::<Value, _>(Method::Get, "retry", Some(&Counted(&count)))
            .await?;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let requests = server
            .received_requests()
            .await
            .ok_or("request recording unavailable")?;
        assert_eq!(requests[0].body, requests[1].body);
        Ok(())
    }
}
