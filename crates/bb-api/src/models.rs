//! Bitbucket REST response representations.
//!
//! Unknown response fields are accepted for compatibility with API additions.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub(crate) struct Page<T> {
    pub values: Vec<T>,
    pub next: Option<String>,
}

/// An API hyperlink.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Link {
    /// Absolute link URL.
    pub href: String,
    /// Optional link relation name.
    #[serde(default)]
    pub name: Option<String>,
}

/// A Bitbucket account or access-token actor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct User {
    /// Stable UUID.
    #[serde(default)]
    pub uuid: String,
    /// Atlassian account identifier, when visible.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Display name.
    #[serde(default)]
    pub display_name: String,
    /// Public nickname, when visible.
    #[serde(default)]
    pub nickname: Option<String>,
    /// Resource hyperlinks.
    #[serde(default)]
    pub links: BTreeMap<String, Link>,
}

/// A repository with its public metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Repository {
    /// Stable UUID.
    #[serde(default)]
    pub uuid: String,
    /// Repository slug.
    #[serde(default)]
    pub slug: String,
    /// Repository display name.
    #[serde(default)]
    pub name: String,
    /// Workspace and slug.
    #[serde(default)]
    pub full_name: String,
    /// Repository description.
    #[serde(default)]
    pub description: String,
    /// Whether access is private.
    #[serde(default)]
    pub is_private: bool,
    /// Default branch, absent for an empty repository.
    #[serde(default)]
    pub mainbranch: Option<NamedRef>,
    /// Optional owning project.
    #[serde(default)]
    pub project: Option<Project>,
    /// Resource links including clone URLs.
    #[serde(default)]
    pub links: RepositoryLinks,
}

/// Links associated with a repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryLinks {
    /// Web URL.
    pub html: Option<Link>,
    /// HTTPS and SSH clone URLs.
    #[serde(default)]
    pub clone: Vec<Link>,
}

/// A branch, tag, or named pipeline selector.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct NamedRef {
    /// Reference name.
    pub name: String,
}

/// A commit reference.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Commit {
    /// Commit hash.
    #[serde(default)]
    pub hash: String,
    /// Commit message when supplied.
    #[serde(default)]
    pub message: String,
}

/// A branch and its current commit.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Branch {
    /// Branch name.
    pub name: String,
    /// Tip commit.
    pub target: Commit,
    /// Allowed merge strategies.
    #[serde(default)]
    pub merge_strategies: Vec<String>,
}

/// One side of a pull request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestRef {
    /// Branch name.
    pub branch: NamedRef,
    /// Commit at this side.
    #[serde(default)]
    pub commit: Commit,
    /// Repository, possibly absent after fork deletion.
    pub repository: Option<Repository>,
}

/// A pull request's native lifecycle state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum PullRequestState {
    /// Open for review.
    Open,
    /// Successfully merged.
    Merged,
    /// Declined without merging.
    Declined,
    /// Superseded by another pull request.
    Superseded,
    /// A future state not yet recognized by this client.
    #[serde(other)]
    Unknown,
}

/// A pull request participant and review state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Participant {
    /// Participating actor.
    pub user: User,
    /// Whether this actor approved the pull request.
    #[serde(default)]
    pub approved: bool,
    /// Reviewer or participant role.
    #[serde(default)]
    pub role: String,
    /// Native review state.
    #[serde(default)]
    pub state: Option<String>,
}

/// A pull request returned by Bitbucket Cloud.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PullRequest {
    /// Repository-local number.
    pub id: u64,
    /// Title.
    pub title: String,
    /// Markdown description.
    #[serde(default)]
    pub description: String,
    /// Lifecycle state.
    pub state: PullRequestState,
    /// Whether this is a draft.
    #[serde(default)]
    pub draft: bool,
    /// Author.
    #[serde(default)]
    pub author: User,
    /// Source branch and repository.
    pub source: PullRequestRef,
    /// Destination branch and repository.
    pub destination: PullRequestRef,
    /// Assigned reviewers.
    #[serde(default)]
    pub reviewers: Vec<User>,
    /// Review participation.
    #[serde(default)]
    pub participants: Vec<Participant>,
    /// Resource links.
    #[serde(default)]
    pub links: BTreeMap<String, Link>,
    /// Creation timestamp.
    #[serde(default)]
    pub created_on: String,
    /// Last modification timestamp.
    #[serde(default)]
    pub updated_on: String,
}

/// Markdown text in Bitbucket's content representation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Content {
    /// Raw Markdown.
    #[serde(default)]
    pub raw: String,
}

/// An inline comment location in a diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Inline {
    /// File path relative to the repository.
    pub path: String,
    /// Line in the old file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<u64>,
    /// Line in the new file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<u64>,
}

/// A reference to an existing comment.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommentRef {
    /// Parent comment number.
    pub id: u64,
}

/// A comment on a pull request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    /// Comment identifier.
    pub id: u64,
    /// Comment author.
    #[serde(default)]
    pub user: User,
    /// Comment text.
    #[serde(default)]
    pub content: Content,
    /// Optional diff location.
    pub inline: Option<Inline>,
    /// Optional parent comment.
    pub parent: Option<CommentRef>,
    /// Whether the comment was deleted.
    #[serde(default)]
    pub deleted: bool,
    /// Resolution metadata, whose fields vary by API version.
    #[serde(default)]
    pub resolution: Option<Resolution>,
    /// Timestamp.
    #[serde(default)]
    pub created_on: String,
    /// Whether this is pending review feedback.
    #[serde(default)]
    pub pending: bool,
}

/// A thread resolution marker.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    /// Actor who resolved the thread, when available.
    #[serde(default)]
    pub user: Option<User>,
    /// Resolution timestamp, when available.
    #[serde(default)]
    pub created_on: Option<String>,
}

/// A pull request review task.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    /// Task identifier.
    pub id: u64,
    /// Task text.
    #[serde(default)]
    pub content: Content,
    /// Native state: UNRESOLVED or RESOLVED.
    #[serde(default)]
    pub state: String,
    /// Author when provided.
    #[serde(default)]
    pub creator: Option<User>,
}

/// A build status associated with a commit.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Check {
    /// Stable build-system key.
    pub key: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Native build state.
    pub state: String,
    /// Build details URL.
    #[serde(default)]
    pub url: String,
    /// Additional details.
    #[serde(default)]
    pub description: String,
}

/// A file on one side of a diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiffFile {
    /// Repository-relative path.
    pub path: String,
}

/// Per-file change statistics.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiffStat {
    /// Native change status.
    pub status: String,
    /// Added lines.
    #[serde(default)]
    pub lines_added: u64,
    /// Removed lines.
    #[serde(default)]
    pub lines_removed: u64,
    /// Old file, absent for additions.
    pub old: Option<DiffFile>,
    /// New file, absent for deletions.
    pub new: Option<DiffFile>,
}

/// A project grouping repositories in a workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    /// Project key.
    pub key: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Project UUID.
    #[serde(default)]
    pub uuid: String,
    /// Visibility.
    #[serde(default)]
    pub is_private: bool,
    /// Resource hyperlinks.
    #[serde(default)]
    pub links: BTreeMap<String, Link>,
}

/// A branch restriction rule.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Restriction {
    /// Rule identifier.
    pub id: u64,
    /// Native restriction kind.
    pub kind: String,
    /// Branch pattern.
    #[serde(default)]
    pub pattern: String,
    /// Numeric threshold for threshold-based rules.
    pub value: Option<u64>,
    /// Exempt users.
    #[serde(default)]
    pub users: Vec<User>,
    /// Exempt groups.
    #[serde(default)]
    pub groups: Vec<Group>,
}

/// A workspace group.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Group {
    /// Group slug.
    #[serde(default)]
    pub slug: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
}

/// An explicit permission assignment.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Permission {
    /// Assigned permission.
    pub permission: String,
    /// User principal, when this is a user assignment.
    pub user: Option<User>,
    /// Group principal, when this is a group assignment.
    pub group: Option<Group>,
}

/// A webhook subscription.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Webhook {
    /// Webhook UUID.
    pub uuid: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Destination URL.
    pub url: String,
    /// Subscribed event identifiers.
    #[serde(default)]
    pub events: Vec<String>,
    /// Whether delivery is enabled.
    #[serde(default)]
    pub active: bool,
}

/// A Pipelines variable; secured values are never returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Variable {
    /// Variable UUID.
    pub uuid: String,
    /// Variable key.
    pub key: String,
    /// Plain value when readable.
    pub value: Option<String>,
    /// Whether the variable is secured.
    #[serde(default)]
    pub secured: bool,
}

/// A deployment environment.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Environment {
    /// Environment UUID.
    pub uuid: String,
    /// Human-readable name.
    pub name: String,
}

/// A pipeline state or result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RunState {
    /// Native state name.
    #[serde(default)]
    pub name: String,
    /// Terminal result, when completed.
    pub result: Option<RunResult>,
}
/// A terminal pipeline result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunResult {
    /// SUCCESSFUL, FAILED, ERROR, STOPPED, or a future result.
    pub name: String,
}

/// A pipeline target.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RunTarget {
    /// Branch or tag name.
    #[serde(default)]
    pub ref_name: String,
    /// Target commit.
    #[serde(default)]
    pub commit: Commit,
}

/// A pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    /// Run UUID.
    pub uuid: String,
    /// Repository-local build number.
    pub build_number: u64,
    /// Attempt number, independent of the repository-local build number.
    #[serde(default = "first_attempt")]
    pub run_number: u64,
    /// Trigger and explicit parent references for child pipelines.
    #[serde(default)]
    pub trigger: PipelineTrigger,
    /// Current execution state.
    pub state: RunState,
    /// Branch and commit.
    #[serde(default)]
    pub target: RunTarget,
    /// Start timestamp.
    #[serde(default)]
    pub created_on: String,
    /// End timestamp.
    pub completed_on: Option<String>,
    /// Resource hyperlinks.
    #[serde(default)]
    pub links: BTreeMap<String, Link>,
}

/// A step within a pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// Step UUID.
    pub uuid: String,
    /// Attempt number. A rerun can also replace the step UUID.
    #[serde(default = "first_attempt")]
    pub run_number: u64,
    /// Start timestamp, absent before execution.
    #[serde(default)]
    pub started_on: Option<String>,
    /// Completion timestamp, absent while executing.
    #[serde(default)]
    pub completed_on: Option<String>,
    /// Step name.
    #[serde(default)]
    pub name: String,
    /// Execution state.
    #[serde(default)]
    pub state: RunState,
}

fn first_attempt() -> u64 {
    1
}

/// One page of a collection, without eagerly loading subsequent pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct CollectionPage<T> {
    /// Values on this page.
    #[serde(default)]
    pub values: Vec<T>,
    /// Opaque URL of the next page.
    pub next: Option<String>,
}

/// A reference to a pipeline or step returned by Bitbucket.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineReference {
    /// UUID of the referenced resource.
    pub uuid: String,
}

/// The original parent execution of a child pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PipelineParent {
    /// Parent pipeline reference.
    pub pipeline_uuid: Option<PipelineReference>,
    /// Original triggering step; it can belong to an earlier attempt.
    pub step_uuid: Option<PipelineReference>,
    /// Parent attempt identifier when exposed by Bitbucket.
    pub pipeline_run_uuid: Option<String>,
}

/// A pipeline trigger; unknown trigger names remain readable.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PipelineTrigger {
    /// Native trigger name, such as `PUSH` or `PARENT_STEP`.
    #[serde(default)]
    pub name: String,
    /// Parent references for a `PARENT_STEP` trigger.
    #[serde(default)]
    pub metadata: PipelineParent,
}

/// A workspace accessible to the current account.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceSummary {
    /// Workspace UUID.
    pub uuid: String,
    /// Workspace identifier used in URLs.
    pub slug: String,
    /// Display name, which some list responses omit.
    #[serde(default)]
    pub name: String,
}

/// One entry from the current user's workspace list.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceAccess {
    /// Accessible workspace.
    pub workspace: WorkspaceSummary,
}
