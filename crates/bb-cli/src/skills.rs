//! Offline distribution of the official skill bundled with this executable.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    args::{SkillAgent, SkillScope, SkillTarget, SkillsCommand},
    output::{Kind, Outcome},
};

const SOURCE: &str = include_str!("../skills/bb-cli/SKILL.md");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MARKER: &str = "\n<!-- bb-cli-managed-skill v1 ";
// Instructions are small; never buffer arbitrary files found at the destination.
const MAX_SKILL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillView {
    name: &'static str,
    agent: SkillAgent,
    scope: SkillScope,
    path: PathBuf,
    bundled_version: &'static str,
    installed_version: Option<String>,
    status: Status,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum Status {
    NotInstalled,
    Current,
    DifferentVersion,
    Modified,
    Unmanaged,
}

#[derive(Debug)]
enum Installation {
    Missing,
    Current,
    Different(String),
    Modified,
    Unmanaged,
}

fn bundle(version: &str) -> String {
    // One file contains instructions, provenance, and their digest, so an atomic
    // replacement cannot leave metadata referring to a different file revision.
    format!("{SOURCE}{MARKER}{version} {} -->\n", digest(SOURCE))
}

fn digest(text: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            // Formatting bytes into a String cannot fail.
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn inspect(path: &Path, bundled: &str) -> Result<Installation> {
    let Some(metadata) = metadata(path)? else {
        return Ok(Installation::Missing);
    };
    if !metadata.is_file() {
        bail!(
            "skill destination is not a regular file: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_SKILL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        return Ok(Installation::Unmanaged);
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(Installation::Unmanaged);
    };
    if text == bundled {
        return Ok(Installation::Current);
    }
    let Some((body, record)) = text.rsplit_once(MARKER) else {
        return Ok(Installation::Unmanaged);
    };
    let Some((version, checksum)) = record
        .strip_suffix(" -->\n")
        .and_then(|record| record.split_once(' '))
    else {
        return Ok(Installation::Unmanaged);
    };
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".+-".contains(&c))
    {
        return Ok(Installation::Unmanaged);
    }
    if checksum != digest(body) {
        return Ok(Installation::Modified);
    }
    Ok(Installation::Different(version.into()))
}

/// Refuse symlinks and Windows reparse points in destinations we manage.
fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", path.display()));
        }
    };
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    if linked {
        bail!(
            "skill destination must not be a symbolic link or reparse point: {}",
            path.display()
        );
    }
    Ok(Some(metadata))
}

fn directories(root: &Path, agent: SkillAgent, create: bool) -> Result<PathBuf> {
    let mut path = root.to_owned();
    for component in [
        match agent {
            SkillAgent::Codex => ".agents",
            SkillAgent::Claude => ".claude",
        },
        "skills",
        "bb-cli",
    ] {
        path.push(component);
        match metadata(&path)? {
            Some(metadata) if !metadata.is_dir() => bail!("not a directory: {}", path.display()),
            None if create => fs::create_dir(&path)
                .with_context(|| format!("cannot create {}", path.display()))?,
            Some(_) | None => {}
        }
    }
    Ok(path)
}

fn root(target: &SkillTarget) -> Result<PathBuf> {
    let root = match target.scope {
        SkillScope::Project => target
            .directory
            .clone()
            .map_or_else(std::env::current_dir, Ok)?,
        SkillScope::User => {
            if target.directory.is_some() {
                bail!("--directory is only supported with --scope project");
            }
            directories::BaseDirs::new()
                .context("cannot determine the user's home directory")?
                .home_dir()
                .to_owned()
        }
    };
    root.canonicalize()
        .context("skill installation root must be an existing directory")
}

fn lock(directory: &Path) -> Result<File> {
    let parent = directory
        .parent()
        .context("skill directory has no parent")?;
    let path = parent.join(".bb-cli.lock");
    if metadata(&path)?.is_some_and(|metadata| !metadata.is_file()) {
        bail!("skill lock is not a regular file");
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => {
            bail!("another bb skills operation is in progress; retry after it finishes")
        }
        Err(fs::TryLockError::Error(error)) => {
            Err(error).context("cannot lock the skill installation")
        }
    }
}

fn ensure_managed(installation: &Installation, path: &Path) -> Result<()> {
    if matches!(
        installation,
        Installation::Modified | Installation::Unmanaged
    ) {
        bail!(
            "skill has local changes or was not installed by bb; preserve or move it before retrying: {}",
            path.display()
        );
    }
    Ok(())
}

fn change(root: &Path, target: &SkillTarget, install: bool, dry_run: bool) -> Result<Outcome> {
    let bundled = bundle(VERSION);
    let directory = directories(root, target.agent, false)?;
    let path = directory.join("SKILL.md");
    let initial = inspect(&path, &bundled)?;
    ensure_managed(&initial, &path)?;
    let mut changed = if install {
        !matches!(initial, Installation::Current)
    } else {
        !matches!(initial, Installation::Missing)
    };
    if changed && !dry_run {
        directories(root, target.agent, true)?;
        // The blocking worker owns the lock through persistence even if cancelled.
        let _lock = lock(&directory)?;
        let current = inspect(&path, &bundled)?;
        ensure_managed(&current, &path)?;
        changed = if install {
            !matches!(current, Installation::Current)
        } else {
            !matches!(current, Installation::Missing)
        };
        if install && changed {
            let mut file = tempfile::NamedTempFile::new_in(&directory)?;
            file.write_all(bundled.as_bytes())?;
            file.as_file().sync_all()?;
            if matches!(current, Installation::Missing) {
                file.persist_noclobber(&path)
                    .context("cannot install the skill without replacing an existing file")?;
            } else {
                file.persist(&path)
                    .context("cannot atomically update the skill")?;
            }
        } else if changed {
            fs::remove_file(&path).context("cannot remove the installed skill")?;
            // Keep the directory and any user additions; only SKILL.md is owned.
        }
    }
    Outcome::action(
        if install {
            "skills install"
        } else {
            "skills remove"
        },
        vec![
            json!({"name":"bb-cli","agent":target.agent,"scope":target.scope,"path":path,"version":VERSION,"changed":changed}),
        ],
        dry_run,
    )
}

pub async fn execute(command: SkillsCommand, dry_run: bool) -> Result<Outcome> {
    tokio::task::spawn_blocking(move || {
        let target = match &command {
            SkillsCommand::Show => return Ok(Outcome::text(bundle(VERSION))),
            SkillsCommand::Install(target)
            | SkillsCommand::Status(target)
            | SkillsCommand::Remove(target) => target,
        };
        let root = root(target)?;
        match &command {
            SkillsCommand::Install(_) => change(&root, target, true, dry_run),
            SkillsCommand::Remove(_) => change(&root, target, false, dry_run),
            SkillsCommand::Status(_) => {
                let path = directories(&root, target.agent, false)?.join("SKILL.md");
                let (status, installed_version) = match inspect(&path, &bundle(VERSION))? {
                    Installation::Missing => (Status::NotInstalled, None),
                    Installation::Current => (Status::Current, Some(VERSION.into())),
                    Installation::Different(version) => (Status::DifferentVersion, Some(version)),
                    Installation::Modified => (Status::Modified, None),
                    Installation::Unmanaged => (Status::Unmanaged, None),
                };
                Outcome::resource(
                    Kind::Skill,
                    SkillView {
                        name: "bb-cli",
                        agent: target.agent,
                        scope: target.scope,
                        path,
                        bundled_version: VERSION,
                        installed_version,
                        status,
                    },
                )
            }
            SkillsCommand::Show => unreachable!("show returned before target resolution"),
        }
    })
    .await?
}

#[cfg(test)]
mod tests;
