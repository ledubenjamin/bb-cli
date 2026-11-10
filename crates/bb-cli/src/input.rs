//! Bounded input and cancellable terminal prompts.

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox, SecretString};
use std::{
    io::{self, IsTerminal, Read, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::AsyncReadExt;

/// The established request-body limit also applies to body files and stdin.
pub const BODY_LIMIT: u64 = 16 * 1024 * 1024;
/// Credentials and the Git credential protocol accept at most 64 KiB.
pub const CREDENTIAL_LIMIT: u64 = 65_536;
// Poll frequently enough to restore terminal settings promptly on external cancellation.
const PROMPT_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Default)]
pub struct Input {
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
enum Echo {
    Visible,
    Hidden,
}

impl Input {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub async fn password(&self, prompt: &str) -> Result<SecretString> {
        self.prompt(prompt, Echo::Hidden).await
    }

    pub async fn confirm(&self, prompt: &str) -> Result<bool> {
        let answer = self
            .prompt(&format!("{prompt} [y/N] "), Echo::Visible)
            .await?;
        let answer = answer.expose_secret().trim();
        Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
    }

    async fn prompt(&self, prompt: &str, echo: Echo) -> Result<SecretString> {
        if !io::stdin().is_terminal() {
            bail!("interactive input requires a terminal");
        }
        let cancelled = Arc::clone(&self.cancelled);
        let prompt = prompt.to_owned();
        // Polling is bounded and checks cancellation, so shutdown can join this worker.
        tokio::task::spawn_blocking(move || read_terminal(&prompt, echo, &cancelled)).await?
    }
}

struct TerminalMode;

impl TerminalMode {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for TerminalMode {
    fn drop(&mut self) {
        // Best effort during unwinding too; no secret is ever emitted by cleanup.
        let _ = disable_raw_mode();
    }
}

fn read_terminal(prompt: &str, echo: Echo, cancelled: &AtomicBool) -> Result<SecretString> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(crate::app::cancelled());
    }
    let mut output = io::stderr().lock();
    write!(output, "{prompt}")?;
    output.flush()?;
    let _terminal = TerminalMode::enter()?;
    let mut buffer = SecretBox::new(Box::new(String::new()));
    loop {
        if cancelled.load(Ordering::Relaxed) {
            write!(output, "\r\n")?;
            return Err(crate::app::cancelled());
        }
        if !event::poll(PROMPT_POLL)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                write!(output, "\r\n")?;
                return Err(crate::app::cancelled());
            }
            KeyCode::Esc => {
                write!(output, "\r\n")?;
                return Err(crate::app::cancelled());
            }
            KeyCode::Enter => {
                write!(output, "\r\n")?;
                return Ok(SecretString::from(buffer.expose_secret().clone()));
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if buffer.expose_secret().is_empty() {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
                }
            }
            KeyCode::Backspace => {
                if buffer.expose_secret_mut().pop().is_some() && matches!(echo, Echo::Visible) {
                    write!(output, "\x08 \x08")?;
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control() =>
            {
                if buffer.expose_secret().len() as u64 + c.len_utf8() as u64 > CREDENTIAL_LIMIT {
                    bail!("interactive input exceeds the {CREDENTIAL_LIMIT} byte limit");
                }
                buffer.expose_secret_mut().push(c);
                if matches!(echo, Echo::Visible) {
                    write!(output, "{c}")?;
                }
            }
            _ => {}
        }
        output.flush()?;
    }
}

pub async fn stdin(limit: u64) -> Result<String> {
    crate::blocking::isolated("bb-stdin", move || {
        tracing::debug!(name: "input.stdin.wait", "reading stdin");
        read(io::stdin().lock(), limit)
    })
    .await
}

fn read(reader: impl Read, limit: u64) -> Result<String> {
    let mut value = String::new();
    reader.take(limit + 1).read_to_string(&mut value)?;
    check_length(&value, limit)?;
    Ok(value)
}

pub async fn file(path: &Path, limit: u64) -> Result<String> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("cannot read body file '{}'", path.display()))?;
    let mut value = String::new();
    file.take(limit + 1).read_to_string(&mut value).await?;
    check_length(&value, limit)?;
    Ok(value)
}

fn check_length(value: &str, limit: u64) -> Result<()> {
    if value.len() as u64 > limit {
        bail!("input exceeds the {limit} byte limit");
    }
    Ok(())
}
