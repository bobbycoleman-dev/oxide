use std::borrow::Cow;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use super::event_loop::{EventLoop, LoopSender, Msg};
pub use super::event_loop::SessionEvent;

/// Bridge from the PTY thread to the GPUI main thread. Invoked on the PTY
/// reader thread, possibly while it holds the term lock — it must do nothing
/// but send on the channel.
#[derive(Clone)]
pub struct EventProxy(UnboundedSender<SessionEvent>);

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        self.0.unbounded_send(SessionEvent::Term(event)).ok();
    }
}

/// Grid geometry: cell counts plus the measured cell box in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl TermSize {
    pub fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.screen_lines as u16,
            num_cols: self.columns as u16,
            cell_width: self.cell_width as u16,
            cell_height: self.cell_height as u16,
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct SessionOptions {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub scrollback: usize,
    pub env: HashMap<String, String>,
}

/// Resolve the shell: config, then $SHELL, then /bin/zsh.
pub fn resolve_shell(configured: Option<&str>) -> String {
    configured
        .map(str::to_string)
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/zsh".to_string())
}

/// The program name of a shell path, for family checks. A version suffix is
/// kept (`bash-5.2` stays whole) so callers match on a prefix.
pub fn shell_name(program: &str) -> &str {
    std::path::Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
}

/// Whether this shell understands Bourne syntax, so Oxide can hand it a `[ -n
/// … ]` snippet directly. fish, the csh family, nushell, xonsh and friends do
/// not, and need such a snippet delegated to `/bin/sh`.
pub fn is_posix_shell(program: &str) -> bool {
    let name = shell_name(program);
    ["sh", "bash", "zsh", "dash", "ksh", "mksh", "pdksh", "ash", "yash"]
        .iter()
        .any(|family| name == *family || name.starts_with(&format!("{family}-")))
}

pub struct TerminalSession {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    sender: LoopSender,
    master_fd: RawFd,
    child_pid: i32,
    join: Option<JoinHandle<()>>,
}

impl TerminalSession {
    pub fn spawn(
        options: SessionOptions,
        size: TermSize,
    ) -> anyhow::Result<(Self, UnboundedReceiver<SessionEvent>)> {
        let (tx, rx) = unbounded();
        let proxy = EventProxy(tx);

        tty::setup_env();
        let mut env = options.env;
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("OXIDE_VERSION".into(), env!("CARGO_PKG_VERSION").into());
        // GUI-launched apps get no locale; a C-locale shell breaks multibyte
        // input and prompt glyphs. Mirror Terminal.app: set one if absent.
        if std::env::var("LANG").is_err() && !env.contains_key("LANG") {
            env.insert("LANG".into(), "en_US.UTF-8".into());
        }

        let pty_options = tty::Options {
            shell: Some(tty::Shell::new(options.program, options.args)),
            working_directory: options.working_directory,
            drain_on_exit: false,
            env,
        };
        let pty = tty::new(&pty_options, size.window_size(), 0)?;
        let master_fd = pty.file().as_raw_fd();
        let child_pid = pty.child().id() as i32;

        let term_config = TermConfig {
            scrolling_history: options.scrollback,
            ..TermConfig::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(term_config, &size, proxy.clone())));

        let marker_tx = proxy.0.clone();
        let event_loop = EventLoop::new(Arc::clone(&term), proxy, pty, move |marker| {
            marker_tx.unbounded_send(SessionEvent::Marker(marker)).ok();
        })?;
        let sender = event_loop.sender();
        let join = event_loop.spawn();

        Ok((Self { term, sender, master_fd, child_pid, join: Some(join) }, rx))
    }

    pub fn write_input(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.sender.write(bytes);
    }

    pub fn resize(&self, size: TermSize) {
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    /// The cwd of the foreground process group on the PTY, via tcgetpgrp +
    /// proc_pidinfo. Works with no shell cooperation at all.
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        unsafe {
            let pgrp = libc::tcgetpgrp(self.master_fd);
            if pgrp <= 0 {
                return None;
            }
            let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
            let written = libc::proc_pidinfo(
                pgrp,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            );
            if written <= 0 {
                return None;
            }
            let path = std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast());
            let path = path.to_str().ok()?;
            if path.is_empty() { None } else { Some(PathBuf::from(path)) }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Teardown must never block the main thread: alacritty's Pty::drop
        // calls wait() on the child, and an interactive shell that ignores
        // SIGHUP would hang quit indefinitely (this presented as "the app
        // won't close"). Signal the shell, then join + drop the event loop
        // (and with it the Pty) on a detached thread, escalating to SIGKILL
        // after a grace period.
        let _ = self.sender.send(Msg::Shutdown);
        let child_pid = self.child_pid;
        unsafe {
            libc::kill(child_pid, libc::SIGHUP);
        }
        if let Some(join) = self.join.take() {
            std::thread::spawn(move || {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    unsafe {
                        libc::kill(child_pid, libc::SIGKILL);
                    }
                });
                let _ = join.join();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn visible_text(session: &TerminalSession) -> String {
        let term = session.term.lock();
        let content = term.renderable_content();
        let mut text = String::new();
        let mut last_line = 0;
        for indexed in content.display_iter {
            if indexed.point.line.0 != last_line {
                text.push('\n');
                last_line = indexed.point.line.0;
            }
            text.push(indexed.cell.c);
        }
        text
    }

    /// The event loop splits parser slices at OSC 133 markers: a real zsh
    /// with the integration installed must deliver `CommandEnd { exit: 1 }`
    /// for `false`, with exact rows, and a non-integrated shell must deliver
    /// no markers at all.
    #[test]
    fn integrated_shell_delivers_markers_with_rows() {
        use super::super::osc::MarkerKind;
        if !std::path::Path::new("/bin/zsh").exists() {
            return;
        }
        let config = crate::config::Config::default();
        let integration = crate::prompt::integration::setup(&config, "/bin/zsh");
        if !integration.env.contains_key("ZDOTDIR") {
            return; // cache dir unavailable in this environment
        }
        let size = TermSize { columns: 100, screen_lines: 24, cell_width: 8.0, cell_height: 16.0 };
        let options = SessionOptions {
            program: "/bin/zsh".into(),
            args: vec![],
            working_directory: Some(std::env::temp_dir()),
            scrollback: 100,
            env: integration.env,
        };
        let (session, mut rx) = TerminalSession::spawn(options, size).expect("spawn zsh");
        session.write_input(b"false\r".to_vec());

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut markers = Vec::new();
        while Instant::now() < deadline {
            while let Ok(event) = rx.try_recv() {
                if let SessionEvent::Marker(m) = event {
                    markers.push(m);
                }
            }
            let started = markers.iter().position(|m| matches!(m.kind, MarkerKind::CommandStart { .. }));
            if started.is_some_and(|ix| markers[ix..].iter().any(|m| matches!(m.kind, MarkerKind::CommandEnd { .. }))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let kinds: Vec<_> = markers.iter().map(|m| &m.kind).collect();
        assert!(kinds.contains(&&MarkerKind::PromptStart), "{kinds:?}");
        assert!(kinds.contains(&&MarkerKind::InputStart), "{kinds:?}");
        // The startup prompt emits its own D;0 before anything runs; the one
        // that matters follows the C marker for `false`.
        let start_ix = markers.iter().position(|m| matches!(m.kind, MarkerKind::CommandStart { .. })).expect("C marker");
        let start = &markers[start_ix];
        assert_eq!(start.kind, MarkerKind::CommandStart { cmdline: Some("false".into()) });
        let end = markers[start_ix..].iter().find(|m| matches!(m.kind, MarkerKind::CommandEnd { .. })).expect("D marker");
        assert_eq!(end.kind, MarkerKind::CommandEnd { exit: Some(1) });
        // Enter moved the cursor to a fresh line before C fired, and `false`
        // prints nothing, so D lands on that same row: exact rows, not
        // "somewhere in the chunk".
        assert_eq!(end.row, start.row, "start {start:?} end {end:?}");
        assert!(markers.iter().any(|m| matches!(m.kind, MarkerKind::Cwd(_))), "OSC 7 should report the cwd");
        assert!(markers.iter().all(|m| !m.alt_screen));
    }

    #[test]
    fn plain_sh_delivers_no_markers() {
        let size = TermSize { columns: 80, screen_lines: 24, cell_width: 8.0, cell_height: 16.0 };
        let options = SessionOptions {
            program: "/bin/sh".into(),
            args: vec![],
            working_directory: None,
            scrollback: 100,
            env: HashMap::new(),
        };
        let (session, mut rx) = TerminalSession::spawn(options, size).expect("spawn sh");
        session.write_input(b"echo marker_free\r".to_vec());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !visible_text(&session).contains("marker_free") {
            std::thread::sleep(Duration::from_millis(50));
        }
        while let Ok(event) = rx.try_recv() {
            assert!(!matches!(event, SessionEvent::Marker(_)), "unexpected {event:?}");
        }
    }

    /// M3: a real shell runs, output lands in the grid, and input round-trips.
    #[test]
    fn shell_round_trip() {
        let size = TermSize { columns: 80, screen_lines: 24, cell_width: 8.0, cell_height: 16.0 };
        let options = SessionOptions {
            program: "/bin/sh".into(),
            args: vec![],
            working_directory: None,
            scrollback: 100,
            env: HashMap::new(),
        };
        let (session, _rx) = TerminalSession::spawn(options, size).expect("spawn pty");
        session.write_input(b"echo oxide_roundtrip_$((20+22))\r".to_vec());

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let text = visible_text(&session);
            if text.contains("oxide_roundtrip_42") {
                break;
            }
            if Instant::now() > deadline {
                panic!("shell output never arrived; grid:\n{text}");
            }
        }

        let cwd = session.foreground_cwd();
        assert!(cwd.is_some(), "foreground cwd lookup failed");
    }
}

