use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::protocol::SessionId;

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
pub const HEARTBEAT_MISS_LIMIT: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    Stopped,
    Preparing,
    Connected,
    Degraded,
    Stopping,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub state: HostState,
    pub session_id: Option<String>,
    pub missed_heartbeats: u32,
    pub reason: Option<String>,
}

#[derive(Debug)]
pub struct StateMachine {
    state: HostState,
    session_id: Option<SessionId>,
    last_heartbeat: Option<Instant>,
    missed_heartbeats: u32,
    reason: Option<String>,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            state: HostState::Stopped,
            session_id: None,
            last_heartbeat: None,
            missed_heartbeats: 0,
            reason: None,
        }
    }

    pub fn begin_start(&mut self, session_id: SessionId) -> Result<(), TransitionError> {
        self.require_state(&[HostState::Stopped, HostState::Error])?;
        self.state = HostState::Preparing;
        self.session_id = Some(session_id);
        self.last_heartbeat = None;
        self.missed_heartbeats = 0;
        self.reason = None;
        Ok(())
    }

    pub fn peer_started(
        &mut self,
        session_id: SessionId,
        now: Instant,
    ) -> Result<(), TransitionError> {
        self.require_session(session_id)?;
        self.require_state(&[HostState::Preparing, HostState::Degraded])?;
        self.state = HostState::Connected;
        self.last_heartbeat = Some(now);
        self.missed_heartbeats = 0;
        self.reason = None;
        Ok(())
    }

    pub fn heartbeat(
        &mut self,
        session_id: SessionId,
        now: Instant,
    ) -> Result<(), TransitionError> {
        self.require_session(session_id)?;
        self.require_state(&[HostState::Connected, HostState::Degraded])?;
        self.state = HostState::Connected;
        self.last_heartbeat = Some(now);
        self.missed_heartbeats = 0;
        self.reason = None;
        Ok(())
    }

    /// Advances heartbeat health without ever transitioning to `Stopped`.
    pub fn tick(&mut self, now: Instant) {
        if self.state != HostState::Connected {
            return;
        }
        let Some(last) = self.last_heartbeat else {
            return;
        };
        let intervals =
            now.saturating_duration_since(last).as_millis() / HEARTBEAT_INTERVAL.as_millis();
        self.missed_heartbeats = intervals.min(u32::MAX as u128) as u32;
        if self.missed_heartbeats >= HEARTBEAT_MISS_LIMIT {
            self.state = HostState::Degraded;
            self.reason = Some("three heartbeat intervals missed; VPN remains active".into());
        }
    }

    pub fn transport_lost(&mut self, reason: impl Into<String>) {
        if matches!(self.state, HostState::Preparing | HostState::Connected) {
            self.state = HostState::Degraded;
            self.reason = Some(reason.into());
        }
    }

    pub fn begin_stop(&mut self) -> Result<(), TransitionError> {
        self.require_state(&[
            HostState::Preparing,
            HostState::Connected,
            HostState::Degraded,
            HostState::Error,
        ])?;
        self.state = HostState::Stopping;
        self.reason = None;
        Ok(())
    }

    pub fn stopped(&mut self) -> Result<(), TransitionError> {
        self.require_state(&[HostState::Stopping])?;
        self.state = HostState::Stopped;
        self.session_id = None;
        self.last_heartbeat = None;
        self.missed_heartbeats = 0;
        self.reason = None;
        Ok(())
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.state = HostState::Error;
        self.last_heartbeat = None;
        self.reason = Some(reason.into());
    }

    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            state: self.state,
            session_id: self.session_id.map(|value| value.to_string()),
            missed_heartbeats: self.missed_heartbeats,
            reason: self.reason.clone(),
        }
    }

    pub fn state(&self) -> HostState {
        self.state
    }

    pub fn session_id(&self) -> Option<SessionId> {
        self.session_id
    }

    fn require_session(&self, actual: SessionId) -> Result<(), TransitionError> {
        match self.session_id {
            Some(expected) if expected == actual => Ok(()),
            expected => Err(TransitionError::SessionMismatch { expected, actual }),
        }
    }

    fn require_state(&self, expected: &[HostState]) -> Result<(), TransitionError> {
        if expected.contains(&self.state) {
            Ok(())
        } else {
            Err(TransitionError::InvalidState {
                actual: self.state,
                expected: expected.to_vec(),
            })
        }
    }
}

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("invalid transition from {actual:?}; expected one of {expected:?}")]
    InvalidState {
        actual: HostState,
        expected: Vec<HostState>,
    },
    #[error("stale or unknown session {actual}; expected {expected:?}")]
    SessionMismatch {
        expected: Option<SessionId>,
        actual: SessionId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_misses_degrade_but_never_stop() {
        let session = SessionId([1; 16]);
        let start = Instant::now();
        let mut machine = StateMachine::new();
        machine.begin_start(session).unwrap();
        machine.peer_started(session, start).unwrap();

        machine.tick(start + Duration::from_secs(2));
        assert_eq!(machine.state(), HostState::Connected);
        assert_eq!(machine.snapshot().missed_heartbeats, 2);

        machine.tick(start + Duration::from_secs(3));
        assert_eq!(machine.state(), HostState::Degraded);

        machine.tick(start + Duration::from_secs(300));
        assert_eq!(machine.state(), HostState::Degraded);
        assert_eq!(machine.session_id(), Some(session));
    }

    #[test]
    fn heartbeat_recovers_degraded_session() {
        let session = SessionId([2; 16]);
        let start = Instant::now();
        let mut machine = StateMachine::new();
        machine.begin_start(session).unwrap();
        machine.peer_started(session, start).unwrap();
        machine.tick(start + Duration::from_secs(3));
        machine
            .heartbeat(session, start + Duration::from_secs(4))
            .unwrap();
        assert_eq!(machine.state(), HostState::Connected);
        assert_eq!(machine.snapshot().missed_heartbeats, 0);
    }

    #[test]
    fn stale_session_is_rejected() {
        let mut machine = StateMachine::new();
        machine.begin_start(SessionId([3; 16])).unwrap();
        assert!(matches!(
            machine.peer_started(SessionId([4; 16]), Instant::now()),
            Err(TransitionError::SessionMismatch { .. })
        ));
    }
}
