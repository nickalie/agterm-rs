//! The keys that shape how a pane's program starts, which libghostty's termio applied to every surface it launched
//! (`Surface.zig`, `termio/Exec.zig`, `termio/Termio.zig` at `LIBGHOSTTY_VT_REV`): `env`, `input`,
//! `working-directory`, `shell-integration`, `wait-after-command` and `abnormal-command-exit-runtime`. The values here
//! are as the lines left them; `agterm_term::Host` applies them to a launch, the launch's own values first where
//! libghostty's embedder options came first (an initial input, a working directory, extra variables).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use strum::{EnumIter, EnumString, IntoStaticStr};

use super::validate;

/// `shell-integration`: whether and how the shell integration is injected into a pane's shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum ShellIntegration {
    /// None injected (`GHOSTTY_SHELL_FEATURES` is still set, for an integration sourced by hand).
    None,
    /// By the shell's name.
    #[default]
    Detect,
    Bash,
    Elvish,
    Fish,
    Nushell,
    Zsh,
}

/// `working-directory`: where a pane with no directory of its own starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkingDirectory {
    /// The user's home directory.
    Home,
    /// The app's own working directory.
    Inherit,
    /// This path; a leading `~/` is the home directory's.
    Path(String),
}

impl WorkingDirectory {
    /// `WorkingDirectory.parseCLI`: `home`, `inherit` or a path, one pair of double quotes around it taken off as
    /// `path.zig` takes them off.
    fn parse(value: &str) -> Self {
        let value = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(value);
        match value {
            "home" => Self::Home,
            "inherit" => Self::Inherit,
            path => Self::Path(path.to_owned()),
        }
    }
}

/// One `input` source: bytes sent as they are, or a file's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputSource {
    /// `raw:<string>` (or a value with no known prefix), its Zig string escapes decoded.
    Raw(Vec<u8>),
    /// `path:<path>`: the file's contents, read when the pane starts; its Zig string escapes decoded too.
    Path(PathBuf),
}

impl InputSource {
    /// `ReadableIO.parseCLI` and `cloneParsed`: `cli.args.parseTaggedUnion` takes the tag before the first `:`
    /// (spaces and tabs trimmed; no `:` leaves an empty value), `raw` and `path` tag the value, any other is raw as
    /// a whole; the value's Zig string escapes decoded. `None` when they do not decode (validation refuses the line,
    /// [`validate::Kind::ReadableIo`]).
    fn parse(value: &str) -> Option<Self> {
        let (tag, tagged) = match value.split_once(':') {
            Some((tag, tagged)) => (tag, tagged),
            None => (value, ""),
        };
        match tag.trim_matches([' ', '\t']) {
            "path" => {
                let path = validate::decode_string(tagged)?;
                Some(Self::Path(path_from_bytes(path)))
            }
            "raw" => validate::decode_string(tagged).map(Self::Raw),
            _ => validate::decode_string(value).map(Self::Raw),
        }
    }
}

/// The path a decoded `path:` value names: its bytes as they are on Unix, where a path is bytes.
#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;

    PathBuf::from(OsString::from_vec(bytes))
}

/// The path a decoded `path:` value names: on Windows, where a path is UTF-16 (Zig spells it in WTF-8), the bytes as
/// UTF-8, each ill-formed sequence replaced by U+FFFD.
#[cfg(windows)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    let text = String::from_utf8(bytes).unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
    PathBuf::from(OsString::from(text))
}

/// `std.ascii.whitespace`, which `RepeatableStringMap.parseCLI` trims (vertical tab included, unlike
/// [`char::is_ascii_whitespace`]).
const ZIG_ASCII_WHITESPACE: [char; 6] = [' ', '\t', '\n', '\r', '\u{0b}', '\u{0c}'];

/// libghostty's default `abnormal-command-exit-runtime`, in milliseconds.
pub const DEFAULT_ABNORMAL_COMMAND_EXIT_RUNTIME: u32 = 250;

/// The launch keys, with Ghostty's defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launching {
    /// `env`: variables over every other a pane's program gets, Ghostty's own included.
    pub env: BTreeMap<String, String>,
    /// `input`: sent to the program once it starts, unless the launch types its own.
    pub input: Vec<InputSource>,
    /// `working-directory`; `None` is Ghostty's default (`inherit`, `home` when launched from a desktop launcher).
    pub working_directory: Option<WorkingDirectory>,
    pub shell_integration: ShellIntegration,
    /// `wait-after-command`: every pane's program, the shell's included, holds its pane on "Process exited".
    pub wait_after_command: bool,
    /// `abnormal-command-exit-runtime` in milliseconds.
    pub abnormal_command_exit_runtime: u32,
}

impl Default for Launching {
    fn default() -> Self {
        Self {
            env: BTreeMap::new(),
            input: Vec::new(),
            working_directory: None,
            shell_integration: ShellIntegration::default(),
            wait_after_command: false,
            abnormal_command_exit_runtime: DEFAULT_ABNORMAL_COMMAND_EXIT_RUNTIME,
        }
    }
}

impl Launching {
    /// Applies `key`'s `value` (`None`: an empty value, which resets the key; `Some("")`: a bare key); false when
    /// `key` is none of these.
    pub(super) fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        match key {
            "env" => match value {
                None => self.env.clear(),
                // `RepeatableStringMap.parseCLI`: `key=value` sets, `key=` drops the key, both sides of the first
                // `=` trimmed; a value without `=` is refused before it gets here
                Some(value) => {
                    if let Some((name, entry)) = value.split_once('=') {
                        let name = name.trim_matches(ZIG_ASCII_WHITESPACE);
                        let entry = entry.trim_matches(ZIG_ASCII_WHITESPACE);
                        if entry.is_empty() {
                            self.env.remove(name);
                        } else {
                            self.env.insert(name.to_owned(), entry.to_owned());
                        }
                    }
                }
            },
            "input" => match value {
                None => self.input.clear(),
                Some("") => {}
                Some(value) => self.input.extend(InputSource::parse(value)),
            },
            "working-directory" => match value {
                None => self.working_directory = None,
                Some("") => {}
                Some(value) => self.working_directory = Some(WorkingDirectory::parse(value)),
            },
            "shell-integration" => {
                self.shell_integration = value.and_then(|value| value.parse().ok()).unwrap_or_default();
            }
            "wait-after-command" => {
                self.wait_after_command = value.is_some_and(|value| {
                    validate::parse_bool(if value.is_empty() { "t" } else { value }).unwrap_or(false)
                });
            }
            "abnormal-command-exit-runtime" => {
                // Zig's base-0 integer, as `Kind::Int` checks it
                self.abnormal_command_exit_runtime = value
                    .and_then(|value| validate::parse_int(value, 0, false, 32).ok())
                    .and_then(|number| u32::try_from(number).ok())
                    .unwrap_or(DEFAULT_ABNORMAL_COMMAND_EXIT_RUNTIME);
            }
            _ => return false,
        }
        true
    }
}
