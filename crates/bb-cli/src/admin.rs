use crate::{
    app::{App, encode},
    args::{
        PermissionLevel, PermsCommand, PermsTarget, PrincipalArgs, PrincipalKind, ProjectCommand,
        Visibility, WebhookCommand,
    },
    output::{Kind, Outcome},
};
use anyhow::{Result, bail};
use bb_api::{Method, models};
use serde_json::json;

pub async fn project(app: &App<'_>, command: &ProjectCommand) -> Result<Outcome> {
    let workspace = app.workspace().await?;
    let path = format!("workspaces/{workspace}/projects");
    match command {
        ProjectCommand::List(args) => {
            let query = crate::discovery::CatalogQuery {
                workspace: workspace.to_string(),
                kind: crate::discovery::CatalogKind::Projects,
                project: None,
                text: args.search.clone().unwrap_or_default(),
            };
            Outcome::resource(
                Kind::Project,
                app.client()
                    .await?
                    .collection::<models::Project>(
                        &query.path(),
                        &query.parameters(),
                        args.list.limit as usize,
                    )
                    .await?,
            )
        }
        ProjectCommand::View { key } => Outcome::resource(
            Kind::Project,
            app.client()
                .await?
                .resource::<models::Project>(&format!("{path}/{}", encode(key.as_str())))
                .await?,
        ),
        ProjectCommand::Create {
            key,
            name,
            description,
            public,
        } => {
            let mut body = json!({"key":key.as_str(),"name":name,"is_private":!public});
            if let Some(description) = description {
                body["description"] = json!(description);
            }
            app.mutation(
                "project create",
                Method::Post,
                &path,
                Some(body),
                Kind::Project,
                false,
            )
            .await
        }
        ProjectCommand::Edit {
            key,
            name,
            description,
            visibility,
        } => {
            let mut body = json!({});
            if let Some(name) = name {
                body["name"] = json!(name);
            }
            if let Some(description) = description {
                body["description"] = json!(description);
            }
            if let Some(visibility) = visibility {
                body["is_private"] = json!(matches!(visibility, Visibility::Private));
            }
            if body.as_object().is_none_or(serde_json::Map::is_empty) {
                bail!("provide a project property to edit");
            }
            app.mutation(
                "project edit",
                Method::Put,
                &format!("{path}/{}", encode(key.as_str())),
                Some(body),
                Kind::Project,
                false,
            )
            .await
        }
        ProjectCommand::Delete { key } => {
            app.mutation(
                "project delete",
                Method::Delete,
                &format!("{path}/{}", encode(key.as_str())),
                None,
                Kind::Action,
                true,
            )
            .await
        }
    }
}

pub async fn permissions(app: &App<'_>, target: &PermsTarget) -> Result<Outcome> {
    let (path, command, project) = match target {
        PermsTarget::Repo { command } => (
            format!("{}/permissions-config", app.repo().await?.path()),
            command,
            false,
        ),
        PermsTarget::Project { key, command } => (
            format!(
                "workspaces/{}/projects/{}/permissions-config",
                app.workspace().await?,
                encode(key.as_str())
            ),
            command,
            true,
        ),
    };
    match command {
        PermsCommand::List { list, kind } => {
            let kind = match kind {
                PrincipalKind::Users => "users",
                PrincipalKind::Groups => "groups",
            };
            Outcome::resource(
                Kind::Permission,
                app.client()
                    .await?
                    .collection::<models::Permission>(
                        &format!("{path}/{kind}"),
                        &[],
                        list.limit as usize,
                    )
                    .await?,
            )
        }
        PermsCommand::Grant {
            principal,
            permission,
        } => {
            if !project && matches!(permission, PermissionLevel::CreateRepo) {
                bail!("create-repo permission only applies to projects");
            }
            let resource = principal_path(&path, principal)?;
            app.mutation(
                "perms grant",
                Method::Put,
                &resource,
                Some(json!({"permission":permission})),
                Kind::Permission,
                false,
            )
            .await
        }
        PermsCommand::Revoke { principal } => {
            app.mutation(
                "perms revoke",
                Method::Delete,
                &principal_path(&path, principal)?,
                None,
                Kind::Action,
                true,
            )
            .await
        }
    }
}
fn principal_path(path: &str, args: &PrincipalArgs) -> Result<String> {
    match (&args.user, &args.group) {
        (Some(user), None) if !user.is_empty() => Ok(format!("{path}/users/{}", encode(user))),
        (None, Some(group)) if !group.is_empty() => Ok(format!("{path}/groups/{}", encode(group))),
        _ => bail!("provide exactly one nonempty --user or --group"),
    }
}

pub async fn webhook(app: &App<'_>, command: &WebhookCommand) -> Result<Outcome> {
    let path = format!("{}/hooks", app.repo().await?.path());
    match command {
        WebhookCommand::List(list) => Outcome::resource(
            Kind::Webhook,
            app.client()
                .await?
                .collection::<models::Webhook>(&path, &[], list.limit as usize)
                .await?,
        ),
        WebhookCommand::Create {
            url,
            description,
            events,
            disabled,
        } => {
            if !matches!(url.scheme(), "https" | "http")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                bail!("webhook URL must use HTTP(S) without embedded credentials");
            }
            app.mutation("webhook create",Method::Post,&path,Some(json!({"url":url.as_str(),"description":description.as_deref().unwrap_or(""),"events":events,"active":!disabled})),Kind::Webhook,false).await
        }
        WebhookCommand::Delete { id } => {
            app.mutation(
                "webhook delete",
                Method::Delete,
                &format!("{path}/{}", encode(&format!("{{{id}}}"))),
                None,
                Kind::Action,
                true,
            )
            .await
        }
    }
}
