use crate::{
    Client, Error, Method, PullRequestId, RepoRef, Workspace,
    models::{Branch, Check, Comment, DiffStat, Project, PullRequest, Repository, Task, User},
};

impl Client {
    /// Fetches the authenticated user; resource tokens may not have a user identity.
    ///
    /// # Errors
    /// Returns transport, decoding, or API permission errors.
    pub async fn current_user(&self) -> Result<User, Error> {
        self.get("user").await
    }

    /// Lists repositories within a workspace.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn repositories(
        &self,
        workspace: &Workspace,
        limit: usize,
    ) -> Result<Vec<Repository>, Error> {
        self.list(&format!("repositories/{workspace}"), &[], limit)
            .await
    }

    /// Fetches a repository's metadata.
    ///
    /// # Errors
    /// Returns transport, decoding, or API errors.
    pub async fn repository(&self, repo: &RepoRef) -> Result<Repository, Error> {
        self.get(&repo.path()).await
    }

    /// Creates a pull request using a validated request body.
    ///
    /// The mutation is submitted once; transport errors can leave its outcome uncertain.
    /// # Errors
    /// Returns transport, permission, conflict, or response decoding errors.
    pub async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &crate::requests::CreatePullRequest,
    ) -> Result<PullRequest, Error> {
        self.mutate(
            Method::Post,
            &format!("{}/pullrequests", repo.path()),
            Some(request),
        )
        .await
    }

    /// Lists pull requests using an optional native Bitbucket query.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn pull_requests(
        &self,
        repo: &RepoRef,
        query: &str,
        limit: usize,
    ) -> Result<Vec<PullRequest>, Error> {
        let mut query = if query.is_empty() {
            vec![]
        } else {
            vec![("q".into(), query.into())]
        };
        // Override the API's implicit OPEN filter before applying the caller's query.
        query.extend(
            ["OPEN", "MERGED", "DECLINED", "SUPERSEDED"]
                .map(|state| ("state".into(), state.into())),
        );
        self.list(&format!("{}/pullrequests", repo.path()), &query, limit)
            .await
    }

    /// Fetches a pull request.
    ///
    /// # Errors
    /// Returns transport, decoding, or API errors.
    pub async fn pull_request(
        &self,
        repo: &RepoRef,
        id: PullRequestId,
    ) -> Result<PullRequest, Error> {
        self.get(&format!("{}/pullrequests/{id}", repo.path()))
            .await
    }

    /// Lists reviewers inherited by a repository.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn default_reviewers(
        &self,
        repo: &RepoRef,
        limit: usize,
    ) -> Result<Vec<User>, Error> {
        #[derive(serde::Deserialize)]
        struct Reviewer {
            user: User,
        }
        let reviewers: Vec<Reviewer> = self
            .list(
                &format!("{}/effective-default-reviewers", repo.path()),
                &[],
                limit,
            )
            .await?;
        Ok(reviewers
            .into_iter()
            .map(|reviewer| reviewer.user)
            .collect())
    }

    /// Lists comments, including inline locations and resolution metadata.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn comments(
        &self,
        repo: &RepoRef,
        id: PullRequestId,
        limit: usize,
    ) -> Result<Vec<Comment>, Error> {
        self.list(
            &format!("{}/pullrequests/{id}/comments", repo.path()),
            &[],
            limit,
        )
        .await
    }

    /// Lists pull request tasks.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn tasks(
        &self,
        repo: &RepoRef,
        id: PullRequestId,
        limit: usize,
    ) -> Result<Vec<Task>, Error> {
        self.list(
            &format!("{}/pullrequests/{id}/tasks", repo.path()),
            &[],
            limit,
        )
        .await
    }

    /// Lists per-file changes for a pull request.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn diffstat(
        &self,
        repo: &RepoRef,
        id: PullRequestId,
        limit: usize,
    ) -> Result<Vec<DiffStat>, Error> {
        self.list(
            &format!("{}/pullrequests/{id}/diffstat", repo.path()),
            &[],
            limit,
        )
        .await
    }

    /// Lists commit build statuses.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn checks(
        &self,
        repo: &RepoRef,
        commit: &str,
        limit: usize,
    ) -> Result<Vec<Check>, Error> {
        self.list(
            &format!("{}/commit/{}/statuses", repo.path(), encode_segment(commit)),
            &[],
            limit,
        )
        .await
    }

    /// Lists repository branches.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn branches(
        &self,
        repo: &RepoRef,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Branch>, Error> {
        let query = if query.is_empty() {
            vec![]
        } else {
            vec![("q".into(), query.into())]
        };
        self.list(&format!("{}/refs/branches", repo.path()), &query, limit)
            .await
    }

    /// Lists workspace projects.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or decoding errors.
    pub async fn projects(
        &self,
        workspace: &Workspace,
        limit: usize,
    ) -> Result<Vec<Project>, Error> {
        self.list(&format!("workspaces/{workspace}/projects"), &[], limit)
            .await
    }

    /// Fetches a typed resource at an API-relative path.
    ///
    /// This escape hatch is useful for resource types not covered by convenience methods.
    /// # Errors
    /// Returns HTTP or deserialization errors and rejects paths outside the API origin.
    pub async fn resource<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.get(path).await
    }

    /// Lists a typed, paginated resource using a total item limit.
    ///
    /// # Errors
    /// Returns HTTP, pagination, or deserialization errors.
    pub async fn collection<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
        limit: usize,
    ) -> Result<Vec<T>, Error> {
        self.list(path, query, limit).await
    }

    /// Submits a typed mutation payload and decodes the response.
    ///
    /// The request is sent once. A transport failure can leave its outcome uncertain.
    /// # Errors
    /// Returns serialization, HTTP, transport, or response decoding errors.
    pub async fn mutate<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, Error> {
        if method.is_read() {
            return Err(Error::invalid("mutate requires a write method"));
        }
        self.json(method, path, body).await
    }
}

pub(crate) fn encode_segment(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .fold(String::new(), |mut output, &byte| {
            use std::fmt::Write;
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                output.push(char::from(byte));
            } else {
                let _ = write!(output, "%{byte:02X}");
            }
            output
        })
}
