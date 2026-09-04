//! The PTY I/O thread: reads the shell's output, feeds the VT parser, writes
//! input back, and watches for the child exiting.
//!
//! This replaces `alacritty_terminal::event_loop::EventLoop` so the byte
//! stream can be split at the OSC markers Oxide's shell integration emits.
//! Alacritty's parser drops unknown OSCs before anyone can see them; here
//! each recognised marker ends a parser slice, and the cursor is sampled in
//! between, so a marker's grid row is exact rather than "somewhere in the
//! chunk". The write channel, resize, shutdown, and child-exit handling keep
//! the same semantics as alacritty's loop — `TerminalSession::drop`'s
//! SIGHUP → join → SIGKILL teardown depends on them.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::Instant;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, OnResize, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use alacritty_terminal::vte::ansi;
use polling::{Event as PollingEvent, Events, PollMode, Poller};

use super::osc::{Marker, OscScanner};

/// The poll keys alacritty's `Pty::register` uses (private constants in
/// `tty::unix`): the master fd, and the SIGCHLD pipe.
const PTY_READ_WRITE_TOKEN: usize = 0;
const PTY_CHILD_EVENT_TOKEN: usize = 1;

/// Max bytes to read from the PTY before forcing terminal synchronisation.
const READ_BUFFER_SIZE: usize = 0x10_0000;
/// Max bytes to parse while holding the terminal lock in one go.
const MAX_LOCKED_READ: usize = u16::MAX as usize;

/// Messages the loop accepts.
#[derive(Debug)]
pub enum Msg {
    /// Bytes to write to the PTY.
    Input(Cow<'static, [u8]>),
    /// Stop the loop; the PTY (and with it the child) is dropped afterwards.
    Shutdown,
    /// Propagate a new window size to the PTY.
    Resize(WindowSize),
}

/// What the loop reports back to the main thread, multiplexed on one
/// channel so ordering between output and markers is preserved.
#[derive(Debug)]
pub enum SessionEvent {
    Term(AlacEvent),
    Marker(Marker),
}

/// Handle for sending messages to the loop; cloneable, wakes the poller.
#[derive(Clone)]
pub struct LoopSender {
    tx: Sender<Msg>,
    poller: Arc<Poller>,
}

impl LoopSender {
    pub fn send(&self, msg: Msg) -> io::Result<()> {
        self.tx.send(msg).map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "event loop gone"))?;
        self.poller.notify()
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        // A zero-length write hangs the loop's writer state machine.
        if bytes.is_empty() {
            return;
        }
        let _ = self.send(Msg::Input(bytes));
    }
}

pub struct EventLoop<L: EventListener> {
    poll: Arc<Poller>,
    pty: tty::Pty,
    rx: PeekableReceiver<Msg>,
    tx: Sender<Msg>,
    term: Arc<FairMutex<Term<L>>>,
    listener: L,
    markers: Box<dyn Fn(Marker) + Send>,
    scanner: OscScanner,
}

impl<L: EventListener + Send + 'static> EventLoop<L> {
    pub fn new(
        term: Arc<FairMutex<Term<L>>>,
        listener: L,
        pty: tty::Pty,
        markers: impl Fn(Marker) + Send + 'static,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            poll: Arc::new(Poller::new()?),
            pty,
            rx: PeekableReceiver::new(rx),
            tx,
            term,
            listener,
            markers: Box::new(markers),
            scanner: OscScanner::new(),
        })
    }

    pub fn sender(&self) -> LoopSender {
        LoopSender { tx: self.tx.clone(), poller: self.poll.clone() }
    }

    /// Returns false when a shutdown was requested.
    fn drain_channel(&mut self, state: &mut State) -> bool {
        while let Some(msg) = self.rx.recv() {
            match msg {
                Msg::Input(input) => state.write_list.push_back(input),
                Msg::Resize(size) => self.pty.on_resize(size),
                Msg::Shutdown => return false,
            }
        }
        true
    }

    fn pty_read(&mut self, state: &mut State, buf: &mut [u8]) -> io::Result<()> {
        let mut unprocessed = 0;
        let mut processed = 0;

        // Reserve the next terminal lock for PTY reading.
        let _lease = Some(self.term.lease());
        let mut terminal = None;

        loop {
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                Ok(0) if unprocessed == 0 => break,
                Ok(got) => unprocessed += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        if unprocessed == 0 {
                            break;
                        }
                    }
                    _ => return Err(err),
                },
            }

            let terminal = match &mut terminal {
                Some(terminal) => terminal,
                None => terminal.insert(match self.term.try_lock_unfair() {
                    None if unprocessed >= READ_BUFFER_SIZE => self.term.lock_unfair(),
                    None => continue,
                    Some(terminal) => terminal,
                }),
            };

            // Parse in slices that end at each marker, sampling the cursor
            // between them. The marker bytes themselves are an OSC the
            // parser ignores, so they move nothing.
            let chunk = &buf[..unprocessed];
            let mut start = 0;
            for (end, kind) in self.scanner.scan(chunk) {
                state.parser.advance(&mut **terminal, &chunk[start..end]);
                start = end;
                let grid = terminal.grid();
                let cursor = grid.cursor.point;
                (self.markers)(Marker {
                    kind,
                    row: grid.history_size() + cursor.line.0.max(0) as usize,
                    column: cursor.column.0,
                    alt_screen: terminal.mode().contains(TermMode::ALT_SCREEN),
                });
            }
            state.parser.advance(&mut **terminal, &chunk[start..]);

            processed += unprocessed;
            unprocessed = 0;

            if processed >= MAX_LOCKED_READ {
                break;
            }
        }

        // Queue a redraw unless everything parsed was inside a synchronised
        // update (DCS 2026), which repaints when it ends.
        if state.parser.sync_bytes_count() < processed && processed > 0 {
            self.listener.send_event(AlacEvent::Wakeup);
        }
        Ok(())
    }

    fn pty_write(&mut self, state: &mut State) -> io::Result<()> {
        state.ensure_next();
        'write_many: while let Some(mut current) = state.take_current() {
            loop {
                match self.pty.writer().write(current.remaining()) {
                    Ok(0) => {
                        state.writing = Some(current);
                        break 'write_many;
                    }
                    Ok(n) => {
                        current.written += n;
                        if current.finished() {
                            state.goto_next();
                            break;
                        }
                    }
                    Err(err) => {
                        state.writing = Some(current);
                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                            _ => return Err(err),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Run the loop on its own thread. The returned handle yields once the
    /// loop has exited and the PTY has been dropped (which waits on the
    /// child), so callers join it off the main thread.
    pub fn spawn(mut self) -> JoinHandle<()> {
        alacritty_terminal::thread::spawn_named("PTY reader", move || {
            let mut state = State::default();
            let mut buf = vec![0u8; READ_BUFFER_SIZE];

            let poll_opts = PollMode::Level;
            let mut interest = PollingEvent::readable(0);
            if let Err(err) = unsafe { self.pty.register(&self.poll, interest, poll_opts) } {
                eprintln!("oxide: pty registration failed: {err}");
                return;
            }

            let mut events = Events::with_capacity(NonZeroUsize::new(1024).unwrap());

            'event_loop: loop {
                // Wake for a synchronised-update timeout even with no I/O.
                let timeout = state
                    .parser
                    .sync_timeout()
                    .sync_timeout()
                    .map(|t| t.saturating_duration_since(Instant::now()));

                events.clear();
                if let Err(err) = self.poll.wait(&mut events, timeout) {
                    match err.kind() {
                        ErrorKind::Interrupted => continue,
                        _ => {
                            eprintln!("oxide: pty poll failed: {err}");
                            break 'event_loop;
                        }
                    }
                }

                if events.is_empty() && self.rx.peek().is_none() {
                    state.parser.stop_sync(&mut *self.term.lock());
                    self.listener.send_event(AlacEvent::Wakeup);
                    continue;
                }

                if !self.drain_channel(&mut state) {
                    break;
                }

                for event in events.iter() {
                    match event.key {
                        PTY_CHILD_EVENT_TOKEN => {
                            if let Some(ChildEvent::Exited(status)) = self.pty.next_child_event() {
                                if let Some(status) = status {
                                    self.listener.send_event(AlacEvent::ChildExit(status));
                                }
                                self.term.lock().exit();
                                self.listener.send_event(AlacEvent::Wakeup);
                                break 'event_loop;
                            }
                        }
                        PTY_READ_WRITE_TOKEN => {
                            if event.is_interrupt() {
                                continue;
                            }
                            if event.readable
                                && let Err(err) = self.pty_read(&mut state, &mut buf)
                            {
                                eprintln!("oxide: pty read failed: {err}");
                                break 'event_loop;
                            }
                            if event.writable
                                && let Err(err) = self.pty_write(&mut state)
                            {
                                eprintln!("oxide: pty write failed: {err}");
                                break 'event_loop;
                            }
                        }
                        _ => {}
                    }
                }

                let needs_write = state.needs_write();
                if needs_write != interest.writable {
                    interest.writable = needs_write;
                    if let Err(err) = self.pty.reregister(&self.poll, interest, poll_opts) {
                        eprintln!("oxide: pty reregistration failed: {err}");
                        break 'event_loop;
                    }
                }
            }

            let _ = self.pty.deregister(&self.poll);
            // `self` (and the Pty, which waits on the child) drops here, on
            // this thread.
        })
    }
}

struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

impl Writing {
    fn remaining(&self) -> &[u8] {
        &self.source[self.written..]
    }

    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

#[derive(Default)]
struct State {
    write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
    parser: ansi::Processor,
}

impl State {
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(|source| Writing { source, written: 0 });
    }

    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }
}

struct PeekableReceiver<T> {
    rx: Receiver<T>,
    peeked: Option<T>,
}

impl<T> PeekableReceiver<T> {
    fn new(rx: Receiver<T>) -> Self {
        Self { rx, peeked: None }
    }

    fn peek(&mut self) -> Option<&T> {
        if self.peeked.is_none() {
            self.peeked = self.rx.try_recv().ok();
        }
        self.peeked.as_ref()
    }

    fn recv(&mut self) -> Option<T> {
        if self.peeked.is_some() {
            return self.peeked.take();
        }
        match self.rx.try_recv() {
            // Every sender dropped: treat like a shutdown rather than spin.
            Err(TryRecvError::Disconnected) => Some(Self::disconnected()),
            res => res.ok(),
        }
    }

    fn disconnected() -> T
    where
        T: Sized,
    {
        unreachable!("the loop keeps its own sender alive")
    }
}
