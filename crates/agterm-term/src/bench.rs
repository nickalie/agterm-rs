//! Measurements of the read path and the VT lock (Task 9's profiling and lock-latency check), run by hand:
//!
//! ```text
//! TERM_BENCH_DIR=<scratch dir> [TERM_BENCH_ZMX=<zmx binary>] [TERM_BENCH_SESSION=<agterm-session binary>] \
//!   cargo test --release -p agterm-term --lib bench -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Each scenario runs a pane with a UI stand-in: a paint thread calling [`Pane::frame`] every 16 ms and a key thread
//! taking the VT lock every 4 ms the way key encoding will (Task 10), and prints one JSON line. Scenarios: a `cat` of a
//! 50 MiB text file on a plain shell, through `zmx attach` (private `ZMX_DIR` under the scratch dir, reaped by it) and
//! in a live pane attached in process to the session server (a private namespace, reaped through its server), a
//! burst of Kitty PNG transmissions (1920x1080), resizes that reflow a full scrollback, and the session server's idle
//! memory for N sessions that each printed 20 000 lines (`idle_sessions_memory`).
//!
//! Linux only: it reads `/proc` and runs zmx.
#![cfg(target_os = "linux")]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::pane::{Pane, PaneConfig, lock};
use super::pty::{GridSize, PaneCommand};

const SIZE: GridSize = GridSize {
    cols: 141,
    rows: 41,
    cell_width: 9,
    cell_height: 18,
};
const FRAME: Duration = Duration::from_millis(16);
const KEY: Duration = Duration::from_millis(4);

#[derive(Debug, Default)]
struct Ui {
    paints: u64,
    stale: u64,
    max_paint: Duration,
    max_stale_streak: Duration,
    key_waits: Vec<Duration>,
}

/// Paint and key stand-ins until `done`.
fn run_ui(pane: &Pane, done: &AtomicBool) -> Ui {
    std::thread::scope(|scope| {
        let keys = scope.spawn(|| {
            let mut waits = Vec::new();
            while !done.load(Ordering::Acquire) {
                let started = Instant::now();
                drop(lock(&pane.shared().vt));
                waits.push(started.elapsed());
                std::thread::sleep(KEY);
            }
            waits
        });
        let mut ui = Ui::default();
        let mut stale_since: Option<Instant> = None;
        while !done.load(Ordering::Acquire) {
            let started = Instant::now();
            let snapshot = pane.frame();
            ui.max_paint = ui.max_paint.max(started.elapsed());
            ui.paints += 1;
            if snapshot.stale {
                ui.stale += 1;
                let since = *stale_since.get_or_insert(started);
                ui.max_stale_streak = ui.max_stale_streak.max(started.elapsed() + (started - since));
            } else {
                stale_since = None;
            }
            std::thread::sleep(FRAME.saturating_sub(started.elapsed()));
        }
        ui.key_waits = keys.join().unwrap();
        ui
    })
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn report(name: &str, pane: &Pane, ui: &mut Ui, extra: &str) {
    ui.key_waits.sort_unstable();
    let percentile = |percent: usize| {
        let index = ui.key_waits.len().saturating_sub(1) * percent / 100;
        ui.key_waits.get(index).copied().unwrap_or_default()
    };
    let stats = pane.stats();
    println!(
        "{{\"scenario\":\"{name}\"{extra},\"bytes\":{},\"batches\":{},\"wakes\":{},\"vt_s\":{:.3},\
         \"max_io_hold_ms\":{:.2},\"paints\":{},\"stale_paints\":{},\"max_stale_streak_ms\":{:.2},\"max_paint_ms\":{:.2},\
         \"key_lock_wait_p99_ms\":{:.3},\"key_lock_wait_max_ms\":{:.3}}}",
        stats.bytes,
        stats.batches,
        stats.wakes,
        Duration::from_nanos(stats.vt_nanos).as_secs_f64(),
        ms(Duration::from_nanos(stats.max_hold_nanos)),
        ui.paints,
        ui.stale,
        ms(ui.max_stale_streak),
        ms(ui.max_paint),
        ms(percentile(99)),
        ms(ui.key_waits.last().copied().unwrap_or_default()),
    );
}

/// `TERM_BENCH_SIZE=COLSxROWS` overrides the grid.
fn bench_size() -> GridSize {
    let parsed = std::env::var("TERM_BENCH_SIZE").ok().and_then(|value| {
        let (cols, rows) = value.split_once('x')?;
        Some(GridSize {
            cols: cols.parse().ok()?,
            rows: rows.parse().ok()?,
            ..SIZE
        })
    });
    parsed.unwrap_or(SIZE)
}

fn bench_dir() -> Option<PathBuf> {
    std::env::var_os("TERM_BENCH_DIR").map(PathBuf::from)
}

/// 50 MiB of printable lines (~80 columns), made once.
fn text_file(dir: &Path) -> PathBuf {
    let path = dir.join("text-50m.txt");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= 50 * 1024 * 1024) {
        return path;
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut written = 0_u64;
    let mut line = String::new();
    for n in 0_u64.. {
        line.clear();
        write!(
            line,
            "{n:>10} the quick brown fox jumps over the lazy dog {:016x} lorem ipsum",
            n.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        )
        .unwrap();
        line.push('\n');
        out.write_all(line.as_bytes()).unwrap();
        written += line.len() as u64;
        if written >= 50 * 1024 * 1024 {
            break;
        }
    }
    out.flush().unwrap();
    path
}

/// `count` Kitty PNG transmissions with placement (`a=T,f=100`), base64 in 4096-byte chunks as `show-image.sh` sends
/// them; the image is noise, so it does not compress away.
fn kitty_file(dir: &Path, count: u32) -> PathBuf {
    use base64::Engine as _;
    let path = dir.join(format!("kitty-{count}.bin"));
    if path.exists() {
        return path;
    }
    let (width, height) = (1920_u32, 1080_u32);
    let mut pixels = vec![0_u8; (width * height * 4) as usize];
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    for byte in &mut pixels {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state.to_le_bytes()[0];
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels).unwrap();
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    let mut out = Vec::new();
    for id in 1..=count {
        let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(4096).collect();
        for (index, chunk) in chunks.iter().enumerate() {
            let more = u8::from(index + 1 < chunks.len());
            let control = if index == 0 {
                format!("a=T,f=100,i={id},c=40,r=12,q=2,m={more}")
            } else {
                format!("m={more}")
            };
            out.extend_from_slice(b"\x1b_G");
            out.extend_from_slice(control.as_bytes());
            out.push(b';');
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\x1b\\");
        }
    }
    std::fs::write(&path, out).unwrap();
    path
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

fn command(script: &str, zmx: Option<(&Path, &Path, &str)>) -> PaneCommand {
    let mut command = match zmx {
        Some((zmx, dir, session)) => PaneCommand::new(zmx)
            .arg("attach")
            .arg(session)
            .arg("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("ZMX_DIR", dir),
        None => PaneCommand::new("/bin/sh").arg("-c").arg(script),
    };
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if (name.starts_with("AGTERM_") || name.starts_with("ZMX_")) && !(zmx.is_some() && name == "ZMX_DIR") {
            command = command.env_remove(key);
        }
    }
    command
}

/// Kill every process whose environment carries `ZMX_DIR=<dir>` (the bench's own zmx only).
fn reap_zmx(dir: &Path) {
    let needle = format!("ZMX_DIR={}", dir.display());
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environ.split(|byte| *byte == 0).any(|var| var == needle.as_bytes()) {
            // SAFETY: plain kill(2) of a process carrying the bench's own ZMX_DIR.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// Run `script` (which ends by printing `MARK`) to completion under the UI stand-ins; the wall time to the mark.
fn run_to_mark(pane: &Pane) -> (Duration, Ui, String) {
    let done = AtomicBool::new(false);
    let started = Instant::now();
    let (ui, screen) = std::thread::scope(|scope| {
        let ui = scope.spawn(|| run_ui(pane, &done));
        let screen = loop {
            let frame = pane.shared().published_frame();
            if let Some(line) = frame.lines().into_iter().find(|line| line.contains("MARK")) {
                break line;
            }
            assert!(started.elapsed() < Duration::from_secs(120), "no mark");
            std::thread::sleep(Duration::from_millis(2));
        };
        done.store(true, Ordering::Release);
        (ui.join().unwrap(), screen)
    });
    (started.elapsed(), ui, screen)
}

/// What a `cat` scenario's pane runs its script through.
#[derive(Clone, Copy)]
enum Via<'a> {
    Plain,
    /// `zmx attach`: the binary, its `ZMX_DIR`.
    Zmx(&'a Path, &'a Path),
    /// A live pane attached in process: the `agterm-session` binary, the namespace directory.
    Live(&'a Path, &'a Path),
}

/// The bench's environment: the test's, without the live agterm's variables.
fn environment() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| !key.starts_with("AGTERM_") && !key.starts_with("ZMX_"))
        .collect()
}

/// A live pane of a new session `name` running `/bin/sh -c script` on the server `server` of namespace `namespace`.
fn live_pane(script: &str, server: &Path, namespace: &Path, name: &str, config: &PaneConfig) -> Pane {
    let session = agterm_session::cli::Prepared {
        directory: namespace.to_path_buf(),
        executable: server.to_path_buf(),
        name: name.to_owned(),
        spec: agterm_session::protocol::SessionSpec {
            argv: ["/bin/sh", "-c", script].map(str::to_owned).to_vec(),
            env: environment(),
            cwd: None,
            tracked_env: Vec::new(),
        },
        managed: None,
        detach_key: true,
    };
    Pane::attach_live(Ok(session), config).unwrap()
}

/// Kill every session of `namespace` through its server, wait for the server's idle exit, remove the directory.
fn reap_live(namespace: &Path) {
    let client = agterm_session::client::Client::new(namespace);
    if let Ok(Some(mut api)) = client.api() {
        let server = api.welcome.pid;
        let names: Vec<String> = api
            .list()
            .unwrap_or_default()
            .into_iter()
            .map(|record| record.name)
            .collect();
        if !names.is_empty() {
            let _ = api.kill(&names, true);
        }
        drop(api);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Path::new(&format!("/proc/{server}")).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = std::fs::remove_dir_all(namespace);
}

fn cat_scenario(name: &str, file: &Path, via: Via<'_>) {
    let script = format!(
        "sleep 1; s=$(date +%s%N); cat {}; e=$(date +%s%N); printf '\\nMARK cat_ns=%s\\n' $((e - s)); exec sleep 30",
        shell_quote(file)
    );
    let session = format!("bench-{}-{name}", std::process::id());
    let config = PaneConfig::new(bench_size());
    let pane = match via {
        Via::Plain => Pane::spawn(&command(&script, None), &config).unwrap(),
        Via::Zmx(zmx, dir) => Pane::spawn(&command(&script, Some((zmx, dir, session.as_str()))), &config).unwrap(),
        Via::Live(server, namespace) => live_pane(&script, server, namespace, &session, &config),
    };
    let (wall, mut ui, mark) = run_to_mark(&pane);
    let cat_ns: f64 = mark
        .rsplit('=')
        .next()
        .and_then(|ns| ns.trim().parse().ok())
        .unwrap_or(f64::NAN);
    report(
        name,
        &pane,
        &mut ui,
        &format!(
            ",\"cat_s\":{:.3},\"wall_to_mark_s\":{:.3}",
            cat_ns / 1e9,
            wall.as_secs_f64() - 1.0
        ),
    );
    drop(pane);
    match via {
        Via::Plain => {}
        Via::Zmx(_, dir) => reap_zmx(dir),
        Via::Live(_, namespace) => reap_live(namespace),
    }
}

/// A private namespace directory for the bench's server, short enough for a socket path.
fn live_namespace(label: &str) -> PathBuf {
    let namespace = PathBuf::from(format!("/tmp/ngterm-bench-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&namespace).unwrap();
    namespace
}

#[test]
#[ignore = "a measurement, run by hand (see the module docs)"]
fn read_path_plain_and_zmx() {
    let Some(dir) = bench_dir() else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let file = text_file(&dir);
    for run in 1..=3 {
        cat_scenario(&format!("cat-plain-{run}"), &file, Via::Plain);
    }
    if let Some(zmx) = std::env::var_os("TERM_BENCH_ZMX").map(PathBuf::from) {
        let zmx_dir = dir.join("zmx");
        std::fs::create_dir_all(&zmx_dir).unwrap();
        for run in 1..=3 {
            cat_scenario(&format!("cat-zmx-{run}"), &file, Via::Zmx(&zmx, &zmx_dir));
        }
        reap_zmx(&zmx_dir);
    }
    if let Some(server) = std::env::var_os("TERM_BENCH_SESSION").map(PathBuf::from) {
        let namespace = live_namespace("cat");
        for run in 1..=3 {
            cat_scenario(&format!("cat-live-{run}"), &file, Via::Live(&server, &namespace));
        }
        reap_live(&namespace);
    }
}

/// The resident and proportional memory of the process `pid`, in KiB.
fn memory_kib(pid: u32) -> (u64, u64) {
    let field = |path: String, key: &str| {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .find_map(|line| line.strip_prefix(key)?.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    };
    (
        field(format!("/proc/{pid}/status"), "VmRSS:"),
        field(format!("/proc/{pid}/smaps_rollup"), "Pss:"),
    )
}

/// Task 2's idle memory baseline against the session server: N sessions, each an idle `/bin/sh` that printed 20 000
/// lines, every client detached; the server's memory, against N idle zmx daemons'.
#[test]
#[ignore = "a measurement, run by hand (see the module docs)"]
fn idle_sessions_memory() {
    let Some(server) = std::env::var_os("TERM_BENCH_SESSION").map(PathBuf::from) else {
        return;
    };
    for count in [1_usize, 4, 16] {
        let namespace = live_namespace(&format!("rss{count}"));
        for index in 0..count {
            let pane = live_pane(
                "seq 1 20000; echo READY; exec sh",
                &server,
                &namespace,
                &format!("s{index}"),
                &PaneConfig::new(SIZE),
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            while !pane.frame().frame.lines().iter().any(|line| line.contains("READY")) {
                assert!(Instant::now() < deadline, "session {index} printed no READY");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        std::thread::sleep(Duration::from_secs(3));
        let pid = agterm_session::client::Client::new(&namespace)
            .api()
            .unwrap()
            .unwrap()
            .welcome
            .pid;
        let (rss, pss) = memory_kib(pid);
        println!(
            "{{\"scenario\":\"idle-sessions\",\"sessions\":{count},\"server_rss_kib\":{rss},\"server_pss_kib\":{pss},\
             \"rss_per_session_kib\":{}}}",
            rss / count as u64
        );
        reap_live(&namespace);
    }
}

#[test]
#[ignore = "a measurement, run by hand (see the module docs)"]
fn kitty_png_burst() {
    let Some(dir) = bench_dir() else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let file = kitty_file(&dir, 10);
    let script = format!(
        "sleep 1; cat {}; printf '\\nMARK\\n'; exec sleep 30",
        shell_quote(&file)
    );
    let pane = Pane::spawn(&command(&script, None), &PaneConfig::new(SIZE)).unwrap();
    // Ghostty's own default storage (320 MB), so all ten images are decoded and kept
    lock(&pane.shared().vt)
        .terminal
        .set_kitty_image_storage_limit(320_000_000)
        .unwrap();
    let (wall, mut ui, _) = run_to_mark(&pane);
    report(
        "kitty-png-burst-10x1080p",
        &pane,
        &mut ui,
        &format!(",\"wall_s\":{:.3}", wall.as_secs_f64() - 1.0),
    );
}

#[test]
#[ignore = "a measurement, run by hand (see the module docs)"]
fn resize_reflows_a_full_scrollback() {
    let Some(dir) = bench_dir() else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let file = text_file(&dir);
    let script = format!("cat {}; printf '\\nMARK\\n'; exec sleep 60", shell_quote(&file));
    let pane = Arc::new(Pane::spawn(&command(&script, None), &PaneConfig::new(SIZE)).unwrap());
    let _ = run_to_mark(&pane);
    let mut resizes = Vec::new();
    let done = AtomicBool::new(false);
    let mut ui = std::thread::scope(|scope| {
        let ui = scope.spawn(|| run_ui(&pane, &done));
        for cols in [120_u16, 141, 100, 141, 80, 141] {
            let started = Instant::now();
            pane.resize(GridSize { cols, ..SIZE }).unwrap();
            resizes.push(started.elapsed());
            std::thread::sleep(Duration::from_millis(100));
        }
        done.store(true, Ordering::Release);
        ui.join().unwrap()
    });
    let max = resizes.iter().max().copied().unwrap_or_default();
    let all = resizes
        .iter()
        .map(|d| format!("{:.1}", ms(*d)))
        .collect::<Vec<_>>()
        .join(",");
    report(
        "resize-full-scrollback",
        &pane,
        &mut ui,
        &format!(",\"resize_ms\":[{all}],\"max_resize_ms\":{:.1}", ms(max)),
    );
}

/// The zmx pipeline's ceiling on this machine: the same `cat` through `zmx attach` into a PTY whose reader only
/// discards (no VT, no locks, blocking reads), so the pane's own cost can be told from zmx's.
fn null_reader(name: &str, file: &Path, zmx: Option<(&Path, &Path)>, buffer_size: usize) {
    use std::io::Read as _;
    let script = format!(
        "sleep 1; s=$(date +%s%N); cat {}; e=$(date +%s%N); printf '\\nMARK cat_ns=%s\\n' $((e - s)); exec sleep 30",
        shell_quote(file)
    );
    let session = format!("null-{}", std::process::id());
    let command = command(&script, zmx.map(|(zmx, dir)| (zmx, dir, session.as_str())));
    let size = bench_size();
    let agterm_pty::Spawned { master, mut child } = agterm_pty::spawn(&command, size.pty()).unwrap();
    let killer = child.killer();
    let mut reader = std::fs::File::from(std::os::fd::OwnedFd::from(master));
    let mut tail = Vec::new();
    let mut buffer = vec![0_u8; buffer_size];
    let mut reads = 0_u64;
    let mut bytes = 0_u64;
    let cat_ns = loop {
        let read = reader.read(&mut buffer).unwrap();
        assert!(read > 0, "eof before the mark");
        reads += 1;
        bytes += read as u64;
        if buffer_size == 4095 {
            std::thread::sleep(Duration::from_micros(100));
        }
        tail.extend_from_slice(&buffer[..read]);
        let keep = tail.len().saturating_sub(256);
        tail.drain(..keep);
        let text = String::from_utf8_lossy(&tail);
        if let Some(found) = text.split("MARK cat_ns=").nth(1)
            && found.contains('\n')
            && let Some(ns) = found.split_whitespace().next()
        {
            break ns.parse::<f64>().unwrap();
        }
    };
    println!(
        "{{\"scenario\":\"{name}\",\"cat_s\":{:.3},\"reads\":{reads},\"bytes\":{bytes},\"buffer\":{buffer_size},\
         \"size\":\"{}x{}\"}}",
        cat_ns / 1e9,
        size.cols,
        size.rows
    );
    killer.kill_tree().unwrap();
    child.wait().unwrap();
    if let Some((_, dir)) = zmx {
        reap_zmx(dir);
    }
}

#[test]
#[ignore = "a measurement, run by hand (see the module docs)"]
fn zmx_ceiling() {
    let Some(dir) = bench_dir() else { return };
    let Some(zmx) = std::env::var_os("TERM_BENCH_ZMX").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let file = text_file(&dir);
    let zmx_dir = dir.join("zmx");
    std::fs::create_dir_all(&zmx_dir).unwrap();
    for run in 1..=3 {
        null_reader(&format!("null-plain-{run}"), &file, None, 256 * 1024);
        null_reader(&format!("null-zmx-{run}"), &file, Some((&zmx, &zmx_dir)), 256 * 1024);
        null_reader(&format!("null-zmx-4k-{run}"), &file, Some((&zmx, &zmx_dir)), 4096);
        // a reader that falls behind: 4 KiB reads with a 100 µs pause each
        null_reader(&format!("null-zmx-slow-{run}"), &file, Some((&zmx, &zmx_dir)), 4095);
    }
    reap_zmx(&zmx_dir);
}
