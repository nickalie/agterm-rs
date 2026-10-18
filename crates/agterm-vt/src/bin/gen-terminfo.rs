//! `gen-terminfo`: writes (or, with `--check`, verifies) `agterm-vt`'s checked-in terminfo source,
//! `terminfo/xterm-ghostty.terminfo`, from Ghostty's entry in the library's `terminfo` module.
//! `scripts/gen-ghostty-terminfo.sh` runs it; the binary needs the crate's `gen` feature.
//!
//! ```text
//! gen-terminfo [--out FILE] [--check]
//! ```
//!
//! `--out` defaults to this crate's `terminfo/xterm-ghostty.terminfo`. The file is a comment naming the Ghostty
//! revision the crate follows (`LIBGHOSTTY_VT_REV`), then the entry as Ghostty's encoder writes it: what Ghostty's
//! build gets from `zig run` over `src/terminfo/ghostty.zig` (`+terminfo`), which the native build compiles with
//! `tic -x`. Before writing, the generator builds the entry's XTGETTCAP answers, refusing an entry with a capability
//! that has none, as Ghostty's comptime map does. `--check` writes nothing and exits 1 when the file differs from the
//! entry (line endings aside, so a checkout with CRLF endings compares equal). Any error (a bad argument, a file that
//! cannot be written) exits 2.
//!
//! The library's `terminfo` and `identity` modules are compiled here too, so the generator encodes the very entry the
//! library answers XTGETTCAP from.

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the library's module: the generator reads the entry's names, not the rest of the identity"
    )
)]
#[path = "../identity.rs"]
mod identity;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the library's module: the generator encodes the entry, the stream handler looks answers up"
    )
)]
#[path = "../terminfo/mod.rs"]
mod terminfo;

// `crate::provenance`, as the library's own modules name it.
use agterm_vt::provenance;

use std::path::PathBuf;
use std::process::ExitCode;

/// The command line.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    out: PathBuf,
    check: bool,
}

const USAGE: &str = "usage: gen-terminfo [--out FILE] [--check]";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(terminfo::SOURCE_FILE);
    let mut check = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = PathBuf::from(args.next().ok_or_else(|| format!("--out needs a value\n{USAGE}"))?),
            "--check" => check = true,
            _ => return Err(format!("unknown argument {arg}\n{USAGE}")),
        }
    }
    Ok(Args { out, check })
}

/// Whether `found` (a file's contents, if it could be read) is `contents`, line endings aside.
fn is_current(found: Option<&str>, contents: &str) -> bool {
    found.is_some_and(|found| found.replace("\r\n", "\n") == contents)
}

fn run(args: &Args) -> Result<bool, String> {
    terminfo::entry::GHOSTTY
        .xtgettcap_map()
        .map_err(|e| format!("Ghostty's entry: {e}"))?;
    let contents = terminfo::source_file(provenance::LIBGHOSTTY_VT_REV);
    let out = args.out.display();
    if is_current(std::fs::read_to_string(&args.out).ok().as_deref(), &contents) {
        eprintln!("gen-terminfo: {out} is current");
        return Ok(true);
    }
    if args.check {
        eprintln!("gen-terminfo: {out} is stale (rerun scripts/gen-ghostty-terminfo.sh)");
        return Ok(false);
    }
    std::fs::write(&args.out, contents).map_err(|e| format!("{out}: {e}"))?;
    eprintln!("gen-terminfo: wrote {out}");
    Ok(true)
}

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)).and_then(|args| run(&args)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("gen-terminfo: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Args, is_current, parse_args};
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|&a| a.to_owned()))
    }

    #[test]
    fn the_default_out_is_the_checked_in_source() {
        let args = parse(&[]).unwrap();
        assert!(args.out.ends_with("terminfo/xterm-ghostty.terminfo"));
        assert!(args.out.starts_with(env!("CARGO_MANIFEST_DIR")));
        assert!(!args.check);
    }

    #[test]
    fn out_and_check_are_taken() {
        assert_eq!(
            parse(&["--check", "--out", "x.terminfo"]).unwrap(),
            Args {
                out: PathBuf::from("x.terminfo"),
                check: true
            }
        );
    }

    #[test]
    fn a_bad_argument_is_refused_with_the_usage() {
        let e = parse(&["--bogus"]).unwrap_err();
        assert!(e.starts_with("unknown argument --bogus\nusage: gen-terminfo"), "{e}");
        let e = parse(&["--out"]).unwrap_err();
        assert!(e.starts_with("--out needs a value\n"), "{e}");
    }

    #[test]
    fn line_endings_aside_the_contents_must_match() {
        assert!(is_current(Some("a,\n\tb,\n"), "a,\n\tb,\n"));
        assert!(is_current(Some("a,\r\n\tb,\r\n"), "a,\n\tb,\n"));
        assert!(!is_current(Some("a,\n\tc,\n"), "a,\n\tb,\n"));
        assert!(!is_current(Some("a,\n"), "a,\n\tb,\n"));
        assert!(!is_current(None, "a,\n"));
    }
}
