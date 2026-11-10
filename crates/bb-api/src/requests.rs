//! Validated request bodies for common write operations.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{BranchName, Error, RepoRef};

/// The source or destination branch of a new pull request.
#[derive(Debug, Clone, Serialize, JsonSchema)]
struct BranchSelection {
    name: BranchName,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
struct RepositorySelection {
    full_name: RepoRef,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
struct PullRequestSide {
    branch: BranchSelection,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<RepositorySelection>,
}

/// An explicit reviewer identified by UUID or Atlassian account ID.
///
/// Parsing a UUID produces Bitbucket's braced, lowercase UUID representation.
/// Other strings are treated as opaque account IDs and must be nonempty, without
/// whitespace or control characters. Use [`Self::from_account_id`] to explicitly
/// select the account-ID representation, including for UUID-shaped identifiers.
///
/// # Examples
/// ```
/// use bb_api::requests::Reviewer;
///
/// let account = Reviewer::from_account_id("712020:example-account")?;
/// let uuid: Reviewer = "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE".parse()?;
/// assert_eq!(
///     serde_json::to_value(uuid)?,
///     serde_json::json!({"uuid": "{aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee}"})
/// );
/// # let _ = account;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// The internal representation is private; callers cannot bypass validation:
///
/// ```compile_fail
/// use bb_api::requests::Reviewer;
/// let reviewer = Reviewer::Uuid { uuid: "not-a-uuid".into() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(description = "An explicit reviewer identified by UUID or Atlassian account ID.")]
pub struct Reviewer(ReviewerId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(inline)]
enum ReviewerId {
    /// A canonical Bitbucket UUID, including braces.
    Uuid {
        /// UUID text.
        uuid: String,
    },
    /// An Atlassian account ID.
    Account {
        /// Account ID text.
        account_id: String,
    },
}

impl Reviewer {
    /// Creates a reviewer from an opaque Atlassian account identifier.
    ///
    /// # Errors
    /// Rejects empty identifiers, whitespace, and control characters.
    pub fn from_account_id(account_id: impl Into<String>) -> Result<Self, Error> {
        let account_id = account_id.into();
        if account_id.is_empty()
            || account_id
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::invalid(
                "reviewer must be a UUID or a nonempty account ID",
            ));
        }
        Ok(Self(ReviewerId::Account { account_id }))
    }
}

impl From<uuid::Uuid> for Reviewer {
    fn from(uuid: uuid::Uuid) -> Self {
        Self(ReviewerId::Uuid {
            uuid: format!("{{{uuid}}}"),
        })
    }
}

impl std::str::FromStr for Reviewer {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        if let Ok(uuid) = uuid::Uuid::parse_str(value) {
            Ok(Self::from(uuid))
        } else {
            Self::from_account_id(value)
        }
    }
}

/// A validated pull request creation payload.
///
/// The target repository is supplied to [`crate::Client::create_pull_request`].
/// Optional source repositories support fork pull requests.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CreatePullRequest {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    source: PullRequestSide,
    destination: PullRequestSide,
    draft: bool,
    close_source_branch: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reviewers: Vec<Reviewer>,
}

impl CreatePullRequest {
    /// Creates a request with a title and validated source/destination branch names.
    ///
    /// # Errors
    /// Rejects an empty title. Fork relationship and permissions are checked by Bitbucket.
    pub fn new(
        title: impl Into<String>,
        head: BranchName,
        base: BranchName,
    ) -> Result<Self, Error> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(Error::invalid("pull request title must not be empty"));
        }
        Ok(Self {
            title,
            description: None,
            source: PullRequestSide {
                branch: BranchSelection { name: head },
                repository: None,
            },
            destination: PullRequestSide {
                branch: BranchSelection { name: base },
                repository: None,
            },
            draft: false,
            close_source_branch: false,
            reviewers: Vec::new(),
        })
    }
    /// Sets the Markdown body, including an explicitly empty body.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
    /// Selects a source fork.
    #[must_use]
    pub fn head_repository(mut self, repository: RepoRef) -> Self {
        self.source.repository = Some(RepositorySelection {
            full_name: repository,
        });
        self
    }
    /// Selects draft or ready state.
    #[must_use]
    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;
        self
    }
    /// Requests source branch deletion when the pull request is eventually merged.
    #[must_use]
    pub fn close_source_branch(mut self, close: bool) -> Self {
        self.close_source_branch = close;
        self
    }
    /// Sets explicit reviewers.
    #[must_use]
    pub fn reviewers(mut self, reviewers: Vec<Reviewer>) -> Self {
        self.reviewers = reviewers;
        self
    }
}
