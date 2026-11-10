use bb_api::{BranchName, ProjectKey, RepoRef, Workspace};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Bitbucket Cloud for developers and coding agents.
#[derive(Debug, Parser)]
#[command(name = "bb", version, about, arg_required_else_help = true)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "These independent switches mirror the public CLI flags"
)]
pub struct Cli {
    /// Select a repository (workspace/repository or Bitbucket URL).
    #[arg(short = 'R', long, global = true, env = "BB_REPO")]
    pub repo: Option<RepoRef>,
    /// Select a workspace for workspace-level commands.
    #[arg(long, global = true, env = "BB_WORKSPACE")]
    pub workspace: Option<Workspace>,
    /// Select a named context.
    #[arg(short = 'c', long, global = true, env = "BB_CONTEXT")]
    pub context: Option<String>,
    /// Select JSON fields; omit the value to list fields, or use '*' for all fields.
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "")]
    pub json: Option<String>,
    /// Filter selected JSON with jq syntax.
    #[arg(long, global = true)]
    pub jq: Option<String>,
    /// Disable interactive questions.
    #[arg(long, global = true, env = "BB_PROMPT_DISABLED", value_parser = clap::builder::BoolishValueParser::new())]
    pub non_interactive: bool,
    /// Confirm destructive operations.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    /// Show a mutation plan without changing remote or local state.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// HTTP request timeout in seconds.
    #[arg(long, global = true, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=86_400))]
    pub http_timeout: u64,
    /// Print operation diagnostics to stderr (never credentials or response bodies).
    #[arg(long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the bundled official skill for coding agents, without network access.
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
    /// Browse projects, pipelines, child pipelines, and logs in the terminal.
    Tui(TuiArgs),
    /// Search locally cached pipeline logs.
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    /// Authenticate, inspect credentials, and configure Git.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Manage named account/workspace/repository contexts.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Read and update non-secret preferences.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Work with repositories.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Work with pull requests.
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Work with branches and restrictions.
    Branch {
        #[command(subcommand)]
        command: BranchCommand,
    },
    /// Compare commits and inspect their checks.
    Commit {
        #[command(subcommand)]
        command: CommitCommand,
    },
    /// Manage workspace projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Manage explicit user and group permissions.
    Perms {
        #[command(subcommand)]
        command: PermsTarget,
    },
    /// Inspect pipeline executions.
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    /// Trigger Bitbucket Pipelines.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Manage Pipelines variables and secrets.
    Variable {
        #[command(subcommand)]
        command: VariableCommand,
    },
    /// Manage repository webhooks.
    Webhook {
        #[command(subcommand)]
        command: WebhookCommand,
    },
    /// Show repository activity or inspect rate-limit response headers.
    Status {
        #[command(subcommand)]
        command: Option<StatusCommand>,
    },
    /// Open a repository, file, or pull request in a browser.
    Browse {
        /// Repository-relative path.
        path: Option<String>,
    },
    /// Make a REST request (raw Bitbucket JSON, without normalized CLI fields).
    Api(ApiArgs),
    /// Describe commands and output schemas as JSON, without network access.
    Schema {
        /// Command path, for example: pr create.
        command: Vec<String>,
    },
    /// Generate shell completions on stdout.
    Completion { shell: clap_complete::Shell },
}

#[derive(Debug, Clone, Copy, ValueEnum, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SkillAgent {
    Codex,
    Claude,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, serde::Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SkillScope {
    #[default]
    Project,
    User,
}

#[derive(Debug, Clone, Args)]
pub struct SkillTarget {
    /// Agent whose local skills directory receives the skill.
    #[arg(long, value_enum)]
    pub agent: SkillAgent,
    /// Install in the current project directory or the user's home directory.
    #[arg(long, value_enum, default_value_t)]
    pub scope: SkillScope,
    /// Project directory instead of the current directory; only with --scope project.
    #[arg(long)]
    pub directory: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SkillsCommand {
    /// Print the bundled SKILL.md instructions.
    Show,
    /// Install or update the official skill; preserve locally modified files.
    Install(SkillTarget),
    /// Inspect the installed version and detect local modifications.
    Status(SkillTarget),
    /// Remove an unmodified official skill, preserving other files.
    Remove(SkillTarget),
}

#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    /// Maximum total items across pages.
    #[arg(short = 'L', long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: u32,
}

#[derive(Debug, Clone, Args)]
pub struct SearchListArgs {
    #[command(flatten)]
    pub list: ListArgs,
    /// Match names or identifiers, ignoring case.
    #[arg(long)]
    pub search: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct RepoListArgs {
    #[command(flatten)]
    pub list: ListArgs,
    /// Match repository names, identifiers, or project names, ignoring case.
    #[arg(long)]
    pub search: Option<String>,
    /// Limit repositories to this project key.
    #[arg(long)]
    pub project: Option<ProjectKey>,
}

#[derive(Debug, Subcommand)]
pub enum FavoriteCommand {
    /// Save a local favorite without contacting Bitbucket.
    Add { repository: RepoRef },
    /// Remove a local favorite without contacting Bitbucket.
    Remove { repository: RepoRef },
    /// List local favorites in the selected workspace.
    List,
}

#[derive(Debug, Clone, Args)]
pub struct BodyArgs {
    /// Markdown body or variable value.
    #[arg(short = 'b', long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read the body from a UTF-8 file; '-' reads stdin.
    #[arg(short = 'F', long)]
    pub body_file: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenType {
    #[default]
    Api,
    Access,
    Oauth,
}

#[derive(Debug, Args)]
pub struct TuiArgs {
    /// Browse only cached resources, without network requests or credential access.
    #[arg(long)]
    pub offline: bool,
}

#[derive(Debug, Subcommand)]
pub enum LogsCommand {
    /// Search cached log text (literal, case-insensitive, 3 to 1024 characters).
    Search {
        query: String,
        /// Maximum number of matches.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
    },
    /// Clear logs and their search index from the selected workspace cache.
    Clear,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Save a token or authorize through a browser.
    Login(AuthLoginArgs),
    /// Inspect the selected credentials without revealing them.
    Status,
    /// Diagnose credentials, configured consumer, and Git availability.
    Doctor,
    /// Remove locally stored credentials (does not revoke the remote token).
    Logout {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Select a stored profile.
    Switch { profile: String },
    /// Renew stored OAuth credentials.
    Refresh,
    /// Install bb's credential helper in user Git configuration for Bitbucket only.
    SetupGit,
    #[command(hide = true)]
    GitCredential { operation: String },
}

#[derive(Debug, Args)]
pub struct AuthLoginArgs {
    #[arg(long, conflicts_with = "with_token")]
    pub web: bool,
    /// Read a token from stdin.
    #[arg(long)]
    pub with_token: bool,
    #[arg(long, value_enum, default_value = "api")]
    pub token_type: TokenType,
    /// Profile name; defaults to the authenticated account ID or 'default'.
    #[arg(long)]
    pub profile: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ContextCommand {
    /// Create a context using the selected workspace and repository.
    Create {
        name: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Select a context.
    Use { name: String },
    /// List stored contexts.
    List,
    /// Delete a stored context.
    Delete { name: String },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Read one preference.
    Get { key: String },
    /// Update `default_workspace`, `git_protocol`, or an OAuth preference.
    Set { key: String, value: String },
    /// List preferences (never secrets).
    List,
}

#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// List repositories in the selected workspace.
    List(RepoListArgs),
    /// Manage persistent local repository favorites.
    Favorite {
        #[command(subcommand)]
        command: FavoriteCommand,
    },
    /// Inspect a repository.
    View {
        repository: Option<RepoRef>,
        #[arg(short = 'w', long)]
        web: bool,
    },
    /// Create a private repository unless --public is supplied.
    Create(RepoCreateArgs),
    /// Edit repository metadata.
    Edit(RepoEditArgs),
    /// Delete the selected repository.
    Delete,
    /// Clone a repository using Git.
    Clone {
        repository: RepoRef,
        directory: Option<std::path::PathBuf>,
        #[arg(long)]
        ssh: bool,
    },
    /// Save the default repository in the current Git repository.
    SetDefault { repository: RepoRef },
    /// List effective default reviewers.
    DefaultReviewers {
        #[command(subcommand)]
        command: ReviewersCommand,
    },
}

#[derive(Debug, Args)]
pub struct RepoCreateArgs {
    pub repository: RepoRef,
    #[arg(long)]
    pub public: bool,
    #[arg(long)]
    pub description: Option<String>,
    #[arg(long)]
    pub project: Option<ProjectKey>,
}

#[derive(Debug, Args)]
pub struct RepoEditArgs {
    #[arg(long)]
    pub description: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, value_enum)]
    pub visibility: Option<Visibility>,
    #[arg(long)]
    pub default_branch: Option<BranchName>,
    #[arg(long)]
    pub project: Option<ProjectKey>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Visibility {
    Private,
    Public,
}
#[derive(Debug, Subcommand)]
pub enum ReviewersCommand {
    List(ListArgs),
}

#[derive(Debug, Clone, Args)]
pub struct PrTarget {
    /// PR number, branch, or URL; defaults to the current branch's open PR.
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

#[derive(Debug, Subcommand)]
pub enum PrCommand {
    /// List pull requests in a repository.
    List(PrListArgs),
    /// Inspect a pull request; select comments/files/tasks/checks using --json.
    View(PrViewArgs),
    /// Create a pull request from a branch, including fork branches.
    Create(PrCreateArgs),
    /// Edit pull request metadata.
    Edit(PrEditArgs),
    /// Fetch and check out a pull request, including forks.
    Checkout(PrCheckoutArgs),
    /// Stream a pull request diff or show file statistics.
    Diff(PrDiffArgs),
    /// Decline a pull request.
    #[command(alias = "decline")]
    Close(PrCloseArgs),
    /// Reopen a declined pull request.
    Reopen {
        #[command(flatten)]
        pr: PrTarget,
    },
    /// Mark a draft ready for review, or revert to a draft.
    #[command(alias = "publish")]
    Ready {
        #[command(flatten)]
        pr: PrTarget,
        #[arg(long)]
        undo: bool,
    },
    /// Merge a pull request and wait for asynchronous merge completion.
    Merge(PrMergeArgs),
    /// Approve a pull request or submit a review comment.
    Review(PrReviewArgs),
    /// Add a general, inline, or threaded comment.
    Comment(PrCommentArgs),
    /// List and manage comment threads.
    Comments {
        #[command(subcommand)]
        command: CommentsCommand,
    },
    /// Manage review tasks.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Inspect build checks; pending checks exit with code 8.
    Checks {
        #[command(flatten)]
        pr: PrTarget,
        #[command(flatten)]
        watch: WatchArgs,
    },
    /// Show your open pull requests and review requests in this repository.
    Status,
}

#[derive(Debug, Args)]
pub struct PrListArgs {
    #[command(flatten)]
    pub list: ListArgs,
    #[arg(long, value_enum, default_value = "open")]
    pub state: PrState,
    #[arg(long)]
    pub author: Option<String>,
    #[arg(long)]
    pub mine: bool,
    #[arg(short = 'B', long)]
    pub base: Option<BranchName>,
    #[arg(short = 'H', long)]
    pub head: Option<BranchName>,
}

#[derive(Debug, Args)]
pub struct PrViewArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[arg(short = 'w', long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct PrCreateArgs {
    #[arg(short = 't', long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    #[arg(short = 'B', long)]
    pub base: Option<BranchName>,
    #[arg(short = 'H', long)]
    pub head: Option<BranchName>,
    #[arg(long)]
    pub head_repo: Option<RepoRef>,
    #[arg(short = 'r', long)]
    pub reviewer: Vec<String>,
    #[arg(short = 'd', long)]
    pub draft: bool,
    #[arg(long)]
    pub close_source_branch: bool,
}

#[derive(Debug, Args)]
pub struct PrEditArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[arg(short = 't', long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    #[arg(short = 'B', long)]
    pub base: Option<BranchName>,
    #[arg(long)]
    pub add_reviewer: Vec<String>,
    #[arg(long)]
    pub remove_reviewer: Vec<String>,
}

#[derive(Debug, Args)]
pub struct PrCheckoutArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[arg(short = 'b', long)]
    pub branch: Option<BranchName>,
}

#[derive(Debug, Args)]
pub struct PrDiffArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[arg(long)]
    pub stat: bool,
}

#[derive(Debug, Args)]
pub struct PrCloseArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[command(flatten)]
    pub body: BodyArgs,
    #[arg(long)]
    pub delete_branch: bool,
}

#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Clap mirrors mutually exclusive strategy switches and a separate deletion flag"
)]
pub struct PrMergeArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[arg(long, conflicts_with_all = ["squash", "ff_only"])]
    pub merge: bool,
    #[arg(long, conflicts_with = "ff_only")]
    pub squash: bool,
    #[arg(long)]
    pub ff_only: bool,
    #[arg(long)]
    pub delete_branch: bool,
    #[arg(long)]
    pub message: Option<String>,
    /// Maximum merge wait in seconds.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(..=31_536_000))]
    pub timeout: u64,
}

#[derive(Debug, Args)]
pub struct PrReviewArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[arg(long, conflicts_with = "comment")]
    pub approve: bool,
    #[arg(long)]
    pub comment: bool,
    #[command(flatten)]
    pub body: BodyArgs,
}

#[derive(Debug, Args)]
pub struct PrCommentArgs {
    #[command(flatten)]
    pub pr: PrTarget,
    #[command(flatten)]
    pub body: BodyArgs,
    #[arg(long, requires = "line")]
    pub path: Option<String>,
    #[arg(long, requires = "path", value_parser = clap::value_parser!(u64).range(1..))]
    pub line: Option<u64>,
    #[arg(long, requires = "line", value_enum, default_value = "new")]
    pub side: DiffSide,
    #[arg(long)]
    pub parent: Option<bb_api::CommentId>,
    #[arg(long)]
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DiffSide {
    Old,
    New,
}
#[derive(Debug, Subcommand)]
pub enum CommentsCommand {
    /// List comments with optional resolution filtering.
    List {
        #[command(flatten)]
        pr: PrTarget,
        #[command(flatten)]
        list: ListArgs,
        #[arg(long, value_enum, default_value = "all")]
        state: CommentState,
    },
    /// Delete a comment.
    Delete {
        pr: bb_api::PullRequestId,
        comment: bb_api::CommentId,
    },
    /// Resolve a top-level comment thread.
    Resolve {
        pr: bb_api::PullRequestId,
        comment: bb_api::CommentId,
    },
    /// Reopen a resolved top-level comment thread.
    Reopen {
        pr: bb_api::PullRequestId,
        comment: bb_api::CommentId,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CommentState {
    All,
    Resolved,
    Unresolved,
    Deleted,
}
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List review tasks.
    List {
        #[command(flatten)]
        pr: PrTarget,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Create a review task.
    Create {
        #[command(flatten)]
        pr: PrTarget,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Mark a task resolved.
    Complete {
        pr: bb_api::PullRequestId,
        task: bb_api::TaskId,
    },
    /// Mark a task unresolved.
    Reopen {
        pr: bb_api::PullRequestId,
        task: bb_api::TaskId,
    },
}

#[derive(Debug, Clone, Args)]
pub struct WatchArgs {
    /// Wait for completion.
    #[arg(long, alias = "wait")]
    pub watch: bool,
    /// Polling interval in seconds.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=86_400))]
    pub interval: u64,
    /// Maximum wait in seconds; 0 waits until interrupted.
    #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(..=31_536_000))]
    pub timeout: u64,
    /// Stop as soon as one check fails.
    #[arg(long)]
    pub fail_fast: bool,
}

#[derive(Debug, Subcommand)]
pub enum BranchCommand {
    /// List branches, optionally matching a name fragment.
    List {
        #[command(flatten)]
        list: ListArgs,
        #[arg(long)]
        filter: Option<String>,
    },
    /// Create a remote branch from a branch or commit.
    Create {
        name: BranchName,
        #[arg(long = "from")]
        source: String,
    },
    /// Delete a remote branch.
    Delete { name: BranchName },
    /// Change the repository's default branch.
    SetDefault { name: BranchName },
    /// Rebase the current local branch onto a remote branch; never pushes.
    Rebase {
        base: BranchName,
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// Manage native Cloud branch restrictions.
    Protect {
        #[command(subcommand)]
        command: ProtectCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum ProtectCommand {
    List(ListArgs),
    View {
        id: u64,
    },
    Add(RestrictionArgs),
    Edit {
        id: u64,
        #[command(flatten)]
        rule: RestrictionArgs,
    },
    Remove {
        id: u64,
    },
}
#[derive(Debug, Args)]
pub struct RestrictionArgs {
    #[arg(long)]
    pub pattern: String,
    #[arg(long, value_enum)]
    pub kind: RestrictionKind,
    #[arg(long)]
    pub value: Option<u64>,
    #[arg(long)]
    pub user: Vec<String>,
    #[arg(long)]
    pub group: Vec<String>,
}
#[derive(Debug, Clone, Copy, ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestrictionKind {
    Push,
    Force,
    Delete,
    RestrictMerges,
    RequireTasksToBeCompleted,
    RequireApprovalsToMerge,
    RequirePassingBuildsToMerge,
    RequireDefaultReviewerApprovalsToMerge,
    RequireNoChangesRequested,
    RequireAllDependenciesMerged,
    EnforceMergeChecks,
    ResetPullrequestApprovalsOnChange,
    SmartResetPullrequestApprovals,
    ResetPullrequestChangesRequestedOnChange,
}

#[derive(Debug, Subcommand)]
pub enum CommitCommand {
    /// Compare two refs with the Cloud diff API.
    Diff {
        base: String,
        head: String,
        #[arg(long)]
        stat: bool,
    },
    /// Show checks for a commit.
    Status {
        sha: String,
        #[command(flatten)]
        watch: WatchArgs,
    },
}
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    List(SearchListArgs),
    View {
        key: ProjectKey,
    },
    Create {
        key: ProjectKey,
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        public: bool,
    },
    Edit {
        key: ProjectKey,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_enum)]
        visibility: Option<Visibility>,
    },
    Delete {
        key: ProjectKey,
    },
}
#[derive(Debug, Subcommand)]
pub enum PermsTarget {
    Repo {
        #[command(subcommand)]
        command: PermsCommand,
    },
    Project {
        key: ProjectKey,
        #[command(subcommand)]
        command: PermsCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum PermsCommand {
    /// List explicit assignments, without flattening inherited permissions.
    List {
        #[command(flatten)]
        list: ListArgs,
        #[arg(long, value_enum, default_value = "users")]
        kind: PrincipalKind,
    },
    Grant {
        #[command(flatten)]
        principal: PrincipalArgs,
        #[arg(long, value_enum)]
        permission: PermissionLevel,
    },
    Revoke {
        #[command(flatten)]
        principal: PrincipalArgs,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PrincipalKind {
    Users,
    Groups,
}
#[derive(Debug, Clone, Copy, ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionLevel {
    Read,
    Write,
    Admin,
    CreateRepo,
}
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct PrincipalArgs {
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum RunCommand {
    List(ListArgs),
    View {
        id: String,
        #[arg(long)]
        log: bool,
        #[arg(long, requires = "log")]
        step: Option<String>,
        #[arg(short = 'w', long, conflicts_with = "log")]
        web: bool,
    },
    Watch {
        id: String,
        #[command(flatten)]
        watch: WatchArgs,
    },
}
#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    /// Run a default or custom pipeline on a branch/tag.
    Run {
        /// Custom pipeline name; omit for the default pipeline.
        name: Option<String>,
        #[arg(short = 'r', long = "ref")]
        reference: Option<String>,
        #[arg(long)]
        tag: bool,
        #[arg(short = 'f', long = "field")]
        variables: Vec<String>,
        #[command(flatten)]
        watch: WatchArgs,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum VariableScope {
    Repository,
    Workspace,
    Deployment,
}
#[derive(Debug, Clone, Args)]
pub struct VariableArgs {
    #[arg(long, value_enum, default_value = "repository")]
    pub scope: VariableScope,
    /// Deployment environment name or UUID; implies deployment scope.
    #[arg(short = 'e', long)]
    pub deployment: Option<String>,
}
#[derive(Debug, Subcommand)]
pub enum VariableCommand {
    List {
        #[command(flatten)]
        target: VariableArgs,
        #[command(flatten)]
        list: ListArgs,
    },
    Get {
        name: String,
        #[command(flatten)]
        target: VariableArgs,
    },
    Set {
        name: Option<String>,
        #[command(flatten)]
        target: VariableArgs,
        #[command(flatten)]
        body: BodyArgs,
        /// Mark new/updated variables as secured.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        secured: Option<bool>,
        #[arg(short = 'f', long, conflicts_with_all = ["name", "body", "body_file"])]
        env_file: Option<std::path::PathBuf>,
    },
    Delete {
        name: String,
        #[command(flatten)]
        target: VariableArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum WebhookCommand {
    List(ListArgs),
    Create {
        #[arg(long)]
        url: url::Url,
        #[arg(long)]
        description: Option<String>,
        #[arg(long = "event", required = true)]
        events: Vec<String>,
        #[arg(long)]
        disabled: bool,
    },
    Delete {
        id: uuid::Uuid,
    },
}
#[derive(Debug, Subcommand)]
pub enum StatusCommand {
    RateLimit,
}

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Relative REST path, optionally containing {workspace} and {repo} placeholders.
    pub path: String,
    #[arg(short = 'X', long)]
    pub method: Option<bb_api::Method>,
    /// Read a JSON request body from a file or '-' for stdin.
    #[arg(long, conflicts_with_all = ["fields", "raw_fields"])]
    pub input: Option<String>,
    /// Add a typed body field (booleans, numbers, null, JSON arrays/objects, or strings).
    #[arg(short = 'F', long = "field")]
    pub fields: Vec<String>,
    /// Add a string body field.
    #[arg(short = 'f', long = "raw-field")]
    pub raw_fields: Vec<String>,
    #[arg(short = 'H', long = "header")]
    pub headers: Vec<String>,
    #[arg(short = 'P', long = "param")]
    pub params: Vec<String>,
    #[arg(long)]
    pub paginate: bool,
    #[arg(long, requires = "paginate")]
    pub slurp: bool,
    #[arg(short = 'i', long)]
    pub include: bool,
}
