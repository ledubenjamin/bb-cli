use crate::{
    args::Cli,
    output::{self, Kind},
};
use anyhow::{Result, bail};
use clap::{Command, CommandFactory};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct Spec {
    pub kind: Kind,
    pub list: bool,
    pub effect: &'static str,
    pub scopes: &'static [&'static str],
    pub api_scopes: &'static [&'static str],
}

pub fn spec(path: &[String]) -> Spec {
    let path = path.join(" ");
    let words: Vec<_> = path.split_whitespace().collect();
    let group = words.first().copied().unwrap_or("");
    let verb = words.last().copied().unwrap_or("");
    let write = matches!(
        verb,
        "create"
            | "edit"
            | "delete"
            | "close"
            | "reopen"
            | "resolve"
            | "ready"
            | "merge"
            | "review"
            | "comment"
            | "complete"
            | "grant"
            | "revoke"
            | "add"
            | "remove"
            | "set"
            | "set-default"
    ) || path == "workflow run";
    let local = matches!(group, "auth" | "config" | "context" | "browse" | "skills")
        || matches!(verb, "clone" | "checkout" | "rebase")
        || path == "repo set-default"
        || path.starts_with("repo favorite ");
    let local_write = write
        || matches!(
            verb,
            "login"
                | "logout"
                | "switch"
                | "refresh"
                | "setup-git"
                | "use"
                | "clone"
                | "checkout"
                | "rebase"
                | "install"
        )
        || group == "browse";
    let kind = output_kind(&path, group, verb);
    let (scopes, api_scopes) = required_scopes(&path, group, verb, write);
    Spec {
        kind,
        list: verb == "list"
            || matches!(verb, "checks")
            || path == "commit status"
            || path == "logs search",
        effect: if group == "tui" || path == "logs clear" || local && local_write {
            "local-write"
        } else if local {
            "read"
        } else if group == "api" {
            "dynamic"
        } else if write {
            "remote-write"
        } else {
            "read"
        },
        scopes,
        api_scopes,
    }
}

fn output_kind(path: &str, group: &str, verb: &str) -> Kind {
    match group {
        "skills" if verb == "show" => Kind::Text,
        "skills" if verb == "status" => Kind::Skill,
        "logs" if verb == "search" => Kind::LogMatch,
        "auth" if matches!(verb, "login" | "status" | "doctor" | "refresh") => Kind::Auth,
        "context" if verb == "list" => Kind::Context,
        "config" => Kind::Config,
        "repo" if path == "repo favorite list" => Kind::Favorite,
        "repo" if path.starts_with("repo favorite ") => Kind::Action,
        "repo" if path.contains("default-reviewers") => Kind::User,
        "repo" if matches!(verb, "delete" | "clone" | "set-default") => Kind::Action,
        "repo" => Kind::Repository,
        "pr" if (path.contains("comments") && verb == "list") || verb == "comment" => Kind::Comment,
        "pr" if path.contains("comments") => Kind::Action,
        "pr" if path.contains("task") => Kind::Task,
        "pr" if verb == "checks" => Kind::Check,
        "pr" if verb == "status" => Kind::Status,
        "pr" if verb == "checkout" => Kind::Action,
        "pr" if verb == "diff" => Kind::Text,
        "pr" => Kind::PullRequest,
        "branch" if matches!(verb, "delete" | "rebase" | "remove") => Kind::Action,
        "branch" if path.contains("protect") => Kind::Restriction,
        "branch" if verb == "set-default" => Kind::Repository,
        "branch" => Kind::Branch,
        "commit" if verb == "status" => Kind::Check,
        "commit" => Kind::Text,
        "project" | "variable" | "webhook" if verb == "delete" => Kind::Action,
        "project" => Kind::Project,
        "perms" if verb == "revoke" => Kind::Action,
        "perms" => Kind::Permission,
        "run" | "workflow" => Kind::Run,
        "variable" => Kind::Variable,
        "webhook" => Kind::Webhook,
        "status" if verb == "rate-limit" => Kind::RateLimit,
        "status" => Kind::Status,
        "skills" | "auth" | "context" | "browse" | "logs" => Kind::Action,
        _ => Kind::Raw,
    }
}

type Scopes = &'static [&'static str];

fn permission_scopes(project: bool, verb: &str) -> (Scopes, Scopes) {
    match (project, verb) {
        (true, "grant") => (
            &["project:admin"],
            &["admin:project:bitbucket", "write:permission:bitbucket"],
        ),
        (true, "revoke") => (
            &["project:admin"],
            &["admin:project:bitbucket", "delete:permission:bitbucket"],
        ),
        (true, _) => (&["project:admin"], &["read:project:bitbucket"]),
        (false, "grant") => (
            &["repository:admin"],
            &["admin:repository:bitbucket", "write:permission:bitbucket"],
        ),
        (false, "revoke") => (
            &["repository:admin"],
            &["admin:repository:bitbucket", "delete:permission:bitbucket"],
        ),
        (false, _) => (&["repository:admin"], &["read:repository:bitbucket"]),
    }
}

fn required_scopes(path: &str, group: &str, verb: &str, write: bool) -> (Scopes, Scopes) {
    if path.starts_with("repo favorite ") {
        return (&[], &[]);
    }
    match group {
        "tui" => (
            &["account", "project", "repository", "pipeline"],
            &[
                "read:user:bitbucket",
                "read:workspace:bitbucket",
                "read:project:bitbucket",
                "read:repository:bitbucket",
                "read:pipeline:bitbucket",
            ],
        ),
        "pr" if path.contains(" task") || path.contains(" comments") || verb == "comment" => {
            (&["pullrequest"], &["read:pullrequest:bitbucket"])
        }
        "pr" if verb == "checks" => (
            &["pullrequest", "repository"],
            &["read:pullrequest:bitbucket", "read:repository:bitbucket"],
        ),
        "pr" if verb == "status" => (
            &["pullrequest", "account"],
            &["read:pullrequest:bitbucket", "read:user:bitbucket"],
        ),
        "pr" if write => (
            &["pullrequest:write"],
            &["read:pullrequest:bitbucket", "write:pullrequest:bitbucket"],
        ),
        "pr" => (&["pullrequest"], &["read:pullrequest:bitbucket"]),
        "project" => {
            if write {
                (&["project:admin"], &["admin:project:bitbucket"])
            } else {
                (&["project"], &["read:project:bitbucket"])
            }
        }
        "perms" => permission_scopes(path.contains("project"), verb),
        "branch" if path.contains("protect") => {
            (&["repository:admin"], &["admin:repository:bitbucket"])
        }
        "branch" if verb == "set-default" => {
            (&["repository:admin"], &["admin:repository:bitbucket"])
        }
        "variable" => {
            if write {
                (
                    &["pipeline", "pipeline:variable"],
                    &["read:pipeline:bitbucket", "admin:pipeline:bitbucket"],
                )
            } else {
                (&["pipeline"], &["read:pipeline:bitbucket"])
            }
        }
        "run" | "workflow" => {
            if write {
                (
                    &["pipeline"],
                    &["read:pipeline:bitbucket", "write:pipeline:bitbucket"],
                )
            } else {
                (&["pipeline"], &["read:pipeline:bitbucket"])
            }
        }
        "webhook" => match verb {
            "create" => (
                &["webhook"],
                &["read:webhook:bitbucket", "write:webhook:bitbucket"],
            ),
            "delete" => (&["webhook"], &["delete:webhook:bitbucket"]),
            _ => (&["webhook"], &["read:webhook:bitbucket"]),
        },
        "auth" if matches!(verb, "login" | "status" | "doctor" | "refresh") => {
            (&["account"], &["read:user:bitbucket"])
        }
        "auth" | "context" | "config" | "schema" | "completion" | "api" | "browse" | "logs"
        | "skills" => (&[], &[]),
        "repo" if path.contains("default-reviewers") => {
            (&["pullrequest"], &["read:pullrequest:bitbucket"])
        }
        "repo" if matches!(verb, "create" | "edit") => {
            (&["repository:admin"], &["admin:repository:bitbucket"])
        }
        "repo" if verb == "delete" => (&["repository:delete"], &["delete:repository:bitbucket"]),
        "status" if verb != "rate-limit" => (
            &["pullrequest", "account"],
            &["read:pullrequest:bitbucket", "read:user:bitbucket"],
        ),
        _ => {
            if write {
                (&["repository:write"], &["write:repository:bitbucket"])
            } else {
                (&["repository"], &["read:repository:bitbucket"])
            }
        }
    }
}

pub fn command_path(matches: &clap::ArgMatches) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = matches;
    while let Some((name, child)) = current.subcommand() {
        path.push(name.to_owned());
        current = child;
    }
    path
}

pub fn describe(path: &[String]) -> Result<Value> {
    let mut root = Cli::command();
    root.build();
    let mut command = &root;
    for part in path {
        command = command
            .find_subcommand(part)
            .ok_or_else(|| anyhow::anyhow!("unknown command path '{}'", path.join(" ")))?;
    }
    if command.is_hide_set() {
        bail!("command is internal");
    }
    Ok(describe_command(command, path))
}

fn describe_command(command: &Command, path: &[String]) -> Value {
    let contract = spec(path);
    let mut schema = output::schema(contract.kind);
    if contract.list {
        schema = output::array_schema(schema);
    }
    let dry_run_schema = if path == ["tui"] {
        Value::Null
    } else if contract.effect == "read" {
        schema.clone()
    } else {
        output::schema(Kind::Action)
    };
    let arguments: Vec<_> = command.get_arguments().filter(|arg|!arg.is_hide_set()).map(|arg| {
        let values: Vec<_> = arg.get_value_parser().possible_values().into_iter().flatten().filter(|v|!v.is_hide_set()).map(|v|v.get_name().to_owned()).collect();
        json!({"name":arg.get_id().as_str(),"long":arg.get_long(),"short":arg.get_short(),
            "help":arg.get_help().map(ToString::to_string),"required":arg.is_required_set(),
            "global":arg.is_global_set(),"positional":arg.is_positional(),"values":values,
            "action":format!("{:?}",arg.get_action()),
            "valueCount":arg.get_num_args().map(|range|json!({"min":range.min_values(),"max":range.max_values()})),
            "conflicts":command.get_arg_conflicts_with(arg).iter().map(|other|other.get_id().as_str()).collect::<Vec<_>>(),
            "default":arg.get_default_values().iter().map(|v|v.to_string_lossy().into_owned()).collect::<Vec<_>>()})
    }).collect();
    let children: Vec<_> = command
        .get_subcommands()
        .filter(|cmd| !cmd.is_hide_set() && cmd.get_name() != "help")
        .map(|child| {
            let mut path = path.to_vec();
            path.push(child.get_name().into());
            describe_command(child, &path)
        })
        .collect();
    json!({"schemaVersion":1,"command":format!("bb {}",path.join(" ")).trim(),
        "description":command.get_about().map(ToString::to_string),"arguments":arguments,
        "effect":contract.effect,"interactive":path.join(" ") == "tui","oauthScopes":contract.scopes,"apiTokenScopes":contract.api_scopes,
        "jsonFields":output::fields(contract.kind),"outputSchema":schema,
        "dryRunOutputSchema":dry_run_schema,
        "apiRequestSchema":(path.join(" ") == "pr create").then(|| schemars::schema_for!(bb_api::requests::CreatePullRequest)),
        "outputVariants":output_variants(path),
        "scopeNotes":scope_notes(path),
        "exitCodes":{"0":"success","1":"error","2":"cancelled","4":"authentication required","8":"pending"},
        "commands":children})
}

fn output_variants(path: &[String]) -> Value {
    match path.join(" ").as_str() {
        "pr diff" | "commit diff" => {
            json!({"--stat":output::array_schema(output::schema(Kind::File))})
        }
        "run view" => json!({"--log":output::schema(Kind::Text)}),
        "variable set" => {
            json!({"--env-file":output::array_schema(output::schema(Kind::Variable))})
        }
        _ => json!({}),
    }
}

fn scope_notes(path: &[String]) -> &'static str {
    match path.join(" ").as_str() {
        "repo favorite add" | "repo favorite remove" | "repo favorite list" => {
            "Local preferences only: no HTTP or keyring access. Favorites are grouped by API origin and workspace and shared between profiles in the same configuration. Add/remove are idempotent and support --dry-run."
        }
        "tui" => {
            "Interactive terminal only; JSON, jq, dry-run, verbose, and non-interactive modes are unsupported. --offline reads cached resources without credentials or HTTP. Workspace listing uses /user/workspaces; an exact slug can be entered when listing is unavailable."
        }
        "logs search" | "logs clear" => {
            "Local cache only: no HTTP or keyring access. Search covers downloaded portions in the current profile/account or BB_TOKEN fingerprint. JSON fields identify the pipeline and step attempts."
        }
        "pr create" => {
            "Inferring --base additionally requires repository / read:repository:bitbucket. Supply --base to avoid that read."
        }
        "pr view" => {
            "Selecting checks additionally requires repository / read:repository:bitbucket on the source repository."
        }
        "pr checks" | "pr checkout" => {
            "Fork PRs require repository access on their source repository. Checkout additionally requires Git read access."
        }
        "pr list" => "--mine additionally requires account / read:user:bitbucket.",
        "pr close" => {
            "--delete-branch additionally requires repository:write and read/write:repository:bitbucket on the source repository."
        }
        "auth login" | "auth status" | "auth doctor" | "auth refresh" => {
            "Access tokens validate the selected repository instead of /user and require repository / read:repository:bitbucket."
        }
        "api" => {
            "Scopes and effects depend on the selected endpoint and HTTP method. Payloads and outputs use native Bitbucket JSON."
        }
        "variable list" | "variable get" | "variable set" | "variable delete" => {
            "Deployment names require resolving environments; UUIDs avoid that lookup. Scope permissions also depend on workspace/repository membership."
        }
        _ => "Scopes do not override repository membership or administrator permissions.",
    }
}
