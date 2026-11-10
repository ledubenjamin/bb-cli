//! HTTP policy: confine credentials, bound retries, and stream downloads.

use crate::{ByteRange, Error, Method};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use std::time::{Duration, SystemTime};
use url::Url;

#[derive(Debug)]
pub(crate) struct Transport {
    pub(crate) http: reqwest::Client,
    pub(crate) base: Url,
    pub(crate) token: Option<SecretString>,
    pub(crate) max_body: usize,
}

#[derive(Clone, Copy)]
pub(crate) enum ResponseMode {
    Api,
    Download,
    Log(ByteRange),
}

impl Transport {
    /// Resolves an API path and confines it to this client's origin and version path.
    ///
    /// # Errors
    /// Rejects invalid URLs, embedded credentials, fragments, and foreign destinations.
    pub(crate) fn resolve_url(&self, path: &str) -> Result<Url, Error> {
        let url = if path.starts_with("https://") || path.starts_with("http://") {
            Url::parse(path)
        } else {
            let path = path
                .strip_prefix("/2.0/")
                .unwrap_or(path)
                .trim_start_matches('/');
            self.base.join(path)
        }?;
        if url.origin() != self.base.origin()
            || !url.path().starts_with(self.base.path())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::invalid(
                "resource URL leaves the configured API origin or version path",
            ));
        }
        Ok(url)
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        self.resolve_url(path)
    }

    async fn response_error(&self, response: reqwest::Response) -> Error {
        let status = response.status();
        let bytes = Self::read_body(response, 16 * 1024)
            .await
            .unwrap_or_default();
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| {
                status
                    .canonical_reason()
                    .unwrap_or("API request failed")
                    .to_owned()
            });
        let message = if let Some(token) = &self.token {
            message.replace(token.expose_secret(), "[REDACTED]")
        } else {
            message
        };
        Error::http(
            status.as_u16(),
            format!("Bitbucket returned {}: {message}", status.as_u16()),
        )
    }

    /// Serializes no data here: retries share immutable request bytes.
    /// Once a signed-storage redirect is followed, credentials stay disabled even
    /// if a later redirect returns to the API origin.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<bytes::Bytes>,
        headers: &[(String, String)],
        mode: ResponseMode,
    ) -> Result<reqwest::Response, Error> {
        let mut url = self.url(path)?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
        let mut extra = extra_headers(headers)?;
        let mut attempt = 0_u32;
        let mut redirects = 0;
        let mut authenticated = true;
        loop {
            let mut request = self
                .http
                .request(method.http(), url.clone())
                .headers(extra.clone());
            if let ResponseMode::Log(range) = mode {
                // Only these typed, non-secret headers survive a signed-storage redirect.
                request = request
                    .header("Range", range.header()?)
                    .header("Accept-Encoding", "identity");
            }
            if authenticated
                && url.origin() == self.base.origin()
                && let Some(token) = &self.token
            {
                request = request.bearer_auth(token.expose_secret());
            }
            if let Some(body) = &body {
                if !extra.contains_key(reqwest::header::CONTENT_TYPE) {
                    request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
                }
                request = request.body(body.clone());
            }
            let response = request.send().await;
            let response = match response {
                Ok(response) => response,
                Err(error)
                    if method.is_read()
                        && attempt < 2
                        && (error.is_connect() || error.is_timeout()) =>
                {
                    tokio::time::sleep(retry_delay(attempt)).await;
                    attempt += 1;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let status = response.status();
            if matches!(mode, ResponseMode::Log(_)) && matches!(status.as_u16(), 304 | 416) {
                return Ok(response);
            }
            tracing::debug!(name: "http.response", method = %method.http(), status = status.as_u16(), "API response");
            if status.is_redirection() && method.is_read() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        Error::http(status.as_u16(), "redirect without a valid Location")
                    })?;
                if redirects >= 5 {
                    return Err(Error::invalid("too many API redirects"));
                }
                let next = url.join(location)?;
                url = match self.url(next.as_str()) {
                    Ok(url) => url,
                    Err(_) if !matches!(mode, ResponseMode::Api) && storage_url(&next) => {
                        // Signed log URLs authenticate through their query. Never forward
                        // the Bitbucket bearer token or caller headers to object storage.
                        authenticated = false;
                        extra.clear();
                        next
                    }
                    Err(error) => return Err(error),
                };
                redirects += 1;
                continue;
            }
            if method.is_read()
                && attempt < 2
                && matches!(status.as_u16(), 408 | 429 | 502 | 503 | 504)
            {
                let delay = retry_after(response.headers()).unwrap_or(retry_delay(attempt));
                if delay <= Duration::from_secs(30) {
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }
            }
            if !status.is_success() {
                return Err(self.response_error(response).await);
            }
            return Ok(response);
        }
    }

    pub(crate) async fn read_body(
        response: reqwest::Response,
        limit: usize,
    ) -> Result<Vec<u8>, Error> {
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(Error::invalid(
                    "response exceeds the 64 MiB JSON limit; use streamed output",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

fn extra_headers(headers: &[(String, String)]) -> Result<HeaderMap, Error> {
    let mut extra = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())?;
        if matches!(
            name.as_str(),
            "authorization" | "proxy-authorization" | "cookie" | "host"
        ) {
            return Err(Error::invalid(
                "authentication and Host headers are managed by bb",
            ));
        }
        let value = HeaderValue::from_str(value)?;
        extra.append(name, value);
    }
    Ok(extra)
}

fn storage_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.host_str().is_some_and(|host| {
            host.ends_with(".amazonaws.com") || host.ends_with(".cloudfront.net")
        })
}

fn retry_delay(attempt: u32) -> Duration {
    let jitter = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|time| u64::from(time.subsec_nanos()) % 125)
        .unwrap_or_default();
    Duration::from_millis(250 * 2_u64.pow(attempt) + jitter)
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(SystemTime::now())
                .ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_redirect_policy_rejects_credentials_and_untrusted_destinations()
    -> Result<(), Box<dyn std::error::Error>> {
        for value in [
            "https://logs.s3.eu-west-1.amazonaws.com/log?signature=abc",
            "https://logs.cloudfront.net/log",
        ] {
            assert!(storage_url(&Url::parse(value)?));
        }
        for value in [
            "http://logs.s3.amazonaws.com/log",
            "https://127.0.0.1/log",
            "https://evil.test/log",
            "https://s3.amazonaws.com.evil.test/log",
            "https://token@logs.s3.amazonaws.com/log",
            "https://logs.s3.amazonaws.com:444/log",
        ] {
            assert!(!storage_url(&Url::parse(value)?), "{value}");
        }
        Ok(())
    }
}
