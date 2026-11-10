use crate::{
    app::App,
    args::LogsCommand,
    log_cache::Cache,
    output::{Kind, Outcome},
};
use anyhow::Result;
use serde_json::json;

pub async fn execute(app: &App<'_>, command: &LogsCommand) -> Result<Outcome> {
    let workspace = app.workspace().await?.to_string();
    match command {
        LogsCommand::Search { query, limit } => {
            crate::log_cache::validate_query(query)?;
            let cache = Cache::for_app(app).await?;
            let repository = app.cli.repo.as_ref().map(ToString::to_string).or_else(|| {
                app.cli
                    .context
                    .as_ref()
                    .or(app.config.active_context.as_ref())
                    .and_then(|name| app.config.contexts.get(name))
                    .and_then(|context| context.repository.as_ref())
                    .map(ToString::to_string)
            });
            Outcome::resource(
                Kind::LogMatch,
                cache
                    .search(workspace, repository, query.clone(), *limit)
                    .await?,
            )
        }
        LogsCommand::Clear => {
            if app.cli.dry_run {
                return Outcome::action("logs clear", vec![json!({"workspace":workspace})], true);
            }
            let cache = Cache::for_app(app).await?;
            let removed = cache.clear(workspace.clone()).await?;
            Outcome::action(
                "logs clear",
                vec![json!({"workspace":workspace,"logsRemoved":removed})],
                false,
            )
        }
    }
}
