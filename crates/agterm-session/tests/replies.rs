//! The session's answers to its program's queries (`server::replies`), on real PTYs. The program is a probe: this test
//! binary run again as the session's program ([`query_probe`]), which puts its terminal in raw mode, writes each query
//! it is given and prints, as `REPLY-<n>:<hex>`, every byte that came back until its input stayed quiet. A test reads
//! those lines from a client's output, or from the server's terminal when no client is attached, and compares them with
//! what a terminal set up as the server's answers (one answer, never two).
//!
//! On Windows the pseudoconsole stands between the probe and the server and answers some queries itself
//! ([`windows_conhost_record`]); the tests whose queries it keeps run on Unix.
//!
//! Every session gets the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables, `TERM` set to
//! `xterm-ghostty`, and a namespace directory of its own (only named in its environment: no server runs).

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot and reply tests alike"
)]
mod support;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agterm_ghostty_vt as vt;
use agterm_session::protocol::{ColorScheme, DelegatedReply, DelegatedRequest, ReplyMetadata, Rgb, SessionSpec};
use agterm_session::server::api::read_screen;
use agterm_session::server::clients::{ClientEvent, RecvError, Subscription};
use agterm_session::server::pane::{Attached, Pane, PaneConfig};
use agterm_session::server::replies::{Facts, Outlet, ServerEffects};
use support::{PATIENCE, SIZE, TestResult, environment};

/// Set in the probe's environment: run [`query_probe`].
const PROBE: &str = "AGTERM_SESSION_TEST_PROBE";
/// The queries, hex, comma-separated.
const QUERIES: &str = "AGTERM_SESSION_PROBE_QUERIES";
/// A directory: the probe writes query `n` once `go-<n>` exists there, and creates `after-<n>` once it wrote what
/// follows the query.
const GATE: &str = "AGTERM_SESSION_PROBE_GATE";
/// How long the probe waits for the first byte of an answer, in milliseconds.
const WAIT: &str = "AGTERM_SESSION_PROBE_WAIT_MS";
/// Hex bytes the probe writes after each query, before it reads the answer.
const AFTER: &str = "AGTERM_SESSION_PROBE_AFTER";
/// KiB of filler lines the probe writes after those bytes.
const FILL: &str = "AGTERM_SESSION_PROBE_FILL_KIB";
/// How long an answer's input stays quiet before the probe takes it as complete.
const QUIET: Duration = Duration::from_millis(700);
/// The `TERM` every probe runs under.
const TERM: &str = "xterm-ghostty";

const DA1: &[u8] = b"\x1b[c";
const DA2: &[u8] = b"\x1b[>c";
const DSR: &[u8] = b"\x1b[5n";
const DECRQM: &[u8] = b"\x1b[?2026$p";
const DECRQSS: &[u8] = b"\x1bP$qm\x1b\\";
const KITTY_KEYBOARD: &[u8] = b"\x1b[?u";
const XTVERSION: &[u8] = b"\x1b[>q";
const XTGETTCAP_TN: &[u8] = b"\x1bP+q544e\x1b\\";
const SCHEME: &[u8] = b"\x1b[?996n";
const BACKGROUND: &[u8] = b"\x1b]11;?\x07";
const CLIPBOARD_READ: &[u8] = b"\x1b]52;c;?\x07";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|at| u8::from_str_radix(text.get(at * 2..at * 2 + 2)?, 16).ok())
        .collect()
}

fn env_ms(name: &str, default: u64) -> Duration {
    Duration::from_millis(
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default),
    )
}

/// Wait for `path` to exist.
fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + PATIENCE;
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Not a test of its own: the session program of the tests below (module docs).
#[test]
#[ignore = "the reply tests' session program"]
fn query_probe() {
    if std::env::var_os(PROBE).is_none() {
        return;
    }
    let queries: Vec<Vec<u8>> = std::env::var(QUERIES)
        .unwrap_or_default()
        .split(',')
        .filter(|query| !query.is_empty())
        .map(unhex)
        .collect();
    let mut after = std::env::var(AFTER).ok().map(|after| unhex(&after));
    if let Some(fill) = std::env::var(FILL).ok().and_then(|fill| fill.parse::<usize>().ok()) {
        let line = format!("{}\r\n", "y".repeat(1022));
        after
            .get_or_insert_with(Vec::new)
            .extend(line.repeat(fill).into_bytes());
    }
    let gate = std::env::var_os(GATE).map(PathBuf::from);
    let wait = env_ms(WAIT, 3000);
    let input = raw::Input::enter();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "PROBE-READY\r\n");
    let _ = out.flush();
    for (index, query) in queries.iter().enumerate() {
        if let Some(gate) = &gate {
            wait_for_file(&gate.join(format!("go-{index}")));
        }
        let _ = out.write_all(query);
        let _ = out.flush();
        if let Some(after) = &after {
            let _ = out.write_all(after);
            let _ = out.flush();
        }
        if let Some(gate) = &gate {
            let _ = std::fs::write(gate.join(format!("after-{index}")), b"");
        }
        let reply = input.collect(wait, QUIET);
        let _ = write!(out, "\r\nREPLY-{index}:{}\r\n", hex(&reply));
        let _ = out.flush();
    }
    let _ = write!(out, "PROBE-DONE\r\n");
    let _ = out.flush();
}

/// The probe's terminal input, raw: every byte its terminal sends, as it comes.
#[cfg(unix)]
mod raw {
    use std::time::{Duration, Instant};

    /// Standard input.
    pub struct Input(i32);

    impl Input {
        /// Standard input in raw mode (no echo, no line discipline).
        pub fn enter() -> Self {
            // SAFETY: `termios` is plain data; zeroed is a valid value for `tcgetattr` to fill.
            let mut termios: libc::termios = unsafe { std::mem::zeroed() };
            // SAFETY: descriptor 0 and a valid out-pointer.
            if unsafe { libc::tcgetattr(0, &raw mut termios) } == 0 {
                // SAFETY: a valid termios.
                unsafe { libc::cfmakeraw(&raw mut termios) };
                // SAFETY: descriptor 0 and a valid termios.
                unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw const termios) };
            }
            Self(0)
        }

        /// What arrives within `wait`, then until it stays quiet for `quiet`.
        pub fn collect(&self, wait: Duration, quiet: Duration) -> Vec<u8> {
            let start = Instant::now();
            let mut got = Vec::new();
            let mut last: Option<Instant> = None;
            loop {
                let limit = last.map_or(start + wait, |last| last + quiet);
                let left = limit.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return got;
                }
                let mut poll = libc::pollfd {
                    fd: self.0,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let timeout = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
                // SAFETY: one valid pollfd.
                if unsafe { libc::poll(&raw mut poll, 1, timeout) } <= 0 {
                    continue;
                }
                let mut buf = [0_u8; 4096];
                // SAFETY: descriptor 0 into a buffer of its length.
                let read = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
                let Ok(read) = usize::try_from(read) else {
                    return got;
                };
                if read == 0 {
                    return got;
                }
                got.extend_from_slice(&buf[..read]);
                last = Some(Instant::now());
            }
        }
    }
}

/// The probe's console input in virtual terminal mode: the characters of its key events, as the console decodes
/// what its terminal sends.
#[cfg(windows)]
mod raw {
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    pub struct Input(HANDLE);

    impl Input {
        /// Console input with virtual terminal input on and nothing else (no echo, no line input), and output with
        /// virtual terminal processing.
        pub fn enter() -> Self {
            // SAFETY: plain calls on the process's standard handles.
            let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            // SAFETY: as above.
            let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            // SAFETY: the console input handle; a failure leaves the mode as it was.
            unsafe { SetConsoleMode(input, ENABLE_VIRTUAL_TERMINAL_INPUT) };
            let mode = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
            // SAFETY: the console output handle.
            unsafe { SetConsoleMode(output, mode) };
            Self(input)
        }

        /// What arrives within `wait`, then until it stays quiet for `quiet`.
        pub fn collect(&self, wait: Duration, quiet: Duration) -> Vec<u8> {
            let start = Instant::now();
            let mut got: Vec<u16> = Vec::new();
            let mut last: Option<Instant> = None;
            loop {
                let limit = last.map_or(start + wait, |last| last + quiet);
                let left = limit.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return String::from_utf16_lossy(&got).into_bytes();
                }
                let timeout = u32::try_from(left.as_millis()).unwrap_or(u32::MAX).max(1);
                // SAFETY: the console input handle.
                if unsafe { WaitForSingleObject(self.0, timeout) } != WAIT_OBJECT_0 {
                    continue;
                }
                if self.read_into(&mut got) {
                    last = Some(Instant::now());
                }
            }
        }

        /// The characters of the key presses waiting; whether there were any.
        fn read_into(&self, got: &mut Vec<u16>) -> bool {
            let mut records = [INPUT_RECORD::default(); 128];
            let mut count = 0_u32;
            // SAFETY: the console input handle and a buffer of `records.len()` records.
            let ok = unsafe { ReadConsoleInputW(self.0, records.as_mut_ptr(), 128, &raw mut count) };
            if ok == 0 {
                return false;
            }
            let before = got.len();
            for record in records.iter().take(count as usize) {
                if u32::from(record.EventType) != KEY_EVENT {
                    continue;
                }
                // SAFETY: a key event's record holds the key member.
                let key = unsafe { record.Event.KeyEvent };
                // SAFETY: the console fills the UTF-16 member for the W API.
                let unit = unsafe { key.uChar.UnicodeChar };
                if key.bKeyDown != 0 && unit != 0 {
                    got.extend(std::iter::repeat_n(unit, usize::from(key.wRepeatCount.max(1))));
                }
            }
            got.len() > before
        }
    }
}

/// The probe as a session's program, writing `queries`, with `extra` in its environment.
fn probe(queries: &[&[u8]], extra: &[(&str, String)]) -> SessionSpec {
    let exe = std::env::current_exe()
        .map(|exe| exe.to_string_lossy().into_owned())
        .unwrap_or_default();
    let argv = [exe.as_str(), "--ignored", "--exact", "query_probe", "--nocapture"];
    let mut env = environment();
    env.push(("TERM".to_owned(), TERM.to_owned()));
    env.push((PROBE.to_owned(), "1".to_owned()));
    let queries: Vec<String> = queries.iter().map(|query| hex(query)).collect();
    env.push((QUERIES.to_owned(), queries.join(",")));
    for (name, value) in extra {
        env.push(((*name).to_owned(), value.clone()));
    }
    SessionSpec {
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        env,
        cwd: None,
        tracked_env: Vec::new(),
    }
}

fn pane_config(name: &str, namespace: &Path, spec: SessionSpec) -> PaneConfig {
    PaneConfig::new(name, namespace, spec, SIZE)
}

/// The answers the probe reported in `text`, by query.
fn replies(text: &str) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    while let Some(at) = text.find(&format!("REPLY-{}:", found.len())) {
        let digits: String = text[at..]
            .split_once(':')
            .map(|(_, rest)| rest.chars().take_while(char::is_ascii_hexdigit).collect())
            .unwrap_or_default();
        found.push(unhex(&digits));
    }
    found
}

/// The text of the server's terminal, scrollback included.
fn server_text(pane: &Pane) -> String {
    pane.with_terminal(|terminal| read_screen(terminal, 0, true).map(|screen| screen.text))
        .unwrap_or_default()
}

/// Wait until the server's terminal shows `needle`.
fn server_expects(pane: &Pane, needle: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let text = server_text(pane);
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "no {needle:?} on the server's screen: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What a terminal set up as the server's (`facts`, the probe's `TERM`, the session's grid) answers `query` with.
fn reference(facts: &Facts, query: &[u8]) -> TestResult<Vec<u8>> {
    let outlet = Arc::new(Outlet::default());
    outlet.set_scheme(facts.color_scheme);
    let mut terminal = vt::Terminal::new(SIZE.cols, SIZE.rows)?;
    terminal.set_terminfo_name(Some(TERM))?;
    facts.apply(&mut terminal, None, SIZE.cols, SIZE.rows)?;
    terminal.set_effects(Box::new(ServerEffects::new(Arc::clone(&outlet))))?;
    terminal.vt_write(query);
    Ok(outlet.take().concat())
}

/// [`reference`] of each query.
fn references(facts: &Facts, queries: &[&[u8]]) -> TestResult<Vec<Vec<u8>>> {
    queries.iter().map(|query| reference(facts, query)).collect()
}

fn shown(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A client attached to a pane, its events read into a terminal of its own whose answers it keeps: the Kitty
/// graphics ones it gave since its previous barrier complete each barrier it is sent, as an attach client's do.
struct Viewer {
    subscription: Subscription,
    terminal: vt::Terminal,
    answers: Arc<Mutex<Vec<Vec<u8>>>>,
    epoch: u64,
    owner: Option<u32>,
    output: Vec<u8>,
    delegated: Vec<(u64, u64, DelegatedRequest)>,
    /// The barriers it was sent, by id and epoch.
    barriers: Vec<(u64, u64)>,
    /// What each barrier was completed with, and what [`Pane::complete`] said.
    completed: Vec<(Vec<u8>, bool)>,
}

struct Answers(Arc<Mutex<Vec<Vec<u8>>>>);

impl vt::Effects for Answers {
    fn write_pty(&mut self, data: &[u8]) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(data.to_vec());
    }
}

impl Viewer {
    /// A viewer of `attached`, its terminal reading Kitty images from temporary files in `temp`.
    fn new(attached: Attached, temp: Option<&Path>) -> TestResult<Self> {
        let answers = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = vt::Terminal::new(attached.size.cols, attached.size.rows)?;
        terminal.set_kitty_image_storage_limit(64 * 1024 * 1024)?;
        let temp = temp.map(|temp| temp.to_string_lossy().into_owned());
        terminal.set_kitty_image_media(false, temp.as_deref(), false)?;
        terminal.resize(attached.size.cols, attached.size.rows, 10, 20)?;
        terminal.set_effects(Box::new(Answers(Arc::clone(&answers))))?;
        Ok(Self {
            subscription: attached.subscription,
            terminal,
            answers,
            epoch: 0,
            owner: None,
            output: Vec::new(),
            delegated: Vec::new(),
            barriers: Vec::new(),
            completed: Vec::new(),
        })
    }

    fn id(&self) -> u64 {
        self.subscription.id()
    }

    /// Receive one event within `timeout`, completing a barrier with its terminal's Kitty answers; false when none
    /// came.
    fn receive(&mut self, pane: &Pane, timeout: Duration) -> bool {
        let event = match self.subscription.recv_timeout(timeout) {
            Ok(event) => event,
            Err(RecvError::Timeout | RecvError::Closed) => return false,
        };
        match event {
            ClientEvent::Snapshot { epoch, data, .. } => {
                self.epoch = epoch;
                self.terminal.vt_write(&data);
                // a snapshot's answers are none of the program's
                self.take_answers();
            }
            ClientEvent::Output { data, .. } => {
                self.output.extend_from_slice(&data);
                self.terminal.vt_write(&data);
            }
            ClientEvent::ReplyOwner { owner, generation } => {
                // only answers given while it owns the replies are the program's
                self.take_answers();
                self.owner = owner.then_some(generation);
            }
            ClientEvent::Delegated { id, epoch, request } => self.delegated.push((id, epoch, request)),
            ClientEvent::KittyBarrier { id, epoch } => {
                self.barriers.push((id, epoch));
                let answers = self.take_answers();
                let taken = pane.complete(self.id(), id, epoch, &answers);
                self.completed.push((answers, taken));
            }
            _ => {}
        }
        true
    }

    /// Its terminal's Kitty answers since the last call (its other answers are the server's to give).
    fn take_answers(&self) -> Vec<u8> {
        let answers = std::mem::take(&mut *self.answers.lock().unwrap_or_else(PoisonError::into_inner));
        answers
            .iter()
            .filter(|answer| answer.starts_with(b"\x1b_G"))
            .flatten()
            .copied()
            .collect()
    }

    fn drain(&mut self, pane: &Pane) {
        while self.receive(pane, Duration::ZERO) {}
    }

    /// Read until `done` holds of the viewer.
    fn until(&mut self, pane: &Pane, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !done(self) {
            assert!(Instant::now() < deadline, "no {what}: {:?}", shown(&self.output));
            self.receive(pane, Duration::from_millis(50));
        }
    }

    fn expect(&mut self, pane: &Pane, needle: &str) {
        self.until(pane, needle, |viewer| shown(&viewer.output).contains(needle));
    }
}

fn metadata() -> ReplyMetadata {
    ReplyMetadata {
        foreground: Some(Rgb {
            r: 0x11,
            g: 0x22,
            b: 0x33,
        }),
        background: Some(Rgb {
            r: 0xfe,
            g: 0xdc,
            b: 0xba,
        }),
        cursor: None,
        palette: Vec::new(),
        cell_width_px: 10,
        cell_height_px: 20,
        kitty_graphics: true,
        color_scheme: Some(ColorScheme::Dark),
    }
}

#[cfg(unix)]
fn managed(nonce: &str, claim: bool) -> agterm_session::protocol::Managed {
    agterm_session::protocol::Managed {
        nonce: nonce.to_owned(),
        claim,
    }
}

/// The queries a program's terminal sends the server and gets the server's answer back from: on Windows, those the
/// pseudoconsole passes on to the server and whose answers it lets through ([`windows_conhost_record`]).
#[cfg(unix)]
const QUERY_SET: [&[u8]; 10] = [
    DA1,
    DA2,
    DSR,
    DECRQM,
    XTVERSION,
    XTGETTCAP_TN,
    SCHEME,
    BACKGROUND,
    DECRQSS,
    KITTY_KEYBOARD,
];
#[cfg(windows)]
const QUERY_SET: [&[u8]; 5] = [DSR, DECRQSS, XTVERSION, KITTY_KEYBOARD, SCHEME];

#[test]
fn every_query_gets_one_answer_with_two_clients_attached() -> TestResult {
    let dir = tempfile::tempdir()?;
    let gate = tempfile::tempdir()?;
    let spec = probe(&QUERY_SET, &gated(gate.path(), 3000));
    let (pane, creator) = Pane::spawn(pane_config("two", dir.path(), spec))?;
    let mut one = Viewer::new(creator, None)?;
    let mut two = Viewer::new(pane.try_attach(SIZE, None)?, None)?;
    // the queries once both are attached
    open(gate.path(), QUERY_SET.len())?;
    one.expect(&pane, "PROBE-DONE");
    two.drain(&pane);
    let got = replies(&shown(&one.output));
    let facts = Facts::default();
    let expected = references(&facts, &QUERY_SET)?;
    assert_eq!(got, expected);
    // Ghostty's identity, Ghostty's default background, this session's TERM
    #[cfg(unix)]
    {
        assert_eq!(got[0], b"\x1b[?62;22;52c");
        assert!(shown(&got[4]).starts_with("\x1bP>|ghostty "), "{:?}", shown(&got[4]));
        let tn = format!("\x1bP1+r544E={}\x1b\\", hex(TERM.as_bytes()).to_uppercase());
        assert_eq!(got[5], tn.into_bytes());
        assert_eq!(got[6], b"\x1b[?997;2n");
        assert_eq!(got[7], b"\x1b]11;rgb:2828/2c2c/3434\x07");
    }
    assert_eq!(replies(&shown(&two.output)), got, "both clients see the same stream");
    Ok(())
}

#[test]
fn every_query_gets_one_answer_with_no_client_attached() -> TestResult {
    let dir = tempfile::tempdir()?;
    let gate = tempfile::tempdir()?;
    let extra = [(GATE, gate.path().to_string_lossy().into_owned())];
    let (pane, creator) = Pane::spawn(pane_config("none", dir.path(), probe(&QUERY_SET, &extra)))?;
    drop(creator);
    assert_eq!(pane.clients(), 0);
    open(gate.path(), QUERY_SET.len())?;
    let text = server_expects(&pane, "PROBE-DONE");
    let facts = Facts::default();
    let expected = references(&facts, &QUERY_SET)?;
    assert_eq!(replies(&text), expected);
    Ok(())
}

/// Let the gated probe write its query `index`.
fn go(gate: &Path, index: usize) -> TestResult {
    std::fs::write(gate.join(format!("go-{index}")), b"")?;
    Ok(())
}

/// Let the gated probe write its first `count` queries.
fn open(gate: &Path, count: usize) -> TestResult {
    (0..count).try_for_each(|index| go(gate, index))
}

fn gated(gate: &Path, wait_ms: u64) -> Vec<(&'static str, String)> {
    vec![(GATE, gate.to_string_lossy().into_owned()), (WAIT, wait_ms.to_string())]
}

/// The theme queries: OSC 10 and 11, XTWINOPS `14 t`, `CSI ? 996 n`; on Windows the last two (the pseudoconsole keeps
/// the colour queries, [`windows_conhost_record`]).
#[cfg(unix)]
const THEME: &[u8] = b"\x1b]10;?\x07\x1b]11;?\x07\x1b[14t\x1b[?996n";
#[cfg(windows)]
const THEME: &[u8] = b"\x1b[14t\x1b[?996n";
const DARK_REPORT: &[u8] = b"\x1b[?997;1n";
const LIGHT_REPORT: &[u8] = b"\x1b[?997;2n";

#[test]
fn theme_colours_cell_size_and_appearance_come_from_the_owner_else_the_defaults() -> TestResult {
    let dir = tempfile::tempdir()?;
    let gate = tempfile::tempdir()?;
    // the program asks for colour scheme reports first (no answer), then the theme three times
    let queries: [&[u8]; 4] = [b"\x1b[?2031h", THEME, THEME, THEME];
    let spec = probe(&queries, &gated(gate.path(), 1500));
    let (pane, creator) = Pane::spawn(pane_config("theme", dir.path(), spec))?;
    let owner = creator.subscription.id();
    go(gate.path(), 0)?;
    go(gate.path(), 1)?;
    server_expects(&pane, "REPLY-1:");
    pane.set_metadata(owner, metadata());
    go(gate.path(), 2)?;
    server_expects(&pane, "REPLY-2:");
    // the owner leaves: no client owns the replies, the defaults answer
    drop(creator);
    assert_eq!(pane.reply_owner().0, None);
    go(gate.path(), 3)?;
    let got = replies(&server_expects(&pane, "PROBE-DONE"));
    let defaults = reference(&Facts::default(), THEME)?;
    let owners = reference(&Facts::of(Some(&metadata())), THEME)?;
    assert_ne!(defaults, owners);
    assert_eq!(got[0], b"");
    assert_eq!(got[1], defaults);
    assert_eq!(got[2], [DARK_REPORT, &owners].concat(), "{:?}", shown(&got[2]));
    assert_eq!(got[3], [LIGHT_REPORT, &defaults].concat(), "{:?}", shown(&got[3]));
    #[cfg(unix)]
    assert!(shown(&got[2]).contains("\x1b]11;rgb:fefe/dcdc/baba\x07"));
    assert!(shown(&got[2]).contains(&format!("\x1b[4;{};{}t", 20 * SIZE.rows, 10 * SIZE.cols)));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_clipboard_read_goes_to_the_owner_alone_and_the_output_after_it_waits_for_the_answer() -> TestResult {
    let dir = tempfile::tempdir()?;
    let gate = tempfile::tempdir()?;
    let mut extra = gated(gate.path(), 30_000);
    extra.push((AFTER, hex(b"AFTER-READ\r\n")));
    // more than any PTY buffers: written while the read waits, the PTY read meanwhile
    extra.push((FILL, "300".to_owned()));
    let query = [b"BEFORE-READ\r\n".as_slice(), CLIPBOARD_READ].concat();
    let (pane, creator) = Pane::spawn(pane_config("read", dir.path(), probe(&[&query], &extra)))?;
    let mut owner = Viewer::new(creator, None)?;
    let mut other = Viewer::new(pane.try_attach(SIZE, None)?, None)?;
    go(gate.path(), 0)?;
    owner.until(&pane, "a delegated read", |viewer| !viewer.delegated.is_empty());
    wait_for_file(&gate.path().join("after-0"));
    assert!(
        gate.path().join("after-0").exists(),
        "the program's output after the read was not read"
    );
    owner.drain(&pane);
    other.drain(&pane);
    for viewer in [&owner, &other] {
        assert!(shown(&viewer.output).contains("BEFORE-READ"));
        assert!(
            !shown(&viewer.output).contains("AFTER-READ"),
            "output went past the waiting read"
        );
    }
    assert!(other.delegated.is_empty(), "a client not owning the replies was asked");
    let (id, epoch, request) = owner.delegated[0].clone();
    let selection = "c".to_owned();
    assert_eq!(
        request,
        DelegatedRequest::ClipboardRead {
            selection,
            list_only: false
        }
    );
    let text = |text: &str| DelegatedReply::Clipboard {
        text: Some(text.to_owned()),
    };
    assert!(
        !pane.answer(other.id(), id, epoch, text("other")),
        "another client's answer"
    );
    assert!(
        !pane.answer(owner.id(), id, epoch + 1, text("epoch")),
        "another epoch's answer"
    );
    assert!(
        !pane.answer(owner.id(), id + 1, epoch, text("id")),
        "another request's answer"
    );
    assert!(pane.answer(owner.id(), id, epoch, text("hello")));
    assert!(!pane.answer(owner.id(), id, epoch, text("again")), "a second answer");
    owner.expect(&pane, "PROBE-DONE");
    other.expect(&pane, "PROBE-DONE");
    assert_eq!(replies(&shown(&owner.output)), [b"\x1b]52;c;aGVsbG8=\x07".to_vec()]);
    assert_eq!(replies(&shown(&other.output)), replies(&shown(&owner.output)));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_clipboard_read_is_denied_by_its_owner_its_deadline_an_owner_change_or_no_owner() -> TestResult {
    let dir = tempfile::tempdir()?;
    let gate = tempfile::tempdir()?;
    let queries = [CLIPBOARD_READ; 4];
    let mut config = pane_config("deny", dir.path(), probe(&queries, &gated(gate.path(), 30_000)));
    config.delegation_wait = Duration::from_secs(1);
    let (pane, creator) = Pane::spawn(config)?;
    let mut owner = Viewer::new(creator, None)?;
    // denied by the owner
    go(gate.path(), 0)?;
    owner.until(&pane, "the first read", |viewer| viewer.delegated.len() == 1);
    let (id, epoch, _) = owner.delegated[0].clone();
    assert!(pane.answer(owner.id(), id, epoch, DelegatedReply::Denied));
    owner.expect(&pane, "REPLY-0:");
    // unanswered: denied at its deadline, a late answer refused; the clock starts before the probe writes the read,
    // so before the server starts the read's deadline
    let asked = Instant::now();
    go(gate.path(), 1)?;
    owner.until(&pane, "the second read", |viewer| viewer.delegated.len() == 2);
    owner.expect(&pane, "REPLY-1:");
    assert!(
        asked.elapsed() >= Duration::from_millis(900),
        "denied before its deadline"
    );
    let (id, epoch, _) = owner.delegated[1].clone();
    assert!(!pane.answer(owner.id(), id, epoch, DelegatedReply::Denied));
    // a managed client claiming the lead takes the replies: the read waiting for the old owner is denied
    go(gate.path(), 2)?;
    owner.until(&pane, "the third read", |viewer| viewer.delegated.len() == 3);
    let claimer = Viewer::new(pane.try_attach(SIZE, Some(&managed("claimer", true)))?, None)?;
    owner.expect(&pane, "REPLY-2:");
    let (id, epoch, _) = owner.delegated[2].clone();
    assert!(!pane.answer(owner.id(), id, epoch, DelegatedReply::Denied));
    assert_eq!(owner.owner, None, "told it no longer owns the replies");
    // the claimer leaves: neither client is managed and none leads, so none owns the replies; denied at once
    drop(claimer);
    assert_eq!(pane.reply_owner().0, None);
    go(gate.path(), 3)?;
    owner.expect(&pane, "PROBE-DONE");
    assert_eq!(owner.delegated.len(), 3, "a read was delegated with no owner");
    let empty = b"\x1b]52;c;\x07".to_vec();
    assert_eq!(
        replies(&shown(&owner.output)),
        [empty.clone(), empty.clone(), empty.clone(), empty]
    );
    Ok(())
}

/// Standard base64 with padding.
#[cfg(unix)]
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let byte = |at: usize| u32::from(chunk.get(at).copied().unwrap_or(0));
        let triple = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(ALPHABET[(triple >> (18 - 6 * index)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(unix)]
#[test]
fn a_temporary_file_image_is_read_by_the_owner_alone_and_acknowledged_once() -> TestResult {
    let dir = tempfile::tempdir()?;
    let temp = tempfile::tempdir()?;
    let temp_dir = temp.path().canonicalize()?;
    let image = temp_dir.join("tty-graphics-protocol-test.rgb");
    std::fs::write(&image, [0xff, 0, 0])?;
    let path = base64(image.to_string_lossy().as_bytes());
    let query = format!("\x1b_Gf=24,s=1,v=1,t=t,a=T,i=7;{path}\x1b\\");
    let gate = tempfile::tempdir()?;
    let spec = probe(&[query.as_bytes()], &gated(gate.path(), 3000));
    let mut config = pane_config("kitty-t", dir.path(), spec);
    config.metadata = metadata();
    config.kitty_wait = PATIENCE;
    let (pane, creator) = Pane::spawn(config)?;
    let mut owner = Viewer::new(creator, Some(&temp_dir))?;
    // a client that does not show images from temporary files: its terminal's error answer is not the program's
    let mut other = Viewer::new(pane.try_attach(SIZE, None)?, None)?;
    open(gate.path(), 1)?;
    owner.expect(&pane, "PROBE-DONE");
    other.expect(&pane, "PROBE-DONE");
    assert_eq!(replies(&shown(&owner.output)), [b"\x1b_Gi=7;OK\x1b\\".to_vec()]);
    assert_eq!(owner.completed, [(b"\x1b_Gi=7;OK\x1b\\".to_vec(), true)]);
    assert!(
        other.barriers.is_empty(),
        "a client not owning the replies was sent a barrier"
    );
    assert!(!image.exists(), "the owner's terminal took the temporary file");
    let shown_image = owner
        .terminal
        .kitty_graphics()
        .is_some_and(|graphics| graphics.image(7).is_some());
    assert!(shown_image, "the owner's terminal has the image");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_direct_image_moves_the_servers_cursor_as_the_owners_and_is_acknowledged_once() -> TestResult {
    let dir = tempfile::tempdir()?;
    // 20 x 40 pixels: 2 x 2 cells of the owner's 10 x 20
    let pixels = base64(&[0x80; 20 * 40 * 3]);
    let query = format!("\x1b_Gf=24,s=20,v=40,t=d,a=T,i=9;{pixels}\x1b\\");
    let gate = tempfile::tempdir()?;
    let spec = probe(&[query.as_bytes()], &gated(gate.path(), 3000));
    let mut config = pane_config("kitty-d", dir.path(), spec);
    config.metadata = metadata();
    config.kitty_wait = PATIENCE;
    let (pane, creator) = Pane::spawn(config)?;
    let mut owner = Viewer::new(creator, None)?;
    let mut other = Viewer::new(pane.try_attach(SIZE, None)?, None)?;
    open(gate.path(), 1)?;
    owner.expect(&pane, "PROBE-DONE");
    other.expect(&pane, "PROBE-DONE");
    assert_eq!(replies(&shown(&owner.output)), [b"\x1b_Gi=9;OK\x1b\\".to_vec()]);
    let ok = b"\x1b_Gi=9;OK\x1b\\".to_vec();
    assert_eq!(owner.completed, [(ok.clone(), true)]);
    assert!(
        other.barriers.is_empty(),
        "a client not owning the replies was sent a barrier"
    );
    let (id, epoch) = owner.barriers[0];
    assert!(
        !pane.complete(other.id(), id, epoch, &ok),
        "another client's completion"
    );
    // both terminals at the session's end, where the program's last output left them
    assert!(pane.wait_exit(PATIENCE).is_some());
    owner.drain(&pane);
    let server = pane.with_terminal(|terminal| terminal.cursor());
    assert_eq!(
        server,
        owner.terminal.cursor(),
        "the server's cursor stands where the owner's does"
    );
    Ok(())
}

/// Kitty's documented detection: a query, then DA1, in one write; the program takes DA1 coming first as no support.
#[cfg(unix)]
const KITTY_QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";

#[cfg(unix)]
#[test]
fn a_kitty_query_is_answered_before_the_queries_after_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let gate = tempfile::tempdir()?;
    // answered by the owner; then a transmission it answers nothing to; then the query once the owner says it shows
    // no Kitty graphics, which no barrier waits for
    let silent: &[u8] = b"\x1b_Gi=32,s=1,v=1,a=T,t=d,f=24,q=2;AAAA\x1b\\\x1b[c";
    let spec = probe(&[KITTY_QUERY, silent, KITTY_QUERY], &gated(gate.path(), 3000));
    let mut config = pane_config("kitty-order", dir.path(), spec);
    config.metadata = metadata();
    config.kitty_wait = PATIENCE;
    let (pane, creator) = Pane::spawn(config)?;
    let mut owner = Viewer::new(creator, None)?;
    go(gate.path(), 0)?;
    owner.expect(&pane, "REPLY-0:");
    go(gate.path(), 1)?;
    owner.expect(&pane, "REPLY-1:");
    let without = ReplyMetadata {
        kitty_graphics: false,
        ..metadata()
    };
    pane.set_metadata(owner.id(), without);
    go(gate.path(), 2)?;
    owner.expect(&pane, "PROBE-DONE");
    let ok = b"\x1b_Gi=31;OK\x1b\\".to_vec();
    let da1 = b"\x1b[?62;22;52c".to_vec();
    assert_eq!(
        replies(&shown(&owner.output)),
        [[ok.clone(), da1.clone()].concat(), da1.clone(), da1],
        "{:?}",
        shown(&owner.output)
    );
    assert_eq!(owner.completed, [(ok, true), (Vec::new(), true)]);
    Ok(())
}

/// The queries [`windows_conhost_record`] records, by name.
#[cfg(windows)]
const RECORDED: [(&str, &[u8]); 19] = [
    ("DA1", DA1),
    ("DA2", DA2),
    ("DA3", b"\x1b[=c"),
    ("DSR", DSR),
    ("CPR", b"\x1b[6n"),
    ("DECRQM 2026", DECRQM),
    ("DECRQSS SGR", DECRQSS),
    ("XTVERSION", XTVERSION),
    ("XTGETTCAP TN", XTGETTCAP_TN),
    ("Kitty keyboard flags", KITTY_KEYBOARD),
    ("colour scheme", SCHEME),
    ("OSC 10", b"\x1b]10;?\x07"),
    ("OSC 11", BACKGROUND),
    ("OSC 12", b"\x1b]12;?\x07"),
    ("OSC 4", b"\x1b]4;1;?\x07"),
    ("XTWINOPS 14 t", b"\x1b[14t"),
    ("XTWINOPS 18 t", b"\x1b[18t"),
    ("OSC 52 read", CLIPBOARD_READ),
    ("Kitty graphics query", b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"),
];

/// What the pseudoconsole does with each query a program writes: whether the query reaches its output pipe (the
/// server's terminal), and who answered the program: the server (its answer, as [`reference`] gives it), conhost
/// (another answer), or nobody. Recorded, not asserted: the table is printed (`--nocapture`), and the module docs of
/// `server::replies` keep it per Windows version.
#[cfg(windows)]
#[test]
fn windows_conhost_record() -> TestResult {
    let dir = tempfile::tempdir()?;
    let queries: Vec<&[u8]> = RECORDED.iter().map(|(_, query)| *query).collect();
    let (pane, creator) = Pane::spawn(pane_config(
        "record",
        dir.path(),
        probe(&queries, &[(WAIT, "2000".to_owned())]),
    ))?;
    let mut owner = Viewer::new(creator, None)?;
    let deadline = Instant::now() + PATIENCE * 3;
    while !shown(&owner.output).contains("PROBE-DONE") {
        assert!(
            Instant::now() < deadline,
            "the probe never finished: {:?}",
            shown(&owner.output)
        );
        owner.receive(&pane, Duration::from_millis(50));
        // a delegated read is denied at once
        for (id, epoch, _) in std::mem::take(&mut owner.delegated) {
            let _ = pane.answer(owner.id(), id, epoch, DelegatedReply::Denied);
        }
    }
    let got = replies(&shown(&owner.output));
    let facts = Facts::default();
    let mut table = format!("conhost record ({}):\n", windows_version());
    for (index, (name, query)) in RECORDED.iter().enumerate() {
        let reached = owner.output.windows(query.len()).any(|window| window == *query);
        let answer = got.get(index).cloned().unwrap_or_default();
        let by = if answer.is_empty() {
            "nobody"
        } else if answer == reference(&facts, query)? {
            "the server"
        } else {
            "conhost"
        };
        let _ = writeln!(
            table,
            "{name}: reached the pipe: {reached}; answered by {by}: {:?}",
            shown(&answer)
        );
    }
    eprintln!("{table}");
    Ok(())
}

/// The Windows version this runs on, as `ver` prints it.
#[cfg(windows)]
fn windows_version() -> String {
    std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "ver"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}
