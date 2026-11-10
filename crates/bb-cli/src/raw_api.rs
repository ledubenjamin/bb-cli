use std::{collections::HashSet, fmt::Write as _};

use anyhow::{Context, Result, bail};
use bb_api::Method;
use serde_json::{Map, Value, json};

use crate::{
    app::{self, App},
    args::{ApiArgs, BodyArgs},
    output::{Kind, Outcome},
};

pub fn pair(value: &str) -> Result<(String, String)> {
    let (key, value) = value.split_once('=').context("expected KEY=VALUE")?;
    if key.is_empty() || key.chars().any(char::is_control) {
        bail!("field name must be nonempty and contain no control characters");
    }
    Ok((key.into(), value.into()))
}

#[derive(Debug)]
struct PreparedRequest {
    url: url::Url,
    method: Method,
    body: Option<Value>,
    headers: Vec<(String, String)>,
}

async fn prepare(app: &App<'_>, args: &ApiArgs) -> Result<PreparedRequest> {
    let mut path = args.path.clone();
    if path.contains("{repo}") {
        path = path.replace("{repo}", app.repo().await?.slug());
    }
    if path.contains("{workspace}") {
        path = path.replace("{workspace}", &app.workspace().await?.to_string());
    }
    let mut payload = Map::new();
    for (fields, typed) in [(&args.fields, true), (&args.raw_fields, false)] {
        for field in fields {
            let (key, value) = pair(field)?;
            let value = if typed {
                serde_json::from_str(&value).unwrap_or(Value::String(value))
            } else {
                Value::String(value)
            };
            if payload.insert(key.clone(), value).is_some() {
                bail!("duplicate body field '{key}'");
            }
        }
    }
    let body = if let Some(input) = &args.input {
        let input = app::body(&BodyArgs {
            body: None,
            body_file: Some(input.clone()),
        })
        .await?
        .context("empty input")?;
        Some(serde_json::from_str::<Value>(&input).context("--input must contain valid JSON")?)
    } else if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    };
    let method = args.method.unwrap_or(if body.is_some() {
        Method::Post
    } else {
        Method::Get
    });
    if args.paginate && method != Method::Get {
        bail!("--paginate requires GET; mutations are never repeated automatically");
    }
    let mut params = args
        .params
        .iter()
        .map(|param| pair(param))
        .collect::<Result<Vec<_>>>()?;
    let body = if method.is_read() {
        if args.input.is_some() {
            bail!("--input requires a write method");
        }
        if let Some(Value::Object(fields)) = body {
            params.extend(fields.into_iter().map(|(key, value)| {
                (
                    key,
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned),
                )
            }));
        }
        None
    } else {
        body
    };
    let headers = parse_headers(&args.headers)?;
    // Validate the destination even when planning a write without credentials.
    let validator = app::client(app.cli, None)?;
    let mut url = validator.resolve_url(&path)?;
    if !params.is_empty() {
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    Ok(PreparedRequest {
        url,
        method,
        body,
        headers,
    })
}

fn parse_headers(headers: &[String]) -> Result<Vec<(String, String)>> {
    headers
        .iter()
        .map(|header| {
            let (key, value) = header
                .split_once(':')
                .context("expected --header 'Name: value'")?;
            let key = key.trim().to_ascii_lowercase();
            if matches!(
                key.as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "host"
            ) {
                bail!("authentication and Host headers are managed by bb");
            }
            reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .context("invalid HTTP header name")?;
            reqwest::header::HeaderValue::from_str(value.trim())
                .context("invalid HTTP header value")?;
            Ok((key, value.trim().to_owned()))
        })
        .collect::<Result<Vec<_>>>()
}

pub async fn execute(app: &App<'_>, args: &ApiArgs) -> Result<Outcome> {
    if args.include && (app.cli.json.is_some() || app.cli.jq.is_some()) {
        bail!("--include cannot be combined with JSON selection or --jq");
    }
    let PreparedRequest {
        mut url,
        method,
        body,
        headers,
    } = prepare(app, args).await?;
    if app.cli.dry_run {
        return Outcome::action(
            "api",
            vec![
                json!({"method":method, "path":url.as_str(), "body":body.as_ref().map(app::redact), "headers":headers.iter().map(|(name,_)|name).collect::<Vec<_>>()}),
            ],
            true,
        );
    }
    if method == Method::Delete {
        app.confirm(&format!("DELETE {}?", url.path())).await?;
    }
    let client = app.client().await?;
    let mut seen = HashSet::new();
    let mut pages = Vec::new();
    let mut total_bytes = 0_usize;
    let mut text = String::new();
    loop {
        if !seen.insert(url.to_string()) {
            bail!("pagination loop detected");
        }
        let response = client
            .raw(method, url.as_str(), &[], body.as_ref(), &headers)
            .await?;
        total_bytes = total_bytes.saturating_add(response.body.len());
        if total_bytes > 64 * 1024 * 1024 {
            bail!("paginated JSON exceeds 64 MiB; narrow the query");
        }
        if args.include {
            writeln!(text, "HTTP {}", response.status)?;
            for (name, value) in &response.headers {
                writeln!(text, "{name}: {value}")?;
            }
            text.push('\n');
        }
        if response.body.is_empty() && !args.paginate {
            return if args.include {
                Ok(Outcome::text(text))
            } else {
                Ok(Outcome::new(Kind::Raw, Value::Null))
            };
        }
        let value = match serde_json::from_slice::<Value>(&response.body) {
            Ok(value) => value,
            Err(error) if args.paginate => {
                return Err(error).context("paginated API response must be JSON");
            }
            Err(_) => {
                text.push_str(
                    std::str::from_utf8(&response.body).context("raw API response is not UTF-8")?,
                );
                return Ok(Outcome::text(text));
            }
        };
        let next = if args.paginate {
            match value.get("next") {
                Some(Value::String(next)) => Some(client.resolve_url(next)?),
                Some(Value::Null) | None => None,
                _ => bail!("invalid pagination link: expected a string"),
            }
        } else {
            None
        };
        if args.include
            || (args.paginate && !args.slurp && app.cli.json.is_none() && app.cli.jq.is_none())
        {
            text.push_str(&serde_json::to_string(&value)?);
            text.push('\n');
        }
        pages.push(value);
        if text.len() > 64 * 1024 * 1024 || pages.len() > 10_000 {
            bail!("API pagination exceeded the output limit; narrow the query");
        }
        if let Some(next) = next {
            url = next;
        } else {
            break;
        }
    }
    if !text.is_empty() {
        Ok(Outcome::text(text))
    } else if args.paginate {
        Ok(Outcome::new(Kind::Raw, Value::Array(pages)))
    } else {
        Ok(Outcome::new(
            Kind::Raw,
            pages.pop().context("missing API response")?,
        ))
    }
}
