use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, bail};
use bb_api::{
    Method,
    models::{Environment, Variable},
};
use serde_json::{Value, json};

use crate::{
    app::{self, App},
    args::{VariableArgs, VariableCommand, VariableScope},
    output::{Kind, Outcome},
};

pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.as_bytes()[0].is_ascii_digit()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!(
            "variable names must start with a letter or underscore and contain only letters, digits, and underscores"
        );
    }
    Ok(())
}

async fn path(app: &App<'_>, args: &VariableArgs) -> Result<String> {
    let scope = if args.deployment.is_some() {
        VariableScope::Deployment
    } else {
        args.scope
    };
    match scope {
        VariableScope::Repository => Ok(format!(
            "{}/pipelines_config/variables",
            app.repo().await?.path()
        )),
        VariableScope::Workspace => Ok(format!(
            "workspaces/{}/pipelines-config/variables",
            app.workspace().await?
        )),
        VariableScope::Deployment => {
            let name = args
                .deployment
                .as_deref()
                .context("deployment scope requires --deployment NAME_OR_UUID")?;
            let repo = app.repo().await?;
            let uuid = if let Ok(uuid) = uuid::Uuid::parse_str(name) {
                format!("{{{uuid}}}")
            } else {
                let environments: Vec<Environment> = app
                    .client()
                    .await?
                    .collection(&format!("{}/environments", repo.path()), &[], usize::MAX)
                    .await?;
                let matched: Vec<_> = environments
                    .into_iter()
                    .filter(|env| env.name == name)
                    .collect();
                match matched.as_slice() {
                    [environment] => environment.uuid.clone(),
                    [] => bail!("deployment environment '{name}' was not found"),
                    _ => bail!("ambiguous deployment name; supply its UUID"),
                }
            };
            Ok(format!(
                "{}/deployments_config/environments/{}/variables",
                repo.path(),
                app::encode(&uuid)
            ))
        }
    }
}

async fn all(app: &App<'_>, path: &str) -> Result<BTreeMap<String, Variable>> {
    let variables: Vec<Variable> = app
        .client()
        .await?
        .collection(path, &[], usize::MAX)
        .await?;
    let mut result = BTreeMap::new();
    for variable in variables {
        if result.insert(variable.key.clone(), variable).is_some() {
            bail!("API returned duplicate variable names; refusing an ambiguous update");
        }
    }
    Ok(result)
}

pub async fn execute(app: &App<'_>, command: &VariableCommand) -> Result<Outcome> {
    match command {
        VariableCommand::List { target, list } => {
            let path = path(app, target).await?;
            let variables: Vec<Variable> = app
                .client()
                .await?
                .collection(&path, &[], list.limit as usize)
                .await?;
            Outcome::resource(Kind::Variable, variables)
        }
        VariableCommand::Get { name, target } => {
            validate_key(name)?;
            let path = path(app, target).await?;
            let variables = all(app, &path).await?;
            let variable = variables
                .get(name)
                .with_context(|| format!("variable '{name}' was not found"))?;
            Outcome::resource(Kind::Variable, variable)
        }
        VariableCommand::Delete { name, target } => {
            validate_key(name)?;
            let path = path(app, target).await?;
            let variables = all(app, &path).await?;
            let variable = variables
                .get(name)
                .with_context(|| format!("variable '{name}' was not found"))?;
            app.mutation(
                "variable delete",
                Method::Delete,
                &format!("{path}/{}", app::encode(&variable.uuid)),
                None,
                Kind::Action,
                true,
            )
            .await
        }
        VariableCommand::Set {
            name,
            target,
            body,
            secured,
            env_file,
        } => {
            let values = if let Some(file) = env_file {
                let content = tokio::fs::read_to_string(file)
                    .await
                    .context("cannot read env file")?;
                parse_env(&content)?
            } else {
                let name = name
                    .as_ref()
                    .context("supply a variable name or --env-file")?;
                validate_key(name)?;
                let value = app::body(body)
                    .await?
                    .context("supply --body or --body-file for the variable value")?;
                vec![(name.clone(), value)]
            };
            let path = path(app, target).await?;
            let existing = all(app, &path).await?;
            // Build and validate the whole batch before sending its first write.
            let actions: Vec<_> = values
                .iter()
                .map(|(key, value)| {
                    let previous = existing.get(key);
                    let secured =
                        secured.unwrap_or_else(|| previous.is_some_and(|var| var.secured));
                    let (method, path) = previous.map_or((Method::Post, path.clone()), |var| {
                        (Method::Put, format!("{path}/{}", app::encode(&var.uuid)))
                    });
                    (
                        method,
                        path,
                        json!({"key":key, "value":value, "secured":secured}),
                    )
                })
                .collect();
            if app.cli.dry_run {
                return Outcome::action("variable set", actions.iter().map(|(method, path, body)| json!({"method":method, "path":path, "body":app::redact(body)})).collect(), true);
            }
            if values.iter().any(|(key, _)| {
                existing.get(key).is_some_and(|var| var.secured) && *secured == Some(false)
            }) {
                app.confirm("Make secured variables public?").await?;
            }
            let mut result = Vec::new();
            for (method, path, payload) in actions {
                let value = app.send(method, &path, Some(&payload), false).await.with_context(|| format!("variable update failed after {} successful updates; no rollback was attempted; inspect state before retrying", result.len()))?;
                // Some Cloud endpoints reply without a body; preserve the submitted metadata.
                let value = if value.is_null() { payload } else { value };
                result.push(crate::output::normalize(Kind::Variable, value)?);
            }
            let value = if env_file.is_some() {
                Value::Array(result)
            } else {
                result.pop().context("missing variable result")?
            };
            Ok(Outcome::new(Kind::Variable, value))
        }
    }
}

/// Parses literal dotenv assignments without shell expansion or execution.
fn parse_env(content: &str) -> Result<Vec<(String, String)>> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let (key, value) = line
            .split_once('=')
            .with_context(|| format!("env file line {}: expected KEY=VALUE", index + 1))?;
        let key = key.trim();
        validate_key(key).with_context(|| format!("env file line {}", index + 1))?;
        if !seen.insert(key.to_owned()) {
            bail!("env file line {}: duplicate variable '{key}'", index + 1);
        }
        let value = value.trim();
        let value = if let Some(quote) = value.chars().next().filter(|c| matches!(c, '\'' | '"')) {
            if value.len() < 2 || !value.ends_with(quote) {
                bail!(
                    "env file line {}: unterminated quoted value (multiline values are not supported)",
                    index + 1
                );
            }
            value[1..value.len() - 1].to_owned()
        } else {
            value
                .split_once(" #")
                .map_or(value, |(value, _)| value)
                .trim_end()
                .to_owned()
        };
        values.push((key.to_owned(), value));
    }
    if values.is_empty() {
        bail!("env file contains no assignments");
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn env_files_are_literal_and_reject_ambiguous_batches() -> Result<()> {
        let values = parse_env(
            "# comment\nexport NAME='a b'\nEMPTY=\nLITERAL=$(touch /tmp/never-created)\n",
        )?;
        assert_eq!(
            values,
            vec![
                ("NAME".into(), "a b".into()),
                ("EMPTY".into(), String::new()),
                ("LITERAL".into(), "$(touch /tmp/never-created)".into())
            ]
        );
        for input in [
            "NAME=one\nNAME=two",
            "1INVALID=x",
            "NAME=\"unclosed",
            "not an assignment",
            "# empty",
        ] {
            parse_env(input)
                .err()
                .ok_or_else(|| anyhow::anyhow!("invalid env file was accepted"))?;
        }
        Ok(())
    }
}
