//! Reading config text and files as libghostty does at `LIBGHOSTTY_VT_REV`: `cli/args.zig`'s `LineIterator` (one
//! `key = value` per line, trimmed, `#` comments, surrounding quotes dropped from the value; a UTF-8 BOM skipped in
//! config files and includes as `Config.loadReader` does, not in themes, which `loadTheme` reads without it), each
//! line checked as it is read, path values made absolute against the file that sets them (`Config.expandPaths`),
//! and the `config-file` includes loaded after every base file in the order they were named, new ones appended as they
//! are found, each file once (`Config.loadRecursiveFiles`).

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::LoadedConfig;
use super::validate::{self, Accepted, ConfigPath, Kind};

/// The longest line libghostty reads: its line buffer is 4096 bytes, two of them taken by a `--` prefix. A longer
/// line stops the file there, with only a log line.
const MAX_LINE: usize = 4094;
/// The key whose values are the includes.
const CONFIG_FILE: &str = "config-file";

/// Where a line came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
}

/// A problem libghostty would record in the config's diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub location: Option<Location>,
    /// The key of a refused line; empty for a file-level problem.
    pub key: String,
    pub message: String,
}

impl Diagnostic {
    fn file_level(message: String) -> Self {
        Self {
            location: None,
            key: String::new(),
            message,
        }
    }
}

/// As ghostty's `Diagnostic.format`: `path:line:key: message`.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(location) = &self.location {
            write!(f, "{}:{}:", location.path.display(), location.line)?;
        }
        if !self.key.is_empty() {
            write!(f, "{}: ", self.key)?;
        } else if self.location.is_some() {
            f.write_str(" ")?;
        }
        f.write_str(&self.message)
    }
}

/// One `key = value` line, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub key: String,
    /// `None` for a bare key (no `=`); `Some("")` resets the key to its default.
    pub value: Option<String>,
    pub location: Option<Location>,
    /// How the key took the value, or the diagnostic message of a refused one.
    pub accepted: Result<Accepted, String>,
    /// A path value made absolute; `None` when it is not a path, empty, or could not be resolved.
    pub path: Option<ConfigPath>,
}

impl Setting {
    /// The diagnostic of a refused line.
    #[must_use]
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        self.accepted.as_ref().err().map(|message| Diagnostic {
            location: self.location.clone(),
            key: self.key.clone(),
            message: message.clone(),
        })
    }
}

/// What loading left, in libghostty's replay order: the lines, and the diagnostics of loading itself (unreadable
/// files, includes, unresolvable paths) where they happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Setting(Setting),
    Diagnostic(Diagnostic),
}

/// The `key = value` lines of `text` with their line numbers, as `LineIterator` yields them. A leading BOM is kept,
/// as `LineIterator` keeps it: it becomes part of the first key.
#[must_use]
pub fn lines(text: &str) -> Vec<(usize, String, Option<String>)> {
    let mut lines = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        if raw.len() > MAX_LINE {
            tracing::warn!(
                line = index + 1,
                "ghostty config line longer than {MAX_LINE} bytes: rest of the file skipped"
            );
            break;
        }
        let line = raw.trim_matches([' ', '\t', '\r']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => {
                let value = value.trim_matches([' ', '\t']);
                let value = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                    &value[1..value.len() - 1]
                } else {
                    value
                };
                (key.trim_matches([' ', '\t']), Some(value.to_owned()))
            }
            None => (line, None),
        };
        lines.push((index + 1, key.to_owned(), value));
    }
    lines
}

/// Builds a config the way the frontends call libghostty: base sources in order, then the includes, then overlays.
#[derive(Debug, Default)]
pub struct Loader {
    home: Option<PathBuf>,
    steps: Vec<Step>,
    /// The `config-file` list: the includes still to load, made absolute.
    includes: Vec<ConfigPath>,
}

impl Loader {
    /// A loader expanding `~/` paths against `home`.
    #[must_use]
    pub fn new(home: Option<PathBuf>) -> Self {
        Self {
            home,
            ..Self::default()
        }
    }

    /// Loads config text from memory. Frontends hand libghostty such text through a file in the temp directory, so
    /// its relative paths resolve there; `name` only labels its diagnostics.
    pub fn load_text(&mut self, name: &str, text: &str) {
        self.load_lines(Some(Path::new(name)), &std::env::temp_dir(), text);
    }

    /// Loads one config file. One that is not a regular file is skipped; one that cannot be read is a diagnostic.
    pub fn load_file(&mut self, path: &Path) {
        let Ok(path) = std::path::absolute(path) else {
            return self.note(format!("{}: cannot make the path absolute", path.display()));
        };
        if !path.is_file() {
            return;
        }
        match std::fs::read(&path) {
            Ok(bytes) => self.load_bytes(&path, &bytes),
            Err(err) => self.note(format!("{}: {err}", path.display())),
        }
    }

    /// Loads the `config-file` includes named so far, and the ones they name, each file once.
    pub fn load_includes(&mut self) {
        let mut loaded = HashSet::new();
        let mut next = 0;
        while let Some(include) = self.includes.get(next).cloned() {
            next += 1;
            let path = include.path;
            if !loaded.insert(path.clone()) {
                self.note(format!("config-file {}: cycle detected", path.display()));
                continue;
            }
            match std::fs::metadata(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound && include.optional => {}
                Err(err) => self.note(format!(
                    "error opening config-file {}: {}",
                    path.display(),
                    zig_error(&err)
                )),
                Ok(meta) if !meta.is_file() => self.note(format!(
                    "config-file {}: not reading because file type is {}",
                    path.display(),
                    file_kind(meta.file_type())
                )),
                Ok(_) => match std::fs::read(&path) {
                    Ok(bytes) => self.load_bytes(&path, &bytes),
                    Err(err) => {
                        self.note(format!(
                            "error opening config-file {}: {}",
                            path.display(),
                            zig_error(&err)
                        ));
                    }
                },
            }
        }
    }

    /// What was loaded, in replay order.
    #[must_use]
    pub fn finish(self) -> LoadedConfig {
        LoadedConfig {
            steps: self.steps,
            home: self.home,
        }
    }

    fn load_bytes(&mut self, path: &Path, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let base = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        self.load_lines(Some(path), &base, &text);
    }

    /// A config file's or text's lines, a leading UTF-8 BOM skipped (`Config.loadReader`).
    fn load_lines(&mut self, source: Option<&Path>, base: &Path, text: &str) {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for (line, key, value) in lines(text) {
            let location = source.map(|path| Location {
                path: path.to_path_buf(),
                line,
            });
            let accepted = validate::check(&key, value.as_deref());
            let mut setting = Setting {
                key,
                value,
                location,
                accepted,
                path: None,
            };
            if is_path(&setting.accepted) {
                self.expand(&mut setting, base);
            }
            self.steps.push(Step::Setting(setting));
        }
    }

    /// Resolves a path value against `base` (`Path.expand`) and tracks the includes.
    fn expand(&mut self, setting: &mut Setting, base: &Path) {
        let value = setting.value.as_deref().unwrap_or("");
        let include = setting.key == CONFIG_FILE;
        if value.is_empty() {
            if include {
                self.includes.clear();
            }
            return;
        }
        let Some(parsed) = validate::parse_path(value) else {
            return;
        };
        match resolve_path(&parsed.path, base, self.home.as_deref()) {
            Ok(path) => {
                let resolved = ConfigPath {
                    path,
                    optional: parsed.optional,
                };
                if include {
                    self.includes.push(resolved.clone());
                }
                setting.path = Some(resolved);
            }
            Err(message) => self.steps.push(Step::Diagnostic(Diagnostic::file_level(message))),
        }
    }

    fn note(&mut self, message: String) {
        self.steps.push(Step::Diagnostic(Diagnostic::file_level(message)));
    }
}

/// Whether an accepted line sets a path key.
fn is_path(accepted: &Result<Accepted, String>) -> bool {
    matches!(accepted, Ok(Accepted::Field(spec)) if matches!(spec.kind, Kind::Path | Kind::RepeatablePath))
}

/// The checked lines of a theme file. Like libghostty, which reads a theme with a bare `LineIterator`, a leading BOM
/// stays part of the first key and path values stay as written (no `expandPaths`).
#[must_use]
pub fn theme_settings(path: &Path, text: &str) -> Vec<Setting> {
    lines(text)
        .into_iter()
        .map(|(line, key, value)| {
            let accepted = validate::check(&key, value.as_deref());
            let path_value = value
                .as_deref()
                .filter(|_| is_path(&accepted))
                .and_then(validate::parse_path);
            Setting {
                key,
                value,
                location: Some(Location {
                    path: path.to_path_buf(),
                    line,
                }),
                accepted,
                path: path_value,
            }
        })
        .collect()
}

/// `theme.open`: an absolute theme path as is; else a bare name looked up in `dirs` in order. `Err` carries the
/// diagnostics: one per directory tried when the name is found nowhere.
///
/// # Errors
///
/// The diagnostics of a theme that cannot be loaded.
pub fn open_theme(theme: &str, dirs: &[PathBuf]) -> Result<(PathBuf, String), Vec<Diagnostic>> {
    let refuse = |message: String| Err(vec![Diagnostic::file_level(message)]);
    let path = Path::new(theme);
    if path.is_absolute() {
        return match std::fs::metadata(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                refuse(format!("failed to load theme from the path \"{theme}\""))
            }
            Err(err) => refuse(format!(
                "failed to load theme from the path \"{theme}\": {}",
                zig_error(&err)
            )),
            Ok(meta) => read_theme(theme, path, &meta),
        };
    }
    if path.file_name().is_none_or(|name| name != path.as_os_str()) {
        return refuse(format!(
            "theme \"{theme}\" cannot include path separators unless it is an absolute path"
        ));
    }
    for dir in dirs {
        let candidate = dir.join(theme);
        match std::fs::metadata(&candidate) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return refuse(format!(
                    "failed to load theme \"{theme}\" from the file \"{}\": {}",
                    candidate.display(),
                    zig_error(&err)
                ));
            }
            Ok(meta) => return read_theme(theme, &candidate, &meta),
        }
    }
    Err(dirs
        .iter()
        .map(|dir| {
            Diagnostic::file_level(format!(
                "theme \"{theme}\" not found, tried path \"{}\"",
                dir.join(theme).display()
            ))
        })
        .collect())
}

/// A found theme file's text; anything but a regular file is refused before it is opened (a FIFO would block).
fn read_theme(theme: &str, path: &Path, meta: &std::fs::Metadata) -> Result<(PathBuf, String), Vec<Diagnostic>> {
    if !meta.is_file() {
        return Err(vec![Diagnostic::file_level(format!(
            "not reading theme from \"{theme}\": it is a {}",
            file_kind(meta.file_type())
        ))]);
    }
    std::fs::read(path)
        .map(|bytes| (path.to_path_buf(), String::from_utf8_lossy(&bytes).into_owned()))
        .map_err(|err| {
            vec![Diagnostic::file_level(format!(
                "not reading theme from \"{theme}\": {}",
                zig_error(&err)
            ))]
        })
}

/// `Path.expand`: `~/` against the home, an absolute path as is, a relative one against `base`, through symlinks when
/// it exists, else lexically.
fn resolve_path(path: &Path, base: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    if let Ok(rest) = path.strip_prefix("~")
        && path.to_string_lossy().starts_with("~/")
    {
        return home.map(|home| home.join(rest)).ok_or_else(|| {
            format!(
                "error expanding home directory for path {}: error.NoHomeDir",
                path.display()
            )
        });
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let joined = base.join(path);
    match std::fs::canonicalize(&joined) {
        Ok(real) => Ok(real),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(normalized(&joined)),
        Err(err) => Err(format!(
            "error resolving file path {}: {}",
            path.display(),
            zig_error(&err)
        )),
    }
}

/// `std.fs.path.resolve`: `.` dropped and `..` applied lexically.
#[must_use]
pub fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The Zig error name libghostty's messages carry for an I/O error.
#[must_use]
pub fn zig_error(err: &io::Error) -> &'static str {
    match err.kind() {
        io::ErrorKind::NotFound => "error.FileNotFound",
        io::ErrorKind::PermissionDenied => "error.AccessDenied",
        io::ErrorKind::IsADirectory => "error.IsDir",
        io::ErrorKind::NotADirectory => "error.NotDir",
        io::ErrorKind::InvalidFilename => "error.NameTooLong",
        _ => "error.Unexpected",
    }
}

/// The tag of Zig's `File.Kind` for a file type.
#[must_use]
pub fn file_kind(kind: std::fs::FileType) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if kind.is_fifo() {
            return "named_pipe";
        } else if kind.is_socket() {
            return "unix_domain_socket";
        } else if kind.is_block_device() {
            return "block_device";
        } else if kind.is_char_device() {
            return "character_device";
        }
    }
    if kind.is_dir() {
        "directory"
    } else if kind.is_symlink() {
        "sym_link"
    } else if kind.is_file() {
        "file"
    } else {
        "unknown"
    }
}
