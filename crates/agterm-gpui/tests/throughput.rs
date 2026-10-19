//! Huge output through a pane, end to end: the GPUI build on a private Xvfb (lavapipe) runs a program that writes
//! `AGTERM_TEST_THROUGHPUT_MB` MiB (64 by default) of mixed terminal text (SGR colors, Cyrillic, CJK, emoji, long
//! lines) as fast as the pane takes it. Measured and bounded: the output's pace (the program blocks on the PTY while
//! the pane is behind, so its run time is the pane's), the screen catching up once it ends (no stall), the app's
//! memory (the scrollback limit holds it, whatever the volume) and how fast the main loop answers a control request
//! meanwhile (the UI stays responsive). One JSON line of numbers is printed (seen with `--nocapture`).
//!
//! A test whose tool is missing prints `SKIPPED:` and passes, unless `AGTERM_REQUIRE_DISPLAY_TESTS` is set.

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::{Sandbox, TestResult, have, lavapipe_icd, wait_for};

/// MiB written unless `AGTERM_TEST_THROUGHPUT_MB` says otherwise.
const DEFAULT_MB: u32 = 64;
/// The slowest pace allowed, in MiB/s: lavapipe paints on the CPU beside the pane's parsing.
const MIN_PACE: f64 = 2.0;
/// How long the screen may take to show the end once the program wrote it.
const CATCH_UP: Duration = Duration::from_secs(10);
/// The slowest answer to a control request while the output runs, and the slowest twentieth.
const MAX_LATENCY: Duration = Duration::from_secs(2);
const P95_LATENCY: Duration = Duration::from_millis(500);
/// How much the app's resident memory may grow over the run: the scrollback (Ghostty's 10 MB by default) and the
/// renderer's caches, never the volume written.
const MAX_GROWTH_MB: u64 = 512;

/// One control request to the sandbox's socket; its result, or the error it answered.
fn request(sandbox: &Sandbox, cmd: &str, target: Option<&str>) -> TestResult<Value> {
    let mut socket = std::os::unix::net::UnixStream::connect(sandbox.path("state/ngterm.sock"))?;
    socket.set_read_timeout(Some(support::DEADLINE))?;
    let mut line = json!({ "cmd": cmd });
    if let Some(target) = target {
        line["target"] = json!(target);
    }
    writeln!(socket, "{line}")?;
    let mut reply = String::new();
    BufReader::new(socket).read_line(&mut reply)?;
    let reply: Value = serde_json::from_str(&reply)?;
    if reply["ok"] == json!(true) {
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(format!("{cmd}: {reply}").into())
    }
}

/// The pane's program: waits for `$HOME/go`, writes `$HOME/block` `$HOME/count` times between two timestamps, then
/// the end marker.
const FLOOD: &str = r#"#!/bin/sh
stty -echo
printf 'FLOOD-WAITING'
while [ ! -f "$HOME/go" ]; do sleep 0.05; done
count=$(cat "$HOME/count")
date +%s%N > "$HOME/start"
i=0
while [ "$i" -lt "$count" ]; do cat "$HOME/block"; i=$((i + 1)); done
date +%s%N > "$HOME/end"
printf '\r\nFLOOD-DONE\r\n'
exec sleep 600
"#;

/// One MiB of terminal text: numbered lines of plain ASCII, SGR-colored words, Cyrillic, CJK and emoji, every
/// eighth line longer than the grid (it wraps).
fn block() -> Vec<u8> {
    const MIB: usize = 1 << 20;
    let mut out = Vec::with_capacity(MIB + 512);
    let mut line = 0u32;
    while out.len() < MIB {
        let text = match line % 4 {
            0 => format!("{line:08} plain ascii output of a build log: compiling crate number {line} of many"),
            1 => format!(
                "{line:08} \x1b[1;31merror\x1b[0m: \x1b[38;5;{}mcolored\x1b[0m \x1b[48;2;10;20;30mtruecolor\x1b[0m",
                line % 256
            ),
            2 => format!("{line:08} Привет, мир — 日本語のテキスト — 🦀 emoji 👍🏽 and ✓ marks"),
            _ => format!(
                "{line:08} {}",
                "long line that wraps ".repeat(if line % 8 == 3 { 12 } else { 2 })
            ),
        };
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\r\n");
        line += 1;
    }
    out
}

/// A field of `/proc/<pid>/status` in kB.
fn status_kb(pid: u32, field: &str) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The process's user plus system CPU time in clock ticks.
fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

/// A timestamp file `date +%s%N` wrote.
fn nanos(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The active session's text holds `needle`.
fn screen_holds(sandbox: &Sandbox, needle: &str) -> bool {
    let Ok(tree) = request(sandbox, "tree", None) else {
        return false;
    };
    let Some(sid) = tree["tree"]["workspaces"]
        .as_array()
        .and_then(|workspaces| workspaces.iter().find(|workspace| workspace["active"] == json!(true)))
        .and_then(|workspace| workspace["sessions"].as_array())
        .and_then(|sessions| sessions.iter().find(|session| session["active"] == json!(true)))
        .and_then(|session| session["id"].as_str())
    else {
        return false;
    };
    request(sandbox, "session.text", Some(sid))
        .ok()
        .and_then(|text| text["text"].as_str().map(|text| text.contains(needle)))
        .unwrap_or(false)
}

/// What the sampler saw while the output ran.
#[derive(Debug, Default)]
struct Samples {
    /// Every request's time to its answer, a failed one's to its failure.
    latencies: Vec<Duration>,
    /// The requests that got no answer (a timeout, a refused connection, an error reply), with their errors.
    failures: Vec<String>,
    peak_rss_kb: u64,
}

impl Samples {
    /// The `p`th percentile of the answers' latencies (sorted).
    fn percentile(&self, p: usize) -> Option<Duration> {
        let mut sorted = self.latencies.clone();
        sorted.sort();
        sorted.get(sorted.len().saturating_sub(1) * p / 100).copied()
    }

    fn max(&self) -> Option<Duration> {
        self.latencies.iter().max().copied()
    }
}

/// Asks the main loop for the tree every 100 ms and reads the app's memory, until `done`.
fn sample(sandbox: &Sandbox, pid: u32, done: &AtomicBool) -> Samples {
    let mut samples = Samples::default();
    while !done.load(Ordering::Acquire) {
        let started = Instant::now();
        let answer = request(sandbox, "tree", None);
        samples.latencies.push(started.elapsed());
        if let Err(error) = answer {
            samples.failures.push(format!("after {:?}: {error}", started.elapsed()));
        }
        samples.peak_rss_kb = samples.peak_rss_kb.max(status_kb(pid, "VmRSS:").unwrap_or(0));
        std::thread::sleep(Duration::from_millis(100));
    }
    samples
}

/// A sandbox on a private Xvfb (lavapipe) whose panes run the flood of `mib` MiB, its primary started and waiting.
fn flood_sandbox(lavapipe: &std::path::Path, mib: u32) -> TestResult<(Sandbox, usize)> {
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(lavapipe);
    let flood = sandbox.path("home/flood.sh");
    std::fs::write(&flood, FLOOD)?;
    std::fs::set_permissions(&flood, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(sandbox.path("home/block"), block())?;
    std::fs::write(sandbox.path("home/count"), mib.to_string())?;
    let config = sandbox.path("state/config");
    std::fs::create_dir_all(&config)?;
    // no welcome dialog (`FirstRunWelcome::has_prior_state`)
    std::fs::create_dir_all(sandbox.path("state/windows"))?;
    std::fs::write(config.join("ghostty.conf"), format!("command = {}\n", flood.display()))?;
    let primary = sandbox.start_gpui_primary("primary.log")?;
    wait_for(|| screen_holds(&sandbox, "FLOOD-WAITING").then_some(()))
        .map_err(|_| format!("the flood never started:\n{}", sandbox.log("primary.log")))?;
    Ok((sandbox, primary))
}

/// Starts the flood and samples the app until the screen shows its end; the samples and how long the writing took.
fn run_flood(sandbox: &Sandbox, pid: u32, mib: u32) -> TestResult<(Samples, Duration)> {
    let done = AtomicBool::new(false);
    let limit = Duration::from_secs_f64((f64::from(mib) / MIN_PACE).max(30.0));
    std::thread::scope(|scope| {
        let watcher = scope.spawn(|| sample(sandbox, pid, &done));
        std::fs::write(sandbox.path("home/go"), "")?;
        let started = Instant::now();
        let end = sandbox.path("home/end");
        while !end.exists() && started.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(50));
        }
        let ended = end.exists();
        let caught_up = Instant::now();
        while ended && !screen_holds(sandbox, "FLOOD-DONE") && caught_up.elapsed() < CATCH_UP {
            std::thread::sleep(Duration::from_millis(50));
        }
        let shown = caught_up.elapsed();
        done.store(true, Ordering::Release);
        let samples = watcher.join().map_err(|_| "the sampler panicked")?;
        assert!(ended, "{mib} MiB not written within {limit:?}");
        assert!(shown < CATCH_UP, "the screen never showed the end: a stall");
        let start = nanos(&sandbox.path("home/start")).unwrap_or(0);
        Ok((
            samples,
            Duration::from_nanos(nanos(&end).unwrap_or(0).saturating_sub(start)),
        ))
    })
}

/// CPU time of `pid` since `before` (clock ticks).
fn cpu_since(pid: u32, before: u64) -> Duration {
    // SAFETY: `sysconf` reads a constant of the system.
    let ticks_per_second = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).unwrap_or(100);
    Duration::from_millis(cpu_ticks(pid).unwrap_or(0).saturating_sub(before) * 1000 / ticks_per_second.max(1))
}

fn millis(duration: Option<Duration>) -> Option<f64> {
    duration.map(|duration| duration.as_secs_f64() * 1000.0)
}

#[test]
fn a_flood_of_output_keeps_its_pace_its_memory_bounded_and_the_ui_answering() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mib: u32 = std::env::var("AGTERM_TEST_THROUGHPUT_MB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MB);
    let (mut sandbox, primary) = flood_sandbox(&lavapipe, mib)?;
    let pid = sandbox.child(primary).id();
    let rss_before = status_kb(pid, "VmRSS:").unwrap_or(0);
    let cpu_before = cpu_ticks(pid).unwrap_or(0);
    let (samples, wrote) = run_flood(&sandbox, pid, mib)?;

    let peak_kb = status_kb(pid, "VmHWM:").unwrap_or(0).max(samples.peak_rss_kb);
    let (p50, p95, max) = (samples.percentile(50), samples.percentile(95), samples.max());
    let pace = f64::from(mib) / wrote.as_secs_f64().max(f64::EPSILON);
    println!(
        "{}",
        json!({
            "mib": mib,
            "seconds": wrote.as_secs_f64(),
            "mib_per_second": pace,
            "app_cpu_seconds": cpu_since(pid, cpu_before).as_secs_f64(),
            "rss_before_mb": rss_before / 1024,
            "rss_peak_mb": peak_kb / 1024,
            "rss_after_mb": status_kb(pid, "VmRSS:").unwrap_or(0) / 1024,
            "control_requests": samples.latencies.len(),
            "control_failures": samples.failures.len(),
            "control_p50_ms": millis(p50),
            "control_p95_ms": millis(p95),
            "control_max_ms": millis(max),
        })
    );
    assert!(pace >= MIN_PACE, "{pace:.1} MiB/s");
    assert!(
        samples.failures.is_empty(),
        "control requests failed: {:?}",
        samples.failures
    );
    assert!(max.is_some_and(|max| max < MAX_LATENCY), "{max:?}");
    assert!(p95.is_some_and(|p95| p95 < P95_LATENCY), "{p95:?}");
    let growth = peak_kb.saturating_sub(rss_before) / 1024;
    assert!(
        growth < MAX_GROWTH_MB,
        "{growth} MB over {} MB at the start",
        rss_before / 1024
    );
    sandbox.terminate(primary)?;
    assert_eq!(sandbox.wait_exit(primary, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}
