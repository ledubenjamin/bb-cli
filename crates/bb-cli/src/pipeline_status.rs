//! Pipeline status interpretation shared by the TUI, watchers, and JSON output.
//! Native names stay in the API models, including names introduced by Bitbucket later.

use bb_api::models::RunState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress,
    Running,
    Paused,
    Completed,
    Successful,
    Failed,
    Error,
    Stopped,
    Expired,
    Unknown,
}

impl Status {
    pub fn from_name(name: &str) -> Self {
        match name {
            "PENDING" => Self::Pending,
            "IN_PROGRESS" => Self::InProgress,
            "RUNNING" => Self::Running,
            "PAUSED" => Self::Paused,
            "COMPLETED" => Self::Completed,
            "SUCCESSFUL" => Self::Successful,
            "FAILED" => Self::Failed,
            "ERROR" => Self::Error,
            "STOPPED" => Self::Stopped,
            "EXPIRED" => Self::Expired,
            _ => Self::Unknown,
        }
    }

    /// Failures requiring attention in the TUI, including timeouts but not cancellations.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed | Self::Error | Self::Expired)
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Pending | Self::InProgress | Self::Running)
    }
}

fn result(state: &RunState) -> Option<Status> {
    state
        .result
        .as_ref()
        .map(|result| Status::from_name(&result.name))
}

pub fn failed(state: &RunState) -> bool {
    result(state).is_some_and(Status::is_failure)
}

pub fn running(state: &RunState) -> bool {
    Status::from_name(&state.name).is_running()
}

/// A cancelled step also stops a fail-fast watcher, without becoming a TUI failure.
pub fn fails_fast(state: &RunState) -> bool {
    result(state).is_some_and(|result| result.is_failure() || result == Status::Stopped)
}

/// Keep watch exit codes conservative: only a completed success exits with zero.
pub fn completion_code(state: &RunState) -> Option<u8> {
    match Status::from_name(&state.name) {
        Status::Completed => Some(u8::from(result(state) != Some(Status::Successful))),
        Status::Error => Some(1),
        Status::Pending
        | Status::InProgress
        | Status::Running
        | Status::Paused
        | Status::Successful
        | Status::Failed
        | Status::Stopped
        | Status::Expired
        | Status::Unknown => None,
    }
}

pub fn normalized_status(name: &str) -> String {
    if Status::from_name(name) == Status::Pending {
        "queued".into()
    } else {
        name.to_ascii_lowercase()
    }
}

/// JSON preserves the cause even when the TUI groups several results as failures.
pub fn normalized_conclusion(name: &str) -> String {
    match Status::from_name(name) {
        Status::Successful => "success".into(),
        Status::Failed | Status::Error => "failure".into(),
        Status::Stopped => "cancelled".into(),
        Status::Expired => "timed_out".into(),
        Status::Pending
        | Status::InProgress
        | Status::Running
        | Status::Paused
        | Status::Completed
        | Status::Unknown => name.to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bb_api::models::RunResult;

    #[test]
    fn failure_policies_keep_cancellations_unknowns_and_missing_results_distinct() {
        for (name, failure, fail_fast, exit) in [
            (Some("SUCCESSFUL"), false, false, 0),
            (Some("FAILED"), true, true, 1),
            (Some("ERROR"), true, true, 1),
            (Some("EXPIRED"), true, true, 1),
            (Some("STOPPED"), false, true, 1),
            (Some("FUTURE_RESULT"), false, false, 1),
            (Some(""), false, false, 1),
            (None, false, false, 1),
        ] {
            let state = RunState {
                name: "COMPLETED".into(),
                result: name.map(|name| RunResult { name: name.into() }),
            };
            assert_eq!(failed(&state), failure, "{name:?}");
            assert_eq!(fails_fast(&state), fail_fast, "{name:?}");
            assert_eq!(completion_code(&state), Some(exit), "{name:?}");
            assert!(!running(&state), "{name:?}");
        }
    }

    #[test]
    fn only_completed_or_error_states_end_a_watch() {
        for (name, active, exit) in [
            ("PENDING", true, None),
            ("IN_PROGRESS", true, None),
            ("RUNNING", true, None),
            ("PAUSED", false, None),
            ("ERROR", false, Some(1)),
            ("FUTURE_STATE", false, None),
            ("", false, None),
        ] {
            let state = RunState {
                name: name.into(),
                result: None,
            };
            assert_eq!(running(&state), active, "{name}");
            assert_eq!(completion_code(&state), exit, "{name}");
            assert!(!fails_fast(&state), "{name}");
        }
    }
}
