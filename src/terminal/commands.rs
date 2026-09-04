//! The per-pane command log: what ran, where, how long it took, and how it
//! ended — built from the OSC 133 markers the shell integration emits.
//!
//! Pure data, no GPUI, so the state machine is unit-testable. The pane
//! feeds it markers; the app reads it for notifications, the tab bar, the
//! status bar, and history search.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::osc::{Marker, MarkerKind};

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub id: u64,
    /// The typed command line, when the shell sent it or the grid gave it up.
    pub text: Option<String>,
    pub cwd: Option<PathBuf>,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub exit: Option<i32>,
    /// Absolute grid row of the prompt this command was typed at.
    pub prompt_row: Option<usize>,
    /// Absolute grid rows of the command's output: from the line after the
    /// command line to the line the next prompt starts on. Exact because
    /// the markers are sampled between parser slices.
    pub output_rows: Option<Range<usize>>,
}

impl Command {
    pub fn duration(&self) -> Duration {
        self.finished.unwrap_or_else(Instant::now).duration_since(self.started)
    }

    pub fn failed(&self) -> bool {
        self.exit.is_some_and(|e| e != 0)
    }

    /// First line of the command, trimmed, for one-line UI.
    pub fn label(&self) -> &str {
        self.text.as_deref().and_then(|t| t.lines().next()).unwrap_or("command").trim()
    }
}

/// What a marker changed, for the pane to react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogEvent {
    Started(u64),
    Finished(u64),
}

#[derive(Debug)]
pub struct CommandLog {
    entries: VecDeque<Command>,
    cap: usize,
    next_id: u64,
    running: Option<u64>,
    /// Row of the most recent prompt start, attached to the next command.
    last_prompt_row: Option<usize>,
    /// Latest cwd the shell reported; commands inherit it.
    pub cwd: Option<PathBuf>,
    /// Whether any marker has arrived — the signal that integration is live
    /// and the Enter-key heuristics can stand down.
    pub markers_seen: bool,
}

impl CommandLog {
    pub fn new(cap: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            cap: cap.max(1),
            next_id: 1,
            running: None,
            last_prompt_row: None,
            cwd: None,
            markers_seen: false,
        }
    }

    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Command> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: u64) -> Option<&Command> {
        self.entries.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Command> {
        self.entries.iter_mut().find(|c| c.id == id)
    }

    /// The command currently executing, if any.
    pub fn running(&self) -> Option<&Command> {
        self.running.and_then(|id| self.get(id))
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// The most recent command that has finished.
    pub fn last_finished(&self) -> Option<&Command> {
        self.entries.iter().rev().find(|c| c.finished.is_some())
    }

    /// The grid was cleared: every stored row now points at nothing. Keep
    /// the entries (history search still wants them) but drop the rows.
    pub fn forget_rows(&mut self) {
        for cmd in &mut self.entries {
            cmd.prompt_row = None;
            cmd.output_rows = None;
        }
        self.last_prompt_row = None;
    }

    /// Feed one marker. Returns what changed so the pane can emit events.
    pub fn on_marker(&mut self, marker: &Marker, now: Instant) -> Option<LogEvent> {
        self.markers_seen = true;
        if marker.alt_screen {
            // A full-screen program is drawing; rows mean nothing here.
            return None;
        }
        match &marker.kind {
            MarkerKind::PromptStart => {
                self.last_prompt_row = Some(marker.row);
                // A prompt appearing while we think a command is running
                // means its D marker never came (killed shell, ctrl-c mid
                // prompt hook). Close it out without an exit code.
                if let Some(id) = self.running.take()
                    && let Some(cmd) = self.get_mut(id)
                {
                    cmd.finished = Some(now);
                    cmd.output_rows = cmd.output_rows.take().map(|r| r.start..marker.row.max(r.start));
                }
                None
            }
            MarkerKind::InputStart => None,
            MarkerKind::CommandStart { cmdline } => {
                let id = self.next_id;
                self.next_id += 1;
                // Enter has already moved the cursor to a fresh line when the
                // shell's preexec fires, so output begins on the marker's row.
                let output_start = marker.row;
                self.entries.push_back(Command {
                    id,
                    text: cmdline.clone(),
                    cwd: self.cwd.clone(),
                    started: now,
                    finished: None,
                    exit: None,
                    prompt_row: self.last_prompt_row.take(),
                    output_rows: Some(output_start..output_start),
                });
                while self.entries.len() > self.cap {
                    self.entries.pop_front();
                }
                self.running = Some(id);
                Some(LogEvent::Started(id))
            }
            MarkerKind::CommandEnd { exit } => {
                let id = self.running.take()?;
                let cmd = self.get_mut(id)?;
                cmd.finished = Some(now);
                cmd.exit = *exit;
                if let Some(r) = &cmd.output_rows {
                    cmd.output_rows = Some(r.start..marker.row.max(r.start));
                }
                Some(LogEvent::Finished(id))
            }
            MarkerKind::Cwd(path) => {
                self.cwd = Some(path.clone());
                None
            }
            MarkerKind::Notify { .. } => None,
        }
    }

    /// Fill in a command's text after the fact, for the grid-reading
    /// fallback when the shell didn't send it.
    pub fn set_text(&mut self, id: u64, text: String) {
        if let Some(cmd) = self.get_mut(id)
            && cmd.text.is_none()
            && !text.trim().is_empty()
        {
            cmd.text = Some(text);
        }
    }
}

/// `2m14s`, `4.2s`, `380ms` — for status bars and notifications.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else if secs < 3600.0 {
        format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
    } else {
        format!("{}h{:02}m", d.as_secs() / 3600, (d.as_secs() % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(kind: MarkerKind, row: usize) -> Marker {
        Marker { kind, row, column: 0, alt_screen: false }
    }

    /// Prompt at `rows.0`; the command line is typed there, so C fires on
    /// the row below; D lands at `rows.1`.
    fn run(log: &mut CommandLog, text: &str, exit: i32, rows: (usize, usize)) -> u64 {
        let t = Instant::now();
        log.on_marker(&m(MarkerKind::PromptStart, rows.0), t);
        log.on_marker(&m(MarkerKind::InputStart, rows.0), t);
        let started = log
            .on_marker(&m(MarkerKind::CommandStart { cmdline: Some(text.into()) }, rows.0 + 1), t)
            .unwrap();
        let LogEvent::Started(id) = started else { panic!() };
        assert!(log.is_running());
        assert_eq!(log.on_marker(&m(MarkerKind::CommandEnd { exit: Some(exit) }, rows.1), t), Some(LogEvent::Finished(id)));
        id
    }

    #[test]
    fn a_full_cycle_records_text_exit_and_rows() {
        let mut log = CommandLog::new(10);
        assert!(!log.markers_seen);
        log.on_marker(&m(MarkerKind::Cwd(PathBuf::from("/repo")), 0), Instant::now());
        let id = run(&mut log, "cargo build", 1, (5, 40));
        let cmd = log.get(id).unwrap();
        assert_eq!(cmd.text.as_deref(), Some("cargo build"));
        assert_eq!(cmd.cwd, Some(PathBuf::from("/repo")));
        assert_eq!(cmd.exit, Some(1));
        assert!(cmd.failed());
        assert_eq!(cmd.prompt_row, Some(5));
        assert_eq!(cmd.output_rows, Some(6..40));
        assert!(!log.is_running());
        assert!(log.markers_seen);
        assert_eq!(log.last_finished().map(|c| c.id), Some(id));
        // Clearing the screen keeps the entry but nothing points at rows.
        log.forget_rows();
        assert_eq!(log.get(id).unwrap().output_rows, None);
        assert_eq!(log.get(id).unwrap().prompt_row, None);
        assert_eq!(log.entries().count(), 1);
    }

    #[test]
    fn a_prompt_without_a_command_end_closes_the_running_command() {
        let mut log = CommandLog::new(10);
        let t = Instant::now();
        log.on_marker(&m(MarkerKind::PromptStart, 1), t);
        log.on_marker(&m(MarkerKind::CommandStart { cmdline: Some("vim".into()) }, 2), t);
        assert!(log.is_running());
        // ctrl-c'd before D: the next prompt arrives directly.
        log.on_marker(&m(MarkerKind::PromptStart, 9), t);
        assert!(!log.is_running());
        let cmd = log.entries().last().unwrap();
        assert!(cmd.finished.is_some());
        assert_eq!(cmd.exit, None);
        assert_eq!(cmd.output_rows, Some(2..9));
    }

    #[test]
    fn stray_command_end_and_alt_screen_markers_are_ignored() {
        let mut log = CommandLog::new(10);
        let t = Instant::now();
        assert_eq!(log.on_marker(&m(MarkerKind::CommandEnd { exit: Some(0) }, 3), t), None);
        let mut alt = m(MarkerKind::CommandStart { cmdline: None }, 3);
        alt.alt_screen = true;
        assert_eq!(log.on_marker(&alt, t), None);
        assert!(log.is_empty());
    }

    #[test]
    fn ring_buffer_caps_entries() {
        let mut log = CommandLog::new(3);
        for i in 0..5 {
            run(&mut log, &format!("cmd{i}"), 0, (i, i + 1));
        }
        assert_eq!(log.entries().map(|c| c.label()).collect::<Vec<_>>(), vec!["cmd2", "cmd3", "cmd4"]);
    }

    #[test]
    fn text_fallback_only_fills_a_gap() {
        let mut log = CommandLog::new(3);
        let t = Instant::now();
        log.on_marker(&m(MarkerKind::CommandStart { cmdline: None }, 1), t);
        let id = log.entries().last().unwrap().id;
        log.set_text(id, "   ".into());
        assert_eq!(log.get(id).unwrap().text, None);
        log.set_text(id, "ls -la".into());
        log.set_text(id, "other".into());
        assert_eq!(log.get(id).unwrap().text.as_deref(), Some("ls -la"));
        assert_eq!(log.get(id).unwrap().label(), "ls -la");
    }

    #[test]
    fn durations_format_for_humans() {
        assert_eq!(format_duration(Duration::from_millis(380)), "380ms");
        assert_eq!(format_duration(Duration::from_millis(4200)), "4.2s");
        assert_eq!(format_duration(Duration::from_secs(134)), "2m14s");
        assert_eq!(format_duration(Duration::from_secs(3720)), "1h02m");
    }
}
