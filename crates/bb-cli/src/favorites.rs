//! Local bookmarks; no authentication or network requests are needed.

use anyhow::Result;
use bb_api::RepoRef;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use crate::{
    app::App,
    args::FavoriteCommand,
    config::{Favorite, Store},
    output::{Kind, Outcome},
};

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteView {
    pub repository: RepoRef,
    pub workspace: String,
    pub api_origin: String,
}

pub fn origin(app: &App<'_>) -> Result<String> {
    Ok(crate::app::client(app.cli, None)?
        .resolve_url("")?
        .to_string())
}

pub fn repositories(favorites: &[Favorite], origin: &str) -> Vec<RepoRef> {
    let mut repositories: Vec<_> = favorites
        .iter()
        .filter(|favorite| favorite.api_origin == origin)
        .map(|favorite| favorite.repository.clone())
        .collect();
    repositories.sort_unstable_by_key(ToString::to_string);
    repositories.dedup();
    repositories
}

pub async fn set(
    store: &Store,
    api_origin: &str,
    repository: &RepoRef,
    enabled: bool,
) -> Result<Vec<RepoRef>> {
    store
        .update(|config| {
            let favorites = &mut config.preferences.favorite_repositories;
            favorites.retain(|favorite| {
                favorite.api_origin != api_origin || favorite.repository != *repository
            });
            if enabled {
                favorites.push(Favorite {
                    api_origin: api_origin.into(),
                    repository: repository.clone(),
                });
            }
            Ok(repositories(favorites, api_origin))
        })
        .await
}

pub async fn execute(app: &App<'_>, command: &FavoriteCommand) -> Result<Outcome> {
    let origin = origin(app)?;
    let (repository, enabled) = match command {
        FavoriteCommand::List => {
            let workspace = app.workspace().await?;
            let values: Vec<_> =
                repositories(&app.config.preferences.favorite_repositories, &origin)
                    .into_iter()
                    .filter(|repository| repository.workspace() == &workspace)
                    .map(|repository| FavoriteView {
                        repository,
                        workspace: workspace.to_string(),
                        api_origin: origin.clone(),
                    })
                    .collect();
            return Outcome::resource(Kind::Favorite, values);
        }
        FavoriteCommand::Add { repository } => (repository, true),
        FavoriteCommand::Remove { repository } => (repository, false),
    };
    if !app.cli.dry_run {
        set(&app.store, &origin, repository, enabled).await?;
    }
    Outcome::action(
        if enabled {
            "repo favorite add"
        } else {
            "repo favorite remove"
        },
        vec![json!({"repository":repository,"favorite":enabled,"apiOrigin":origin})],
        app.cli.dry_run,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_updates_merge_origins_and_preserve_preferences() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::for_test(directory.path().to_owned());
        store
            .update(|config| {
                config.preferences.default_workspace = Some("acme".parse()?);
                Ok(())
            })
            .await?;
        let a: RepoRef = "acme/service".parse()?;
        let b: RepoRef = "other/worker".parse()?;
        let (first, second, third) = tokio::join!(
            set(&store, "https://api.bitbucket.org/2.0/", &a, true),
            set(&store, "https://api.bitbucket.org/2.0/", &b, true),
            set(&store, "http://localhost:1234/2.0/", &a, true),
        );
        first?;
        second?;
        third?;
        set(&store, "https://api.bitbucket.org/2.0/", &a, true).await?;
        let config = store.load().await?;
        assert_eq!(config.preferences.favorite_repositories.len(), 3);
        assert_eq!(
            config
                .preferences
                .default_workspace
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("acme")
        );
        for _ in 0..2 {
            set(&store, "https://api.bitbucket.org/2.0/", &a, false).await?;
        }
        let config = store.load().await?;
        assert_eq!(
            repositories(
                &config.preferences.favorite_repositories,
                "https://api.bitbucket.org/2.0/"
            ),
            vec![b]
        );
        assert_eq!(
            repositories(
                &config.preferences.favorite_repositories,
                "http://localhost:1234/2.0/"
            ),
            vec![a]
        );
        Ok(())
    }

    #[test]
    fn old_configuration_has_no_favorites() -> Result<()> {
        let config: crate::config::Config =
            toml::from_str("[preferences]\ndefault_workspace = 'acme'\n")?;
        assert_eq!(
            config.preferences.favorite_repositories,
            [] as [crate::config::Favorite; 0]
        );
        Ok(())
    }
}
