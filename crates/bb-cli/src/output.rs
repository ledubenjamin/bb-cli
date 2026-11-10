use anyhow::{Context, Result, bail};
use bb_api::models;
use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{self, Write};

use crate::pipeline_status;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Repository,
    PullRequest,
    User,
    Branch,
    Comment,
    Task,
    Check,
    File,
    Project,
    Restriction,
    Permission,
    Run,
    Variable,
    Webhook,
    Action,
    Context,
    Config,
    Auth,
    Status,
    RateLimit,
    Raw,
    Text,
    LogMatch,
    Favorite,
    Skill,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PrView {
    number: u64,
    title: String,
    body: String,
    state: String,
    native_state: models::PullRequestState,
    is_draft: bool,
    author: Actor,
    reviewers: Vec<Actor>,
    participants: Vec<models::Participant>,
    head_ref_name: String,
    base_ref_name: String,
    head_ref_oid: String,
    head_repository: Option<String>,
    base_repository: Option<String>,
    url: String,
    created_at: String,
    updated_at: String,
    comments: Option<Vec<models::Comment>>,
    files: Option<Vec<models::DiffStat>>,
    tasks: Option<Vec<models::Task>>,
    checks: Option<Vec<models::Check>>,
}
#[derive(Debug, Serialize, JsonSchema)]
struct Actor {
    uuid: String,
    login: String,
    name: String,
}
impl From<models::User> for Actor {
    fn from(user: models::User) -> Self {
        Self {
            login: user.nickname.unwrap_or_else(|| user.uuid.clone()),
            uuid: user.uuid,
            name: user.display_name,
        }
    }
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct RepoView {
    uuid: String,
    name: String,
    name_with_owner: String,
    description: String,
    is_private: bool,
    default_branch: Option<String>,
    url: String,
    project: Option<models::Project>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct RunView {
    uuid: String,
    number: u64,
    status: String,
    conclusion: Option<String>,
    native_state: models::RunState,
    head_branch: String,
    head_sha: String,
    created_at: String,
    completed_at: Option<String>,
    url: String,
    steps: Option<Vec<models::Step>>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub operation: String,
    pub status: String,
    pub dry_run: bool,
    pub actions: Vec<Value>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct AuthView {
    pub profile: String,
    pub source: String,
    pub token_type: String,
    pub authenticated: bool,
    pub user: Option<Value>,
    pub diagnostics: Vec<String>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct ContextView {
    pub name: String,
    pub profile: Option<String>,
    pub workspace: Option<String>,
    pub repository: Option<String>,
    pub active: bool,
}

#[derive(Debug)]
pub struct Outcome {
    pub kind: Kind,
    pub value: Value,
    pub code: u8,
    pub text: Option<String>,
}
impl Outcome {
    pub fn new(kind: Kind, value: Value) -> Self {
        Self {
            kind,
            value,
            code: 0,
            text: None,
        }
    }
    pub fn resource(kind: Kind, value: impl Serialize) -> Result<Self> {
        Ok(Self::new(
            kind,
            normalize(kind, serde_json::to_value(value)?)?,
        ))
    }
    pub fn action(operation: &str, actions: Vec<Value>, dry_run: bool) -> Result<Self> {
        Self::resource(
            Kind::Action,
            Action {
                operation: operation.into(),
                status: if dry_run { "planned" } else { "success" }.into(),
                dry_run,
                actions,
            },
        )
    }
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            kind: Kind::Text,
            value: json!({"text": text}),
            text: Some(text),
            code: 0,
        }
    }
}

fn camel(name: &str) -> String {
    let mut upper = false;
    name.chars()
        .filter_map(|c| {
            if c == '_' {
                upper = true;
                None
            } else if upper {
                upper = false;
                Some(c.to_ascii_uppercase())
            } else {
                Some(c)
            }
        })
        .collect()
}

pub fn camel_value(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(k, v)| (camel(&k), camel_value(v)))
                .collect(),
        ),
        Value::Array(array) => Value::Array(array.into_iter().map(camel_value).collect()),
        value => value,
    }
}

impl From<models::PullRequest> for PrView {
    fn from(pr: models::PullRequest) -> Self {
        let state = match pr.state {
            models::PullRequestState::Open => "OPEN",
            models::PullRequestState::Merged => "MERGED",
            models::PullRequestState::Declined | models::PullRequestState::Superseded => "CLOSED",
            _ => "UNKNOWN",
        };
        Self {
            number: pr.id,
            title: pr.title,
            body: pr.description,
            state: state.into(),
            native_state: pr.state,
            is_draft: pr.draft,
            author: pr.author.into(),
            reviewers: pr.reviewers.into_iter().map(Into::into).collect(),
            participants: pr.participants,
            head_ref_name: pr.source.branch.name,
            base_ref_name: pr.destination.branch.name,
            head_ref_oid: pr.source.commit.hash,
            head_repository: pr.source.repository.map(|r| r.full_name),
            base_repository: pr.destination.repository.map(|r| r.full_name),
            url: pr
                .links
                .get("html")
                .map(|l| l.href.clone())
                .unwrap_or_default(),
            created_at: pr.created_on,
            updated_at: pr.updated_on,
            comments: None,
            files: None,
            tasks: None,
            checks: None,
        }
    }
}

pub fn normalize(kind: Kind, value: Value) -> Result<Value> {
    if matches!(
        kind,
        Kind::Action | Kind::Raw | Kind::Config | Kind::RateLimit
    ) {
        return Ok(value);
    }
    if let Value::Array(values) = value {
        return values
            .into_iter()
            .map(|value| normalize(kind, value))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array);
    }
    let value = match kind {
        Kind::PullRequest => serde_json::to_value(PrView::from(
            serde_json::from_value::<models::PullRequest>(value)
                .context("invalid pull request response")?,
        ))?,
        Kind::Repository => {
            let repo: models::Repository =
                serde_json::from_value(value).context("invalid repository response")?;
            serde_json::to_value(RepoView {
                uuid: repo.uuid,
                name: repo.name,
                name_with_owner: repo.full_name,
                description: repo.description,
                is_private: repo.is_private,
                default_branch: repo.mainbranch.map(|b| b.name),
                url: repo.links.html.map(|link| link.href).unwrap_or_default(),
                project: repo.project,
            })?
        }
        Kind::User => {
            serde_json::to_value(Actor::from(serde_json::from_value::<models::User>(value)?))?
        }
        Kind::Variable => {
            let mut variable: models::Variable = serde_json::from_value(value)?;
            if variable.secured {
                variable.value = None;
            }
            serde_json::to_value(variable)?
        }
        Kind::Run => {
            let run: models::Run =
                serde_json::from_value(value).context("invalid pipeline response")?;
            let status = pipeline_status::normalized_status(&run.state.name);
            let conclusion = run
                .state
                .result
                .as_ref()
                .map(|result| pipeline_status::normalized_conclusion(&result.name));
            serde_json::to_value(RunView {
                uuid: run.uuid,
                number: run.build_number,
                status,
                conclusion,
                native_state: run.state,
                head_branch: run.target.ref_name,
                head_sha: run.target.commit.hash,
                created_at: run.created_on,
                completed_at: run.completed_on,
                url: run
                    .links
                    .get("html")
                    .map(|link| link.href.clone())
                    .unwrap_or_default(),
                steps: None,
            })?
        }
        _ => value,
    };
    Ok(camel_value(value))
}

fn camel_schema(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::Object(properties)) = object.get_mut("properties") {
                *properties = std::mem::take(properties)
                    .into_iter()
                    .map(|(key, value)| (camel(&key), value))
                    .collect();
            }
            if let Some(Value::Array(required)) = object.get_mut("required") {
                for name in required {
                    if let Some(key) = name.as_str() {
                        *name = Value::String(camel(key));
                    }
                }
            }
            for value in object.values_mut() {
                camel_schema(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                camel_schema(value);
            }
        }
        _ => {}
    }
}

pub fn schema(kind: Kind) -> Value {
    let schema = match kind {
        Kind::PullRequest => schema_for!(PrView),
        Kind::Repository => schema_for!(RepoView),
        Kind::User => schema_for!(Actor),
        Kind::Branch => schema_for!(models::Branch),
        Kind::Comment => schema_for!(models::Comment),
        Kind::Task => schema_for!(models::Task),
        Kind::Check => schema_for!(models::Check),
        Kind::File => schema_for!(models::DiffStat),
        Kind::Project => schema_for!(models::Project),
        Kind::Restriction => schema_for!(models::Restriction),
        Kind::Permission => schema_for!(models::Permission),
        Kind::Run => schema_for!(RunView),
        Kind::LogMatch => schema_for!(crate::log_cache::LogMatch),
        Kind::Favorite => schema_for!(crate::favorites::FavoriteView),
        Kind::Skill => schema_for!(crate::skills::SkillView),
        Kind::Variable => schema_for!(models::Variable),
        Kind::Webhook => schema_for!(models::Webhook),
        Kind::Action => schema_for!(Action),
        Kind::Context => schema_for!(ContextView),
        Kind::Auth => schema_for!(AuthView),
        Kind::Text => {
            return json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]});
        }
        Kind::Status => {
            return json!({"type":"object","properties":{"createdByMe":{"type":"array"},"reviewRequested":{"type":"array"},"repository":{"type":"string"}}});
        }
        Kind::Config | Kind::RateLimit | Kind::Raw => {
            return json!({"type":"object","additionalProperties":true});
        }
    };
    let mut value = serde_json::to_value(schema).unwrap_or(Value::Null);
    camel_schema(&mut value);
    value
}

pub fn fields(kind: Kind) -> Vec<String> {
    schema(kind)
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

pub fn array_schema(mut item: Value) -> Value {
    let definitions = item
        .as_object_mut()
        .and_then(|object| object.remove("$defs"));
    let mut schema = json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "type":"array", "items":item});
    if let Some(definitions) = definitions {
        schema["$defs"] = definitions;
    }
    schema
}

pub fn validate_fields(kind: Kind, selection: &str) -> Result<()> {
    if selection == "*"
        || selection.is_empty()
        || matches!(kind, Kind::Raw | Kind::Config | Kind::RateLimit)
    {
        return Ok(());
    }
    let allowed = fields(kind);
    for field in selection.split(',') {
        if !allowed.iter().any(|key| key == field) {
            bail!(
                "unknown JSON field '{field}'; available fields: {}",
                allowed.join(",")
            );
        }
    }
    Ok(())
}

fn select(value: Value, selection: &str) -> Result<Value> {
    if selection == "*" {
        return Ok(value);
    }
    if let Value::Array(values) = value {
        return values
            .into_iter()
            .map(|v| select(v, selection))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array);
    }
    let object = value
        .as_object()
        .context("JSON field selection requires an object or array of objects")?;
    Ok(Value::Object(
        selection
            .split(',')
            .map(|field| {
                (
                    field.to_owned(),
                    object.get(field).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    ))
}

pub async fn render(
    outcome: Outcome,
    json_fields: Option<String>,
    jq: Option<String>,
) -> Result<()> {
    crate::blocking::isolated("bb-output", move || {
        render_sync(outcome, json_fields.as_deref(), jq.as_deref())
    })
    .await
}

fn render_sync(outcome: Outcome, json_fields: Option<&str>, jq: Option<&str>) -> Result<()> {
    let mut out = io::stdout().lock();
    if let Some(selection) = json_fields {
        let value = select(outcome.value, selection)?;
        if let Some(filter) = jq {
            for value in crate::jq::filter(value, filter)? {
                if let Value::String(text) = value {
                    writeln!(out, "{text}")?;
                } else {
                    serde_json::to_writer(&mut out, &value)?;
                    writeln!(out)?;
                }
            }
        } else {
            serde_json::to_writer(&mut out, &value)?;
            writeln!(out)?;
        }
    } else if let Some(text) = &outcome.text {
        out.write_all(text.as_bytes())?;
        if !text.is_empty() && !text.ends_with('\n') {
            writeln!(out)?;
        }
    } else {
        let values = match &outcome.value {
            Value::Array(values) => values.as_slice(),
            value => std::slice::from_ref(value),
        };
        if values.is_empty() {
            writeln!(out, "No results.")?;
        }
        for value in values {
            let columns: &[&str] = match outcome.kind {
                Kind::PullRequest => &["number", "title", "state", "headRefName", "url"],
                Kind::Repository => &["nameWithOwner", "description", "url"],
                Kind::Favorite => &["repository"],
                Kind::Skill => &[
                    "name",
                    "status",
                    "installedVersion",
                    "bundledVersion",
                    "path",
                ],
                Kind::Comment => &["id", "user", "content", "inline", "resolution"],
                Kind::Task => &["id", "state", "content"],
                Kind::Branch => &["name", "target"],
                Kind::Check => &["key", "state", "name", "url"],
                Kind::Run => &["number", "status", "conclusion", "uuid"],
                Kind::Project => &["key", "name", "description"],
                Kind::Variable => &["key", "secured", "value"],
                Kind::Webhook => &["uuid", "active", "url", "events"],
                Kind::User => &["login", "name", "uuid"],
                Kind::Action => &["operation", "status", "actions"],
                Kind::Context => &["name", "active", "profile", "workspace", "repository"],
                _ => &[],
            };
            if columns.is_empty() {
                serde_json::to_writer_pretty(&mut out, value)?;
                writeln!(out)?;
            } else {
                writeln!(
                    out,
                    "{}",
                    columns
                        .iter()
                        .map(|key| cell(value.get(*key).unwrap_or(&Value::Null)))
                        .collect::<Vec<_>>()
                        .join("\t")
                )?;
            }
        }
    }
    out.flush()?;
    Ok(())
}

fn cell(value: &Value) -> String {
    use std::borrow::Cow;
    let text = match value {
        Value::String(text) => Cow::Borrowed(text.as_str()),
        Value::Null => Cow::Borrowed(""),
        _ => Cow::Owned(value.to_string()),
    };
    let mut output = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() {
            output.extend(ch.escape_default());
        } else {
            output.push(ch);
        }
    }
    output
}
