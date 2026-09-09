//! Workspace startup commands: what a restored pane runs, what happens when
//! it exits, and the restart gate that keeps a crash-looping command from
//! spinning.
//!
//! Pure data, no GPUI. The pane owns a `StartupCommand` and drives it from
//! the shell's OSC 133 markers; `workspaces.rs` persists the same shape.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What to do when a startup command exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnExit {
    /// Drop back to the shell prompt. Almost always what you want.
    #[default]
    Shell,
    /// Close the pane — on a clean exit. A failure stays readable.
    Close,
    /// Run it again, with backoff; see `RestartGate`.
    Restart,
}

impl OnExit {
    pub fn label(self) -> &'static str {
        match self {
            OnExit::Shell => "shell",
            OnExit::Close => "close",
            OnExit::Restart => "restart",
        }
    }

    /// The next choice, for a `tab`-to-cycle control.
    pub fn next(self) -> OnExit {
        match self {
            OnExit::Shell => OnExit::Close,
            OnExit::Close => OnExit::Restart,
            OnExit::Restart => OnExit::Shell,
        }
    }
}

/// A pane's startup command as the app holds it: the same fields the saved
/// file carries, minus the directory (the pane already knows that).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupCommand {
    pub command: String,
    pub on_exit: OnExit,
}

/// How many exits inside `WINDOW` trip the breaker.
pub const MAX_EXITS_PER_WINDOW: usize = 5;
/// The rolling window the breaker counts exits over. A run that lasts at
/// least this long was healthy, and its exit starts a fresh count.
pub const WINDOW: Duration = Duration::from_secs(60);
/// First restart delay; doubles on every exit inside the window.
pub const BASE_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// Run it again after this long.
    After(Duration),
    /// Too many exits too fast: stop, and tell the user.
    Stop,
}

/// Backoff plus circuit breaker for `OnExit::Restart`. A command that fails
/// instantly would otherwise be re-run as fast as the event loop allows.
#[derive(Debug, Default)]
pub struct RestartGate {
    /// Exits inside the current window, oldest first.
    exits: Vec<Instant>,
    started: Option<Instant>,
}

impl RestartGate {
    pub fn record_start(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// The command exited. Decide whether — and how soon — to run it again.
    pub fn record_exit(&mut self, now: Instant) -> RestartDecision {
        // A long healthy run resets the count: this exit is a new episode,
        // not the fifth crash of a loop.
        if self.started.take().is_some_and(|t| now.duration_since(t) >= WINDOW) {
            self.exits.clear();
        }
        self.exits.retain(|t| now.duration_since(*t) < WINDOW);
        self.exits.push(now);
        if self.exits.len() >= MAX_EXITS_PER_WINDOW {
            return RestartDecision::Stop;
        }
        let exponent = (self.exits.len() - 1) as u32;
        RestartDecision::After(BASE_DELAY * 2u32.pow(exponent))
    }

    /// Exits counted so far in the current window.
    pub fn exits_in_window(&self) -> usize {
        self.exits.len()
    }
}

/// The banner shown when the breaker trips.
pub fn breaker_message(exits: usize) -> String {
    format!("startup command exited {exits} times in a minute — stopped restarting")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn backoff_doubles_then_breaker_trips() {
        let t0 = Instant::now();
        let mut gate = RestartGate::default();
        let mut now = t0;
        let mut delays = Vec::new();
        for _ in 0..4 {
            gate.record_start(now);
            now += Duration::from_millis(100);
            match gate.record_exit(now) {
                RestartDecision::After(d) => delays.push(d),
                RestartDecision::Stop => panic!("tripped too early"),
            }
            now += delays.last().copied().unwrap();
        }
        assert_eq!(delays, vec![secs(1), secs(2), secs(4), secs(8)]);
        gate.record_start(now);
        now += Duration::from_millis(100);
        assert_eq!(gate.record_exit(now), RestartDecision::Stop);
        assert_eq!(gate.exits_in_window(), MAX_EXITS_PER_WINDOW);
        assert!(breaker_message(gate.exits_in_window()).contains("5 times"));
    }

    #[test]
    fn a_healthy_run_resets_the_counter() {
        let mut now = Instant::now();
        let mut gate = RestartGate::default();
        for _ in 0..3 {
            gate.record_start(now);
            now += secs(1);
            assert!(matches!(gate.record_exit(now), RestartDecision::After(_)));
        }
        assert_eq!(gate.exits_in_window(), 3);
        // Ran for over a minute: whatever happened before no longer counts.
        gate.record_start(now);
        now += secs(61);
        assert_eq!(gate.record_exit(now), RestartDecision::After(secs(1)));
        assert_eq!(gate.exits_in_window(), 1);
    }

    #[test]
    fn old_exits_fall_out_of_the_window() {
        let mut now = Instant::now();
        let mut gate = RestartGate::default();
        for _ in 0..4 {
            gate.record_start(now);
            now += secs(2);
            gate.record_exit(now);
        }
        assert_eq!(gate.exits_in_window(), 4);
        // Nothing happens for a minute; the next quick exit is the only one
        // in the window, so the delay is back to the base.
        now += secs(61);
        gate.record_start(now);
        now += secs(1);
        assert_eq!(gate.record_exit(now), RestartDecision::After(secs(1)));
    }

    #[test]
    fn on_exit_cycles_and_serialises_snake_case() {
        assert_eq!(OnExit::Shell.next(), OnExit::Close);
        assert_eq!(OnExit::Restart.next(), OnExit::Shell);
        assert_eq!(serde_json::to_string(&OnExit::Restart).unwrap(), "\"restart\"");
        assert_eq!(serde_json::from_str::<OnExit>("\"close\"").unwrap(), OnExit::Close);
        assert_eq!(OnExit::default(), OnExit::Shell);
    }
}
