use crate::args::{
    PrCheckoutArgs, PrCloseArgs, PrCommentArgs, PrCreateArgs, PrDiffArgs, PrEditArgs, PrListArgs,
    PrMergeArgs, PrReviewArgs, PrViewArgs,
};
use crate::{
    app::{self, App, Failure, encode, quoted},
    args::{
        CommentState, CommentsCommand, CommitCommand, DiffSide, PrCommand, PrState, PrTarget,
        TaskCommand, WatchArgs,
    },
    git,
    output::{self, Kind, Outcome},
};
use anyhow::{Context as _, Result, bail};
use bb_api::{Method, PullRequestId, RepoRef, models};
use serde_json::{Value, json};
use std::time::Duration;

async fn target(app: &App<'_>, pr: &PrTarget) -> Result<(RepoRef, PullRequestId)> {
    if let Some(value) = &pr.target {
        if value.starts_with("https://") {
            let url = url::Url::parse(value)?;
            if url.host_str() != Some("bitbucket.org")
                || !url.username().is_empty()
                || url.port().is_some()
            {
                bail!("expected a Bitbucket Cloud PR URL");
            }
            let segments: Vec<_> = url.path().trim_matches('/').split('/').collect();
            if segments.len() != 4 || segments[2] != "pull-requests" {
                bail!("expected https://bitbucket.org/workspace/repository/pull-requests/number");
            }
            let repo: RepoRef = format!("{}/{}", segments[0], segments[1]).parse()?;
            if app
                .cli
                .repo
                .as_ref()
                .is_some_and(|explicit| explicit != &repo)
            {
                bail!("PR URL conflicts with --repo/BB_REPO");
            }
            return Ok((repo, segments[3].parse()?));
        }
        if value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok((app.repo().await?.clone(), value.parse()?));
        }
    }
    let repo = app.repo().await?.clone();
    let branch = match &pr.target {
        Some(branch) => branch.parse::<bb_api::BranchName>()?,
        None => git::current_branch().await?,
    };
    let query = format!(
        "state = \"OPEN\" AND source.branch.name = {}",
        quoted(branch.as_str())
    );
    let prs = app.client().await?.pull_requests(&repo, &query, 2).await?;
    if prs.len() != 1 {
        bail!(
            "expected one open PR for branch '{branch}', found {}; specify a PR number",
            prs.len()
        );
    }
    Ok((
        repo,
        prs.first().context("missing PR")?.id.to_string().parse()?,
    ))
}
fn path(repo: &RepoRef, id: PullRequestId) -> String {
    format!("{}/pullrequests/{id}", repo.path())
}
fn identity(value: &str) -> Value {
    if let Ok(id) = uuid::Uuid::parse_str(value) {
        json!({"uuid":format!("{{{id}}}")})
    } else {
        json!({"account_id":value})
    }
}

pub async fn execute(app: &App<'_>, command: &PrCommand) -> Result<Outcome> {
    match command {
        PrCommand::List(args) => list(app, args).await,
        PrCommand::View(args) => view(app, args).await,
        PrCommand::Create(args) => create(app, args).await,
        PrCommand::Edit(args) => edit(app, args).await,
        PrCommand::Checkout(args) => checkout(app, args).await,
        PrCommand::Diff(args) => diff_command(app, args).await,
        PrCommand::Close(args) => close(app, args).await,
        PrCommand::Reopen { pr } | PrCommand::Ready { pr, .. } => {
            let (repo, id) = target(app, pr).await?;
            let (operation, payload) = match command {
                PrCommand::Reopen { .. } => ("pr reopen", json!({"state":"OPEN"})),
                PrCommand::Ready { undo, .. } => ("pr ready", json!({"draft":undo})),
                _ => unreachable!("matched reopen or ready"),
            };
            app.mutation(
                operation,
                Method::Put,
                &path(&repo, id),
                Some(payload),
                Kind::PullRequest,
                false,
            )
            .await
        }
        PrCommand::Merge(args) => merge(app, args).await,
        PrCommand::Review(args) => review(app, args).await,
        PrCommand::Comment(args) => comment(app, args).await,
        PrCommand::Comments { command } => comments(app, command).await,
        PrCommand::Task { command } => tasks(app, command).await,
        PrCommand::Checks { pr, watch } => {
            let (repo, id) = target(app, pr).await?;
            let pr = app.client().await?.pull_request(&repo, id).await?;
            let source = pr
                .source
                .repository
                .as_ref()
                .map(|r| r.full_name.parse::<RepoRef>())
                .transpose()?
                .unwrap_or(repo);
            checks(app, &source, &pr.source.commit.hash, watch).await
        }
        PrCommand::Status => status(app).await,
    }
}

async fn list(app: &App<'_>, args: &PrListArgs) -> Result<Outcome> {
    let PrListArgs {
        list,
        state,
        author,
        mine,
        base,
        head,
    } = args;
    let mut filters = Vec::new();
    match state {
        PrState::Open => filters.push("state = \"OPEN\"".into()),
        PrState::Closed => {
            filters.push("(state = \"DECLINED\" OR state = \"SUPERSEDED\")".into());
        }
        PrState::Merged => filters.push("state = \"MERGED\"".into()),
        PrState::All => {}
    }
    if *mine && author.is_some() {
        bail!("--mine conflicts with --author");
    }
    if *mine {
        let user = app.client().await?.current_user().await?;
        filters.push(format!("author.uuid = {}", quoted(&user.uuid)));
    }
    if let Some(author) = author {
        filters.push(format!("author.uuid = {}", quoted(author)));
    }
    if let Some(base) = base {
        filters.push(format!(
            "destination.branch.name = {}",
            quoted(base.as_str())
        ));
    }
    if let Some(head) = head {
        filters.push(format!("source.branch.name = {}", quoted(head.as_str())));
    }
    Outcome::resource(
        Kind::PullRequest,
        app.client()
            .await?
            .pull_requests(
                app.repo().await?,
                &filters.join(" AND "),
                list.limit as usize,
            )
            .await?,
    )
}

async fn view(app: &App<'_>, args: &PrViewArgs) -> Result<Outcome> {
    let PrViewArgs { pr, web } = args;
    let (repo, id) = target(app, pr).await?;
    let client = app.client().await?;
    let pr = client.pull_request(&repo, id).await?;
    if *web && !app.cli.dry_run {
        app::open_browser(&format!("{}/pull-requests/{id}", repo.web_url())).await?;
    }
    let head = pr.source.commit.hash.clone();
    let source = pr
        .source
        .repository
        .as_ref()
        .map(|r| r.full_name.parse::<RepoRef>())
        .transpose()?
        .unwrap_or_else(|| repo.clone());
    let wants = |field: &str| {
        app.cli
            .json
            .as_ref()
            .is_some_and(|s| s == "*" || s.split(',').any(|name| name == field))
    };
    let (comments, files, tasks, checks) = tokio::try_join!(
        async {
            if wants("comments") {
                Ok::<_, anyhow::Error>(Some(client.comments(&repo, id, usize::MAX).await?))
            } else {
                Ok(None)
            }
        },
        async {
            if wants("files") {
                Ok::<_, anyhow::Error>(Some(client.diffstat(&repo, id, usize::MAX).await?))
            } else {
                Ok(None)
            }
        },
        async {
            if wants("tasks") {
                Ok::<_, anyhow::Error>(Some(client.tasks(&repo, id, usize::MAX).await?))
            } else {
                Ok(None)
            }
        },
        async {
            if wants("checks") {
                Ok::<_, anyhow::Error>(Some(client.checks(&source, &head, usize::MAX).await?))
            } else {
                Ok(None)
            }
        }
    )?;
    let mut outcome = Outcome::resource(Kind::PullRequest, pr)?;
    for (field, value) in [
        ("comments", serde_json::to_value(comments)?),
        ("files", serde_json::to_value(files)?),
        ("tasks", serde_json::to_value(tasks)?),
        ("checks", serde_json::to_value(checks)?),
    ] {
        outcome.value[field] = output::camel_value(value);
    }
    Ok(outcome)
}

async fn create(app: &App<'_>, args: &PrCreateArgs) -> Result<Outcome> {
    let PrCreateArgs {
        title,
        body,
        base,
        head,
        head_repo,
        reviewer,
        draft,
        close_source_branch,
    } = args;
    let title = app::required(title.clone(), "--title")?;
    let repo = app.repo().await?;
    let head = match head {
        Some(head) => head.clone(),
        None => git::current_branch().await?,
    };
    let base = match base {
        Some(base) => base.clone(),
        None => app
            .client()
            .await?
            .repository(repo)
            .await?
            .mainbranch
            .context("repository has no default branch; provide --base")?
            .name
            .parse()?,
    };
    if head_repo.as_ref().is_none_or(|source| source == repo) && head == base {
        bail!("source and destination branches must differ");
    }
    let reviewers = reviewer
        .iter()
        .map(|value| value.parse::<bb_api::requests::Reviewer>())
        .collect::<Result<Vec<_>, _>>()?;
    let mut payload = bb_api::requests::CreatePullRequest::new(title, head, base)?
        .draft(*draft)
        .close_source_branch(*close_source_branch)
        .reviewers(reviewers);
    if let Some(source) = head_repo {
        payload = payload.head_repository(source.clone());
    }
    if let Some(body) = app::body(body).await? {
        payload = payload.description(body);
    }
    let payload = serde_json::to_value(payload)?;
    app.mutation(
        "pr create",
        Method::Post,
        &format!("{}/pullrequests", repo.path()),
        Some(payload),
        Kind::PullRequest,
        false,
    )
    .await
}

async fn edit(app: &App<'_>, args: &PrEditArgs) -> Result<Outcome> {
    let PrEditArgs {
        pr,
        title,
        body,
        base,
        add_reviewer,
        remove_reviewer,
    } = args;
    let (repo, id) = target(app, pr).await?;
    let mut payload = json!({});
    if let Some(title) = title {
        payload["title"] = json!(app::required(Some(title.clone()), "--title")?);
    }
    if let Some(body) = app::body(body).await? {
        payload["description"] = json!(body);
    }
    if let Some(base) = base {
        payload["destination"] = json!({"branch":{"name":base.as_str()}});
    }
    if !add_reviewer.is_empty() || !remove_reviewer.is_empty() {
        let existing = app.client().await?.pull_request(&repo, id).await?;
        let mut reviewers: Vec<Value> = existing
            .reviewers
            .into_iter()
            .filter(|user| {
                !remove_reviewer.iter().any(|id| {
                    id == &user.uuid
                        || user.account_id.as_ref() == Some(id)
                        || uuid::Uuid::parse_str(id)
                            .is_ok_and(|id| uuid::Uuid::parse_str(&user.uuid).ok() == Some(id))
                })
            })
            .map(|user| identity(&user.uuid))
            .collect();
        for reviewer in add_reviewer {
            let reviewer = identity(reviewer);
            if !reviewers.contains(&reviewer) {
                reviewers.push(reviewer);
            }
        }
        payload["reviewers"] = json!(reviewers);
    }
    if payload.as_object().is_none_or(serde_json::Map::is_empty) {
        bail!("provide a pull request property to edit");
    }
    app.mutation(
        "pr edit",
        Method::Put,
        &path(&repo, id),
        Some(payload),
        Kind::PullRequest,
        false,
    )
    .await
}

async fn checkout(app: &App<'_>, args: &PrCheckoutArgs) -> Result<Outcome> {
    let PrCheckoutArgs { pr, branch } = args;
    let (repo, id) = target(app, pr).await?;
    git::require_clean(&repo).await?;
    let pr = app.client().await?.pull_request(&repo, id).await?;
    let source: RepoRef = pr
        .source
        .repository
        .context("source fork was deleted")?
        .full_name
        .parse()?;
    let source_branch: bb_api::BranchName = pr.source.branch.name.parse()?;
    let local = branch
        .as_ref()
        .map_or_else(|| format!("pr/{id}"), ToString::to_string);
    let fetch = vec![
        "fetch".into(),
        "--".into(),
        git::clone_url(&source, app.ssh()),
        source_branch.to_string(),
    ];
    let checkout = vec![
        "switch".into(),
        "--create".into(),
        local,
        "FETCH_HEAD".into(),
    ];
    if !app.cli.dry_run {
        git::run_with_context(&fetch, true, app.cli.context.as_deref()).await?;
        git::run(&checkout, false).await?;
    }
    Outcome::action(
        "pr checkout",
        vec![json!({"git":fetch}), json!({"git":checkout})],
        app.cli.dry_run,
    )
}

async fn diff_command(app: &App<'_>, args: &PrDiffArgs) -> Result<Outcome> {
    let PrDiffArgs { pr, stat } = args;
    let (repo, id) = target(app, pr).await?;
    if *stat {
        Outcome::resource(
            Kind::File,
            app.client().await?.diffstat(&repo, id, usize::MAX).await?,
        )
    } else {
        diff(app, &format!("{}/diff", path(&repo, id))).await
    }
}

async fn close(app: &App<'_>, args: &PrCloseArgs) -> Result<Outcome> {
    let PrCloseArgs {
        pr,
        body,
        delete_branch,
    } = args;
    let (repo, id) = target(app, pr).await?;
    let payload = app::body(body).await?.map(|body| json!({"message":body}));
    let old = if *delete_branch {
        let old = app.client().await?.pull_request(&repo, id).await?;
        let source: RepoRef = old
            .source
            .repository
            .as_ref()
            .context("source repository is missing")?
            .full_name
            .parse()?;
        if app
            .client()
            .await?
            .repository(&source)
            .await?
            .mainbranch
            .is_some_and(|branch| branch.name == old.source.branch.name)
        {
            bail!("the source is a default branch; omit --delete-branch");
        }
        Some(old)
    } else {
        None
    };
    let branch_path = old
        .as_ref()
        .map(|old| {
            let source: RepoRef = old
                .source
                .repository
                .as_ref()
                .context("source repository is missing")?
                .full_name
                .parse()?;
            Ok::<_, anyhow::Error>(format!(
                "{}/refs/branches/{}",
                source.path(),
                encode(&old.source.branch.name)
            ))
        })
        .transpose()?;
    if app.cli.dry_run {
        let mut outcome = App::plan(
            "pr close",
            Method::Post,
            &format!("{}/decline", path(&repo, id)),
            payload.as_ref(),
        )?;
        if let Some(branch) = branch_path {
            outcome.value["actions"]
                .as_array_mut()
                .context("invalid plan")?
                .push(json!({"method":"DELETE","path":branch}));
        }
        return Ok(outcome);
    }
    if *delete_branch {
        app.confirm("Decline the PR and delete its source branch?")
            .await?;
    }
    app.send(
        Method::Post,
        &format!("{}/decline", path(&repo, id)),
        payload.as_ref(),
        false,
    )
    .await?;
    if let Some(branch) = branch_path {
        app.send(Method::Delete, &branch, None, false)
            .await
            .context("PR declined, but branch deletion failed")?;
    }
    Outcome::resource(
        Kind::PullRequest,
        app.client().await?.pull_request(&repo, id).await?,
    )
}

async fn merge(app: &App<'_>, args: &PrMergeArgs) -> Result<Outcome> {
    let PrMergeArgs {
        pr,
        merge,
        squash,
        ff_only,
        delete_branch,
        message,
        timeout,
    } = args;
    let (repo, id) = target(app, pr).await?;
    let mut payload = json!({"close_source_branch":delete_branch});
    if *merge {
        payload["merge_strategy"] = json!("merge_commit");
    } else if *squash {
        payload["merge_strategy"] = json!("squash");
    } else if *ff_only {
        payload["merge_strategy"] = json!("fast_forward");
    }
    if let Some(message) = message {
        payload["message"] = json!(message);
    }
    let path = path(&repo, id);
    if app.cli.dry_run {
        return App::plan(
            "pr merge",
            Method::Post,
            &format!("{path}/merge"),
            Some(&payload),
        );
    }
    if *delete_branch {
        app.confirm("Merge the PR and delete its source branch?")
            .await?;
    }
    let response = app
        .send(
            Method::Post,
            &format!("{path}/merge"),
            Some(&payload),
            false,
        )
        .await?;
    if let Some(task) = response.get("task_id").and_then(Value::as_str) {
        let wait = async {
            loop {
                let status: Value = app
                    .client()
                    .await?
                    .resource(&format!("{path}/merge/task-status/{}", encode(task)))
                    .await?;
                match status.get("task_status").and_then(Value::as_str) {
                    Some("SUCCESS") => break,
                    Some("PENDING") => tokio::time::sleep(Duration::from_secs(2)).await,
                    other => bail!("merge task {task} failed with status {other:?}"),
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        if *timeout == 0 {
            wait.await?;
        } else {
            match tokio::time::timeout(Duration::from_secs(*timeout),wait).await {
                Ok(result)=>result?,Err(_)=>return Err(Failure{code:"pending",message:format!("merge task {task} may still be running; inspect bb pr view {id} before retrying"),exit:8}.into()),
            }
        }
    }
    let pr = app.client().await?.pull_request(&repo, id).await?;
    if !matches!(pr.state, models::PullRequestState::Merged) {
        bail!("PR is not MERGED; inspect its state before retrying");
    }
    Outcome::resource(Kind::PullRequest, pr)
}

async fn review(app: &App<'_>, args: &PrReviewArgs) -> Result<Outcome> {
    let PrReviewArgs {
        pr,
        approve,
        comment,
        body,
    } = args;
    if !approve && !comment {
        bail!("choose --approve or --comment");
    }
    if *approve && (body.body.is_some() || body.body_file.is_some()) {
        bail!("--approve does not accept a body; submit a separate --comment review");
    }
    let (repo, id) = target(app, pr).await?;
    let (suffix, payload) = if *approve {
        ("approve", None)
    } else {
        (
            "comments",
            Some(
                json!({"content":{"raw":app::required(app::body(body).await?,"--body or --body-file")?}}),
            ),
        )
    };
    if app.cli.dry_run {
        return App::plan(
            "pr review",
            Method::Post,
            &format!("{}/{suffix}", path(&repo, id)),
            payload.as_ref(),
        );
    }
    app.send(
        Method::Post,
        &format!("{}/{suffix}", path(&repo, id)),
        payload.as_ref(),
        false,
    )
    .await?;
    Outcome::resource(
        Kind::PullRequest,
        app.client().await?.pull_request(&repo, id).await?,
    )
}

async fn comment(app: &App<'_>, args: &PrCommentArgs) -> Result<Outcome> {
    let PrCommentArgs {
        pr,
        body,
        path: file,
        line,
        side,
        parent,
        pending,
    } = args;
    let content = app::required(app::body(body).await?, "--body or --body-file")?;
    let (repo, id) = target(app, pr).await?;
    let mut payload = json!({"content":{"raw":content},"pending":pending});
    if let Some(parent) = parent {
        payload["parent"] = json!({"id":parent.get()});
    }
    if let (Some(file), Some(line)) = (file, line) {
        if file.is_empty() || file.starts_with('/') || file.split('/').any(|part| part == "..") {
            bail!("inline path must be repository-relative");
        }
        let side = match side {
            DiffSide::Old => "from",
            DiffSide::New => "to",
        };
        payload["inline"] = json!({"path":file,side:line});
    }
    app.mutation(
        "pr comment",
        Method::Post,
        &format!("{}/comments", path(&repo, id)),
        Some(payload),
        Kind::Comment,
        false,
    )
    .await
}

async fn comments(app: &App<'_>, command: &CommentsCommand) -> Result<Outcome> {
    match command {
        CommentsCommand::List { pr, list, state } => {
            let (repo, id) = target(app, pr).await?;
            let limit = if matches!(state, CommentState::All) {
                list.limit as usize
            } else {
                usize::MAX
            };
            let comments = app.client().await?.comments(&repo, id, limit).await?;
            let values: Vec<_> = comments
                .into_iter()
                .filter(|comment| match state {
                    CommentState::All => true,
                    CommentState::Resolved => !comment.deleted && comment.resolution.is_some(),
                    CommentState::Unresolved => !comment.deleted && comment.resolution.is_none(),
                    CommentState::Deleted => comment.deleted,
                })
                .take(list.limit as usize)
                .collect();
            Outcome::resource(Kind::Comment, values)
        }
        CommentsCommand::Delete { pr, comment }
        | CommentsCommand::Resolve { pr, comment }
        | CommentsCommand::Reopen { pr, comment } => {
            let mut path = format!("{}/comments/{comment}", path(app.repo().await?, *pr));
            let (operation, method, destructive) = match command {
                CommentsCommand::Delete { .. } => ("pr comments delete", Method::Delete, true),
                CommentsCommand::Resolve { .. } => ("pr comments resolve", Method::Post, false),
                _ => ("pr comments reopen", Method::Delete, false),
            };
            if !destructive {
                let current: models::Comment = app.client().await?.resource(&path).await?;
                if current.parent.is_some() {
                    bail!("use a top-level thread comment ID");
                }
                path.push_str("/resolve");
            }
            app.mutation(operation, method, &path, None, Kind::Action, destructive)
                .await
        }
    }
}
async fn tasks(app: &App<'_>, command: &TaskCommand) -> Result<Outcome> {
    match command {
        TaskCommand::List { pr, list } => {
            let (repo, id) = target(app, pr).await?;
            Outcome::resource(
                Kind::Task,
                app.client()
                    .await?
                    .tasks(&repo, id, list.limit as usize)
                    .await?,
            )
        }
        TaskCommand::Create { pr, body } => {
            let body = app::required(app::body(body).await?, "--body or --body-file")?;
            let (repo, id) = target(app, pr).await?;
            app.mutation(
                "pr task create",
                Method::Post,
                &format!("{}/tasks", path(&repo, id)),
                Some(json!({"content":{"raw":body}})),
                Kind::Task,
                false,
            )
            .await
        }
        TaskCommand::Complete { pr, task } | TaskCommand::Reopen { pr, task } => {
            let state = if matches!(command, TaskCommand::Complete { .. }) {
                "RESOLVED"
            } else {
                "UNRESOLVED"
            };
            app.mutation(
                "pr task update",
                Method::Put,
                &format!("{}/tasks/{task}", path(app.repo().await?, *pr)),
                Some(json!({"state":state})),
                Kind::Task,
                false,
            )
            .await
        }
    }
}
pub async fn status(app: &App<'_>) -> Result<Outcome> {
    let repo = app.repo().await?;
    let client = app.client().await?;
    let user = client.current_user().await?;
    let mine = format!("state = \"OPEN\" AND author.uuid = {}", quoted(&user.uuid));
    let assigned = format!(
        "state = \"OPEN\" AND reviewers.uuid = {}",
        quoted(&user.uuid)
    );
    let (created, review) = tokio::try_join!(
        client.pull_requests(repo, &mine, 30),
        client.pull_requests(repo, &assigned, 30)
    )?;
    Ok(Outcome::new(
        Kind::Status,
        json!({"repository":repo.to_string(),"createdByMe":output::normalize(Kind::PullRequest,serde_json::to_value(created)?)?,"reviewRequested":output::normalize(Kind::PullRequest,serde_json::to_value(review)?)?}),
    ))
}
pub async fn commit(app: &App<'_>, command: &CommitCommand) -> Result<Outcome> {
    let repo = app.repo().await?;
    match command {
        CommitCommand::Status { sha, watch } => checks(app, repo, sha, watch).await,
        CommitCommand::Diff { base, head, stat } => {
            let path = format!(
                "{}/{}/{}..{}",
                repo.path(),
                if *stat { "diffstat" } else { "diff" },
                encode(head),
                encode(base)
            );
            if *stat {
                Outcome::resource(
                    Kind::File,
                    app.client()
                        .await?
                        .collection::<models::DiffStat>(&path, &[], usize::MAX)
                        .await?,
                )
            } else {
                diff(app, &path).await
            }
        }
    }
}
async fn diff(app: &App<'_>, path: &str) -> Result<Outcome> {
    if app.cli.json.is_none() {
        app.client()
            .await?
            .download(path, &mut tokio::io::stdout())
            .await?;
        return Ok(Outcome::text(""));
    }
    let response = app
        .client()
        .await?
        .raw(Method::Get, path, &[], None, &[])
        .await?;
    Ok(Outcome::text(
        String::from_utf8(response.body).context("diff is not UTF-8")?,
    ))
}
pub async fn checks(
    app: &App<'_>,
    repo: &RepoRef,
    sha: &str,
    watch: &WatchArgs,
) -> Result<Outcome> {
    let deadline = if !watch.watch || watch.timeout == 0 {
        None
    } else {
        Some(tokio::time::Instant::now() + Duration::from_secs(watch.timeout))
    };
    let mut previous: Option<Vec<models::Check>> = None;
    loop {
        let poll = async {
            if previous.is_some() {
                tokio::time::sleep(Duration::from_secs(watch.interval)).await;
            }
            Ok::<_, anyhow::Error>(app.client().await?.checks(repo, sha, usize::MAX).await?)
        };
        let checks = if let Some(deadline) = deadline {
            if let Ok(result) = tokio::time::timeout_at(deadline, poll).await {
                result?
            } else {
                let mut outcome = Outcome::resource(Kind::Check, previous.unwrap_or_default())?;
                outcome.code = 8;
                return Ok(outcome);
            }
        } else {
            poll.await?
        };
        let failed = checks
            .iter()
            .any(|check| matches!(check.state.as_str(), "FAILED" | "STOPPED" | "ERROR"));
        let pending = checks.is_empty()
            || checks.iter().any(|check| {
                !matches!(
                    check.state.as_str(),
                    "SUCCESSFUL" | "FAILED" | "STOPPED" | "ERROR"
                )
            });
        if !watch.watch || !pending || failed && watch.fail_fast {
            let mut outcome = Outcome::resource(Kind::Check, checks)?;
            outcome.code = if failed {
                1
            } else if pending {
                8
            } else {
                0
            };
            return Ok(outcome);
        }
        previous = Some(checks);
    }
}
