//! Usage errors and help in swift-argument-parser's format. Upstream's CLI (`agtermctlKit/Commands.swift`) is an
//! `ArgumentParser` command tree (swift-argument-parser 1.8.2, `agtermCore/Package.resolved`), so its usage line, its
//! error wording and its help layout are that library's (`MessageInfo.swift`, `HelpGenerator.swift`,
//! `UsageGenerator.swift`):
//!
//! - a usage error: `Error: <message>`, a `Help:  <argument>  <abstract>` line for an argument-specific error, the
//!   `Usage:` line spelling every option, and `  See '<command> --help' for more information.`, on stderr, exit 64;
//! - help, asked for (`-h`, `--help`, `agtermctl help <command>`) or shown for a command that needs a subcommand:
//!   `OVERVIEW:`, `USAGE:`, then `ARGUMENTS:`, `OPTIONS:` (ending in `-h, --help`) and `SUBCOMMANDS:`, wrapped to the
//!   terminal width, on stdout, exit 0.
//!
//! Everything is rendered from the clap command definition, so a command group added later gets the format by
//! declaring its arguments:
//!
//! - a command's `about` is its abstract (`CommandConfiguration.abstract`), its `long_about` the discussion;
//! - an argument's `help` is its abstract; an explicit `value_name` is kept, clap derive's default (the field name in
//!   capitals) becomes the library's own (the long name, else the field name in kebab case);
//! - an argument is optional unless `required`, repeating with `ArgAction::Append` or several values, and taken after
//!   `--` with `last`; its possible values and default join its abstract as `(values: ...; default: ...)`;
//! - a `help_heading` is a titled section between `ARGUMENTS:` and `OPTIONS:`.
//!
//! clap derive drops a doc comment's trailing period, so abstracts are `about = "..."`/`help = "..."` attributes,
//! written as upstream's `abstract:`/`help:` strings.

use agterm_core::paths::Env;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Arg, ArgAction, Command};

/// `EX_USAGE`: the exit status of an argument error (swift-argument-parser's `ExitCode.validationFailure`).
pub const USAGE_EXIT: u8 = 64;

/// Indent of a help line's label.
const HELP_INDENT: usize = 2;
/// Column a help line's abstract starts at.
const LABEL_COLUMN_WIDTH: usize = 26;
/// Width when neither `COLUMNS` nor the terminal says.
const DEFAULT_SCREEN_WIDTH: usize = 80;
/// Above this many arguments the usage line keeps only the positional and required ones.
const MAX_USAGE_ARGUMENTS: usize = 12;
/// An unknown option closer than this (edit distance) to a known one is suggested.
const SIMILARITY_FLOOR: usize = 4;

/// What to print for a failed parse, where, and the exit status.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseExit {
    /// The whole text, trailing newline included.
    pub text: String,
    pub to_stdout: bool,
    pub code: u8,
}

impl ParseExit {
    fn help(stack: &Stack<'_>, width: usize) -> Self {
        // `print` adds its newline to the rendered help
        Self {
            text: format!("{}\n", help_text(stack, width)),
            to_stdout: true,
            code: 0,
        }
    }

    fn usage(stack: &Stack<'_>, message: &str, help: &str) -> Self {
        Self {
            text: usage_error(stack, message, help),
            to_stdout: false,
            code: USAGE_EXIT,
        }
    }
}

/// The width help wraps to: `COLUMNS`, else the terminal's on stdout, else 80.
#[must_use]
pub fn screen_width(env: &dyn Env) -> usize {
    env.var("COLUMNS")
        .and_then(|columns| columns.to_str().and_then(|columns| columns.parse().ok()))
        .or_else(stdout_columns)
        .unwrap_or(DEFAULT_SCREEN_WIDTH)
}

fn stdout_columns() -> Option<usize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `TIOCGWINSZ` writes one `winsize` through the pointer, which points at a live local.
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut size) };
    (result == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

/// Renders a clap parse failure of `root` over `args` (without the program name) as swift-argument-parser reports
/// it, help wrapped to `width`.
#[must_use]
pub fn parse_exit(root: &Command, args: &[String], error: &clap::Error, width: usize) -> ParseExit {
    if let Some((first, rest)) = args.split_first()
        && first == "help"
        && root.find_subcommand("help").is_none()
    {
        return help_command_exit(root, rest, width);
    }
    let (stack, named) = Stack::named_by(root, args);
    let rest = args.get(named..).unwrap_or_default();
    if asks_for_help(error, rest) {
        return ParseExit::help(&stack, width);
    }
    let (message, help) = error_message(stack.leaf, rest, error);
    ParseExit::usage(&stack, &message, &help)
}

/// The library's `help` command (`HelpCommand`): the command names to show help for, and a private
/// `-h`/`--help`/`-help` flag it accepts and ignores. It has no help names of its own, so its usage error ends
/// without a `See '... --help'` line.
fn help_command() -> Command {
    Command::new("help")
        .about("Show subcommand help information.")
        .no_binary_name(true)
        .disable_help_flag(true)
        .args_override_self(true)
        .arg(Arg::new("subcommands").action(ArgAction::Append))
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::SetTrue)
                .hide(true),
        )
}

/// `agtermctl help <args>`, parsed as the library parses its help command: options and `--` first, then the
/// command names resolve the stack (from the first that names no subcommand on, they are ignored). An option the
/// help command does not take is a usage error.
fn help_command_exit(root: &Command, args: &[String], width: usize) -> ParseExit {
    let help = help_command();
    // `-help` is one of the flag's names: a single-dash long, which clap has no notion of
    let mut separated = false;
    let args: Vec<String> = args
        .iter()
        .map(|arg| {
            separated |= arg == "--";
            if !separated && arg == "-help" {
                "--help".to_owned()
            } else {
                arg.clone()
            }
        })
        .collect();
    match help.clone().try_get_matches_from(&args) {
        Ok(matches) => {
            let topics: Vec<String> = matches
                .get_many::<String>("subcommands")
                .map(|topics| topics.cloned().collect())
                .unwrap_or_default();
            ParseExit::help(&Stack::named_by(root, &topics).0, width)
        }
        Err(error) => {
            let stack = Stack {
                path: vec![root, &help],
                leaf: &help,
            };
            let (message, abstract_line) = error_message(&help, &args, &error);
            ParseExit::usage(&stack, &message, &abstract_line)
        }
    }
}

/// The commands from the root down to the one being parsed.
struct Stack<'c> {
    path: Vec<&'c Command>,
    leaf: &'c Command,
}

impl<'c> Stack<'c> {
    /// The commands `args` names, as the library's `CommandParser` descends: each leading argument that names a
    /// subcommand of the one before. Also returns how many arguments named one.
    fn named_by(root: &'c Command, args: &[String]) -> (Self, usize) {
        let mut stack = Self {
            path: vec![root],
            leaf: root,
        };
        for arg in args {
            let Some(next) = stack.leaf.find_subcommand(arg) else {
                break;
            };
            stack.path.push(next);
            stack.leaf = next;
        }
        let named = stack.path.len() - 1;
        (stack, named)
    }

    /// `agtermctl version`.
    fn names(&self) -> String {
        self.path
            .iter()
            .map(|command| command.get_name())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Help asked for or due: clap's own help request, a `-h`/`--help` anywhere before `--` (which wins over any other
/// error, as the library looks for it first), or a command that needs a subcommand run without one.
fn asks_for_help(error: &clap::Error, rest: &[String]) -> bool {
    matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand | ErrorKind::MissingSubcommand
    ) || rest
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "-h" || arg == "--help")
}

/// `Error: <message>`, the `Help:` line when there is one, the usage line and the hint, as `MessageInfo.fullText`
/// prints a validation failure.
fn usage_error(stack: &Stack<'_>, message: &str, help: &str) -> String {
    let error = if message.is_empty() {
        String::new()
    } else {
        format!("Error: {message}\n")
    };
    let help = if help.is_empty() {
        String::new()
    } else {
        format!("Help:  {help}\n")
    };
    // the hint names the command's help flag; a command without one (the help command) gets none
    let see = if stack.leaf.is_disable_help_flag_set() {
        String::new()
    } else {
        format!("  See '{} --help' for more information.\n", stack.names())
    };
    format!("{error}{help}Usage: {}\n{see}", usage_synopsis(stack))
}

// MARK: - error messages (`ErrorMessageGenerator`)

/// The message and the `Help:` line (empty when none) for a parse error of `command`, whose arguments are `rest`.
fn error_message(command: &Command, rest: &[String], error: &clap::Error) -> (String, String) {
    let fallback = || (clap_message(error), String::new());
    match error.kind() {
        ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => (
            stray_message(command, rest).unwrap_or_else(|| clap_message(error)),
            String::new(),
        ),
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => invalid_value(command, error).unwrap_or_else(fallback),
        ErrorKind::MissingRequiredArgument => missing_argument(command, error).unwrap_or_else(fallback),
        ErrorKind::TooManyValues => unexpected_value(command, error).unwrap_or_else(fallback),
        _ => fallback(),
    }
}

/// clap's own message for an error the library has no counterpart of, as a sentence.
fn clap_message(error: &clap::Error) -> String {
    let rendered = error.render().to_string();
    let line = rendered.lines().next().unwrap_or_default();
    capitalized(line.strip_prefix("error: ").unwrap_or(line))
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What `rest` holds that `command` does not take: an unknown option, else the values past its positional
/// arguments. The library rejects unknown options before it assigns values, so one anywhere wins.
fn stray_message(command: &Command, rest: &[String]) -> Option<String> {
    match scan(command, rest) {
        Scan::UnknownOption(name) => Some(unknown_option_message(command, &name)),
        Scan::Values(values) => {
            let slots = positional_slots(command)?;
            unexpected_message(values.get(slots..).unwrap_or_default())
        }
    }
}

enum Scan {
    UnknownOption(String),
    Values(Vec<String>),
}

/// Sorts `args` into option names and values the way the library splits them: everything after `--` is a value,
/// and an option that takes a value takes the next argument unless it looks like an option.
fn scan(command: &Command, args: &[String]) -> Scan {
    let mut values = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        if arg == "--" {
            values.extend(args.by_ref().cloned());
        } else if arg.len() < 2 || !arg.starts_with('-') {
            values.push(arg.clone());
        } else {
            let (name, inline) = arg
                .split_once('=')
                .map_or((arg.as_str(), false), |(name, _)| (name, true));
            match option_takes_next(command, name) {
                None => return Scan::UnknownOption(name.to_owned()),
                Some(true) if !inline && args.peek().is_some_and(|next| !next.starts_with('-')) => {
                    args.next();
                }
                Some(_) => {}
            }
        }
    }
    Scan::Values(values)
}

/// Whether the option `name` (`--long`, `-s` or a cluster of shorts) takes the next argument as its value, `None`
/// when `command` has no such option.
fn option_takes_next(command: &Command, name: &str) -> Option<bool> {
    if let Some(long) = name.strip_prefix("--") {
        return own_args(command)
            .find(|arg| arg.get_long() == Some(long))
            .map(|arg| takes_value(arg) && !optional_value(arg));
    }
    let shorts: Vec<char> = name.strip_prefix('-')?.chars().collect();
    for (at, short) in shorts.iter().enumerate() {
        let arg = own_args(command).find(|arg| arg.get_short() == Some(*short))?;
        if takes_value(arg) {
            // the rest of the cluster is the value, else the next argument is
            return Some(at + 1 == shorts.len() && !optional_value(arg));
        }
    }
    Some(false)
}

/// How many values the positional arguments take, `None` when one takes any number.
fn positional_slots(command: &Command) -> Option<usize> {
    own_args(command)
        .filter(|arg| arg.is_positional())
        .try_fold(0, |slots, arg| (!repeating(arg)).then_some(slots + 1))
}

fn unknown_option_message(command: &Command, name: &str) -> String {
    let is_short = name
        .strip_prefix('-')
        .is_some_and(|short| !short.starts_with('-') && short.chars().count() == 1);
    let suggestion = if is_short { None } else { suggestion(command, name) };
    match suggestion {
        Some(known) => format!("Unknown option '{name}'. Did you mean '{known}'?"),
        None => format!("Unknown option '{name}'"),
    }
}

/// The long option of `command` closest to `name`, if close enough; the first on a tie.
fn suggestion(command: &Command, name: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for arg in own_args(command).filter(|arg| !arg.is_hide_set()) {
        let Some(long) = arg.get_long() else {
            continue;
        };
        let candidate = format!("--{long}");
        let distance = edit_distance(&candidate, name);
        if distance < SIMILARITY_FLOOR && best.as_ref().is_none_or(|(closest, _)| distance < *closest) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// Levenshtein distance over characters.
fn edit_distance(from: &str, to: &str) -> usize {
    let to: Vec<char> = to.chars().collect();
    let mut row: Vec<usize> = (0..=to.len()).collect();
    for (i, source) in from.chars().enumerate() {
        let mut diagonal = i;
        row[0] = i + 1;
        for (j, target) in to.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if source == *target {
                diagonal
            } else {
                diagonal.min(above).min(row[j]) + 1
            };
            diagonal = above;
        }
    }
    row[to.len()]
}

fn unexpected_message(values: &[String]) -> Option<String> {
    match values {
        [] => None,
        [value] => Some(format!("Unexpected argument '{value}'")),
        _ => Some(format!(
            "{} unexpected arguments: '{}'",
            values.len(),
            values.join("', '")
        )),
    }
}

/// A value an argument refused, or an option given no value: `Missing value for '--x <x>'` and `The value 'v' is
/// invalid for '--x <x>'`, with the argument's `Help:` line. Only a refused choice lists the choices: the library
/// adds nothing to a value its type could not convert.
fn invalid_value(command: &Command, error: &clap::Error) -> Option<(String, String)> {
    let arg = context_arg(command, error)?;
    let value = context_string(error, ContextKind::InvalidValue).unwrap_or_default();
    let target = value_label(arg);
    let help = format!("{target}  {}", abstract_text(arg));
    if error.kind() == ErrorKind::InvalidValue && value.is_empty() && !arg.is_positional() {
        return Some((format!("Missing value for '{target}'"), help));
    }
    let choices = if error.kind() == ErrorKind::InvalidValue {
        value_list(&possible_values(arg))
    } else {
        String::new()
    };
    Some((format!("The value '{value}' is invalid for '{target}'{choices}"), help))
}

/// The choices after a refused one, as `formattedValueList`.
fn value_list(values: &[String]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| format!("'{value}'")).collect();
    match quoted.as_slice() {
        [] => String::new(),
        [_] | [_, _] => format!(". Please provide one of {}.", quoted.join(" and ")),
        [init @ .., last] if quoted.len() < 6 => format!(". Please provide one of {} or {last}.", init.join(", ")),
        _ => {
            let bullets: Vec<String> = values.iter().map(|value| format!("  - {value}")).collect();
            format!(". Please provide one of the following:\n{}", bullets.join("\n"))
        }
    }
}

/// `Missing expected argument '<id>'` for the first missing argument in declaration order.
fn missing_argument(command: &Command, error: &clap::Error) -> Option<(String, String)> {
    let missing = match error.get(ContextKind::InvalidArg)? {
        ContextValue::Strings(names) => names.clone(),
        ContextValue::String(name) => vec![name.clone()],
        _ => return None,
    };
    let arg = own_args(command).find(|arg| missing.iter().any(|name| displays_as(arg, name)))?;
    let mut expected = unadorned(arg);
    if repeating(arg) {
        expected.push_str(" ...");
    }
    Some((
        format!("Missing expected argument '{expected}'"),
        format!("{}  {}", value_label(arg), abstract_text(arg)),
    ))
}

/// A flag given a value (`--json=3`).
fn unexpected_value(command: &Command, error: &clap::Error) -> Option<(String, String)> {
    let arg = context_arg(command, error).filter(|arg| !takes_value(arg))?;
    let value = context_string(error, ContextKind::InvalidValue)?;
    let name = preferred_name(arg)?;
    Some((
        format!("The option '{name}' does not take any value, but '{value}' was specified."),
        String::new(),
    ))
}

fn context_string(error: &clap::Error, kind: ContextKind) -> Option<String> {
    match error.get(kind)? {
        ContextValue::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// The argument clap names in the error's `InvalidArg` context.
fn context_arg<'c>(command: &'c Command, error: &clap::Error) -> Option<&'c Arg> {
    let name = context_string(error, ContextKind::InvalidArg)?;
    own_args(command).find(|arg| displays_as(arg, &name))
}

/// Whether clap's rendering of an argument (`--width <WIDTH>`, `-w <WIDTH>`, `<ID>...`) names `arg`.
fn displays_as(arg: &Arg, rendered: &str) -> bool {
    let word = rendered.split(' ').next().unwrap_or(rendered);
    if let Some(long) = word.strip_prefix("--") {
        return arg.get_long() == Some(long);
    }
    if let Some(short) = word.strip_prefix('-') {
        return arg.get_short().is_some_and(|own| short == own.to_string());
    }
    let inner = word.trim_start_matches('<').split('>').next().unwrap_or_default();
    let clap_name = arg
        .get_value_names()
        .and_then(<[_]>::first)
        .map_or(arg.get_id().as_str(), |name| name.as_str());
    arg.is_positional() && clap_name == inner
}

// MARK: - arguments

/// The arguments `command` declares, without clap's built-in help and version flags.
fn own_args(command: &Command) -> impl Iterator<Item = &Arg> {
    command.get_arguments().filter(|arg| {
        !matches!(
            arg.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
    })
}

fn visible_args(command: &Command) -> impl Iterator<Item = &Arg> {
    own_args(command).filter(|arg| !arg.is_hide_set())
}

fn takes_value(arg: &Arg) -> bool {
    arg.get_action().takes_values()
}

/// An option whose value may be left out (`num_args(0..=1)`).
fn optional_value(arg: &Arg) -> bool {
    takes_value(arg) && arg.get_num_args().is_some_and(|range| range.min_values() == 0)
}

fn repeating(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Append | ArgAction::Count)
        || arg.get_num_args().is_some_and(|range| range.max_values() > 1)
}

/// The value's name: an explicit `value_name`, else the long name, else the field name in kebab case. clap derive
/// fills in the field name in capitals, which counts as unset.
fn value_name(arg: &Arg) -> String {
    let derived = arg.get_id().as_str().to_uppercase();
    match arg.get_value_names().and_then(<[_]>::first) {
        Some(name) if name.as_str() != derived => name.to_string(),
        _ => arg.get_long().map_or_else(
            || {
                arg.get_short()
                    .map_or_else(|| arg.get_id().as_str().replace('_', "-"), String::from)
            },
            str::to_owned,
        ),
    }
}

/// The long name, else the short one.
fn preferred_name(arg: &Arg) -> Option<String> {
    arg.get_long()
        .map(|long| format!("--{long}"))
        .or_else(|| arg.get_short().map(|short| format!("-{short}")))
}

/// What follows an option's name: ` <v>`, ` [<v>]` or nothing for a flag.
fn value_suffix(arg: &Arg) -> String {
    if !takes_value(arg) {
        String::new()
    } else if optional_value(arg) {
        format!(" [<{}>]", value_name(arg))
    } else {
        format!(" <{}>", value_name(arg))
    }
}

/// `--x <x>` or `<x>`: an argument in an error message.
fn value_label(arg: &Arg) -> String {
    match preferred_name(arg) {
        Some(name) if !arg.is_positional() => format!("{name} <{}>", value_name(arg)),
        _ => format!("<{}>", value_name(arg)),
    }
}

/// The argument in the usage line, before marking it repeating or optional.
fn unadorned(arg: &Arg) -> String {
    match preferred_name(arg) {
        Some(name) if !arg.is_positional() => format!("{name}{}", value_suffix(arg)),
        _ => format!("<{}>", value_name(arg)),
    }
}

fn synopsis(arg: &Arg) -> String {
    let mut synopsis = unadorned(arg);
    if repeating(arg) {
        synopsis.push_str(" ...");
    }
    if !arg.is_required_set() {
        synopsis = format!("[{synopsis}]");
    }
    if arg.is_last_set() {
        synopsis = format!("-- {synopsis}");
    }
    synopsis
}

/// The argument's help text.
fn abstract_text(arg: &Arg) -> String {
    arg.get_help().map(ToString::to_string).unwrap_or_default()
}

fn possible_values(arg: &Arg) -> Vec<String> {
    if !takes_value(arg) {
        return Vec::new();
    }
    arg.get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect()
}

// MARK: - usage and help (`UsageGenerator`, `HelpGenerator`)

/// `agtermctl version [--socket <socket>] [--json]`; a command with subcommands ends in `<subcommand>`.
fn usage_synopsis(stack: &Stack<'_>) -> String {
    let tool = stack.names();
    let args: Vec<&Arg> = visible_args(stack.leaf).collect();
    let mut usage = if args.is_empty() {
        tool
    } else if args.len() > MAX_USAGE_ARGUMENTS {
        let kept: Vec<String> = args
            .iter()
            .filter(|arg| arg.is_positional() || arg.is_required_set())
            .map(|arg| synopsis(arg))
            .collect();
        if kept.is_empty() || kept.len() > MAX_USAGE_ARGUMENTS {
            format!("{tool} <options>")
        } else {
            format!("{tool} [<options>] {}", kept.join(" "))
        }
    } else {
        let all: Vec<String> = args.iter().map(|arg| synopsis(arg)).collect();
        format!("{tool} {}", all.join(" "))
    };
    if stack.leaf.has_subcommands() {
        if !usage.ends_with(' ') {
            usage.push(' ');
        }
        usage.push_str("<subcommand>");
    }
    usage
}

/// The help screen without its final newline.
fn help_text(stack: &Stack<'_>, width: usize) -> String {
    let mut overview = stack.leaf.get_about().map(ToString::to_string).unwrap_or_default();
    if let Some(discussion) = stack.leaf.get_long_about().map(ToString::to_string)
        && !discussion.is_empty()
    {
        if !overview.is_empty() {
            overview.push('\n');
        }
        overview.push('\n');
        overview.push_str(&discussion);
    }
    let overview = if overview.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", wrap(&format!("OVERVIEW: {overview}"), width, 0))
    };
    let sections = sections(stack.leaf);
    let has_subcommands = sections
        .iter()
        .any(|section| section.header == SUBCOMMANDS && !section.elements.is_empty());
    let rendered: Vec<String> = sections
        .iter()
        .map(|section| section.render(width))
        .filter(|section| !section.is_empty())
        .collect();
    let see = if has_subcommands {
        let mut names: Vec<&str> = stack.path.iter().map(|command| command.get_name()).collect();
        names.insert(1.min(names.len()), "help");
        format!("\n  See '{} <subcommand>' for detailed help.", names.join(" "))
    } else {
        String::new()
    };
    format!(
        "{overview}USAGE: {}\n\n{}{see}",
        usage_synopsis(stack),
        rendered.join("\n")
    )
}

const SUBCOMMANDS: &str = "Subcommands";

struct Section {
    /// The title as declared; rendered in capitals.
    header: String,
    elements: Vec<(String, String)>,
}

impl Section {
    fn new(header: &str) -> Self {
        Self {
            header: header.to_owned(),
            elements: Vec::new(),
        }
    }

    fn render(&self, width: usize) -> String {
        if self.elements.is_empty() {
            return String::new();
        }
        let elements: String = self
            .elements
            .iter()
            .map(|(label, description)| render_element(label, description, width))
            .collect();
        format!("{}:\n{elements}", self.header.to_uppercase())
    }
}

/// `ARGUMENTS`, the titled sections, `OPTIONS` (help last) and `SUBCOMMANDS`, empty ones included.
fn sections(command: &Command) -> Vec<Section> {
    let mut positionals = Section::new("Arguments");
    let mut titled: Vec<Section> = Vec::new();
    let mut options = Section::new("Options");
    for arg in visible_args(command) {
        let element = (help_label(arg), help_description(arg));
        if let Some(heading) = arg.get_help_heading() {
            if let Some(section) = titled.iter_mut().find(|section| section.header == heading) {
                section.elements.push(element);
            } else {
                let mut section = Section::new(heading);
                section.elements.push(element);
                titled.push(section);
            }
        } else if arg.is_positional() {
            positionals.elements.push(element);
        } else {
            options.elements.push(element);
        }
    }
    options
        .elements
        .push(("-h, --help".to_owned(), "Show help information.".to_owned()));
    let mut subcommands = Section::new(SUBCOMMANDS);
    for subcommand in command.get_subcommands().filter(|subcommand| !subcommand.is_hide_set()) {
        let names: Vec<&str> = std::iter::once(subcommand.get_name())
            .chain(subcommand.get_visible_aliases())
            .collect();
        let label = names.join(", ");
        let about = subcommand.get_about().map(ToString::to_string).unwrap_or_default();
        subcommands.elements.push((label, about));
    }
    let mut all = vec![positionals];
    all.extend(titled);
    all.push(options);
    all.push(subcommands);
    all
}

/// `-s, --socket <socket>` or `<id>`.
fn help_label(arg: &Arg) -> String {
    if arg.is_positional() {
        return format!("<{}>", value_name(arg));
    }
    let names: Vec<String> = arg
        .get_short()
        .map(|short| format!("-{short}"))
        .into_iter()
        .chain(arg.get_long().map(|long| format!("--{long}")))
        .collect();
    format!("{}{}", names.join(", "), value_suffix(arg))
}

/// The abstract, then `(values: ...; default: ...)` for an argument with choices or a default.
fn help_description(arg: &Arg) -> String {
    let values = possible_values(arg);
    let default = if takes_value(arg) {
        let defaults: Vec<String> = arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        defaults.join(", ")
    } else {
        String::new()
    };
    let values_and_default = match (values.is_empty(), default.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("(values: {})", values.join(", ")),
        (true, false) => format!("(default: {default})"),
        (false, false) => format!("(values: {}; default: {default})", values.join(", ")),
    };
    [abstract_text(arg), values_and_default]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A help line: the label indented by 2, the description from column 26 (on the next line when the label reaches
/// it), wrapped to `width`.
fn render_element(label: &str, description: &str, width: usize) -> String {
    let padded = format!("{}{label}", " ".repeat(HELP_INDENT));
    let padded_len = padded.chars().count();
    let description = if description.is_empty() {
        String::new()
    } else if padded_len < LABEL_COLUMN_WIDTH {
        wrap(description, width, LABEL_COLUMN_WIDTH)
            .chars()
            .skip(padded_len)
            .collect()
    } else {
        format!("\n{}", wrap(description, width, LABEL_COLUMN_WIDTH))
    };
    format!("{padded}{description}\n")
}

/// The library's `wrapped(to:wrappingIndent:)`: lines of at most `width - indent` characters broken at the last
/// space (a word longer than that runs on to the next space), existing newlines kept, every non-empty line
/// indented. Empty when `indent` leaves no room.
fn wrap(text: &str, width: usize, indent: usize) -> String {
    let Some(columns) = width.checked_sub(indent).filter(|columns| *columns > 0) else {
        return String::new();
    };
    let chars: Vec<char> = text.chars().collect();
    let piece = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut lines: Vec<String> = Vec::new();
    let mut start = 0;
    loop {
        let end = (start + columns).min(chars.len());
        let chunk = &chars[start..end];
        if let Some(newline) = chunk.iter().rposition(|c| *c == '\n') {
            lines.extend(piece(start, start + newline).split('\n').map(str::to_owned));
            start += newline + 1;
        } else if end == chars.len() {
            lines.push(piece(start, end));
            break;
        } else if let Some(space) = chunk
            .iter()
            .rposition(|c| *c == ' ')
            .or_else(|| chars[start..].iter().position(|c| *c == ' '))
        {
            lines.push(piece(start, start + space));
            start += space + 1;
        } else {
            lines.push(piece(start, chars.len()));
            break;
        }
    }
    let pad = " ".repeat(indent);
    let indented: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect();
    indented.join("\n")
}

#[cfg(test)]
mod tests {
    use agterm_core::paths::MapEnv;

    use super::*;

    /// A command with a required argument, a choice, a number, a default and a nested group, as later command
    /// groups have.
    fn root() -> Command {
        Command::new("agtermctl")
            .about("Drive it.")
            .subcommand_required(true)
            .arg_required_else_help(true)
            .disable_help_subcommand(true)
            .args_override_self(true)
            .subcommand(
                Command::new("pick")
                    .about("Pick one.")
                    .arg(Arg::new("id").required(true).help("The item id."))
                    .arg(
                        Arg::new("mode")
                            .long("mode")
                            .value_parser(["fast", "slow"])
                            .help("How to pick."),
                    )
                    .arg(
                        Arg::new("width")
                            .long("width")
                            .value_name("COLUMNS")
                            .value_parser(clap::value_parser!(u32))
                            .help("Panel width."),
                    )
                    .arg(
                        Arg::new("target")
                            .long("target")
                            .default_value("active")
                            .help("Target session id, unique prefix, or 'active'."),
                    )
                    .arg(
                        Arg::new("json")
                            .long("json")
                            .action(ArgAction::SetTrue)
                            .help("Print JSON."),
                    ),
            )
            .subcommand(
                Command::new("group")
                    .about("A group.")
                    .subcommand_required(true)
                    .arg_required_else_help(true)
                    .subcommand(Command::new("leaf").about("The leaf."))
                    .subcommand(Command::new("secret").hide(true)),
            )
    }

    fn report(args: &[&str]) -> ParseExit {
        let root = root();
        let error = root
            .clone()
            .try_get_matches_from(std::iter::once("agtermctl").chain(args.iter().copied()));
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        parse_exit(&root, &args, &error.unwrap_err(), 80)
    }

    const PICK_USAGE: &str = "Usage: agtermctl pick <id> [--mode <mode>] [--width <COLUMNS>] [--target <target>] \
                              [--json]\n  See 'agtermctl pick --help' for more information.\n";

    fn usage(message: &str, help: Option<&str>) -> ParseExit {
        let help = help.map(|help| format!("Help:  {help}\n")).unwrap_or_default();
        ParseExit {
            text: format!("Error: {message}\n{help}{PICK_USAGE}"),
            to_stdout: false,
            code: USAGE_EXIT,
        }
    }

    #[test]
    fn a_missing_argument_is_named_with_its_help() {
        assert_eq!(
            report(&["pick"]),
            usage("Missing expected argument '<id>'", Some("<id>  The item id."))
        );
    }

    #[test]
    fn a_refused_choice_lists_the_choices() {
        assert_eq!(
            report(&["pick", "x", "--mode", "warp"]),
            usage(
                "The value 'warp' is invalid for '--mode <mode>'. Please provide one of 'fast' and 'slow'.",
                Some("--mode <mode>  How to pick.")
            )
        );
    }

    #[test]
    fn an_unconvertible_value_is_invalid_without_more() {
        assert_eq!(
            report(&["pick", "x", "--width", "wide"]),
            usage(
                "The value 'wide' is invalid for '--width <COLUMNS>'",
                Some("--width <COLUMNS>  Panel width.")
            )
        );
    }

    #[test]
    fn a_missing_value_is_named() {
        assert_eq!(
            report(&["pick", "x", "--mode"]),
            usage("Missing value for '--mode <mode>'", Some("--mode <mode>  How to pick."))
        );
        assert_eq!(
            report(&["pick", "x", "--mode", "--json"]),
            usage("Missing value for '--mode <mode>'", Some("--mode <mode>  How to pick."))
        );
    }

    #[test]
    fn strays_are_named() {
        assert_eq!(report(&["pick", "x", "y"]), usage("Unexpected argument 'y'", None));
        assert_eq!(
            report(&["pick", "x", "y", "--target", "t", "z"]),
            usage("2 unexpected arguments: 'y', 'z'", None)
        );
        assert_eq!(
            report(&["pick", "x", "--", "--y"]),
            usage("Unexpected argument '--y'", None)
        );
    }

    #[test]
    fn an_unknown_option_wins_and_a_close_one_is_suggested() {
        assert_eq!(
            report(&["pick", "x", "y", "--bogus=1"]),
            usage("Unknown option '--bogus'", None)
        );
        assert_eq!(
            report(&["pick", "x", "--mod", "fast"]),
            usage("Unknown option '--mod'. Did you mean '--mode'?", None)
        );
        assert_eq!(
            report(&["pick", "x", "-target"]),
            usage("Unknown option '-target'. Did you mean '--target'?", None)
        );
        assert_eq!(report(&["pick", "x", "-j"]), usage("Unknown option '-j'", None));
    }

    #[test]
    fn a_flag_given_a_value_is_refused() {
        assert_eq!(
            report(&["pick", "x", "--json=3"]),
            usage(
                "The option '--json' does not take any value, but '3' was specified.",
                None
            )
        );
    }

    #[test]
    fn a_group_reports_against_its_own_usage() {
        let expected = "Error: Unexpected argument 'bogus'\nUsage: agtermctl group <subcommand>\n  See 'agtermctl \
                        group --help' for more information.\n";
        assert_eq!(report(&["group", "bogus"]).text, expected);
        let root = "Error: Unknown option '--mode'\nUsage: agtermctl <subcommand>\n  See 'agtermctl --help' for more \
                    information.\n";
        assert_eq!(report(&["--mode", "fast", "pick", "x"]).text, root);
    }

    const PICK_HELP: &str = "\
OVERVIEW: Pick one.

USAGE: agtermctl pick <id> [--mode <mode>] [--width <COLUMNS>] [--target <target>] [--json]

ARGUMENTS:
  <id>                    The item id.

OPTIONS:
  --mode <mode>           How to pick. (values: fast, slow)
  --width <COLUMNS>       Panel width.
  --target <target>       Target session id, unique prefix, or 'active'.
                          (default: active)
  --json                  Print JSON.
  -h, --help              Show help information.

";

    #[test]
    fn help_lists_arguments_and_options() {
        for args in [
            &["pick", "--help"][..],
            &["pick", "-h"],
            &["help", "pick"],
            &["pick", "--bogus", "-h"],
        ] {
            let report = report(args);
            assert_eq!((report.to_stdout, report.code), (true, 0), "{args:?}");
            assert_eq!(report.text, PICK_HELP, "{args:?}");
        }
    }

    const GROUP_HELP: &str = "\
OVERVIEW: A group.

USAGE: agtermctl group <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  leaf                    The leaf.

  See 'agtermctl help group <subcommand>' for detailed help.
";

    #[test]
    fn a_group_without_its_subcommand_shows_its_help() {
        for args in [&["group"][..], &["group", "--help"], &["help", "group", "extra"]] {
            assert_eq!(report(args).text, GROUP_HELP, "{args:?}");
        }
    }

    #[test]
    fn the_help_command_parses_its_options_and_separator_first() {
        for args in [
            &["help", "--", "pick"][..],
            &["help", "pick", "--help"],
            &["help", "-h", "pick"],
            &["help", "-help", "pick"],
            &["help", "pick", "extra"],
        ] {
            let report = report(args);
            assert_eq!((report.to_stdout, report.code), (true, 0), "{args:?}");
            assert_eq!(report.text, PICK_HELP, "{args:?}");
        }
        assert_eq!(report(&["help", "--", "group", "leaf", "--x"]).code, 0);
    }

    #[test]
    fn the_help_command_refuses_an_unknown_option() {
        let refused = |option: &str| ParseExit {
            text: format!("Error: Unknown option '{option}'\nUsage: agtermctl help [<subcommands> ...]\n"),
            to_stdout: false,
            code: USAGE_EXIT,
        };
        assert_eq!(report(&["help", "pick", "--bogus"]), refused("--bogus"));
        assert_eq!(report(&["help", "--json", "pick"]), refused("--json"));
        assert_eq!(report(&["help", "pick", "-x"]), refused("-x"));
        assert_eq!(
            report(&["help", "--help=1"]).text,
            "Error: The option '--help' does not take any value, but '1' was specified.\nUsage: agtermctl help \
             [<subcommands> ...]\n"
        );
    }

    #[test]
    fn the_usage_line_shortens_past_twelve_arguments() {
        let mut command = Command::new("many").arg(Arg::new("id").required(true));
        for name in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"] {
            command = command.arg(Arg::new(name).long(name).action(ArgAction::SetTrue));
        }
        let stack = Stack::named_by(&command, &[]).0;
        assert_eq!(usage_synopsis(&stack), "many [<options>] <id>");
    }

    #[test]
    fn a_long_label_puts_its_description_on_the_next_line() {
        let label = "--a-very-long-option <value>";
        assert_eq!(
            render_element(label, "Does it.", 80),
            format!("  {label}\n{}Does it.\n", " ".repeat(LABEL_COLUMN_WIDTH))
        );
        assert_eq!(render_element("--x", "", 80), "  --x\n");
    }

    #[test]
    fn wrapping_breaks_at_the_last_space_and_keeps_newlines() {
        // a chunk is `width - indent` characters and breaks at its last space, so a line is one shorter
        assert_eq!(wrap("aaa bbb ccc", 7, 0), "aaa\nbbb ccc");
        assert_eq!(wrap("aaa bbb ccc", 8, 0), "aaa bbb\nccc");
        assert_eq!(wrap("aaa bbb ccc", 10, 2), "  aaa bbb\n  ccc");
        assert_eq!(wrap("abcdefghij kl", 4, 0), "abcdefghij\nkl");
        assert_eq!(wrap("one\n\ntwo", 80, 2), "  one\n\n  two");
        assert_eq!(wrap("text", 4, 4), "");
    }

    #[test]
    fn edit_distance_is_levenshtein() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("--json", "--j"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn the_width_comes_from_columns_first() {
        assert_eq!(screen_width(&MapEnv::new().with("COLUMNS", "120")), 120);
        let fallback = screen_width(&MapEnv::new().with("COLUMNS", "wide"));
        assert_eq!(fallback, stdout_columns().unwrap_or(DEFAULT_SCREEN_WIDTH));
    }
}
