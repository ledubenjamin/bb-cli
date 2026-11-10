//! Browser authorization and refresh-token exchange, independent of persistence.

use super::{Credential, now};
use crate::{args::TokenType, config::Config};
use anyhow::{Context as _, Result, anyhow, bail};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, RefreshToken,
    TokenResponse, TokenUrl, basic::BasicClient,
};
use secrecy::{ExposeSecret, SecretString};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub(super) fn consumer(config: &Config) -> Result<(String, SecretString)> {
    let id = std::env::var("BB_OAUTH_CLIENT_ID").ok().or_else(||config.preferences.oauth_client_id.clone())
        .filter(|v|!v.is_empty()).context("OAuth requires BB_OAUTH_CLIENT_ID (or config oauth_client_id) and BB_OAUTH_CLIENT_SECRET; see bb auth login --help")?;
    let secret = std::env::var("BB_OAUTH_CLIENT_SECRET")
        .ok()
        .filter(|v| !v.is_empty())
        .context("set BB_OAUTH_CLIENT_SECRET for your Bitbucket OAuth consumer")?;
    Ok((id, SecretString::from(secret)))
}

async fn oauth_http(request: oauth2::HttpRequest) -> Result<oauth2::HttpResponse, reqwest::Error> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(30))
        .build()?;
    let (parts, body) = request.into_parts();
    let response = client
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await?.to_vec();
    let mut result = oauth2::HttpResponse::new(body);
    *result.status_mut() = status;
    *result.headers_mut() = headers;
    Ok(result)
}

pub(super) async fn refresh(config: &Config, credential: &Credential) -> Result<Credential> {
    let (id, secret) = consumer(config)?;
    refresh_at(
        credential,
        id,
        secret,
        "https://bitbucket.org/site/oauth2/access_token",
    )
    .await
}

#[expect(
    clippy::map_err_ignore,
    reason = "OAuth errors can echo refresh tokens or consumer secrets in response bodies"
)]
pub(super) async fn refresh_at(
    credential: &Credential,
    id: String,
    secret: SecretString,
    endpoint: &str,
) -> Result<Credential> {
    let refresh = credential
        .refresh_token
        .as_ref()
        .context("credential has no refresh token; log in again")?;
    let client = BasicClient::new(ClientId::new(id))
        .set_client_secret(ClientSecret::new(secret.expose_secret().to_owned()))
        .set_token_uri(TokenUrl::new(endpoint.into())?);
    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh.expose_secret().to_owned()))
        .request_async(&oauth_http)
        .await
        .map_err(|_| crate::app::Failure {
            code: "authentication_required",
            message: "OAuth refresh failed; run bb auth login --web again".into(),
            exit: 4,
        })?;
    Ok(Credential {
        token: SecretString::from(token.access_token().secret().clone()),
        refresh_token: Some(SecretString::from(token.refresh_token().map_or_else(
            || refresh.expose_secret().to_owned(),
            |v| v.secret().clone(),
        ))),
        expires_at: Some(
            now()?.saturating_add(
                token
                    .expires_in()
                    .unwrap_or(Duration::from_secs(3600))
                    .as_secs(),
            ),
        ),
        token_type: TokenType::Oauth,
    })
}

#[expect(
    clippy::map_err_ignore,
    reason = "OAuth errors can echo authorization codes or consumer secrets in response bodies"
)]
pub(super) async fn web_login(config: &Config) -> Result<Credential> {
    let (id, secret) = consumer(config)?;
    let redirect = std::env::var("BB_OAUTH_REDIRECT_URI")
        .unwrap_or_else(|_| config.preferences.oauth_redirect_uri.clone());
    let uri = url::Url::parse(&redirect).context("invalid OAuth redirect URI")?;
    let ip: std::net::IpAddr = uri
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .parse()
        .context("OAuth callback must use a literal loopback IP address")?;
    if !ip.is_loopback()
        || uri.scheme() != "http"
        || uri.query().is_some()
        || uri.fragment().is_some()
        || !uri.username().is_empty()
    {
        bail!("OAuth callback must be a loopback HTTP URL without credentials, query, or fragment");
    }
    let listener = TcpListener::bind((ip, uri.port_or_known_default().unwrap_or(8765)))
        .await
        .context("cannot bind OAuth callback; check oauth_redirect_uri and port availability")?;
    let client = BasicClient::new(ClientId::new(id))
        .set_client_secret(ClientSecret::new(secret.expose_secret().to_owned()))
        .set_auth_uri(AuthUrl::new(
            "https://bitbucket.org/site/oauth2/authorize".into(),
        )?)
        .set_token_uri(TokenUrl::new(
            "https://bitbucket.org/site/oauth2/access_token".into(),
        )?)
        .set_redirect_uri(RedirectUrl::new(redirect)?);
    let (url, state) = client.authorize_url(CsrfToken::new_random).url();
    eprintln!("Authorize bb in your browser: {url}");
    if crate::app::open_browser(url.as_str()).await.is_err() {
        eprintln!("Open the URL above in your browser to continue.");
    }
    let code = tokio::time::timeout(
        Duration::from_secs(300),
        callback(listener, uri.path(), state.secret()),
    )
    .await
    .context("OAuth browser login timed out after five minutes")??;
    let token = client.exchange_code(AuthorizationCode::new(code)).request_async(&oauth_http).await
        .map_err(|_|anyhow!("OAuth token exchange failed; verify the consumer credentials and registered callback"))?;
    Ok(Credential {
        token: SecretString::from(token.access_token().secret().clone()),
        refresh_token: token
            .refresh_token()
            .map(|v| SecretString::from(v.secret().clone())),
        expires_at: Some(
            now()?.saturating_add(
                token
                    .expires_in()
                    .unwrap_or(Duration::from_secs(3600))
                    .as_secs(),
            ),
        ),
        token_type: TokenType::Oauth,
    })
}

/// Accepts only the configured callback path and matching CSRF state.
/// Bounded reads and per-connection timeouts prevent an unrelated local client
/// from monopolizing the loopback listener during browser authorization.
pub(super) async fn callback(
    listener: TcpListener,
    path: &str,
    expected_state: &str,
) -> Result<String> {
    loop {
        let (mut connection, _) = listener.accept().await?;
        let mut request = Vec::new();
        let received = tokio::time::timeout(Duration::from_secs(5), async {
            let mut byte = [0u8; 1024];
            while request.len() < 8192 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let count = connection.read(&mut byte).await?;
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&byte[..count]);
            }
            Ok::<(), std::io::Error>(())
        })
        .await;
        if !matches!(received, Ok(Ok(()))) {
            continue;
        }
        let line = String::from_utf8_lossy(&request);
        let mut words = line.lines().next().unwrap_or("").split_whitespace();
        let method = words.next();
        let target = words.next().unwrap_or("");
        let parsed = url::Url::parse(&format!("http://127.0.0.1{target}"));
        let result = parsed
            .ok()
            .filter(|url| method == Some("GET") && url.path() == path);
        let parameters: std::collections::BTreeMap<_, _> = result
            .as_ref()
            .map(|url| url.query_pairs().into_owned().collect())
            .unwrap_or_default();
        let valid = parameters
            .get("state")
            .is_some_and(|state| state == expected_state);
        let code = parameters.get("code").filter(|code| !code.is_empty());
        let denied = valid && parameters.contains_key("error");
        let accepted = valid && code.is_some();
        let (status, body) = if accepted {
            ("200 OK", "Authorization received. You may close this tab.")
        } else {
            (
                "400 Bad Request",
                "Invalid or denied authorization callback.",
            )
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
            body.len()
        );
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            connection.write_all(response.as_bytes()),
        )
        .await;
        if denied {
            bail!("OAuth authorization was denied");
        }
        if accepted {
            return code.cloned().context("missing OAuth authorization code");
        }
    }
}
