//! Terminal graphics protocol and font-size capability prober.
//!
//! Adapted from `ratatui-image` (The MIT License):
//! - Copyright (c) 2022 Atanas Yankov (code and inspiration taken from <https://github.com/atanunq/viuer>)
//! - Copyright (c) 2022 wooster0 (inspiration taken from <https://github.com/wooster0/hanbun>)
//! - Copyright (c) 2023 Benjamin Große (<https://github.com/benjajaja/ratatui-image>)

use std::env;
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::time::{Duration, Instant};

/// Default stdio query timeout in milliseconds.
const STDIN_READ_TIMEOUT: Duration = Duration::from_millis(2000);

/// Base64-encoded zlib stream of a single black RGB pixel (`[0, 0, 0]`) for Kitty `o=z` probing.
const KITTY_COMPRESSION_PROBE: &str = "eJxjYGAAAAADAAE=";

/// Terminal character cell dimensions in pixels `(width, height)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontSize {
    /// Width of a single terminal character cell in pixels.
    pub width: u16,
    /// Height of a single terminal character cell in pixels.
    pub height: u16,
}

impl FontSize {
    /// Creates a new [`FontSize`] with the given pixel width and height.
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

impl From<(u16, u16)> for FontSize {
    fn from((width, height): (u16, u16)) -> Self {
        Self::new(width, height)
    }
}

/// Supported terminal graphics protocol families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolType {
    /// Unicode halfblock character rendering fallback.
    Halfblocks,
    /// DEC Sixel graphics protocol.
    Sixel,
    /// Kitty graphics protocol.
    Kitty,
    /// iTerm2 inline images protocol (`OSC 1337`).
    Iterm2,
}

/// Options controlling terminal capability probing over stdio.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryStdioOptions {
    /// Whether to probe for Kitty `o=z` zlib transmission compression (`_Gi=32`).
    pub kitty_compression: bool,
}

/// Returns the `(start, escape, end)` wrapper sequences for terminal escape codes.
///
/// Inside `tmux`, escape sequences must be wrapped in a DCS passthrough block
/// (`\x1bPtmux; ... \x1b\\`) with every inner ESC doubled (`\x1b\x1b`).
pub fn tmux_start_escape_end(is_tmux: bool) -> (&'static str, &'static str, &'static str) {
    if is_tmux {
        ("\x1bPtmux;", "\x1b\x1b", "\x1b\\")
    } else {
        ("", "\x1b", "")
    }
}

/// Terminal graphics capability and font-size picker.
#[derive(Clone, Debug)]
pub struct Picker {
    font_size: FontSize,
    protocol_type: ProtocolType,
    is_tmux: bool,
    kitty_compression: bool,
}

impl Picker {
    /// Queries terminal stdio for graphics capabilities and character cell pixel size.
    ///
    /// Temporarily disables canonical mode and echo on `stdin` via an RAII termios guard so the
    /// original terminal attributes are always restored on return, error, or panic.
    pub fn from_query_stdio_with_options(options: QueryStdioOptions) -> Result<Self, String> {
        let env_hints = EnvHints::from_process_env();

        if env_hints.is_tmux {
            enable_tmux_passthrough();
        }

        let query = build_query(
            env_hints.is_tmux,
            options,
            env_hints.blacklist_kitty_and_sixel,
        );

        match query_stdio_raw(&query, STDIN_READ_TIMEOUT) {
            Ok(responses) if !responses.is_empty() => Ok(resolve_picker_from_responses(
                &responses,
                &env_hints,
                font_size_fallback(),
            )),
            Ok(_) => Ok(fallback_picker(&env_hints, font_size_fallback())),
            Err(QueryError::NoResponse) => Ok(fallback_picker(&env_hints, font_size_fallback())),
            Err(QueryError::Io(msg)) => Err(msg),
        }
    }

    /// Creates a fallback picker using Unicode halfblocks and an 8x16 cell pixel size.
    pub fn halfblocks() -> Self {
        Self {
            font_size: FontSize::new(8, 16),
            protocol_type: ProtocolType::Halfblocks,
            is_tmux: false,
            kitty_compression: false,
        }
    }

    /// Returns the detected or fallback character cell size in pixels.
    pub fn font_size(&self) -> FontSize {
        self.font_size
    }

    /// Returns the currently selected terminal graphics protocol type.
    pub fn protocol_type(&self) -> ProtocolType {
        self.protocol_type
    }

    /// Overrides the selected terminal graphics protocol type.
    pub fn set_protocol_type(&mut self, proto: ProtocolType) {
        self.protocol_type = proto;
    }

    /// Returns whether launch-time environment detection concluded this session runs inside `tmux`.
    pub fn tmux_detected(&self) -> bool {
        self.is_tmux
    }

    /// Returns whether the terminal positively answered the Kitty `o=z` zlib compression probe.
    pub fn supports_kitty_compression(&self) -> bool {
        self.kitty_compression
    }
}

/// Environment-derived terminal hints used alongside stdio capability probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnvHints {
    is_tmux: bool,
    tmux_outer_proto: Option<ProtocolType>,
    iterm2_hint: Option<ProtocolType>,
    blacklist_kitty_and_sixel: bool,
}

impl EnvHints {
    fn from_process_env() -> Self {
        Self::from_env_lookup(|key| env::var(key).ok())
    }

    fn from_env_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let is_tmux = get("TERM").is_some_and(|t| t.starts_with("tmux"))
            || get("TERM_PROGRAM").is_some_and(|tp| tp == "tmux")
            || get("TMUX").is_some_and(|s| !s.is_empty());

        let has_nonempty = |key: &str| get(key).is_some_and(|s| !s.is_empty());

        let tmux_outer_proto = if is_tmux {
            if has_nonempty("ITERM_SESSION_ID") || has_nonempty("WEZTERM_EXECUTABLE") {
                Some(ProtocolType::Iterm2)
            } else {
                None
            }
        } else {
            None
        };

        let is_wezterm = has_nonempty("WEZTERM_EXECUTABLE");
        let is_konsole = has_nonempty("KONSOLE_VERSION");
        let blacklist_kitty_and_sixel = is_wezterm || is_konsole;

        let iterm2_hint = detect_iterm2_from_env(&get);

        Self {
            is_tmux,
            tmux_outer_proto,
            iterm2_hint,
            blacklist_kitty_and_sixel,
        }
    }
}

fn detect_iterm2_from_env(get: &impl Fn(&str) -> Option<String>) -> Option<ProtocolType> {
    if get("TERM_PROGRAM").is_some_and(|tp| {
        tp.contains("iTerm")
            || tp.contains("WezTerm")
            || tp.contains("mintty")
            || tp.contains("vscode")
            || tp.contains("Tabby")
            || tp.contains("Hyper")
            || tp.contains("rio")
            || tp.contains("Bobcat")
            || tp.contains("WarpTerminal")
    }) {
        return Some(ProtocolType::Iterm2);
    }
    if get("LC_TERMINAL").is_some_and(|lc| lc.contains("iTerm")) {
        return Some(ProtocolType::Iterm2);
    }
    None
}

fn enable_tmux_passthrough() {
    let _ = std::process::Command::new("tmux")
        .args(["set", "-p", "allow-passthrough", "on"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| child.wait());
}

/// Builds the combined terminal capability query string.
fn build_query(
    is_tmux: bool,
    options: QueryStdioOptions,
    blacklist_kitty_and_sixel: bool,
) -> String {
    let (start, escape, end) = tmux_start_escape_end(is_tmux);
    let mut buf = String::with_capacity(128);
    buf.push_str(start);

    if !blacklist_kitty_and_sixel {
        let _ = write!(buf, "{escape}_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA{escape}\\");
        if options.kitty_compression {
            let _ = write!(
                buf,
                "{escape}_Gi=32,s=1,v=1,a=q,t=d,f=24,o=z;{KITTY_COMPRESSION_PROBE}{escape}\\"
            );
        }
        // Device Attributes Report 1 (DA1) for Sixel support (`;4;`).
        let _ = write!(buf, "{escape}[c");
    }

    // Character cell size in pixels (`CSI 16t` -> `CSI 6;height;width t`).
    let _ = write!(buf, "{escape}[16t");
    // Device Status Report (`CSI 5n` -> `CSI 0n`) as end-of-response sentinel.
    let _ = write!(buf, "{escape}[5n");
    buf.push_str(end);
    buf
}

#[derive(Debug, PartialEq, Eq)]
enum ParseState {
    Unknown,
    CsiResponse,
    KittyResponse,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum CapResponse {
    Kitty,
    KittyCompression,
    Sixel,
    CellSize(Option<FontSize>),
    Status,
}

/// Streaming parser for terminal capability responses.
struct CapParser {
    data: String,
    state: ParseState,
}

impl CapParser {
    fn new() -> Self {
        Self {
            data: String::with_capacity(64),
            state: ParseState::Unknown,
        }
    }

    fn restart(&mut self) {
        self.data.clear();
        self.state = ParseState::Unknown;
    }

    fn push(&mut self, next: char) -> Option<CapResponse> {
        match self.state {
            ParseState::Unknown => {
                match (self.data.as_str(), next) {
                    (_, '\x1b') => {
                        self.restart();
                        return None;
                    }
                    ("_Gi=31" | "_Gi=32", ';') => {
                        self.state = ParseState::KittyResponse;
                    }
                    ("[", _) => {
                        self.state = ParseState::CsiResponse;
                    }
                    _ => {}
                }
                if self.data.len() < 256 {
                    self.data.push(next);
                }
            }
            ParseState::CsiResponse => {
                if self.data == "[0" && next == 'n' {
                    self.restart();
                    return Some(CapResponse::Status);
                }
                match next {
                    'c' if self.data.starts_with("[?") => {
                        let has_sixel = self.data[2..].split(';').any(|cap| cap == "4");
                        self.restart();
                        if has_sixel {
                            return Some(CapResponse::Sixel);
                        }
                    }
                    't' => {
                        let mut cell_size = None;
                        let mut parts = self.data.split(';');
                        if let (Some("[6"), Some(h), Some(w), None) =
                            (parts.next(), parts.next(), parts.next(), parts.next())
                            && let (Ok(h), Ok(w)) = (h.parse::<u16>(), w.parse::<u16>())
                            && w > 0
                            && h > 0
                        {
                            cell_size = Some(FontSize::new(w, h));
                        }
                        self.restart();
                        return Some(CapResponse::CellSize(cell_size));
                    }
                    '\x1b' => {
                        self.restart();
                    }
                    _ => {
                        if self.data.len() < 256 {
                            self.data.push(next);
                        } else {
                            self.restart();
                        }
                    }
                }
            }
            ParseState::KittyResponse => match next {
                '\\' => {
                    let resp = match self.data.as_str() {
                        "_Gi=31;OK\x1b" => Some(CapResponse::Kitty),
                        "_Gi=32;OK\x1b" => Some(CapResponse::KittyCompression),
                        _ => None,
                    };
                    self.restart();
                    return resp;
                }
                _ => {
                    if self.data.len() < 256 {
                        self.data.push(next);
                    } else {
                        self.restart();
                    }
                }
            },
        }
        None
    }
}

fn resolve_picker_from_responses(
    responses: &[CapResponse],
    env_hints: &EnvHints,
    ioctl_font_size: Option<FontSize>,
) -> Picker {
    let mut capability_proto = None;
    let mut kitty_compression = false;
    let mut font_size = None;

    for resp in responses {
        match *resp {
            CapResponse::Kitty => {
                capability_proto = Some(ProtocolType::Kitty);
            }
            CapResponse::KittyCompression => {
                kitty_compression = true;
            }
            CapResponse::Sixel => {
                if capability_proto.is_none() {
                    capability_proto = Some(ProtocolType::Sixel);
                }
            }
            CapResponse::CellSize(Some(size)) => {
                font_size = Some(size);
            }
            CapResponse::CellSize(None) | CapResponse::Status => {}
        }
    }

    let font_size = font_size.or(ioctl_font_size);

    // IO-based detection is authoritative; env-based hints are fallbacks.
    let protocol_type = capability_proto
        .or(env_hints.tmux_outer_proto)
        .or(env_hints.iterm2_hint)
        .unwrap_or(ProtocolType::Halfblocks);

    if let Some(font_size) = font_size {
        Picker {
            font_size,
            protocol_type,
            is_tmux: env_hints.is_tmux,
            kitty_compression,
        }
    } else {
        // Without a known cell pixel size, pixel protocols cannot render accurately.
        Picker {
            font_size: FontSize::new(8, 16),
            protocol_type: ProtocolType::Halfblocks,
            is_tmux: env_hints.is_tmux,
            kitty_compression: false,
        }
    }
}

fn fallback_picker(env_hints: &EnvHints, ioctl_font_size: Option<FontSize>) -> Picker {
    let env_proto = env_hints.tmux_outer_proto.or(env_hints.iterm2_hint);
    if let Some(font_size) = ioctl_font_size {
        Picker {
            font_size,
            protocol_type: env_proto.unwrap_or(ProtocolType::Halfblocks),
            is_tmux: env_hints.is_tmux,
            kitty_compression: false,
        }
    } else {
        Picker {
            font_size: FontSize::new(8, 16),
            protocol_type: ProtocolType::Halfblocks,
            is_tmux: env_hints.is_tmux,
            kitty_compression: false,
        }
    }
}

enum QueryError {
    NoResponse,
    Io(String),
}

/// RAII guard that disables `ICANON` and `ECHO` on `stdin` and always restores original termios.
struct RawModeGuard {
    fd: libc::c_int,
    original: libc::termios,
}

impl RawModeGuard {
    fn enter(fd: libc::c_int) -> Result<Self, String> {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
            return Err(format!("tcgetattr failed: {}", io::Error::last_os_error()));
        }
        let original = unsafe { termios.assume_init() };
        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
            return Err(format!("tcsetattr failed: {}", io::Error::last_os_error()));
        }
        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

fn font_size_fallback() -> Option<FontSize> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return None;
    }
    if ws.ws_xpixel == 0 || ws.ws_ypixel == 0 || ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    let cell_w = ws.ws_xpixel / ws.ws_col;
    let cell_h = ws.ws_ypixel / ws.ws_row;
    if cell_w == 0 || cell_h == 0 {
        return None;
    }
    Some(FontSize::new(cell_w, cell_h))
}

fn query_stdio_raw(query: &str, timeout: Duration) -> Result<Vec<CapResponse>, QueryError> {
    let _raw_guard = RawModeGuard::enter(libc::STDIN_FILENO).map_err(QueryError::Io)?;

    let mut stdout = io::stdout();
    stdout
        .write_all(query.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| QueryError::Io(e.to_string()))?;

    let mut parser = CapParser::new();
    let mut responses = Vec::new();
    let mut chunk = [0u8; 64];
    let start_time = Instant::now();
    let mut poll_fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        let elapsed = start_time.elapsed();
        if elapsed >= timeout {
            break;
        }
        let remaining_ms = (timeout - elapsed).as_millis().min(i32::MAX as u128) as libc::c_int;
        poll_fd.revents = 0;
        let ret = unsafe { libc::poll(&mut poll_fd, 1, remaining_ms) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(QueryError::Io(err.to_string()));
        }
        if ret == 0 {
            break;
        }
        if (poll_fd.revents & libc::POLLIN) != 0 {
            let n = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    chunk.as_mut_ptr().cast::<libc::c_void>(),
                    chunk.len(),
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(QueryError::Io(err.to_string()));
            }
            if n == 0 {
                break;
            }
            for &b in &chunk[..n as usize] {
                if let Some(resp) = parser.push(char::from(b)) {
                    if resp == CapResponse::Status {
                        return Ok(responses);
                    }
                    responses.push(resp);
                }
            }
        } else if (poll_fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)) != 0 {
            break;
        }
    }

    if responses.is_empty() {
        Err(QueryError::NoResponse)
    } else {
        Ok(responses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(input: &str) -> Vec<CapResponse> {
        let mut parser = CapParser::new();
        let mut out = Vec::new();
        for ch in input.chars() {
            if let Some(resp) = parser.push(ch) {
                out.push(resp);
            }
        }
        out
    }

    fn parse_chunked(chunks: &[&str]) -> Vec<CapResponse> {
        let mut parser = CapParser::new();
        let mut out = Vec::new();
        for chunk in chunks {
            for ch in chunk.chars() {
                if let Some(resp) = parser.push(ch) {
                    out.push(resp);
                }
            }
        }
        out
    }

    fn env_from_pairs<'a>(pairs: &'a [(&'a str, &'a str)]) -> EnvHints {
        EnvHints::from_env_lookup(|key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        })
    }

    #[test]
    fn test_tmux_start_escape_end() {
        assert_eq!(tmux_start_escape_end(false), ("", "\x1b", ""));
        assert_eq!(
            tmux_start_escape_end(true),
            ("\x1bPtmux;", "\x1b\x1b", "\x1b\\")
        );
    }

    #[test]
    fn test_parse_csi_16t_font_size() {
        assert_eq!(
            parse_str("\x1b[6;16;8t\x1b[0n"),
            vec![
                CapResponse::CellSize(Some(FontSize::new(8, 16))),
                CapResponse::Status
            ]
        );
        // Zero dimensions are rejected as None.
        assert_eq!(
            parse_str("\x1b[6;0;8t\x1b[0n"),
            vec![CapResponse::CellSize(None), CapResponse::Status]
        );
        // Window pixel size (`CSI 4;...t`) is not confused with cell size (`CSI 6;...t`).
        assert_eq!(
            parse_str("\x1b[4;1080;1920t\x1b[0n"),
            vec![CapResponse::CellSize(None), CapResponse::Status]
        );
    }

    #[test]
    fn test_parse_kitty_and_compression_responses() {
        assert_eq!(
            parse_str("\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;OK\x1b\\\x1b[0n"),
            vec![
                CapResponse::Kitty,
                CapResponse::KittyCompression,
                CapResponse::Status,
            ]
        );
        // Refused compression (`EINVAL`) must not produce KittyCompression.
        assert_eq!(
            parse_str("\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;EINVAL:bad\x1b\\\x1b[0n"),
            vec![CapResponse::Kitty, CapResponse::Status]
        );
    }

    #[test]
    fn test_parse_da1_sixel_with_and_without_param_4() {
        assert_eq!(
            parse_str("\x1b[?62;1;2;4;6;9;15;22c\x1b[0n"),
            vec![CapResponse::Sixel, CapResponse::Status]
        );
        // Parameter "42" must not match "4".
        assert_eq!(
            parse_str("\x1b[?62;1;42;22c\x1b[0n"),
            vec![CapResponse::Status]
        );
        // Plain DA1 without Sixel.
        assert_eq!(parse_str("\x1b[?64;5c\x1b[0n"), vec![CapResponse::Status]);
    }

    #[test]
    fn test_parse_chunked_and_malformed_responses() {
        let chunked = parse_chunked(&[
            "\x1b_Gi=",
            "31;OK",
            "\x1b\\",
            "\x1b[?64;",
            "4c\x1b[6;",
            "18;9t\x1b",
            "[0n",
        ]);
        assert_eq!(
            chunked,
            vec![
                CapResponse::Kitty,
                CapResponse::Sixel,
                CapResponse::CellSize(Some(FontSize::new(9, 18))),
                CapResponse::Status,
            ]
        );

        // Empty and garbage responses.
        assert!(parse_str("").is_empty());
        assert!(parse_str("\x1bhonkey\x1btonkey\x1b[42\x1b\\").is_empty());
        assert_eq!(
            parse_str("\x1bgarbage...\x1b[6;20;10t\x1bgarbage\x1b[0n"),
            vec![
                CapResponse::CellSize(Some(FontSize::new(10, 20))),
                CapResponse::Status,
            ]
        );
    }

    #[test]
    fn test_query_compression_probe_opt_in_and_valid_zlib() {
        let default_q = build_query(false, QueryStdioOptions::default(), false);
        assert!(default_q.contains("_Gi=31"));
        assert!(!default_q.contains("_Gi=32"));
        assert!(default_q.ends_with("\x1b[5n"));

        let comp_q = build_query(
            false,
            QueryStdioOptions {
                kitty_compression: true,
            },
            false,
        );
        let (_, rest) = comp_q
            .split_once("\x1b_Gi=32,s=1,v=1,a=q,t=d,f=24,o=z;")
            .expect("compressed probe should be present");
        let (payload, _) = rest
            .split_once("\x1b\\")
            .expect("compressed probe should be ST-terminated");
        assert_eq!(payload, KITTY_COMPRESSION_PROBE);

        // Decode Base64 payload and inflate with zlib to verify it yields 1 RGB pixel ([0, 0, 0]).
        let decode_b64 = |b64: &str| -> Vec<u8> {
            let val = |b: u8| -> u8 {
                match b {
                    b'A'..=b'Z' => b - b'A',
                    b'a'..=b'z' => b - b'a' + 26,
                    b'0'..=b'9' => b - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => panic!("invalid base64 byte: {b}"),
                }
            };
            let bytes = b64.as_bytes();
            assert_eq!(bytes.len() % 4, 0);
            let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
            for chunk in bytes.as_chunks::<4>().0 {
                let a = val(chunk[0]);
                let b = val(chunk[1]);
                out.push((a << 2) | (b >> 4));
                if chunk[2] != b'=' {
                    let c = val(chunk[2]);
                    out.push((b << 4) | (c >> 2));
                    if chunk[3] != b'=' {
                        let d = val(chunk[3]);
                        out.push((c << 6) | d);
                    }
                }
            }
            out
        };
        let zlib_bytes = decode_b64(payload);
        let mut raw = Vec::new();
        std::io::copy(
            &mut flate2::read::ZlibDecoder::new(zlib_bytes.as_slice()),
            &mut raw,
        )
        .expect("probe payload must be a valid zlib stream");
        assert_eq!(raw, vec![0, 0, 0], "one RGB pixel as f=24,s=1,v=1 promises");

        let tmux_q = build_query(true, QueryStdioOptions::default(), false);
        assert!(tmux_q.starts_with("\x1bPtmux;\x1b\x1b"));
        assert!(tmux_q.ends_with("\x1b\x1b[5n\x1b\\"));

        // Blacklisted terminals (WezTerm/Konsole) omit Kitty and DA1 queries.
        let blacklisted_q = build_query(false, QueryStdioOptions::default(), true);
        assert!(!blacklisted_q.contains("_Gi=31"));
        assert!(!blacklisted_q.contains("\x1b[c"));
        assert!(blacklisted_q.contains("\x1b[16t"));
        assert!(blacklisted_q.contains("\x1b[5n"));
    }

    #[test]
    fn test_env_heuristics_for_known_terminals() {
        let cell = Some(FontSize::new(8, 16));

        // iTerm2 via TERM_PROGRAM
        let iterm_env = env_from_pairs(&[("TERM_PROGRAM", "iTerm.app")]);
        let iterm_picker = resolve_picker_from_responses(
            &[CapResponse::CellSize(cell), CapResponse::Status],
            &iterm_env,
            None,
        );
        assert_eq!(iterm_picker.protocol_type(), ProtocolType::Iterm2);
        assert_eq!(iterm_picker.font_size(), FontSize::new(8, 16));

        // WezTerm: Kitty/Sixel blacklisted, selects iTerm2 via TERM_PROGRAM / WEZTERM_EXECUTABLE
        let wez_env = env_from_pairs(&[
            ("WEZTERM_EXECUTABLE", "/usr/bin/wezterm"),
            ("TERM_PROGRAM", "WezTerm"),
        ]);
        assert!(wez_env.blacklist_kitty_and_sixel);
        let wez_picker = resolve_picker_from_responses(
            &[CapResponse::CellSize(cell), CapResponse::Status],
            &wez_env,
            None,
        );
        assert_eq!(wez_picker.protocol_type(), ProtocolType::Iterm2);

        // Kitty (and Ghostty): responds with _Gi=31;OK + _Gi=32;OK
        let kitty_env = env_from_pairs(&[("TERM", "xterm-kitty")]);
        let kitty_picker = resolve_picker_from_responses(
            &[
                CapResponse::Kitty,
                CapResponse::KittyCompression,
                CapResponse::CellSize(cell),
            ],
            &kitty_env,
            None,
        );
        assert_eq!(kitty_picker.protocol_type(), ProtocolType::Kitty);
        assert!(kitty_picker.supports_kitty_compression());

        let ghostty_env = env_from_pairs(&[("TERM", "xterm-ghostty")]);
        let ghostty_picker = resolve_picker_from_responses(
            &[CapResponse::Kitty, CapResponse::CellSize(cell)],
            &ghostty_env,
            None,
        );
        assert_eq!(ghostty_picker.protocol_type(), ProtocolType::Kitty);
        assert!(!ghostty_picker.supports_kitty_compression());

        // foot: responds with DA1 ;4; -> Sixel
        let foot_env = env_from_pairs(&[("TERM", "foot")]);
        let foot_picker = resolve_picker_from_responses(
            &[CapResponse::Sixel, CapResponse::CellSize(cell)],
            &foot_env,
            None,
        );
        assert_eq!(foot_picker.protocol_type(), ProtocolType::Sixel);

        // Konsole: Kitty & Sixel blacklisted -> Halfblocks
        let konsole_env = env_from_pairs(&[("KONSOLE_VERSION", "230801")]);
        assert!(konsole_env.blacklist_kitty_and_sixel);
        let konsole_picker = resolve_picker_from_responses(
            &[CapResponse::CellSize(cell), CapResponse::Status],
            &konsole_env,
            None,
        );
        assert_eq!(konsole_picker.protocol_type(), ProtocolType::Halfblocks);

        // Unknown terminal without graphics capabilities -> Halfblocks
        let unknown_env = env_from_pairs(&[("TERM", "dumb")]);
        let unknown_picker =
            resolve_picker_from_responses(&[CapResponse::CellSize(cell)], &unknown_env, None);
        assert_eq!(unknown_picker.protocol_type(), ProtocolType::Halfblocks);

        // Inside tmux with outer iTerm2 hint
        let tmux_iterm_env = env_from_pairs(&[
            ("TERM", "tmux-256color"),
            ("ITERM_SESSION_ID", "w0t0p0:123"),
        ]);
        assert!(tmux_iterm_env.is_tmux);
        let tmux_picker = fallback_picker(&tmux_iterm_env, cell);
        assert!(tmux_picker.tmux_detected());
        assert_eq!(tmux_picker.protocol_type(), ProtocolType::Iterm2);

        // Without any font size, even hinted protocols fall back to Halfblocks.
        let no_font_picker = fallback_picker(&tmux_iterm_env, None);
        assert_eq!(no_font_picker.protocol_type(), ProtocolType::Halfblocks);
        assert_eq!(no_font_picker.font_size(), FontSize::new(8, 16));
    }

    #[test]
    fn test_halfblocks_defaults_and_set_protocol_type() {
        let mut picker = Picker::halfblocks();
        assert_eq!(picker.font_size(), FontSize::new(8, 16));
        assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);
        assert!(!picker.tmux_detected());
        assert!(!picker.supports_kitty_compression());

        picker.set_protocol_type(ProtocolType::Sixel);
        assert_eq!(picker.protocol_type(), ProtocolType::Sixel);
    }
}
