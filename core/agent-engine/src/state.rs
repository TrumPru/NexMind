use serde::{Deserialize, Serialize};

use crate::AgentError;

/// Agent lifecycle states.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentState {
    Idle,
    Planning,
    Executing,
    WaitingApproval { approval_id: String },
    Completed,
    Failed { error: String },
    Suspended { reason: String },
}

impl AgentState {
    /// Name for display/storage.
    pub fn name(&self) -> &str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Planning => "planning",
            AgentState::Executing => "executing",
            AgentState::WaitingApproval { .. } => "waiting_approval",
            AgentState::Completed => "completed",
            AgentState::Failed { .. } => "failed",
            AgentState::Suspended { .. } => "suspended",
        }
    }

    /// Validate a state transition.
    pub fn can_transition_to(&self, next: &AgentState) -> bool {
        use AgentState::*;
        matches!(
            (self, next),
            // From Idle
            (Idle, Planning) | (Idle, Executing) |
            // From Planning
            (Planning, Executing) | (Planning, Failed { .. }) |
            // From Executing
            (Executing, Completed) |
            (Executing, Failed { .. }) |
            (Executing, Suspended { .. }) |
            (Executing, WaitingApproval { .. }) |
            (Executing, Planning) |
            // From WaitingApproval
            (WaitingApproval { .. }, Executing) |
            (WaitingApproval { .. }, Suspended { .. }) |
            (WaitingApproval { .. }, Failed { .. }) |
            // From Suspended
            (Suspended { .. }, Executing) |
            (Suspended { .. }, Idle) |
            (Suspended { .. }, Failed { .. }) |
            // Terminal states can reset to Idle
            (Completed, Idle) |
            (Failed { .. }, Idle)
        )
    }

    /// Attempt a state transition.
    pub fn transition(self, next: AgentState) -> Result<AgentState, AgentError> {
        if self.can_transition_to(&next) {
            Ok(next)
        } else {
            Err(AgentError::InvalidTransition(
                self.name().to_string(),
                next.name().to_string(),
            ))
        }
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(AgentState::Idle.can_transition_to(&AgentState::Executing));
        assert!(AgentState::Idle.can_transition_to(&AgentState::Planning));
        assert!(AgentState::Executing.can_transition_to(&AgentState::Completed));
        assert!(AgentState::Executing.can_transition_to(&AgentState::Failed {
            error: "oops".into()
        }));
        assert!(AgentState::Executing.can_transition_to(&AgentState::WaitingApproval {
            approval_id: "a1".into()
        }));
    }

    #[test]
    fn test_invalid_transitions() {
        assert!(!AgentState::Idle.can_transition_to(&AgentState::Completed));
        assert!(!AgentState::Completed.can_transition_to(&AgentState::Executing));
        assert!(!AgentState::Failed {
            error: "e".into()
        }
        .can_transition_to(&AgentState::Completed));
    }

    #[test]
    fn test_transition_ok() {
        let state = AgentState::Idle;
        let next = state.transition(AgentState::Executing).unwrap();
        assert_eq!(next, AgentState::Executing);
    }

    #[test]
    fn test_transition_err() {
        let state = AgentState::Idle;
        let result = state.transition(AgentState::Completed);
        assert!(result.is_err());
    }

    #[test]
    fn test_state_name() {
        assert_eq!(AgentState::Idle.name(), "idle");
        assert_eq!(AgentState::Executing.name(), "executing");
        assert_eq!(
            AgentState::WaitingApproval {
                approval_id: "x".into()
            }
            .name(),
            "waiting_approval"
        );
    }
}
