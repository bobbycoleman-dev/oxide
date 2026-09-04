//! Byte-level scanner for the OSC sequences Oxide consumes itself: the OSC
//! 133 prompt markers the shell integration emits, OSC 7 (cwd), and OSC 9 /
//! 777 (desktop notifications).
//!
//! `alacritty_terminal` drops unknown OSCs inside its parser, so this runs
//! on the raw byte stream in the PTY thread. It is resumable across `read`
//! chunks and mirrors vte's rules: BEL or `ESC \` terminates, CAN/SUB abort,
//! other C0 controls are ignored (so a newline inside a command line is
//! harmless). The payload is capped so a malformed sequence can't grow a
//! buffer without bound.

use std::path::PathBuf;

/// What a marker means, with any payload decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkerKind {
    /// OSC 133;A — the shell is about to draw a prompt.
    PromptStart,
    /// OSC 133;B — the prompt is drawn; what follows is user input.
    InputStart,
    /// OSC 133;C — a command is starting. Oxide's integration adds the typed
    /// line as `cmdline=…`.
    CommandStart { cmdline: Option<String> },
    /// OSC 133;D;<exit> — the command finished. `None` when the exit code is
    /// missing or unparseable.
    CommandEnd { exit: Option<i32> },
    /// OSC 7 — the shell's working directory, local hosts only.
    Cwd(PathBuf),
    /// OSC 9 / OSC 777;notify — a program asked for a desktop notification.
    Notify { title: Option<String>, body: String },
}

/// A marker with where it landed: the cursor's absolute row
/// (history + screen line) and column at the moment it was parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub kind: MarkerKind,
    pub row: usize,
    pub column: usize,
    pub alt_screen: bool,
}

/// Longest OSC payload we'll accumulate. OSC 7 carries a path and OSC 777 a
/// notification body; anything longer is not one of ours.
const MAX_PAYLOAD: usize = 4096;

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    /// Saw ESC; next byte decides.
    Escape,
    /// Inside `ESC ]`, collecting the payload.
    Osc,
    /// Inside the payload and saw ESC: `\` ends it, anything else aborts.
    OscEscape,
    /// Payload overflowed: swallow until the terminator, emit nothing.
    Overflow,
}

#[derive(Debug, Default)]
pub struct OscScanner {
    state: State,
    payload: Vec<u8>,
}

impl OscScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan a chunk, returning every recognised sequence that *completed* in
    /// it, with the offset just past its terminator. A sequence split across
    /// chunks is reported in the chunk where it ends; nothing before that
    /// offset can have moved the cursor, so sampling the cursor there is
    /// exact.
    pub fn scan(&mut self, chunk: &[u8]) -> Vec<(usize, MarkerKind)> {
        let mut out = Vec::new();
        for (i, &b) in chunk.iter().enumerate() {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Escape;
                    }
                }
                State::Escape => {
                    if b == b']' {
                        self.payload.clear();
                        self.state = State::Osc;
                    } else if b == 0x1b {
                        // ESC ESC: still one escape pending.
                    } else {
                        self.state = State::Ground;
                    }
                }
                State::Osc | State::Overflow => match b {
                    0x07 => {
                        if let (State::Osc, Some(kind)) = (&self.state, parse_payload(&self.payload)) {
                            out.push((i + 1, kind));
                        }
                        self.state = State::Ground;
                    }
                    0x1b => self.state = State::OscEscape,
                    0x18 | 0x1a => self.state = State::Ground,
                    0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
                    _ => {
                        if matches!(self.state, State::Osc) {
                            if self.payload.len() < MAX_PAYLOAD {
                                self.payload.push(b);
                            } else {
                                self.payload.clear();
                                self.state = State::Overflow;
                            }
                        }
                    }
                },
                State::OscEscape => {
                    if b == b'\\' {
                        if let Some(kind) = parse_payload(&self.payload) {
                            out.push((i + 1, kind));
                        }
                        self.state = State::Ground;
                    } else if b == b']' {
                        // `ESC ]` inside an unterminated OSC: start over.
                        self.payload.clear();
                        self.state = State::Osc;
                    } else if b == 0x1b {
                        self.state = State::Escape;
                    } else {
                        self.state = State::Ground;
                    }
                }
            }
        }
        // Whatever is buffered belongs to the next chunk, but an overflowed
        // or abandoned payload must not keep growing forever.
        if matches!(self.state, State::Overflow) {
            self.payload.clear();
        }
        out
    }
}

/// Decode an OSC payload (everything between `ESC ]` and the terminator)
/// into a marker, or `None` for sequences that aren't ours.
pub fn parse_payload(payload: &[u8]) -> Option<MarkerKind> {
    let text = std::str::from_utf8(payload).ok()?;
    let (code, rest) = match text.split_once(';') {
        Some((code, rest)) => (code, rest),
        None => (text, ""),
    };
    match code {
        "133" => parse_133(rest),
        "7" => parse_cwd(rest).map(MarkerKind::Cwd),
        "9" => {
            // ConEmu/Windows Terminal use `9;4;…` for progress bars; only a
            // plain message is a notification.
            if rest.is_empty() || rest.starts_with("4;") {
                return None;
            }
            Some(MarkerKind::Notify { title: None, body: rest.to_string() })
        }
        "777" => {
            let mut parts = rest.splitn(3, ';');
            if parts.next()? != "notify" {
                return None;
            }
            let title = parts.next().unwrap_or("").to_string();
            let body = parts.next().unwrap_or("").to_string();
            if title.is_empty() && body.is_empty() {
                return None;
            }
            Some(MarkerKind::Notify {
                title: (!title.is_empty()).then_some(title),
                body,
            })
        }
        _ => None,
    }
}

fn parse_133(rest: &str) -> Option<MarkerKind> {
    let mut parts = rest.split(';');
    let kind = parts.next()?;
    match kind {
        "A" => Some(MarkerKind::PromptStart),
        "B" => Some(MarkerKind::InputStart),
        "C" => {
            // Our integration appends `cmdline=<text>`; the text may itself
            // contain semicolons, so take everything after the key.
            let cmdline = rest
                .strip_prefix("C")
                .and_then(|r| r.split_once("cmdline="))
                .map(|(_, line)| line.trim().to_string())
                .filter(|s| !s.is_empty());
            Some(MarkerKind::CommandStart { cmdline })
        }
        "D" => {
            let exit = parts.next().and_then(|s| s.trim().parse::<i32>().ok());
            Some(MarkerKind::CommandEnd { exit })
        }
        _ => None,
    }
}

/// `file://host/path` → the path, for hosts that mean this machine. A
/// remote shell's cwd (over ssh) is real but not somewhere the file tree
/// can go, so it is ignored.
fn parse_cwd(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let (host, path) = match rest.find('/') {
        Some(ix) => (&rest[..ix], &rest[ix..]),
        None => return None,
    };
    if !host_is_local(host) {
        return None;
    }
    let decoded = percent_decode(path);
    (!decoded.is_empty()).then(|| PathBuf::from(decoded))
}

fn host_is_local(host: &str) -> bool {
    if host.is_empty() || host == "localhost" {
        return true;
    }
    let Ok(mine) = hostname() else { return false };
    let short = |h: &str| h.split('.').next().unwrap_or(h).to_ascii_lowercase();
    short(host) == short(&mine)
}

fn hostname() -> Result<String, ()> {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return Err(());
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).map_err(|_| ())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(scanner: &mut OscScanner, chunk: &[u8]) -> Vec<MarkerKind> {
        scanner.scan(chunk).into_iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn recognises_each_marker_with_both_terminators() {
        let mut s = OscScanner::new();
        assert_eq!(kinds(&mut s, b"\x1b]133;A\x1b\\"), vec![MarkerKind::PromptStart]);
        assert_eq!(kinds(&mut s, b"\x1b]133;B\x07"), vec![MarkerKind::InputStart]);
        assert_eq!(kinds(&mut s, b"\x1b]133;C\x1b\\"), vec![MarkerKind::CommandStart { cmdline: None }]);
        assert_eq!(kinds(&mut s, b"\x1b]133;D;0\x07"), vec![MarkerKind::CommandEnd { exit: Some(0) }]);
        assert_eq!(kinds(&mut s, b"\x1b]133;D;127\x1b\\"), vec![MarkerKind::CommandEnd { exit: Some(127) }]);
    }

    #[test]
    fn reports_the_offset_just_past_the_terminator() {
        let mut s = OscScanner::new();
        let chunk = b"hello\x1b]133;A\x07world";
        let found = s.scan(chunk);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 13);
        assert_eq!(&chunk[found[0].0..], b"world");
        let chunk = b"\x1b]133;B\x1b\\x";
        let found = s.scan(chunk);
        assert_eq!(found[0].0, 9);
    }

    #[test]
    fn survives_splits_at_every_byte_boundary() {
        let seq = b"prompt\x1b]133;D;1\x1b\\tail";
        for split in 0..seq.len() {
            let mut s = OscScanner::new();
            let mut got = kinds(&mut s, &seq[..split]);
            got.extend(kinds(&mut s, &seq[split..]));
            assert_eq!(got, vec![MarkerKind::CommandEnd { exit: Some(1) }], "split at {split}");
        }
    }

    #[test]
    fn missing_or_bad_exit_code_is_none_not_a_panic() {
        let mut s = OscScanner::new();
        assert_eq!(kinds(&mut s, b"\x1b]133;D\x07"), vec![MarkerKind::CommandEnd { exit: None }]);
        assert_eq!(kinds(&mut s, b"\x1b]133;D;\x07"), vec![MarkerKind::CommandEnd { exit: None }]);
        assert_eq!(kinds(&mut s, b"\x1b]133;D;abc\x07"), vec![MarkerKind::CommandEnd { exit: None }]);
    }

    #[test]
    fn cmdline_is_extracted_including_semicolons() {
        let mut s = OscScanner::new();
        assert_eq!(
            kinds(&mut s, b"\x1b]133;C;cmdline=cargo build; echo done\x07"),
            vec![MarkerKind::CommandStart { cmdline: Some("cargo build; echo done".into()) }]
        );
        assert_eq!(
            kinds(&mut s, b"\x1b]133;C;cmdline=\x07"),
            vec![MarkerKind::CommandStart { cmdline: None }]
        );
    }

    #[test]
    fn ignores_unrelated_and_malformed_sequences() {
        let mut s = OscScanner::new();
        assert!(kinds(&mut s, b"\x1b]0;title\x07").is_empty());
        assert!(kinds(&mut s, b"\x1b]133;Z\x07").is_empty());
        assert!(kinds(&mut s, b"\x1b[31mred\x1b[0m").is_empty());
        // CAN aborts; the next real marker still parses.
        assert_eq!(kinds(&mut s, b"\x1b]133;A\x18\x1b]133;B\x07"), vec![MarkerKind::InputStart]);
        // An unterminated OSC followed by a fresh `ESC ]` starts over.
        assert_eq!(kinds(&mut s, b"\x1b]133;A\x1b]133;B\x07"), vec![MarkerKind::InputStart]);
        // A control character inside the payload is dropped, like vte does.
        assert_eq!(
            kinds(&mut s, b"\x1b]133;C;cmdline=a\nb\x07"),
            vec![MarkerKind::CommandStart { cmdline: Some("ab".into()) }]
        );
    }

    #[test]
    fn overlong_payload_is_dropped_and_scanner_recovers() {
        let mut s = OscScanner::new();
        let mut junk = b"\x1b]133;C;cmdline=".to_vec();
        junk.extend(std::iter::repeat_n(b'x', MAX_PAYLOAD + 10));
        junk.extend_from_slice(b"\x07\x1b]133;A\x07");
        assert_eq!(kinds(&mut s, &junk), vec![MarkerKind::PromptStart]);
    }

    #[test]
    fn cwd_accepts_local_hosts_and_decodes_percent_escapes() {
        assert_eq!(parse_payload(b"7;file:///Users/x/a%20b"), Some(MarkerKind::Cwd(PathBuf::from("/Users/x/a b"))));
        assert_eq!(parse_payload(b"7;file://localhost/tmp"), Some(MarkerKind::Cwd(PathBuf::from("/tmp"))));
        let mine = hostname().unwrap();
        assert_eq!(parse_payload(format!("7;file://{mine}/tmp").as_bytes()), Some(MarkerKind::Cwd(PathBuf::from("/tmp"))));
        assert_eq!(parse_payload(b"7;file://build-box.example.com/srv"), None);
        assert_eq!(parse_payload(b"7;/no/scheme"), None);
    }

    #[test]
    fn notifications_from_osc_9_and_777() {
        assert_eq!(
            parse_payload(b"9;Build finished"),
            Some(MarkerKind::Notify { title: None, body: "Build finished".into() })
        );
        assert_eq!(parse_payload(b"9;4;1;50"), None, "progress reports are not notifications");
        assert_eq!(
            parse_payload(b"777;notify;Deploy;All green"),
            Some(MarkerKind::Notify { title: Some("Deploy".into()), body: "All green".into() })
        );
        assert_eq!(parse_payload(b"777;other;x;y"), None);
    }
}
