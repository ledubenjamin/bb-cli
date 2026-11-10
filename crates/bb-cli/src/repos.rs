use crate::args::{RepoCreateArgs, RepoEditArgs};
use crate::{
    app::{App, encode, quoted},
    args::{
        BranchCommand, ProtectCommand, RepoCommand, RestrictionArgs, RestrictionKind,
        ReviewersCommand, Visibility,
    },
    git,
    output::{Kind, Outcome},
};
use anyhow::{Context as _, Result, bail};
use bb_api::{Method, models};
use serde_json::{Value, json};

pub async fn repository(app: &App<'_>, command: &RepoCommand) -> Result<Outcome> {
    match command {
        RepoCommand::List(args) => {
            let query = crate::discovery::CatalogQuery {
                workspace: app.workspace().await?.to_string(),
                kind: crate::discovery::CatalogKind::Repositories,
                project: args.project.as_ref().map(ToString::to_string),
                text: args.search.clone().unwrap_or_default(),
            };
            Outcome::resource(
                Kind::Repository,
                app.client()
                    .await?
                    .collection::<models::Repository>(
                        &query.path(),
                        &query.parameters(),
                        args.list.limit as usize,
                    )
                    .await?,
            )
        }
        RepoCommand::Favorite { command } => crate::favorites::execute(app, command).await,
        RepoCommand::View { repository, web } => {
            let repo = match repository {
                Some(repo) => repo,
                None => app.repo().await?,
            };
            if *web && !app.cli.dry_run {
                crate::app::open_browser(&repo.web_url()).await?;
            }
            Outcome::resource(
                Kind::Repository,
                app.client().await?.repository(repo).await?,
            )
        }
        RepoCommand::Create(args) => create_repository(app, args).await,
        RepoCommand::Edit(args) => edit_repository(app, args).await,
        RepoCommand::Delete => {
            app.mutation(
                "repo delete",
                Method::Delete,
                &app.repo().await?.path(),
                None,
                Kind::Action,
                true,
            )
            .await
        }
        RepoCommand::Clone {
            repository,
            directory,
            ssh,
        } => {
            let url = git::clone_url(repository, *ssh || app.ssh());
            let mut args = vec!["clone".into(), "--".into(), url];
            if let Some(directory) = directory {
                args.push(
                    directory
                        .to_str()
                        .context("clone directory is not UTF-8")?
                        .into(),
                );
            }
            if !app.cli.dry_run {
                git::run_with_context(&args, true, app.cli.context.as_deref()).await?;
            }
            Outcome::action("repo clone", vec![json!({"git":args})], app.cli.dry_run)
        }
        RepoCommand::SetDefault { repository } => {
            let args = vec![
                "config".into(),
                "--local".into(),
                "bb.repository".into(),
                repository.to_string(),
            ];
            if !app.cli.dry_run {
                git::run(&args, false).await?;
            }
            Outcome::action(
                "repo set-default",
                vec![json!({"repository":repository})],
                app.cli.dry_run,
            )
        }
        RepoCommand::DefaultReviewers {
            command: ReviewersCommand::List(list),
        } => Outcome::resource(
            Kind::User,
            app.client()
                .await?
                .default_reviewers(app.repo().await?, list.limit as usize)
                .await?,
        ),
    }
}

async fn create_repository(app: &App<'_>, args: &RepoCreateArgs) -> Result<Outcome> {
    let RepoCreateArgs {
        repository,
        public,
        description,
        project,
    } = args;
    let mut body = json!({"scm":"git","is_private":!public});
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    if let Some(project) = project {
        body["project"] = json!({"key":project.as_str()});
    }
    app.mutation(
        "repo create",
        Method::Post,
        &repository.path(),
        Some(body),
        Kind::Repository,
        false,
    )
    .await
}

async fn edit_repository(app: &App<'_>, args: &RepoEditArgs) -> Result<Outcome> {
    let RepoEditArgs {
        description,
        name,
        visibility,
        default_branch,
        project,
    } = args;
    let mut body = json!({});
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    if let Some(visibility) = visibility {
        body["is_private"] = json!(matches!(visibility, Visibility::Private));
    }
    if let Some(branch) = default_branch {
        body["mainbranch"] = json!({"name":branch.as_str()});
    }
    if let Some(project) = project {
        body["project"] = json!({"key":project.as_str()});
    }
    if body.as_object().is_none_or(serde_json::Map::is_empty) {
        bail!("provide at least one repository property to edit");
    }
    app.mutation(
        "repo edit",
        Method::Put,
        &app.repo().await?.path(),
        Some(body),
        Kind::Repository,
        false,
    )
    .await
}

pub async fn branch(app: &App<'_>, command: &BranchCommand) -> Result<Outcome> {
    let repo = app.repo().await?;
    let path = format!("{}/refs/branches", repo.path());
    match command {
        BranchCommand::List { list, filter } => {
            let query = filter
                .as_ref()
                .map(|value| format!("name ~ {}", quoted(value)))
                .unwrap_or_default();
            Outcome::resource(
                Kind::Branch,
                app.client()
                    .await?
                    .branches(repo, &query, list.limit as usize)
                    .await?,
            )
        }
        BranchCommand::Create { name, source } => {
            app.mutation(
                "branch create",
                Method::Post,
                &path,
                Some(json!({"name":name.as_str(),"target":{"hash":source}})),
                Kind::Branch,
                false,
            )
            .await
        }
        BranchCommand::Delete { name } => {
            if !app.cli.dry_run {
                let repository = app.client().await?.repository(repo).await?;
                if repository
                    .mainbranch
                    .is_some_and(|branch| branch.name == name.as_str())
                {
                    bail!("the default branch cannot be deleted");
                }
            }
            app.mutation(
                "branch delete",
                Method::Delete,
                &format!("{path}/{}", encode(name.as_str())),
                None,
                Kind::Action,
                true,
            )
            .await
        }
        BranchCommand::SetDefault { name } => {
            app.mutation(
                "branch set-default",
                Method::Put,
                &repo.path(),
                Some(json!({"mainbranch":{"name":name.as_str()}})),
                Kind::Repository,
                false,
            )
            .await
        }
        BranchCommand::Rebase { base, remote } => {
            if remote.starts_with('-') || remote.chars().any(char::is_control) {
                bail!("invalid Git remote name");
            }
            git::require_clean(repo).await?;
            let _branch = git::current_branch().await?;
            let remote_url =
                git::run(&["remote".into(), "get-url".into(), remote.clone()], false).await?;
            let _: bb_api::RepoRef = remote_url
                .parse()
                .context("rebase remote must be a Bitbucket Cloud repository")?;
            let fetch = vec![
                "fetch".into(),
                "--".into(),
                remote.clone(),
                base.to_string(),
            ];
            let rebase = vec!["rebase".into(), "FETCH_HEAD".into()];
            if !app.cli.dry_run {
                git::run_with_context(&fetch, true, app.cli.context.as_deref()).await?;
                git::run(&rebase, false).await.context(
                    "resolve conflicts and run git rebase --continue, or git rebase --abort",
                )?;
            }
            Outcome::action(
                "branch rebase",
                vec![json!({"git":fetch}), json!({"git":rebase})],
                app.cli.dry_run,
            )
        }
        BranchCommand::Protect { command } => protection(app, command).await,
    }
}

async fn protection(app: &App<'_>, command: &ProtectCommand) -> Result<Outcome> {
    let path = format!("{}/branch-restrictions", app.repo().await?.path());
    match command {
        ProtectCommand::List(list) => Outcome::resource(
            Kind::Restriction,
            app.client()
                .await?
                .collection::<models::Restriction>(&path, &[], list.limit as usize)
                .await?,
        ),
        ProtectCommand::View { id } => Outcome::resource(
            Kind::Restriction,
            app.client()
                .await?
                .resource::<models::Restriction>(&format!("{path}/{id}"))
                .await?,
        ),
        ProtectCommand::Remove { id } => {
            app.mutation(
                "branch protect remove",
                Method::Delete,
                &format!("{path}/{id}"),
                None,
                Kind::Action,
                true,
            )
            .await
        }
        ProtectCommand::Add(rule) => {
            app.mutation(
                "branch protect add",
                Method::Post,
                &path,
                Some(restriction(rule)?),
                Kind::Restriction,
                false,
            )
            .await
        }
        ProtectCommand::Edit { id, rule } => {
            app.mutation(
                "branch protect edit",
                Method::Put,
                &format!("{path}/{id}"),
                Some(restriction(rule)?),
                Kind::Restriction,
                false,
            )
            .await
        }
    }
}
fn restriction(rule: &RestrictionArgs) -> Result<Value> {
    if rule.pattern.is_empty() {
        bail!("branch restriction pattern must not be empty");
    }
    let threshold = matches!(
        rule.kind,
        RestrictionKind::RequireApprovalsToMerge
            | RestrictionKind::RequirePassingBuildsToMerge
            | RestrictionKind::RequireDefaultReviewerApprovalsToMerge
    );
    if threshold != rule.value.is_some() {
        bail!("--value is required only for approval/build threshold restrictions");
    }
    let mut body = json!({"kind":rule.kind,"branch_match_kind":"glob","pattern":rule.pattern,
        "users":rule.user.iter().map(|user|json!({"uuid":user})).collect::<Vec<_>>(),
        "groups":rule.group.iter().map(|group|json!({"slug":group})).collect::<Vec<_>>()});
    if let Some(value) = rule.value {
        body["value"] = json!(value);
    }
    Ok(body)
}
