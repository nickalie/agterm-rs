//! `gen-unicode`: writes (or, with `--check`, verifies) `agterm-vt`'s generated Unicode tables in
//! `src/unicode/generated` from the UCD that uucode ships, by the rules of uucode and of Ghostty's uucode config.
//! `scripts/gen-vt-unicode.sh` fetches the pinned uucode and runs it; the binary needs the crate's `gen` feature.
//!
//! ```text
//! gen-unicode --uucode DIR --uucode-rev REV --ucd-version VERSION [--out DIR] [--check]
//! ```
//!
//! `DIR` is an unpacked uucode source tree (its `build.zig.zon` gives the uucode version, its `ucd/` the UCD),
//! `REV` the uucode commit it is, `VERSION` the UCD version every versioned UCD file must name. `--out` defaults to
//! this crate's `src/unicode/generated`. Before writing, the generator checks its own output: every table, read back
//! as the library reads it, gives every code point its derived value, and the precomputed grapheme-break table
//! passes uucode's `GraphemeBreakTest.txt` tests. `--check` writes nothing and exits 1 when a file differs from what
//! the inputs give (line endings aside, so a checkout with CRLF endings compares equal). Any error (a bad argument,
//! a UCD file that is missing, of another version or breaks an assumption of the rules) exits 2.
//!
//! `props.rs` is the library's own module, compiled here too, so the generator writes the very `Properties` and
//! `GraphemeBreakNoControl` the library reads, discriminants included.
//!
//! The generator ports Ghostty's table generators (`src/unicode/props_uucode.zig`, `symbols_uucode.zig`, `lut.zig`,
//! `grapheme.zig`'s precompute and `src/build/uucode_config.zig`, at `LIBGHOSTTY_VT_REV`) and the parts of uucode
//! (`https://github.com/jacobsandlund/uucode`, MIT, at `UUCODE_REV`) that they build on: the UCD parsing, the
//! derived fields and the grapheme-break rules. Ghostty runs them in its Zig build; here they are this binary, and
//! the tables are checked-in sources. Each module names its sources.

mod derive;
mod grapheme;
mod lut;
#[path = "../../unicode/props.rs"]
mod props;
mod ucd;
mod write;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use derive::Derived;
use ucd::{Result, Ucd, UcdFile};

/// What the generated headers name about the inputs.
pub(crate) struct Inputs {
    /// uucode's version (`build.zig.zon`).
    pub(crate) uucode_version: String,
    /// The uucode commit, shortened to eight digits.
    pub(crate) uucode_rev_short: String,
    /// The UCD version.
    pub(crate) ucd_version: String,
}

/// The command line.
struct Args {
    uucode: PathBuf,
    uucode_rev: String,
    ucd_version: String,
    out: PathBuf,
    check: bool,
}

const USAGE: &str = "usage: gen-unicode --uucode DIR --uucode-rev REV --ucd-version VERSION [--out DIR] [--check]";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args> {
    let (mut uucode, mut uucode_rev, mut ucd_version, mut check) = (None, None, None, false);
    let mut out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/unicode/generated");
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value\n{USAGE}"));
        match arg.as_str() {
            "--uucode" => uucode = Some(PathBuf::from(value()?)),
            "--uucode-rev" => uucode_rev = Some(value()?),
            "--ucd-version" => ucd_version = Some(value()?),
            "--out" => out = PathBuf::from(value()?),
            "--check" => check = true,
            _ => return Err(format!("unknown argument {arg}\n{USAGE}")),
        }
    }
    let missing = |what: &str| format!("{what} is required\n{USAGE}");
    Ok(Args {
        uucode: uucode.ok_or_else(|| missing("--uucode"))?,
        uucode_rev: uucode_rev.ok_or_else(|| missing("--uucode-rev"))?,
        ucd_version: ucd_version.ok_or_else(|| missing("--ucd-version"))?,
        out,
        check,
    })
}

/// uucode's version: the `.version = "..."` line of its `build.zig.zon`.
fn uucode_version(uucode: &Path) -> Result<String> {
    let path = uucode.join("build.zig.zon");
    let zon = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    zon.lines()
        .find_map(|line| line.trim().strip_prefix(".version = \"")?.strip_suffix("\","))
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: no .version line", path.display()))
}

/// The generated files, by name, from the inputs under `uucode`.
fn generate(uucode: &Path, inputs: &Inputs) -> Result<Vec<(&'static str, String)>> {
    let ucd_dir = uucode.join("ucd");
    let ucd = Ucd::read(&ucd_dir, &inputs.ucd_version)?;
    let derived = Derived::new(&ucd)?;

    let props = lut::generate(|cp| derived.properties_of(cp)).map_err(|e| format!("props table: {e}"))?;
    let symbols = lut::generate(|cp| derived.is_symbol_of(cp)).map_err(|e| format!("symbols table: {e}"))?;
    for cp in 0..=lut::MAX_CODE_POINT {
        if props.get(cp) != Some(derived.properties_of(cp)) || symbols.get(cp) != Some(derived.is_symbol_of(cp)) {
            return Err(format!("the tables do not read back U+{cp:04X}'s values"));
        }
    }

    let precompute = grapheme::precompute();
    let test = UcdFile::read(&ucd_dir, "auxiliary/GraphemeBreakTest.txt", &inputs.ucd_version)?;
    let pairs = grapheme::check_grapheme_break_test(&test, &derived, &precompute)?;
    eprintln!("gen-unicode: GraphemeBreakTest.txt: {pairs} pairs agree with the precomputed table");

    let full_entries = ucd
        .case_folding
        .iter()
        .filter(|(_, fold)| !fold.full.is_empty())
        .count();
    Ok(vec![
        (
            "case_folding_data.rs",
            write::case_folding_data(inputs, &derived.case_folding_full, full_entries),
        ),
        ("grapheme_data.rs", write::grapheme_data(inputs, &precompute)),
        ("props_data.rs", write::props_data(inputs, &props)),
        ("symbols_data.rs", write::symbols_data(inputs, &symbols)),
    ])
}

/// The files under `out` that differ from `files` (or are missing), line endings aside.
fn stale_files(out: &Path, files: &[(&'static str, String)]) -> Vec<String> {
    files
        .iter()
        .filter(|(name, contents)| {
            std::fs::read_to_string(out.join(name)).map_or(true, |found| found.replace("\r\n", "\n") != *contents)
        })
        .map(|(name, _)| (*name).to_owned())
        .collect()
}

/// Writes the files that changed into `out`, and names them.
fn write_files(out: &Path, files: &[(&'static str, String)]) -> Result<Vec<String>> {
    let stale = stale_files(out, files);
    for (name, contents) in files.iter().filter(|(name, _)| stale.iter().any(|s| s == name)) {
        let path = out.join(name);
        std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(stale)
}

fn run(args: &Args) -> Result<bool> {
    let inputs = Inputs {
        uucode_version: uucode_version(&args.uucode)?,
        uucode_rev_short: args.uucode_rev.chars().take(8).collect(),
        ucd_version: args.ucd_version.clone(),
    };
    let files = generate(&args.uucode, &inputs)?;
    let out = args.out.display();
    if args.check {
        let stale = stale_files(&args.out, &files);
        if stale.is_empty() {
            eprintln!("gen-unicode: {out} is current");
            return Ok(true);
        }
        eprintln!(
            "gen-unicode: stale in {out} (rerun scripts/gen-vt-unicode.sh): {}",
            stale.join(", ")
        );
        return Ok(false);
    }
    let written = write_files(&args.out, &files)?;
    if written.is_empty() {
        eprintln!("gen-unicode: {out} is current");
    } else {
        eprintln!("gen-unicode: wrote {} in {out}", written.join(", "));
    }
    Ok(true)
}

fn main() -> ExitCode {
    let result = parse_args(std::env::args().skip(1)).and_then(|args| run(&args));
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("gen-unicode: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests;
