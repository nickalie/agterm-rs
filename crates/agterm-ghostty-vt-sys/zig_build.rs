//! The `zig build` of libghostty-vt for one cargo target, as functions of the build script's inputs: `build.rs` runs
//! what they return, and `tests/zig_build.rs` checks them without running zig.
//!
//! herdr: `crates/ghostty-vt/build.rs` at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0,
//! modified. Kept: the target map, the arguments (`--prefix` under `OUT_DIR`, `-Demit-lib-vt`, the optimize mode,
//! SIMD, `-Dtarget`, `-Dversion-string`), the Apple link directory and the MSVC archive name. Changed: herdr builds a
//! source tree vendored in its repository with the zig on `PATH`; here the tree, its Zig packages and the pinned zig
//! come from the prefix `scripts/build-ghostty-vt.sh` prepared (its stamp names the zig and the Zig global cache), the
//! local cache goes under `OUT_DIR` too, the map drops iOS and adds Linux's glibc floor and the macOS deployment
//! target, the CPU is `baseline` (never the build host's), and the optimize mode and SIMD are fixed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The prefix's stamp, relative to the prefix: its inputs as `name=value` lines (`scripts/build-ghostty-vt.sh`).
pub const STAMP: &str = "share/agterm/ghostty-vt-STAMP";

/// The patched source tree, relative to the prefix: the build's root, with its Zig packages in `zig-pkg/`.
pub const SOURCE: &str = "src";

/// The cargo targets libghostty-vt builds for, each with its Zig triple without the OS version (herdr's map without
/// iOS). `scripts/build-ghostty-vt.sh` fetches the Zig packages of each (its `ZIG_TARGETS`, kept equal by a test).
pub const ZIG_TARGETS: [(&str, &str); 8] = [
    ("x86_64-unknown-linux-gnu", "x86_64-linux-gnu"),
    ("aarch64-unknown-linux-gnu", "aarch64-linux-gnu"),
    ("x86_64-unknown-linux-musl", "x86_64-linux-musl"),
    ("aarch64-unknown-linux-musl", "aarch64-linux-musl"),
    ("x86_64-apple-darwin", "x86_64-macos"),
    ("aarch64-apple-darwin", "aarch64-macos"),
    ("x86_64-pc-windows-msvc", "x86_64-windows-msvc"),
    ("aarch64-pc-windows-msvc", "aarch64-windows-msvc"),
];

/// The glibc a Linux gnu build targets, the libghostty build's floor: one archive serves every supported distribution.
pub const GLIBC_FLOOR: &str = "2.39";

/// The repository files whose change reruns the build script, relative to the repository root: a pin or patch change
/// moves the prefix (its key hashes them), so the rerun finds the new prefix, or fails naming the script when it is
/// not prepared yet, and builds the new tree.
pub const RERUN_FILES: [&str; 7] = [
    "build/pins.env",
    "build/patches/ghostty-vt",
    "scripts/build-ghostty-vt.sh",
    "scripts/lib/native.sh",
    "crates/agterm-ghostty-vt-sys/build.rs",
    "crates/agterm-ghostty-vt-sys/zig_build.rs",
    "crates/agterm-ghostty-vt-sys/script.rs",
];

/// The environment whose change reruns the build script: the prefix override (`AGTERM_GHOSTTY_VT_PREFIX`,
/// `script.rs`), the vendor root (`AGTERM_VENDOR`, else under `HOME`) and the macOS deployment target.
pub const RERUN_ENV: [&str; 4] = [
    "AGTERM_GHOSTTY_VT_PREFIX",
    "AGTERM_VENDOR",
    "HOME",
    "MACOSX_DEPLOYMENT_TARGET",
];

/// What the prefix's stamp records for the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    /// The pinned zig the prefix was prepared with, the binary itself.
    pub zig: PathBuf,
    /// That zig's version (`LIBGHOSTTY_VT_ZIG_VERSION`), checked again before the build.
    pub zig_version: String,
    /// Zig's global cache in the vendor root, keyed by the Zig version.
    pub global_cache: PathBuf,
    /// The version string the build passes, so Ghostty's build never asks git.
    pub version_string: String,
}

impl Stamp {
    /// Parse the stamp's `name=value` lines (`#` comments and other names, such as the patches', are skipped).
    ///
    /// # Errors
    ///
    /// A missing or empty `zig`, `zig-version`, `zig-global-cache` or `version-string`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let value = |name: &str| {
            text.lines()
                .filter(|line| !line.starts_with('#'))
                .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("the libghostty-vt prefix's stamp has no {name}"))
        };
        Ok(Self {
            zig: PathBuf::from(value("zig")?),
            zig_version: value("zig-version")?.to_owned(),
            global_cache: PathBuf::from(value("zig-global-cache")?),
            version_string: value("version-string")?.to_owned(),
        })
    }
}

/// The stamp of a prepared prefix: the tree, the umbrella header and the stamp are there.
///
/// # Errors
///
/// A prefix that is missing or not prepared, with the error that names the script, or an unreadable stamp.
pub fn prepared(prefix: &Path) -> Result<Stamp, String> {
    let stamp = prefix.join(STAMP);
    let complete = prefix.join(SOURCE).join("build.zig").is_file()
        && prefix.join("include/ghostty/vt.h").is_file()
        && stamp.is_file();
    if !complete {
        return Err(format!(
            "no libghostty-vt at {}: run scripts/build-ghostty-vt.sh (it prepares the pinned libghostty-vt source there)",
            prefix.display()
        ));
    }
    let text = std::fs::read_to_string(&stamp).map_err(|err| format!("cannot read {}: {err}", stamp.display()))?;
    Stamp::parse(&text)
}

/// The oldest macOS libghostty-vt builds for: Ghostty's own floor (`src/build/Config.zig` `osVersionMin`, the oldest
/// macOS Apple still supports), which its build substitutes for any `-Dtarget` version on a macOS host. The workspace
/// links for it too (`MACOSX_DEPLOYMENT_TARGET` in `.cargo/config.toml`), so no member of the archive is built for a
/// newer macOS than the binaries that link it.
pub const MACOS_FLOOR: &str = "13.0";

/// A `major[.minor[.patch]]` version as numbers padded to three with zeros (as rustc reads a deployment target, so
/// `13` is `13.0`), for comparing; `None` when it is not one.
fn version(text: &str) -> Option<Vec<u32>> {
    let mut parts: Vec<u32> = text.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
    parts.resize(parts.len().max(3), 0);
    Some(parts)
}

/// The `-Dtarget` of a cargo target: Linux gnu at the glibc floor, macOS at `MACOSX_DEPLOYMENT_TARGET` or else at
/// [`MACOS_FLOOR`], the others as mapped.
///
/// # Errors
///
/// A target outside the map, or a macOS deployment target that is not a version or is older than [`MACOS_FLOOR`]
/// (Ghostty's build would build for the floor anyway on a macOS host, newer than what the binaries claim).
pub fn zig_target(target: &str, macos_deployment_target: Option<&str>) -> Result<String, String> {
    let (_, triple) = ZIG_TARGETS
        .iter()
        .find(|(cargo, _)| *cargo == target)
        .ok_or_else(|| format!("libghostty-vt has no Zig target for {target} (ZIG_TARGETS in zig_build.rs)"))?;
    if triple.ends_with("-linux-gnu") {
        return Ok(format!("{triple}.{GLIBC_FLOOR}"));
    }
    if triple.ends_with("-macos") {
        let requested = macos_deployment_target
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .unwrap_or(MACOS_FLOOR);
        let supported = version(requested)
            .zip(version(MACOS_FLOOR))
            .map(|(requested, floor)| requested >= floor);
        if supported != Some(true) {
            return Err(format!(
                "MACOSX_DEPLOYMENT_TARGET={requested}: libghostty-vt builds for macOS {MACOS_FLOOR} or newer"
            ));
        }
        return Ok(format!("{triple}.{requested}"));
    }
    Ok((*triple).to_owned())
}

/// The inputs of one build.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// The prepared prefix.
    pub prefix: &'a Path,
    /// What its stamp records.
    pub stamp: &'a Stamp,
    /// Cargo's `OUT_DIR` of this build (one per target and profile).
    pub out_dir: &'a Path,
    /// Cargo's `TARGET`.
    pub target: &'a str,
    /// `MACOSX_DEPLOYMENT_TARGET`, when set.
    pub macos_deployment_target: Option<&'a str>,
    /// Cargo's `NUM_JOBS`, when set.
    pub jobs: Option<&'a str>,
}

/// What one build runs and links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// The zig to run.
    pub zig: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// The directory it runs in: the prefix's tree.
    pub source: PathBuf,
    /// The static archive the build installs.
    pub archive: PathBuf,
    /// The directory to link from: the install's `lib/`, or on Apple one holding a copy of the archive alone (Apple's
    /// linker prefers the sibling dylib for `-l`).
    pub link_dir: PathBuf,
    /// The static library's link name: `ghostty-vt-static` on MSVC (beside the import library of the DLL),
    /// `ghostty-vt` elsewhere.
    pub link_name: &'static str,
    /// The `-Dtarget` it builds for.
    pub zig_target: String,
}

/// The build of `inputs`: `zig build -Demit-lib-vt` over the prefix's tree, installing into and caching under
/// `OUT_DIR` (so builds for different targets or profiles never share an install), with the global cache the stamp
/// names. `ReleaseFast`: Debug's terminal integrity checks make sustained PTY output unusably slow.
///
/// # Errors
///
/// A target outside the map, or a macOS deployment target [`zig_target`] refuses.
pub fn plan(inputs: &Inputs<'_>) -> Result<Build, String> {
    let target = zig_target(inputs.target, inputs.macos_deployment_target)?;
    let install = inputs.out_dir.join("zig-out");
    let mut args: Vec<OsString> = vec!["build".into(), "--prefix".into(), install.clone().into()];
    args.extend(["--cache-dir".into(), inputs.out_dir.join("zig-cache").into()]);
    args.extend(["--global-cache-dir".into(), inputs.stamp.global_cache.clone().into()]);
    args.extend(
        [
            "-Demit-lib-vt=true".to_owned(),
            "-Doptimize=ReleaseFast".to_owned(),
            "-Dsimd=true".to_owned(),
            "-Dcpu=baseline".to_owned(),
            format!("-Dtarget={target}"),
            format!("-Dversion-string={}", inputs.stamp.version_string),
        ]
        .map(OsString::from),
    );
    if let Some(jobs) = inputs.jobs.filter(|jobs| !jobs.is_empty()) {
        args.push(format!("-j{jobs}").into());
    }
    let msvc = inputs.target.ends_with("-windows-msvc");
    let archive = install.join("lib").join(if msvc {
        "ghostty-vt-static.lib"
    } else {
        "libghostty-vt.a"
    });
    let link_dir = if inputs.target.contains("-apple-") {
        inputs.out_dir.join("lib")
    } else {
        install.join("lib")
    };
    Ok(Build {
        zig: inputs.stamp.zig.clone(),
        args,
        source: inputs.prefix.join(SOURCE),
        archive,
        link_dir,
        link_name: if msvc { "ghostty-vt-static" } else { "ghostty-vt" },
        zig_target: target,
    })
}
