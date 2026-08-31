use std::borrow::Cow;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier, State};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty::{self, Pty};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

/// Bridge from the PTY thread to the GPUI main thread. Invoked on the PTY
/// reader thread, possibly while it holds the term lock — it must do nothing
/// but send on the channel.
#[derive(Clone)]
pub struct EventProxy(UnboundedSender<AlacEvent>);

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        self.0.unbounded_send(event).ok();
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

pub struct TerminalSession {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    master_fd: RawFd,
    child_pid: i32,
    join: Option<JoinHandle<(EventLoop<Pty, EventProxy>, State)>>,
}

impl TerminalSession {
    pub fn spawn(
        options: SessionOptions,
        size: TermSize,
    ) -> anyhow::Result<(Self, UnboundedReceiver<AlacEvent>)> {
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

        let event_loop = EventLoop::new(Arc::clone(&term), proxy, pty, false, false)?;
        let notifier = Notifier(event_loop.channel());
        let join = event_loop.spawn();

        Ok((Self { term, notifier, master_fd, child_pid, join: Some(join) }, rx))
    }

    pub fn write_input(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.notifier.notify(bytes);
    }

    pub fn resize(&self, size: TermSize) {
        self.term.lock().resize(size);
        let _ = self.notifier.0.send(Msg::Resize(size.window_size()));
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
        let _ = self.notifier.0.send(Msg::Shutdown);
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

