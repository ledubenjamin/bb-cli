use std::{
    process::Output,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::Command;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

struct Fixture {
    directory: TempDir,
    server: MockServer,
}

impl Fixture {
    async fn new() -> Result<Self> {
        Ok(Self {
            directory: tempfile::tempdir()?,
            server: MockServer::start().await,
        })
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bb"));
        for key in [
            "BB_TOKEN",
            "BB_TOKEN_TYPE",
            "BB_REPO",
            "BB_WORKSPACE",
            "BB_CONTEXT",
            "BITBUCKET_REPO_FULL_NAME",
            "BB_OAUTH_CLIENT_ID",
            "BB_OAUTH_CLIENT_SECRET",
            "BB_OAUTH_REDIRECT_URI",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
        ] {
            command.env_remove(key);
        }
        command
            .current_dir(self.directory.path())
            .env("BB_CONFIG_DIR", self.directory.path().join("config"))
            .env("BB_API_URL", format!("{}/2.0/", self.server.uri()))
            .env("BB_TOKEN", "fixture-token")
            .env("BB_PROMPT_DISABLED", "true")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.directory.path().join("gitconfig"))
            .args(args)
            .kill_on_drop(true);
        command
    }
    async fn run(&self, args: &[&str]) -> Result<Output> {
        Ok(tokio::time::timeout(Duration::from_secs(15), self.command(args).output()).await??)
    }
    async fn no_requests(&self) -> Result<()> {
        assert!(
            self.server
                .received_requests()
                .await
                .context("request recording missing")?
                .is_empty()
        );
        Ok(())
    }
}

fn data(output: &Output, code: i32) -> Result<Value> {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "invalid stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn pull_request(state: &str) -> Value {
    json!({"id":7,"title":"Improve checks","description":"Review me","state":state,"draft":true,
        "author":{"uuid":"{actor}","nickname":"alice","display_name":"Alice"},
        "source":{"branch":{"name":"feature/checks"},"commit":{"hash":"abc123"},"repository":{"full_name":"acme/widgets"}},
        "destination":{"branch":{"name":"main"},"repository":{"full_name":"acme/widgets"}},
        "links":{"html":{"href":"https://bitbucket.org/acme/widgets/pull-requests/7"}}})
}

#[tokio::test]
async fn repository_favorites_are_local_idempotent_and_discoverable() -> Result<()> {
    let fixture = Fixture::new().await?;
    let run = |args: Vec<&'static str>| {
        let mut command = fixture.command(&args);
        command.env_remove("BB_TOKEN");
        async move { Ok::<_, anyhow::Error>(command.output().await?) }
    };
    let dry = run(vec![
        "repo",
        "favorite",
        "add",
        "acme/service",
        "--dry-run",
        "--json",
        "*",
    ])
    .await?;
    assert_eq!(data(&dry, 0)?["dryRun"], true);
    assert!(!fixture.directory.path().join("config/config.toml").exists());
    for repository in ["acme/service", "acme/service", "other/worker"] {
        let output = fixture
            .command(&["repo", "favorite", "add", repository, "--json", "*"])
            .env_remove("BB_TOKEN")
            .output()
            .await?;
        data(&output, 0)?;
    }
    let args = vec![
        "repo",
        "favorite",
        "list",
        "--workspace",
        "acme",
        "--json",
        "repository,workspace",
    ];
    assert_eq!(
        data(&run(args.clone()).await?, 0)?,
        json!([{"repository":"acme/service","workspace":"acme"}])
    );
    let discovery = data(&run(vec!["repo", "favorite", "list", "--json"]).await?, 0)?;
    assert!(
        discovery["fields"]
            .as_array()
            .context("missing fields")?
            .contains(&json!("repository"))
    );
    let schema = data(&run(vec!["schema", "repo", "favorite", "add"]).await?, 0)?;
    assert_eq!(schema["effect"], "local-write");
    assert_eq!(schema["apiTokenScopes"], json!([]));
    let jq = run(vec![
        "repo",
        "favorite",
        "list",
        "--workspace",
        "acme",
        "--json",
        "repository",
        "--jq",
        ".[].repository",
    ])
    .await?;
    assert_eq!(jq.stdout, b"acme/service\n");
    for _ in 0..2 {
        data(
            &run(vec![
                "repo",
                "favorite",
                "remove",
                "acme/service",
                "--json",
                "*",
            ])
            .await?,
            0,
        )?;
    }
    assert_eq!(data(&run(args).await?, 0)?, json!([]));
    fixture.no_requests().await
}

#[tokio::test]
async fn repository_search_filters_before_the_limit_and_follows_matching_pages() -> Result<()> {
    let fixture = Fixture::new().await?;
    let query = "project.key=\"APP\" AND (name ~ \"service\" OR slug ~ \"service\" OR full_name ~ \"service\" OR project.key ~ \"service\" OR project.name ~ \"service\")";
    Mock::given(method("GET")).and(path("/2.0/repositories/acme"))
        .and(query_param("q",query))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values":[{"full_name":"acme/service-one"}],"next":format!("{}/2.0/repositories/acme?page=2",fixture.server.uri())})))
        .expect(1).mount(&fixture.server).await;
    Mock::given(path("/2.0/repositories/acme"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"values":[{"full_name":"acme/service-two"},{"full_name":"acme/service-three"}]}),
        ))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "repo",
            "list",
            "--workspace",
            "acme",
            "--project",
            "APP",
            "--search",
            "service",
            "--limit",
            "2",
            "--json",
            "nameWithOwner",
        ])
        .await?;
    assert_eq!(
        data(&output, 0)?,
        json!([{"nameWithOwner":"acme/service-one"},{"nameWithOwner":"acme/service-two"}])
    );
    Ok(())
}

#[tokio::test]
async fn project_search_quotes_literal_input() -> Result<()> {
    let fixture = Fixture::new().await?;
    let literal = "a\\b\" OR key != \"x";
    let escaped = literal.replace('\\', "\\\\").replace('"', "\\\"");
    Mock::given(path("/2.0/workspaces/acme/projects"))
        .and(query_param(
            "q",
            format!("(name ~ \"{escaped}\" OR key ~ \"{escaped}\")"),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"values":[{"key":"APP","name":literal}]})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    assert_eq!(
        data(
            &fixture
                .run(&[
                    "project",
                    "list",
                    "--workspace",
                    "acme",
                    "--search",
                    literal,
                    "--json",
                    "key"
                ])
                .await?,
            0
        )?,
        json!([{"key":"APP"}])
    );
    Ok(())
}

#[tokio::test]
async fn favorite_read_dry_run_matches_its_discovered_schema() -> Result<()> {
    let fixture = Fixture::new().await?;
    data(
        &fixture
            .run(&["repo", "favorite", "add", "acme/service", "--json", "*"])
            .await?,
        0,
    )?;
    let schema = data(
        &fixture.run(&["schema", "repo", "favorite", "list"]).await?,
        0,
    )?;
    let value = data(
        &fixture
            .run(&[
                "repo",
                "favorite",
                "list",
                "--workspace",
                "acme",
                "--dry-run",
                "--json",
                "*",
            ])
            .await?,
        0,
    )?;
    assert!(jsonschema::is_valid(&schema["dryRunOutputSchema"], &value));
    assert_eq!(value[0]["repository"], "acme/service");
    fixture.no_requests().await
}

#[tokio::test]
async fn default_workspace_is_persistent_and_explicit_inputs_take_precedence() -> Result<()> {
    let fixture = Fixture::new().await?;
    data(
        &fixture
            .run(&["config", "set", "default_workspace", "saved", "--json", "*"])
            .await?,
        0,
    )?;
    for workspace in ["saved", "explicit", "repository", "contextual"] {
        Mock::given(path(format!("/2.0/workspaces/{workspace}/projects")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"values":[{"key":workspace}]})),
            )
            .expect(1)
            .mount(&fixture.server)
            .await;
    }
    assert_eq!(
        data(
            &fixture.run(&["project", "list", "--json", "key"]).await?,
            0
        )?,
        json!([{"key":"saved"}])
    );
    assert_eq!(
        data(
            &fixture
                .run(&[
                    "project",
                    "list",
                    "--workspace",
                    "explicit",
                    "--json",
                    "key"
                ])
                .await?,
            0
        )?,
        json!([{"key":"explicit"}])
    );
    assert_eq!(
        data(
            &fixture
                .run(&[
                    "project",
                    "list",
                    "-R",
                    "repository/service",
                    "--json",
                    "key"
                ])
                .await?,
            0
        )?,
        json!([{"key":"repository"}])
    );
    data(
        &fixture
            .run(&[
                "context",
                "create",
                "selected",
                "--workspace",
                "contextual",
                "--json",
                "*",
            ])
            .await?,
        0,
    )?;
    assert_eq!(
        data(
            &fixture
                .run(&["project", "list", "--context", "selected", "--json", "key"])
                .await?,
            0
        )?,
        json!([{"key":"contextual"}])
    );
    let invalid = fixture
        .run(&[
            "config",
            "set",
            "default_workspace",
            "../invalid",
            "--json",
            "*",
        ])
        .await?;
    assert_eq!(invalid.status.code(), Some(1));
    Ok(())
}

#[tokio::test]
async fn terminal_contract_and_local_log_commands_require_no_api() -> Result<()> {
    let fixture = Fixture::new().await?;
    let terminal = data(&fixture.run(&["schema", "tui"]).await?, 0)?;
    assert_eq!(terminal["interactive"], true);
    assert!(terminal["dryRunOutputSchema"].is_null());
    let search = data(&fixture.run(&["schema", "logs", "search"]).await?, 0)?;
    assert_eq!(search["apiTokenScopes"], json!([]));
    assert!(
        search["jsonFields"]
            .as_array()
            .context("missing fields")?
            .contains(&json!("parentRunAttemptUuid"))
    );
    data(
        &fixture
            .run(&[
                "logs",
                "clear",
                "--workspace",
                "acme",
                "--dry-run",
                "--json",
                "*",
            ])
            .await?,
        0,
    )?;
    for arguments in [
        vec!["tui", "--json"],
        vec!["tui", "--json", "*"],
        vec!["tui", "--offline", "--non-interactive"],
    ] {
        assert_eq!(fixture.run(&arguments).await?.status.code(), Some(1));
    }
    assert!(!fixture.directory.path().join("config").exists());
    assert_eq!(
        data(
            &fixture
                .run(&[
                    "logs",
                    "search",
                    "literal \" OR *",
                    "--workspace",
                    "acme",
                    "--json",
                    "*"
                ])
                .await?,
            0
        )?,
        json!([])
    );
    fixture.no_requests().await
}

#[tokio::test]
async fn discovery_works_without_arguments_credentials_config_or_network() -> Result<()> {
    let fixture = Fixture::new().await?;
    for args in [
        vec!["schema", "pr", "create"],
        vec!["project", "create", "--json"],
        vec!["repo", "delete", "--json"],
    ] {
        let output = fixture
            .command(&args)
            .env_remove("BB_TOKEN")
            .output()
            .await?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(output.stderr, [] as [u8; 0]);
    }
    assert!(!fixture.directory.path().join("config").exists());
    fixture.no_requests().await
}

#[tokio::test]
async fn every_command_has_offline_help_and_a_schema() -> Result<()> {
    let fixture = Fixture::new().await?;
    let schema = data(&fixture.run(&["schema"]).await?, 0)?;
    let mut commands = Vec::new();
    schema_paths(&schema, &mut commands);
    assert!(
        commands.len() >= 90,
        "only {} commands discovered",
        commands.len()
    );
    for command in commands {
        let mut args: Vec<_> = command.split_whitespace().skip(1).collect();
        args.push("--help");
        let output = fixture.run(&args).await?;
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fixture.no_requests().await
}

#[tokio::test]
async fn invalid_json_fields_and_jq_fail_before_mutations() -> Result<()> {
    let fixture = Fixture::new().await?;
    for args in [
        vec![
            "pr",
            "create",
            "-R",
            "acme/widgets",
            "--title",
            "x",
            "--base",
            "main",
            "--head",
            "feature",
            "--json",
            "invalid",
        ],
        vec![
            "pr",
            "create",
            "-R",
            "acme/widgets",
            "--title",
            "x",
            "--base",
            "main",
            "--head",
            "feature",
            "--json",
            "number",
            "--jq",
            "[",
        ],
    ] {
        let output = fixture.run(&args).await?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, [] as [u8; 0]);
        let error: Value = serde_json::from_slice(&output.stderr)?;
        assert_eq!(error["exitCode"], 1);
    }
    fixture.no_requests().await
}

#[tokio::test]
async fn dry_run_preserves_native_payload_and_sends_no_writes() -> Result<()> {
    let fixture = Fixture::new().await?;
    let output = fixture
        .command(&[
            "pr",
            "create",
            "-R",
            "acme/widgets",
            "--title",
            "New PR",
            "--head",
            "feature/one",
            "--base",
            "main",
            "--close-source-branch",
            "--draft",
            "--dry-run",
            "--json",
            "*",
        ])
        .env_remove("BB_TOKEN")
        .output()
        .await?;
    let value = data(&output, 0)?;
    assert_eq!(value["dryRun"], true);
    assert_eq!(value["actions"][0]["body"]["close_source_branch"], true);
    assert_eq!(
        value["actions"][0]["body"]["source"]["branch"]["name"],
        "feature/one"
    );
    fixture.no_requests().await
}

#[tokio::test]
async fn destructive_operations_require_confirmation_without_prompting() -> Result<()> {
    let fixture = Fixture::new().await?;
    let output = fixture
        .run(&["repo", "delete", "-R", "acme/widgets", "--json", "*"])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--yes"));
    fixture.no_requests().await
}

#[tokio::test]
async fn pr_list_normalizes_states_and_embeds_jq() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widgets/pullrequests"))
        .and(query_param("state", "DECLINED"))
        .and(query_param(
            "q",
            "(state = \"DECLINED\" OR state = \"SUPERSEDED\")",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"values":[pull_request("DECLINED")]})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "pr",
            "list",
            "-R",
            "acme/widgets",
            "--state",
            "closed",
            "--json",
            "number,state,isDraft",
            "--jq",
            ".[] | [.number, .state, .isDraft]",
        ])
        .await?;
    assert_eq!(data(&output, 0)?, json!([7, "CLOSED", true]));
    Ok(())
}

#[tokio::test]
async fn inline_comments_send_exact_cloud_coordinates() -> Result<()> {
    let fixture = Fixture::new().await?;
    let payload = json!({"content":{"raw":"Please handle errors"},"inline":{"path":"src/lib.rs","to":12},"pending":true});
    Mock::given(method("POST")).and(path("/2.0/repositories/acme/widgets/pullrequests/7/comments")).and(body_json(payload))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":19,"content":{"raw":"Please handle errors"},"inline":{"path":"src/lib.rs","to":12},"pending":true}))).expect(1).mount(&fixture.server).await;
    let output = fixture
        .run(&[
            "pr",
            "comment",
            "7",
            "-R",
            "acme/widgets",
            "--body",
            "Please handle errors",
            "--path",
            "src/lib.rs",
            "--line",
            "12",
            "--pending",
            "--json",
            "id,inline,pending",
        ])
        .await?;
    let value = data(&output, 0)?;
    assert_eq!(value["inline"]["to"], 12);
    assert_eq!(value["pending"], true);
    Ok(())
}

#[tokio::test]
async fn resolving_a_reply_is_rejected_before_writing() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("GET"))
        .and(path(
            "/2.0/repositories/acme/widgets/pullrequests/7/comments/19",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":19,"parent":{"id":10}})))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "pr",
            "comments",
            "resolve",
            "7",
            "19",
            "-R",
            "acme/widgets",
            "--json",
            "*",
        ])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("top-level"));
    assert_eq!(
        fixture
            .server
            .received_requests()
            .await
            .context("requests missing")?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn async_merge_waits_for_success_and_verifies_final_state() -> Result<()> {
    let fixture = Fixture::new().await?;
    let counter = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&counter);
    Mock::given(method("POST"))
        .and(path("/2.0/repositories/acme/widgets/pullrequests/7/merge"))
        .and(body_json(
            json!({"close_source_branch":false,"merge_strategy":"squash"}),
        ))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({"task_id":"merge-job"})))
        .expect(1)
        .mount(&fixture.server)
        .await;
    Mock::given(method("GET")).and(path("/2.0/repositories/acme/widgets/pullrequests/7/merge/task-status/merge-job"))
        .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({"task_status":if calls.fetch_add(1,Ordering::SeqCst)==0 {"PENDING"} else {"SUCCESS"}}))).expect(2).mount(&fixture.server).await;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widgets/pullrequests/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request("MERGED")))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "pr",
            "merge",
            "7",
            "-R",
            "acme/widgets",
            "--squash",
            "--json",
            "number,state",
        ])
        .await?;
    assert_eq!(data(&output, 0)?["state"], "MERGED");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn merge_poll_errors_are_not_swallowed() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("POST"))
        .and(path("/2.0/repositories/acme/widgets/pullrequests/7/merge"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({"task_id":"job"})))
        .expect(1)
        .mount(&fixture.server)
        .await;
    Mock::given(path(
        "/2.0/repositories/acme/widgets/pullrequests/7/merge/task-status/job",
    ))
    .respond_with(ResponseTemplate::new(403))
    .expect(1)
    .mount(&fixture.server)
    .await;
    let output = fixture
        .run(&["pr", "merge", "7", "-R", "acme/widgets", "--json", "number"])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "permission_denied");
    assert_eq!(output.stdout, [] as [u8; 0]);
    Ok(())
}

#[tokio::test]
async fn checks_distinguish_pending_failure_and_success() -> Result<()> {
    for (state, code) in [("INPROGRESS", 8), ("FAILED", 1), ("SUCCESSFUL", 0)] {
        let fixture = Fixture::new().await?;
        Mock::given(path(
            "/2.0/repositories/acme/widgets/commit/abc123/statuses",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"values":[{"key":"build","state":state}]})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
        let output = fixture
            .run(&[
                "commit",
                "status",
                "abc123",
                "-R",
                "acme/widgets",
                "--json",
                "key,state",
            ])
            .await?;
        assert_eq!(data(&output, code)?[0]["state"], state);
    }
    Ok(())
}

#[tokio::test]
async fn secured_variable_update_preserves_security_and_redacts_output() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("GET"))
        .and(path(
            "/2.0/repositories/acme/widgets/pipelines_config/variables",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"values":[{"uuid":"{variable}","key":"SECRET","secured":true}]}),
            ),
        )
        .expect(2)
        .mount(&fixture.server)
        .await;
    Mock::given(method("PUT"))
        .and(path(
            "/2.0/repositories/acme/widgets/pipelines_config/variables/%7Bvariable%7D",
        ))
        .and(body_json(
            json!({"key":"SECRET","value":"do-not-print","secured":true}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"uuid":"{variable}","key":"SECRET","value":"do-not-print","secured":true}),
        ))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let args = [
        "variable",
        "set",
        "SECRET",
        "-R",
        "acme/widgets",
        "--body",
        "do-not-print",
        "--json",
        "*",
    ];
    let output = fixture.run(&args).await?;
    assert_eq!(data(&output, 0)?["value"], Value::Null);
    let mut args = args.to_vec();
    args.push("--dry-run");
    let output = fixture.run(&args).await?;
    assert_eq!(
        data(&output, 0)?["actions"][0]["body"]["value"],
        "[REDACTED]"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("do-not-print"));
    Ok(())
}

#[tokio::test]
async fn project_permissions_use_cloud_permission_routes() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("PUT"))
        .and(path(
            "/2.0/workspaces/acme/projects/OPS/permissions-config/groups/developers",
        ))
        .and(body_json(json!({"permission":"create-repo"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"permission":"create-repo","group":{"slug":"developers"}})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "perms",
            "project",
            "OPS",
            "grant",
            "--group",
            "developers",
            "--permission",
            "create-repo",
            "--workspace",
            "acme",
            "--json",
            "permission",
        ])
        .await?;
    assert_eq!(data(&output, 0)?["permission"], "create-repo");
    Ok(())
}

#[tokio::test]
async fn slash_branch_names_are_single_encoded_path_segments() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widgets"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"mainbranch":{"name":"main"}})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(
            "/2.0/repositories/acme/widgets/refs/branches/feature%2Fone",
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "branch",
            "delete",
            "feature/one",
            "-R",
            "acme/widgets",
            "--yes",
            "--json",
            "*",
        ])
        .await?;
    assert_eq!(data(&output, 0)?["status"], "success");
    Ok(())
}

#[tokio::test]
async fn raw_api_keeps_native_json_and_follows_opaque_pagination() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("GET")).and(path("/2.0/repositories/acme/widgets/custom"))
        .and(query_param("pagelen","2")).and(header("x-test","value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values":[{"native_name":"first"}],"next":format!("{}/2.0/next?cursor=x",fixture.server.uri())}))).expect(1).mount(&fixture.server).await;
    Mock::given(path("/2.0/next"))
        .and(query_param("cursor", "x"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"values":[{"native_name":"second"}]})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "api",
            "repositories/{workspace}/{repo}/custom",
            "-R",
            "acme/widgets",
            "-P",
            "pagelen=2",
            "-H",
            "X-Test: value",
            "--paginate",
            "--slurp",
            "--jq",
            "[.[].values[].native_name]",
        ])
        .await?;
    assert_eq!(data(&output, 0)?, json!(["first", "second"]));
    Ok(())
}

#[tokio::test]
async fn auth_failures_are_structured_and_redacted() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(path("/2.0/user"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"message":"bad fixture-token"}})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture.run(&["auth", "status", "--json", "*"]).await?;
    assert_eq!(output.status.code(), Some(4));
    let value: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(value["error"]["code"], "authentication_required");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-token"));
    Ok(())
}

#[tokio::test]
async fn git_helper_is_scoped_to_bitbucket_and_token_type() -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let fixture = Fixture::new().await?;
    for (host, secret_expected) in [("bitbucket.org", true), ("evil.test", false)] {
        let mut child = fixture
            .command(&["auth", "git-credential", "get"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().context("missing stdin")?;
        stdin
            .write_all(format!("protocol=https\nhost={host}\n\n").as_bytes())
            .await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout)?;
        assert_eq!(text.contains("password=fixture-token"), secret_expected);
        if secret_expected {
            assert!(text.contains("username=x-bitbucket-api-token-auth"));
        }
    }
    fixture.no_requests().await
}

#[tokio::test]
async fn streamed_diff_preserves_bytes_and_cloud_commit_order() -> Result<()> {
    let fixture = Fixture::new().await?;
    let bytes = vec![b'+', 0xff, b'\n'];
    Mock::given(method("GET"))
        .and(path(
            "/2.0/repositories/acme/widgets/diff/feature%2Fx..main",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&["commit", "diff", "main", "feature/x", "-R", "acme/widgets"])
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, bytes);
    Ok(())
}

#[tokio::test]
async fn advertised_schemas_validate_actual_list_and_detail_outputs() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(path("/2.0/repositories/acme/widgets/pullrequests"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"values":[pull_request("OPEN")]})),
        )
        .mount(&fixture.server)
        .await;
    let schema = data(&fixture.run(&["schema", "pr", "list"]).await?, 0)?;
    let value = data(
        &fixture
            .run(&["pr", "list", "-R", "acme/widgets", "--json", "*"])
            .await?,
        0,
    )?;
    let validator = jsonschema::validator_for(&schema["outputSchema"])?;
    assert!(
        validator.is_valid(&value),
        "schema errors: {:?}",
        validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    let schema = data(&fixture.run(&["schema", "pr", "create"]).await?, 0)?;
    let value = data(
        &fixture
            .run(&[
                "pr",
                "create",
                "-R",
                "acme/widgets",
                "--title",
                "Plan",
                "--base",
                "main",
                "--head",
                "feature",
                "--dry-run",
                "--json",
                "*",
            ])
            .await?,
        0,
    )?;
    assert!(jsonschema::is_valid(&schema["dryRunOutputSchema"], &value));
    Ok(())
}

fn pipeline(state: &str, result: Option<&str>) -> Value {
    json!({"uuid":"{12345678-1234-4234-8234-123456789abc}","build_number":42,
        "state":{"name":state,"result":result.map(|name|json!({"name":name}))},
        "target":{"ref_name":"main","commit":{"hash":"abc123"}}})
}

#[tokio::test]
async fn pipeline_json_preserves_distinct_conclusions_and_native_states() -> Result<()> {
    let fixture = Fixture::new().await?;
    let cases = [
        ("PENDING", None, "queued", None),
        ("IN_PROGRESS", None, "in_progress", None),
        ("RUNNING", None, "running", None),
        ("PAUSED", None, "paused", None),
        ("ERROR", None, "error", None),
        (
            "COMPLETED",
            Some("SUCCESSFUL"),
            "completed",
            Some("success"),
        ),
        ("COMPLETED", Some("FAILED"), "completed", Some("failure")),
        ("COMPLETED", Some("ERROR"), "completed", Some("failure")),
        ("COMPLETED", Some("EXPIRED"), "completed", Some("timed_out")),
        ("COMPLETED", Some("STOPPED"), "completed", Some("cancelled")),
        ("COMPLETED", None, "completed", None),
        (
            "FUTURE_STATE",
            Some("FUTURE_RESULT"),
            "future_state",
            Some("future_result"),
        ),
    ];
    let runs: Vec<_> = cases
        .iter()
        .map(|(name, result, _, _)| pipeline(name, *result))
        .collect();
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widgets/pipelines"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values": runs})))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "run",
            "list",
            "-R",
            "acme/widgets",
            "--json",
            "status,conclusion,nativeState",
        ])
        .await?;
    let expected: Vec<_> = cases
        .into_iter()
        .map(|(name, result, status, conclusion)| {
            json!({
                "status": status,
                "conclusion": conclusion,
                "nativeState": {"name": name, "result": result.map(|name| json!({"name": name}))}
            })
        })
        .collect();
    assert_eq!(data(&output, 0)?, json!(expected));
    Ok(())
}

#[tokio::test]
async fn pipeline_runs_normalize_results_and_watch_timeout_is_pending() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(path("/2.0/repositories/acme/widgets/pipelines"))
        .and(query_param("q", "build_number=42"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"values":[pipeline("IN_PROGRESS",None)]})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "run",
            "watch",
            "42",
            "-R",
            "acme/widgets",
            "--timeout",
            "1",
            "--interval",
            "3",
            "--json",
            "number,status,conclusion",
        ])
        .await?;
    assert_eq!(
        data(&output, 8)?,
        json!({"number":42,"status":"in_progress","conclusion":null})
    );
    // The total deadline expires before the next poll; no extra HTTP call is sent.
    assert_eq!(
        fixture
            .server
            .received_requests()
            .await
            .context("requests missing")?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn custom_pipeline_is_triggered_once_and_runtime_secrets_are_redacted_in_plans() -> Result<()>
{
    let fixture = Fixture::new().await?;
    Mock::given(method("POST")).and(path("/2.0/repositories/acme/widgets/pipelines"))
        .and(body_json(json!({"target":{"type":"pipeline_ref_target","ref_type":"branch","ref_name":"main","selector":{"type":"custom","pattern":"deploy"}},"variables":[{"key":"TOKEN","value":"sensitive-value","secured":true}]})))
        .respond_with(ResponseTemplate::new(201).set_body_json(pipeline("PENDING",None))).expect(1).mount(&fixture.server).await;
    let args = [
        "workflow",
        "run",
        "deploy",
        "--ref",
        "main",
        "-R",
        "acme/widgets",
        "-f",
        "TOKEN=sensitive-value",
        "--json",
        "*",
    ];
    assert_eq!(data(&fixture.run(&args).await?, 0)?["number"], 42);
    let mut args = args.to_vec();
    args.push("--dry-run");
    let output = fixture.run(&args).await?;
    assert_eq!(
        data(&output, 0)?["actions"][0]["body"]["variables"][0]["value"],
        "[REDACTED]"
    );
    Ok(())
}

#[tokio::test]
async fn check_watch_timeout_covers_http_response_time() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(path(
        "/2.0/repositories/acme/widgets/commit/abc123/statuses",
    ))
    .respond_with(
        ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(5))
            .set_body_json(json!({"values":[]})),
    )
    .expect(1)
    .mount(&fixture.server)
    .await;
    let start = std::time::Instant::now();
    let output = fixture
        .run(&[
            "commit",
            "status",
            "abc123",
            "-R",
            "acme/widgets",
            "--watch",
            "--timeout",
            "1",
            "--json",
            "*",
        ])
        .await?;
    assert_eq!(data(&output, 8)?, json!([]));
    assert!(start.elapsed() < Duration::from_secs(4));
    Ok(())
}

#[tokio::test]
async fn batch_variable_updates_report_partial_failure_without_retrying() -> Result<()> {
    let fixture = Fixture::new().await?;
    let file = fixture.directory.path().join("values.env");
    std::fs::write(&file, "FIRST=one\nSECOND=two\n")?;
    Mock::given(method("GET"))
        .and(path("/2.0/workspaces/acme/pipelines-config/variables"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values":[]})))
        .expect(1)
        .mount(&fixture.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/2.0/workspaces/acme/pipelines-config/variables"))
        .and(body_json(
            json!({"key":"FIRST","value":"one","secured":false}),
        ))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"uuid":"first","key":"FIRST","value":"one","secured":false})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/2.0/workspaces/acme/pipelines-config/variables"))
        .and(body_json(
            json!({"key":"SECOND","value":"two","secured":false}),
        ))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "variable",
            "set",
            "--scope",
            "workspace",
            "--workspace",
            "acme",
            "--env-file",
            file.to_str().context("invalid test path")?,
            "--json",
            "*",
        ])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("after 1 successful updates"));
    Ok(())
}

async fn git(fixture: &Fixture, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(fixture.directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            fixture.directory.path().join("gitconfig"),
        )
        .args(args)
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[tokio::test]
async fn git_repository_resolution_rejects_ambiguity_and_respects_local_default() -> Result<()> {
    let fixture = Fixture::new().await?;
    git(&fixture, &["init", "--initial-branch=main"]).await?;
    git(
        &fixture,
        &["remote", "add", "origin", "git@bitbucket.org:acme/one.git"],
    )
    .await?;
    git(
        &fixture,
        &[
            "remote",
            "add",
            "upstream",
            "https://bitbucket.org/acme/two.git",
        ],
    )
    .await?;
    let output = fixture
        .run(&["repo", "view", "--json", "nameWithOwner"])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("multiple Bitbucket"));
    fixture.no_requests().await?;
    let output = fixture
        .run(&["repo", "set-default", "acme/two", "--json", "*"])
        .await?;
    data(&output, 0)?;
    Mock::given(path("/2.0/repositories/acme/two"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"full_name":"acme/two"})))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&["repo", "view", "--json", "nameWithOwner"])
        .await?;
    assert_eq!(data(&output, 0)?["nameWithOwner"], "acme/two");
    Ok(())
}

#[tokio::test]
async fn dirty_checkout_is_rejected_without_fetching_or_changing_files() -> Result<()> {
    let fixture = Fixture::new().await?;
    git(&fixture, &["init", "--initial-branch=main"]).await?;
    git(
        &fixture,
        &[
            "remote",
            "add",
            "origin",
            "https://bitbucket.org/acme/widgets.git",
        ],
    )
    .await?;
    std::fs::write(fixture.directory.path().join("important.txt"), "local work")?;
    let output = fixture.run(&["pr", "checkout", "7", "--json", "*"]).await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("local changes"));
    assert_eq!(
        std::fs::read_to_string(fixture.directory.path().join("important.txt"))?,
        "local work"
    );
    fixture.no_requests().await
}

#[tokio::test]
async fn concurrent_context_writes_do_not_lose_updates() -> Result<()> {
    let fixture = Fixture::new().await?;
    let mut children = Vec::new();
    for index in 0..8 {
        children.push(
            fixture
                .command(&[
                    "context",
                    "create",
                    &format!("ctx{index}"),
                    "--workspace",
                    "acme",
                    "-R",
                    "acme/widgets",
                    "--json",
                    "*",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()?,
        );
    }
    for child in children {
        let output = child.wait_with_output().await?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = fixture.run(&["context", "list", "--json", "name"]).await?;
    assert_eq!(
        data(&output, 0)?
            .as_array()
            .context("missing contexts")?
            .len(),
        8
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::metadata(fixture.directory.path().join("config/config.toml"))?
            .permissions()
            .mode();
        assert_eq!(permissions & 0o077, 0);
    };
    fixture.no_requests().await
}

#[tokio::test]
async fn pipeline_fail_fast_stops_after_a_failed_timed_out_or_cancelled_step() -> Result<()> {
    for result in ["FAILED", "ERROR", "EXPIRED", "STOPPED"] {
        let fixture = Fixture::new().await?;
        Mock::given(path("/2.0/repositories/acme/widgets/pipelines"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"values":[pipeline("IN_PROGRESS",None)]})),
            )
            .expect(1)
            .mount(&fixture.server)
            .await;
        Mock::given(path("/2.0/repositories/acme/widgets/pipelines/%7B12345678-1234-4234-8234-123456789abc%7D/steps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values":[{"uuid":"step","name":"Build","state":{"name":"COMPLETED","result":{"name":result}}}]}))).expect(1).mount(&fixture.server).await;
        let output = fixture
            .run(&[
                "run",
                "watch",
                "42",
                "-R",
                "acme/widgets",
                "--fail-fast",
                "--json",
                "number,status",
            ])
            .await?;
        assert_eq!(data(&output, 1)?["status"], "in_progress");
    }
    Ok(())
}

#[tokio::test]
async fn renderer_failure_reports_that_the_write_already_completed() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("POST"))
        .and(path("/2.0/repositories/acme/widgets/pullrequests"))
        .respond_with(ResponseTemplate::new(201).set_body_json(pull_request("OPEN")))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let output = fixture
        .run(&[
            "pr",
            "create",
            "-R",
            "acme/widgets",
            "--title",
            "x",
            "--base",
            "main",
            "--head",
            "feature",
            "--json",
            "number",
            "--jq",
            ".number | .field",
        ])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("command completed"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn interrupt_during_watch_returns_cancelled() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(path(
        "/2.0/repositories/acme/widgets/commit/abc123/statuses",
    ))
    .respond_with(
        ResponseTemplate::new(200)
            .set_body_json(json!({"values":[{"key":"build","state":"INPROGRESS"}]})),
    )
    .mount(&fixture.server)
    .await;
    let child = fixture
        .command(&[
            "commit",
            "status",
            "abc123",
            "-R",
            "acme/widgets",
            "--watch",
            "--json",
            "*",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let pid = child.id().context("missing child PID")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture
                .server
                .received_requests()
                .await
                .is_some_and(|requests| !requests.is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert!(
        Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .await?
            .success()
    );
    let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output()).await??;
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "cancelled");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn git_subprocess_receives_context_and_credentials_stay_out_of_arguments() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new().await?;
    data(
        &fixture
            .run(&[
                "context",
                "create",
                "work",
                "--workspace",
                "acme",
                "-R",
                "acme/widgets",
                "--json",
                "*",
            ])
            .await?,
        0,
    )?;
    let tools = fixture.directory.path().join("tools");
    std::fs::create_dir(&tools)?;
    let fake = tools.join("git");
    std::fs::write(
        &fake,
        r"#!/usr/bin/env python3
import json,os,sys
with open('git-invocation.json','w') as f:
 json.dump({'args':sys.argv[1:],'context':os.getenv('BB_CONTEXT'),'prompt':os.getenv('GIT_TERMINAL_PROMPT'),'ssh':os.getenv('GIT_SSH_COMMAND')},f)
",
    )?;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    let mut paths = vec![tools];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = fixture
        .command(&[
            "repo",
            "clone",
            "acme/widgets",
            "--context",
            "work",
            "--json",
            "*",
        ])
        .env("PATH", std::env::join_paths(paths)?)
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .output()
        .await?;
    data(&output, 0)?;
    let recorded: Value = serde_json::from_str(&std::fs::read_to_string(
        fixture.directory.path().join("git-invocation.json"),
    )?)?;
    assert_eq!(recorded["context"], "work");
    assert_eq!(recorded["prompt"], "0");
    assert_eq!(recorded["ssh"], "ssh -oBatchMode=yes");
    assert!(!recorded.to_string().contains("fixture-token"));
    assert!(
        recorded
            .to_string()
            .contains("credential.https://bitbucket.org.helper=")
    );
    fixture.no_requests().await
}

#[cfg(unix)]
async fn interrupt_at_trace(mut child: tokio::process::Child, marker: &str) -> Result<Output> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let pid = child.id().context("missing child PID")?;
    let stderr = child.stderr.take().context("stderr must be piped")?;
    // Keep stdin open until process exit, so EOF cannot race with interruption.
    let _stdin = child.stdin.take();
    let mut reader = BufReader::new(stderr);
    let mut prefix = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                anyhow::bail!("process stopped before {marker}: {prefix}");
            }
            let ready = line.contains(marker);
            prefix.push_str(&line);
            if ready {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert!(
        Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .await?
            .success()
    );
    let mut output =
        tokio::time::timeout(Duration::from_secs(2), child.wait_with_output()).await??;
    reader.read_to_string(&mut prefix).await?;
    output.stderr = prefix.into_bytes();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(output.stdout, [] as [u8; 0]);
    let stderr = std::str::from_utf8(&output.stderr)?;
    let error: Value = serde_json::from_str(
        stderr
            .lines()
            .find(|line| line.starts_with("{\"error\""))
            .context("missing structured cancellation")?,
    )?;
    assert_eq!(error["error"]["code"], "cancelled");
    Ok(output)
}

#[cfg(unix)]
#[tokio::test]
async fn interrupt_while_stdin_is_open_exits_without_eof() -> Result<()> {
    let fixture = Fixture::new().await?;
    let child = fixture
        .command(&["auth", "login", "--with-token", "--json", "*", "--verbose"])
        .env_remove("BB_TOKEN")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    interrupt_at_trace(child, "reading stdin").await?;
    fixture.no_requests().await
}

#[cfg(unix)]
#[tokio::test]
async fn interrupt_during_jq_computation_exits_without_a_result() -> Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(path("/2.0/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name":"fixture"})))
        .expect(1)
        .mount(&fixture.server)
        .await;
    let child = fixture
        .command(&[
            "api",
            "user",
            "--jq",
            "last(range(0; 1000000000000))",
            "--json",
            "*",
            "--verbose",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    interrupt_at_trace(child, "evaluating jq filter").await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn interrupt_while_waiting_for_config_lock_performs_no_write() -> Result<()> {
    let fixture = Fixture::new().await?;
    let config = fixture.directory.path().join("config");
    std::fs::create_dir(&config)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.join("config.lock"))?;
    lock.try_lock()?;
    let child = fixture
        .command(&["context", "create", "cancelled", "--json", "*", "--verbose"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    interrupt_at_trace(child, "waiting for configuration lock").await?;
    assert!(!config.join("config.toml").exists());
    fixture.no_requests().await
}

#[tokio::test]
async fn broken_git_configuration_never_selects_context_repository() -> Result<()> {
    let fixture = Fixture::new().await?;
    data(
        &fixture
            .run(&[
                "context",
                "create",
                "fallback",
                "-R",
                "acme/widgets",
                "--json",
                "*",
            ])
            .await?,
        0,
    )?;
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(fixture.directory.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                fixture.directory.path().join("gitconfig")
            )
            .status()
            .await?
            .success()
    );
    std::fs::write(
        fixture.directory.path().join(".git/config"),
        "[broken configuration\n",
    )?;
    let output = fixture.run(&["repo", "view", "--json", "*"]).await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot inspect the Git repository"));
    fixture.no_requests().await
}

#[tokio::test]
async fn context_repository_is_used_when_no_git_repository_exists() -> Result<()> {
    let fixture = Fixture::new().await?;
    data(
        &fixture
            .run(&[
                "context",
                "create",
                "fallback",
                "-R",
                "acme/widgets",
                "--json",
                "*",
            ])
            .await?,
        0,
    )?;
    Mock::given(path("/2.0/repositories/acme/widgets"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"name":"widgets","full_name":"acme/widgets"})),
        )
        .expect(1)
        .mount(&fixture.server)
        .await;
    assert_eq!(
        data(
            &fixture
                .run(&["repo", "view", "--json", "nameWithOwner"])
                .await?,
            0
        )?["nameWithOwner"],
        "acme/widgets"
    );
    Ok(())
}

#[cfg(unix)]
async fn run_terminal_test(script: &str) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        Command::new("python3")
            .arg("-I")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .join(script),
            )
            .arg(env!("CARGO_BIN_EXE_bb"))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .with_context(|| format!("terminal harness {script} exceeded 45 seconds"))??;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_prompts_restore_settings_after_interruptions() -> Result<()> {
    run_terminal_test("terminal.py").await
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_harness_bounds_waits_and_cleans_up_children() -> Result<()> {
    run_terminal_test("test_terminal.py").await
}

fn schema_paths(schema: &Value, result: &mut Vec<String>) {
    if let Some(children) = schema["commands"].as_array() {
        for child in children {
            result.push(child["command"].as_str().unwrap_or_default().into());
            schema_paths(child, result);
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn unavailable_keyring_preserves_authentication_error_contract() -> Result<()> {
    let fixture = Fixture::new().await?;
    let directory = fixture.directory.path().join("config");
    tokio::fs::create_dir_all(&directory).await?;
    tokio::fs::write(
        directory.join("config.toml"),
        "active_profile = 'test'\n[profiles.test]\ntoken_type = 'oauth'\n",
    )
    .await?;
    let output = fixture
        .command(&["auth", "status", "--json", "*"])
        .env_remove("BB_TOKEN")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/missing-bus-secret", directory.display()),
        )
        .output()
        .await?;
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(output.stdout, [] as [u8; 0]);
    let value: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(value["error"]["code"], "authentication_required");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("missing-bus-secret"));
    fixture.no_requests().await?;
    Ok(())
}
