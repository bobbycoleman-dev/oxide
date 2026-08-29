pub mod colors;
pub mod element;
pub mod keys;
pub mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::Event as AlacEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::vte::ansi::Rgb;
use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, ScrollDelta, ScrollWheelEvent, ShapedLine,
    Styled, Window, div,
};

use crate::config::schema::BellMode;
use crate::config::{Config, Theme, theme::hsla_to_rgb8};
use crate::keymap::actions::{ClearScrollback, Copy, Paste, PromptDown, PromptUp, Search, SelectAll};
use element::TerminalElement;
use session::{SessionOptions, TermSize, TerminalSession, resolve_shell};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    fn NSBeep();
}

pub enum TerminalEvent {
    TitleChanged,
    CwdChanged(PathBuf),
}

/// Geometry of the last painted frame, for mouse -> grid math.
#[derive(Clone, Copy)]
pub struct LastLayout {
    pub bounds: Bounds<Pixels>,
    pub cell_width: f32,
    pub cell_height: f32,
    pub display_offset: usize,
}

pub struct TerminalPane {
    pub session: Option<TerminalSession>,
    pub size: TermSize,
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub child_exited: Option<Option<i32>>,
    focus_handle: FocusHandle,
    config: Rc<Config>,
    theme: Rc<Theme>,
    pub font_delta: f32,
    initial_dir: PathBuf,

    repaint_scheduled: bool,
    pub shape_cache: HashMap<u64, ShapedLine>,
    pub prev_shape_cache: HashMap<u64, ShapedLine>,
    pub last_layout: Option<LastLayout>,
    last_cwd_poll: Instant,
    cwd_poll_scheduled: bool,

    pub blink_show: bool,
    last_input: Instant,
    selecting: bool,
    scroll_accum: f32,
    bell_until: Option<Instant>,
    search: Option<SearchState>,
    /// Prompt rows in absolute line coordinates (history_size + line). Stable
    /// until the scrollback cap rotates lines out; see prompt_up/down.
    prompt_marks: Vec<usize>,
}

struct SearchState {
    query: String,
    current: Option<Match>,
}

/// Escape a literal string for use as a regex pattern.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl EventEmitter<TerminalEvent> for TerminalPane {}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalPane {
    pub fn new(
        config: Rc<Config>,
        theme: Rc<Theme>,
        working_dir: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            session: None,
            // Plausible bring-up size; real measurement happens on first layout.
            size: TermSize { columns: 80, screen_lines: 24, cell_width: 8.0, cell_height: 17.0 },
            title: "oxide".into(),
            cwd: Some(working_dir.clone()),
            child_exited: None,
            focus_handle: cx.focus_handle(),
            config,
            theme,
            font_delta: 0.0,
            initial_dir: working_dir,
            repaint_scheduled: false,
            shape_cache: HashMap::new(),
            prev_shape_cache: HashMap::new(),
            last_layout: None,
            last_cwd_poll: Instant::now(),
            cwd_poll_scheduled: false,
            blink_show: true,
            last_input: Instant::now(),
            selecting: false,
            scroll_accum: 0.0,
            bell_until: None,
            search: None,
            prompt_marks: Vec::new(),
        };
        this.spawn_session(cx);
        this.spawn_blink_task(cx);
        this
    }

    pub fn set_config(&mut self, config: Rc<Config>, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.config = config;
        self.theme = theme;
        self.shape_cache.clear();
        self.prev_shape_cache.clear();
        cx.notify();
    }

    pub fn config(&self) -> &Rc<Config> {
        &self.config
    }

    pub fn theme(&self) -> &Rc<Theme> {
        &self.theme
    }

    pub fn adjust_font(&mut self, delta: Option<f32>, cx: &mut Context<Self>) {
        match delta {
            Some(d) => self.font_delta = (self.font_delta + d).clamp(-8.0, 24.0),
            None => self.font_delta = 0.0,
        }
        self.shape_cache.clear();
        self.prev_shape_cache.clear();
        cx.notify();
    }

    fn spawn_session(&mut self, cx: &mut Context<Self>) {
        let shell = self.config.shell.clone();
        let program = resolve_shell(shell.program.as_deref());
        // Regenerated per session so a config reload applies to restarts.
        let integration = crate::prompt::integration::setup(&self.config, &program);
        let cwd = self.cwd.clone().unwrap_or_else(|| self.initial_dir.clone());
        let options = SessionOptions {
            program,
            args: integration.args_override.unwrap_or(shell.args),
            working_directory: Some(cwd),
            scrollback: shell.scrollback,
            env: integration.env,
        };
        match TerminalSession::spawn(options, self.size) {
            Ok((session, mut rx)) => {
                self.session = Some(session);
                self.child_exited = None;
                cx.spawn(async move |this, cx| {
                    while let Some(event) = rx.next().await {
                        let mut batch = vec![event];
                        while batch.len() < 1024 {
                            match rx.try_recv() {
                                Ok(ev) => batch.push(ev),
                                Err(_) => break,
                            }
                        }
                        let alive = this
                            .update(cx, |pane, cx| {
                                for event in batch {
                                    pane.handle_alac_event(event, cx);
                                }
                            })
                            .is_ok();
                        if !alive {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(e) => {
                self.title = format!("failed to spawn shell: {e}");
                self.child_exited = Some(None);
            }
        }
    }

    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.spawn_session(cx);
        cx.notify();
    }

    fn handle_alac_event(&mut self, event: AlacEvent, cx: &mut Context<Self>) {
        match event {
            AlacEvent::Wakeup => {
                self.schedule_repaint(cx);
                self.poll_cwd(cx);
            }
            AlacEvent::Title(title) => {
                self.title = title;
                cx.emit(TerminalEvent::TitleChanged);
                cx.notify();
            }
            AlacEvent::ResetTitle => {
                self.title = "oxide".into();
                cx.emit(TerminalEvent::TitleChanged);
                cx.notify();
            }
            AlacEvent::PtyWrite(text) => {
                // Device Attributes and cursor-position query responses; if
                // these are dropped, querying programs hang forever.
                if let Some(session) = &self.session {
                    session.write_input(text.into_bytes());
                }
            }
            AlacEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            AlacEvent::ClipboardLoad(_, formatter) => {
                if let Some(session) = &self.session {
                    let text = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .unwrap_or_default();
                    session.write_input(formatter(&text).into_bytes());
                }
            }
            AlacEvent::ColorRequest(index, formatter) => {
                if let Some(session) = &self.session {
                    let color = match index {
                        0..=255 => colors::resolve_indexed(index as u8, &self.theme),
                        256 => self.theme.foreground,
                        257 => self.theme.background,
                        258 => self.theme.cursor,
                        _ => self.theme.foreground,
                    };
                    let (r, g, b) = hsla_to_rgb8(color);
                    session.write_input(formatter(Rgb { r, g, b }).into_bytes());
                }
            }
            AlacEvent::TextAreaSizeRequest(formatter) => {
                if let Some(session) = &self.session {
                    session.write_input(formatter(self.size.window_size()).into_bytes());
                }
            }
            AlacEvent::Bell => match self.config.bell {
                BellMode::None => {}
                BellMode::Sound => unsafe { NSBeep() },
                BellMode::Visual => {
                    self.bell_until = Some(Instant::now() + Duration::from_millis(150));
                    let timer = cx.background_executor().timer(Duration::from_millis(160));
                    cx.spawn(async move |this, cx| {
                        timer.await;
                        this.update(cx, |pane, cx| {
                            pane.bell_until = None;
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                    cx.notify();
                }
            },
            AlacEvent::CursorBlinkingChange => {
                self.blink_show = true;
                cx.notify();
            }
            AlacEvent::MouseCursorDirty => {}
            AlacEvent::ChildExit(code) => {
                self.child_exited = Some(code.code());
                cx.notify();
            }
            AlacEvent::Exit => {
                self.session = None;
                cx.notify();
            }
        }
    }

    /// Coalesce repaints: however fast Wakeups arrive, we repaint at most once
    /// per ~8ms tick.
    fn schedule_repaint(&mut self, cx: &mut Context<Self>) {
        if self.repaint_scheduled {
            return;
        }
        self.repaint_scheduled = true;
        let timer = cx.background_executor().timer(Duration::from_millis(8));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |pane, cx| {
                pane.repaint_scheduled = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn spawn_blink_task(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let timer = match this.update(cx, |_, cx| {
                    cx.background_executor().timer(Duration::from_millis(530))
                }) {
                    Ok(timer) => timer,
                    Err(_) => break,
                };
                timer.await;
                let alive = this
                    .update(cx, |pane, cx| {
                        // A cursor that blinks mid-typing is distracting.
                        if pane.last_input.elapsed() < Duration::from_millis(530) {
                            if !pane.blink_show {
                                pane.blink_show = true;
                                cx.notify();
                            }
                        } else {
                            pane.blink_show = !pane.blink_show;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    const CWD_POLL_INTERVAL: Duration = Duration::from_millis(150);

    /// Throttled with a trailing edge: a burst of output always gets one more
    /// poll after it settles, otherwise the cd that produced the final prompt
    /// redraw is missed and the tree lags until the next keystroke.
    fn poll_cwd(&mut self, cx: &mut Context<Self>) {
        let elapsed = self.last_cwd_poll.elapsed();
        if elapsed < Self::CWD_POLL_INTERVAL {
            if !self.cwd_poll_scheduled {
                self.cwd_poll_scheduled = true;
                let timer = cx
                    .background_executor()
                    .timer(Self::CWD_POLL_INTERVAL.saturating_sub(elapsed));
                cx.spawn(async move |this, cx| {
                    timer.await;
                    this.update(cx, |pane, cx| {
                        pane.cwd_poll_scheduled = false;
                        pane.poll_cwd_now(cx);
                    })
                    .ok();
                })
                .detach();
            }
            return;
        }
        self.poll_cwd_now(cx);
    }

    fn poll_cwd_now(&mut self, cx: &mut Context<Self>) {
        self.last_cwd_poll = Instant::now();
        if let Some(session) = &self.session
            && let Some(cwd) = session.foreground_cwd()
            && self.cwd.as_ref() != Some(&cwd)
        {
            self.cwd = Some(cwd.clone());
            cx.emit(TerminalEvent::CwdChanged(cwd));
        }
    }

    pub fn write_command(&mut self, command: &str) {
        if let Some(session) = &self.session {
            session.write_input(command.as_bytes().to_vec());
            session.term.lock().scroll_display(Scroll::Bottom);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            self.handle_search_key(event, cx);
            cx.stop_propagation();
            return;
        }
        if self.child_exited.is_some() {
            if event.keystroke.key == "enter" {
                self.restart(cx);
            }
            return;
        }
        let Some(session) = &self.session else { return };
        let mode = *session.term.lock().mode();
        let option_as_meta = self.config.shell.option_as_meta;
        if let Some(bytes) = keys::to_bytes(&event.keystroke, &mode, option_as_meta) {
            session.write_input(bytes);
            let mut term = session.term.lock();
            // Typing snaps you out of scrollback.
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
            }
            // Record a prompt mark: the row where Enter was pressed is (about
            // to be) a completed prompt line — the anchor cmd-up jumps to.
            if event.keystroke.key == "enter"
                && !event.keystroke.modifiers.modified()
                && !term.mode().contains(TermMode::ALT_SCREEN)
            {
                let abs = term.grid().history_size() + term.renderable_content().cursor.point.line.0.max(0) as usize;
                if self.prompt_marks.last() != Some(&abs) {
                    self.prompt_marks.push(abs);
                    if self.prompt_marks.len() > 500 {
                        self.prompt_marks.remove(0);
                    }
                }
            }
            drop(term);
            self.last_input = Instant::now();
            self.blink_show = true;
            cx.stop_propagation();
            cx.notify();
        }
    }

    // --- Scrollback search ---

    fn toggle_search(&mut self, _: &Search, _window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            self.close_search(cx);
        } else {
            self.search = Some(SearchState { query: String::new(), current: None });
        }
        cx.notify();
    }

    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search = None;
        if let Some(session) = &self.session {
            session.term.lock().selection = None;
        }
        cx.notify();
    }

    fn handle_search_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        match ks.key.as_str() {
            "escape" => {
                self.close_search(cx);
                return;
            }
            "enter" => {
                // enter walks older matches; shift-enter walks newer.
                let direction = if ks.modifiers.shift { Direction::Right } else { Direction::Left };
                self.run_search(direction, true, cx);
                return;
            }
            "backspace" => {
                if let Some(state) = &mut self.search {
                    if state.query.pop().is_none() {
                        self.close_search(cx);
                        return;
                    }
                    state.current = None;
                }
                self.run_search(Direction::Left, false, cx);
                return;
            }
            _ => {}
        }
        if ks.modifiers.platform || ks.modifiers.control {
            return;
        }
        if let Some(key_char) = ks.key_char.clone()
            && let Some(state) = &mut self.search
        {
            state.query.push_str(&key_char);
            state.current = None;
            self.run_search(Direction::Left, false, cx);
        }
    }

    fn run_search(&mut self, direction: Direction, from_current: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let Some(state) = &mut self.search else { return };
        if state.query.is_empty() {
            state.current = None;
            session.term.lock().selection = None;
            cx.notify();
            return;
        }
        let Ok(mut regex) = RegexSearch::new(&format!("(?i){}", regex_escape(&state.query))) else {
            return;
        };
        let mut term = session.term.lock();
        let screen_lines = term.screen_lines() as i32;
        let last_column = term.last_column();
        let origin = match (&state.current, from_current) {
            (Some(m), true) => {
                // Step one cell past the current match so we advance.
                let p = if direction == Direction::Left { *m.start() } else { *m.end() };
                let stepped = if direction == Direction::Left {
                    if p.column.0 > 0 {
                        GridPoint::new(p.line, Column(p.column.0 - 1))
                    } else {
                        GridPoint::new(p.line - 1, last_column)
                    }
                } else if p.column < last_column {
                    GridPoint::new(p.line, Column(p.column.0 + 1))
                } else {
                    GridPoint::new(p.line + 1, Column(0))
                };
                stepped.grid_clamp(&*term, alacritty_terminal::index::Boundary::Grid)
            }
            _ => GridPoint::new(Line(screen_lines - 1), last_column),
        };
        let found = term.search_next(&mut regex, origin, direction, Side::Left, None);
        if let Some(m) = &found {
            let mut selection = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
            selection.update(*m.end(), Side::Right);
            term.selection = Some(selection);
            // Scroll the match into the middle of the viewport.
            let history = term.grid().history_size() as i32;
            let offset = term.grid().display_offset() as i32;
            let line = m.start().line.0;
            if line < -offset || line >= screen_lines - offset {
                let target = (-line + screen_lines / 2).clamp(0, history);
                term.scroll_display(Scroll::Delta(target - offset));
            }
        }
        drop(term);
        if let Some(state) = &mut self.search {
            state.current = found;
        }
        cx.notify();
    }

    // --- Prompt jumping ---

    fn prompt_up(&mut self, _: &PromptUp, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let mut term = session.term.lock();
        let history = term.grid().history_size();
        let offset = term.grid().display_offset();
        for &mark in self.prompt_marks.iter().rev() {
            let target = history.saturating_sub(mark).min(history);
            if target > offset {
                term.scroll_display(Scroll::Delta(target as i32 - offset as i32));
                drop(term);
                cx.notify();
                return;
            }
        }
    }

    fn prompt_down(&mut self, _: &PromptDown, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let mut term = session.term.lock();
        let history = term.grid().history_size();
        let offset = term.grid().display_offset();
        if offset == 0 {
            return;
        }
        for &mark in self.prompt_marks.iter() {
            let target = history.saturating_sub(mark).min(history);
            if target < offset {
                term.scroll_display(Scroll::Delta(target as i32 - offset as i32));
                drop(term);
                cx.notify();
                return;
            }
        }
        term.scroll_display(Scroll::Bottom);
        drop(term);
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = session.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        session.write_input(keys::prepare_paste(&text, bracketed));
        session.term.lock().scroll_display(Scroll::Bottom);
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        if let Some(text) = session.term.lock().selection_to_string() {
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let mut term = session.term.lock();
        let top = GridPoint::new(term.topmost_line(), Column(0));
        let bottom = GridPoint::new(term.bottommost_line(), term.last_column());
        let mut selection = Selection::new(SelectionType::Lines, top, Side::Left);
        selection.update(bottom, Side::Right);
        term.selection = Some(selection);
        drop(term);
        cx.notify();
    }

    fn clear_scrollback(&mut self, _: &ClearScrollback, _window: &mut Window, cx: &mut Context<Self>) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        let Some(session) = &self.session else { return };
        let mut term = session.term.lock();
        term.clear_screen(ClearMode::Saved);
        drop(term);
        self.prompt_marks.clear();
        cx.notify();
    }

    /// Convert a window position to a grid point plus cell side.
    fn grid_point(&self, position: gpui::Point<Pixels>) -> Option<(GridPoint, Side, usize, usize)> {
        let layout = self.last_layout?;
        let x = f32::from(position.x - layout.bounds.origin.x) - self.config.window.padding.x;
        let y = f32::from(position.y - layout.bounds.origin.y) - self.config.window.padding.y;
        let col_f = (x / layout.cell_width).max(0.0);
        let col = (col_f as usize).min(self.size.columns.saturating_sub(1));
        let row = ((y / layout.cell_height).max(0.0) as usize).min(self.size.screen_lines.saturating_sub(1));
        let side = if col_f.fract() < 0.5 { Side::Left } else { Side::Right };
        let line = row as i32 - layout.display_offset as i32;
        Some((GridPoint::new(Line(line), Column(col)), side, col, row))
    }

    fn mouse_mode_active(&self, shift: bool) -> bool {
        if shift {
            return false; // shift bypasses reporting to force local selection
        }
        self.session
            .as_ref()
            .map(|s| s.term.lock().mode().intersects(TermMode::MOUSE_MODE))
            .unwrap_or(false)
    }

    fn send_mouse_report(&self, button: u8, col: usize, row: usize, pressed: bool, mods: &gpui::Modifiers) {
        let Some(session) = &self.session else { return };
        let mode = *session.term.lock().mode();
        let mut b = button;
        if mods.shift {
            b += 4;
        }
        if mods.alt {
            b += 8;
        }
        if mods.control {
            b += 16;
        }
        if mode.contains(TermMode::SGR_MOUSE) {
            let ch = if pressed { 'M' } else { 'm' };
            session.write_input(format!("\x1b[<{};{};{}{}", b, col + 1, row + 1, ch).into_bytes());
        } else if col < 223 && row < 223 {
            let b = if pressed { b } else { 3 };
            session.write_input(vec![
                0x1b,
                b'[',
                b'M',
                32 + b,
                33 + col as u8,
                33 + row as u8,
            ]);
        }
    }

    /// The OSC 8 hyperlink or URL-looking whitespace-delimited token at `point`.
    fn url_at(&self, point: GridPoint) -> Option<String> {
        let session = self.session.as_ref()?;
        let term = session.term.lock();
        let grid = term.grid();
        // Explicit hyperlinks (OSC 8) win over textual detection.
        if let Some(link) = grid[point.line][point.column].hyperlink() {
            return Some(link.uri().to_string());
        }
        let cols = self.size.columns;
        let chars: Vec<char> = (0..cols)
            .map(|c| grid[point.line][Column(c)].c)
            .collect();
        drop(term);
        let is_break = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}');
        let ix = point.column.0.min(cols.saturating_sub(1));
        if is_break(chars[ix]) {
            return None;
        }
        let start = (0..=ix).rev().find(|&i| is_break(chars[i])).map_or(0, |i| i + 1);
        let end = (ix..cols).find(|&i| is_break(chars[i])).unwrap_or(cols);
        let token: String = chars[start..end].iter().collect();
        let token = token.trim_end_matches([',', '.', ';', ':', '!', '?']).to_string();
        (token.starts_with("http://") || token.starts_with("https://") || token.starts_with("file://"))
            .then_some(token)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let Some((point, side, col, row)) = self.grid_point(event.position) else { return };
        if event.modifiers.platform {
            if let Some(url) = self.url_at(point) {
                cx.open_url(&url);
            }
            return;
        }
        if self.mouse_mode_active(event.modifiers.shift) {
            self.send_mouse_report(0, col, row, true, &event.modifiers);
            return;
        }
        let ty = match event.click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        if let Some(session) = &self.session {
            let mut term = session.term.lock();
            let mut selection = Selection::new(ty, point, side);
            if event.click_count > 1 {
                selection.include_all();
            }
            term.selection = Some(selection);
            drop(term);
            self.selecting = true;
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some((point, side, col, row)) = self.grid_point(event.position) else { return };
        if self.mouse_mode_active(event.modifiers.shift) {
            let drag = self
                .session
                .as_ref()
                .map(|s| {
                    s.term
                        .lock()
                        .mode()
                        .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                })
                .unwrap_or(false);
            if drag {
                self.send_mouse_report(32, col, row, true, &event.modifiers);
            }
            return;
        }
        if self.selecting
            && let Some(session) = &self.session
        {
            let mut term = session.term.lock();
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, side);
            }
            drop(term);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.selecting = false;
            if self.config.copy_on_select
                && let Some(session) = &self.session
                && let Some(text) = session.term.lock().selection_to_string()
                && !text.is_empty()
            {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            return;
        }
        let Some((_, _, col, row)) = self.grid_point(event.position) else { return };
        if self.mouse_mode_active(event.modifiers.shift) {
            self.send_mouse_report(0, col, row, false, &event.modifiers);
        }
    }

    fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let cell_height = self.last_layout.map(|l| l.cell_height).unwrap_or(17.0);
        let delta_lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / cell_height,
        };
        self.scroll_accum += delta_lines;
        let lines = self.scroll_accum.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_accum -= lines as f32;

        let mode = *session.term.lock().mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Alt-screen apps with alternate scroll get arrow keys, otherwise
            // scrolling in vim/less does nothing.
            let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let seq = if mode.contains(TermMode::APP_CURSOR) {
                seq.to_vec()
            } else if lines > 0 {
                b"\x1b[A".to_vec()
            } else {
                b"\x1b[B".to_vec()
            };
            let mut bytes = Vec::new();
            for _ in 0..lines.abs() {
                bytes.extend_from_slice(&seq);
            }
            session.write_input(bytes);
        } else if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            if let Some((_, _, col, row)) = self.grid_point(event.position) {
                let button = if lines > 0 { 64 } else { 65 };
                for _ in 0..lines.abs() {
                    self.send_mouse_report(button, col, row, true, &event.modifiers);
                }
            }
        } else {
            session.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let theme = self.theme.clone();
        let mut bg = theme.background;
        bg.a = self.config.window.opacity.clamp(0.1, 1.0);

        div()
            .id("terminal-pane")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(bg)
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear_scrollback))
            .on_action(cx.listener(Self::toggle_search))
            .on_action(cx.listener(Self::prompt_up))
            .on_action(cx.listener(Self::prompt_down))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(TerminalElement::new(cx.entity(), focused))
            .when_some(self.search.as_ref(), |this, search| {
                let hint = if search.current.is_some() { "⏎ older  ⇧⏎ newer  esc" } else if search.query.is_empty() { "type to search" } else { "no match" };
                this.child(
                    div()
                        .absolute()
                        .top_2()
                        .right_2()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme.ansi[0])
                        .border_1()
                        .border_color(if search.current.is_some() || search.query.is_empty() {
                            theme.ansi[4]
                        } else {
                            theme.ansi[1]
                        })
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .text_size(gpui::px(12.0))
                        .child(format!("/{}", search.query))
                        .child(
                            div()
                                .text_color(colors::blend(theme.foreground, theme.background, 0.45))
                                .child(hint),
                        ),
                )
            })
            .when(self.bell_until.is_some_and(|t| Instant::now() < t), |this| {
                let mut flash = theme.foreground;
                flash.a = 0.12;
                this.child(div().absolute().inset_0().bg(flash))
            })
            .when_some(self.child_exited, |this, code| {
                let message = match code {
                    Some(code) => format!("[process exited with code {code}]"),
                    None => "[process exited]".to_string(),
                };
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .px_4()
                                .py_2()
                                .rounded_md()
                                .bg(theme.ansi[0])
                                .text_color(theme.foreground)
                                .child(format!("{message} — press ⏎ to restart")),
                        ),
                )
            })
    }
}
