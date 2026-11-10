use super::*;

fn target(root: &Path) -> SkillTarget {
    SkillTarget {
        agent: SkillAgent::Codex,
        scope: SkillScope::Project,
        directory: Some(root.into()),
    }
}

#[test]
fn updates_old_managed_skills_and_preserves_user_files_on_removal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let target = target(temp.path());
    let directory = directories(temp.path(), target.agent, true)?;
    let path = directory.join("SKILL.md");
    fs::write(&path, bundle("0.0.1"))?;
    fs::write(directory.join("notes.txt"), "personal notes")?;
    assert!(
        matches!(inspect(&path, &bundle(VERSION))?, Installation::Different(version) if version == "0.0.1")
    );
    change(temp.path(), &target, true, false)?;
    assert_eq!(fs::read_to_string(&path)?, bundle(VERSION));
    change(temp.path(), &target, false, false)?;
    assert!(!path.exists());
    assert_eq!(
        fs::read_to_string(directory.join("notes.txt"))?,
        "personal notes"
    );
    Ok(())
}

#[test]
fn local_changes_and_unmanaged_files_survive_install_and_remove() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let target = target(temp.path());
    let path = directories(temp.path(), target.agent, true)?.join("SKILL.md");
    for content in [
        bundle("0.0.1").replacen("Bitbucket Cloud with bb", "My instructions", 1),
        "personal skill".into(),
    ] {
        fs::write(&path, &content)?;
        for install in [true, false] {
            change(temp.path(), &target, install, false)
                .err()
                .context("modified skill was overwritten")?;
            assert_eq!(fs::read_to_string(&path)?, content);
        }
    }
    Ok(())
}

#[test]
fn competing_writer_cannot_replace_a_locked_installation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let target = target(temp.path());
    let directory = directories(temp.path(), target.agent, true)?;
    let path = directory.join("SKILL.md");
    fs::write(&path, bundle("0.0.1"))?;
    let _guard = lock(&directory)?;
    let error = change(temp.path(), &target, true, false)
        .err()
        .context("second writer acquired lock")?;
    assert!(error.to_string().contains("in progress"));
    assert_eq!(fs::read_to_string(path)?, bundle("0.0.1"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_destinations_cannot_redirect_writes_or_deletes() -> Result<()> {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let target = target(temp.path());
    let outside = tempfile::tempdir()?;
    symlink(outside.path(), temp.path().join(".agents"))?;
    change(temp.path(), &target, true, false)
        .err()
        .context("followed directory symlink")?;
    assert_eq!(fs::read_dir(outside.path())?.count(), 0);
    fs::remove_file(temp.path().join(".agents"))?;
    let directory = directories(temp.path(), target.agent, true)?;
    let original = outside.path().join("important.md");
    fs::write(&original, bundle(VERSION))?;
    symlink(&original, directory.join("SKILL.md"))?;
    change(temp.path(), &target, false, false)
        .err()
        .context("followed skill symlink")?;
    assert!(original.exists());
    Ok(())
}

#[cfg(windows)]
#[test]
fn junction_cannot_redirect_skill_installation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let junction = temp.path().join(".agents");
    let created = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(outside.path())
        .output()?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let result = change(temp.path(), &target(temp.path()), true, false);
    fs::remove_dir(&junction)?;
    result
        .err()
        .context("followed a Windows directory junction")?;
    assert_eq!(fs::read_dir(outside.path())?.count(), 0);
    Ok(())
}
