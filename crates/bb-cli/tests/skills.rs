use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(root: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_bb"))
        .current_dir(root)
        .env_remove("BB_TOKEN")
        .env_remove("BB_REPO")
        .env_remove("BB_WORKSPACE")
        .env("BB_CONFIG_DIR", root.join("broken-config"))
        .env("BB_CONTEXT", "nonexistent")
        .env("BB_API_URL", "this is intentionally not a URL")
        .args(args)
        .output()?)
}

fn data(output: &Output) -> Result<Value> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn skills_are_offline_discoverable_and_installed_without_changing_existing_configuration()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("project with spaces é");
    std::fs::create_dir_all(root.join("broken-config"))?;
    std::fs::write(root.join("broken-config/config.toml"), "invalid [ toml")?;
    for agent in ["codex", "claude"] {
        let path = root
            .join(if agent == "codex" {
                ".agents"
            } else {
                ".claude"
            })
            .join("skills/bb-cli/SKILL.md");
        let args = ["skills", "install", "--agent", agent];
        let mut preview = args.to_vec();
        preview.extend(["--dry-run", "--json", "*"]);
        assert_eq!(data(&run(&root, &preview)?)?["dryRun"], true);
        assert!(!path.exists());
        // Bare --json and invalid field selections must never install anything.
        let mut discovery = args.to_vec();
        discovery.push("--json");
        assert!(
            data(&run(&root, &discovery)?)?["fields"]
                .as_array()
                .context("fields missing")?
                .contains(&json!("actions"))
        );
        let mut invalid = discovery.clone();
        invalid.push("notAField");
        assert!(!run(&root, &invalid)?.status.success());
        assert!(!path.exists());
        let mut execute = args.to_vec();
        execute.extend(["--json", "*"]);
        assert_eq!(data(&run(&root, &execute)?)?["actions"][0]["changed"], true);
        assert_eq!(
            data(&run(&root, &execute)?)?["actions"][0]["changed"],
            false
        );
        let shown = run(&root, &["skills", "show"])?;
        assert!(shown.status.success());
        assert_eq!(std::fs::read(&path)?, shown.stdout);
        let status = data(&run(
            &root,
            &["skills", "status", "--agent", agent, "--json", "*"],
        )?)?;
        assert_eq!(status["status"], "current");
        let schema = data(&run(&root, &["schema", "skills", "status"])?)?;
        assert!(jsonschema::is_valid(&schema["outputSchema"], &status));
        for _ in 0..2 {
            data(&run(
                &root,
                &["skills", "remove", "--agent", agent, "--json", "*"],
            )?)?;
            assert!(!path.exists());
        }
    }
    for verb in ["show", "status", "install", "remove"] {
        let schema = data(&run(&root, &["schema", "skills", verb])?)?;
        assert_eq!(schema["apiTokenScopes"], json!([]));
        assert_eq!(schema["oauthScopes"], json!([]));
        assert_eq!(
            schema["effect"],
            if matches!(verb, "install" | "remove") {
                "local-write"
            } else {
                "read"
            }
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("broken-config/config.toml"))?,
        "invalid [ toml"
    );
    Ok(())
}

#[test]
fn skill_conflicts_are_reported_as_json_and_preserve_local_edits() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    data(&run(
        root,
        &["skills", "install", "--agent", "codex", "--json", "*"],
    )?)?;
    let path = root.join(".agents/skills/bb-cli/SKILL.md");
    let changed =
        std::fs::read_to_string(&path)?.replacen("Bitbucket Cloud with bb", "Personal changes", 1);
    std::fs::write(&path, &changed)?;
    let status = data(&run(
        root,
        &["skills", "status", "--agent", "codex", "--json", "status"],
    )?)?;
    assert_eq!(status["status"], "modified");
    for verb in ["install", "remove"] {
        let output = run(root, &["skills", verb, "--agent", "codex", "--json", "*"])?;
        assert_eq!(output.status.code(), Some(1));
        let error: Value = serde_json::from_slice(&output.stderr)?;
        assert!(
            error["error"]["message"]
                .as_str()
                .context("missing error")?
                .contains("local changes")
        );
        assert_eq!(std::fs::read_to_string(&path)?, changed);
    }
    Ok(())
}
