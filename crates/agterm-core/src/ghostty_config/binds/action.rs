//! The action table: every `Binding.Action` of ghostty at `GHOSTTY_REV`, parsed as `Action.parse` parses it
//! (`name` or `name:parameter`, a parameter optional only where the type has a default).

use std::fmt;
use std::str::FromStr;

use strum::{EnumDiscriminants, EnumIter, EnumString, IntoStaticStr};

use super::BindError;
use crate::ghostty_config::validate;

/// `CopyToClipboard`: the formats a copy puts on the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum CopyFormat {
    Plain,
    Vt,
    Html,
    /// Plain text and HTML together, the default.
    Mixed,
}

/// `NavigateSearch` and `GotoWindow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum Direction {
    Previous,
    Next,
}

/// `AdjustSelection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum SelectionAdjust {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    BeginningOfLine,
    EndOfLine,
}

/// `SplitDirection`: where `new_split` puts the new pane; `auto` (the default) splits the longer side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
    Left,
    Up,
    Auto,
}

/// `SplitFocusDirection`: `top` and `bottom` are still taken for `up` and `down`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum SplitFocus {
    Previous,
    Next,
    #[strum(to_string = "up", serialize = "top")]
    Up,
    Left,
    #[strum(to_string = "down", serialize = "bottom")]
    Down,
    Right,
}

/// `SplitResizeDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum ResizeDirection {
    Up,
    Down,
    Left,
    Right,
}

/// `WriteScreen.Action`: what happens to the written file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum WriteAction {
    Copy,
    Paste,
    Open,
}

/// `WriteScreen.Format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum WriteFormat {
    Plain,
    Vt,
    Html,
}

/// `WriteScreen`: `ACTION` or `ACTION,FORMAT` (plain by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteScreen {
    pub action: WriteAction,
    pub format: WriteFormat,
}

/// `InspectorMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum InspectorMode {
    Toggle,
    Show,
    Hide,
}

/// `OpenConfig`: `os_open` (the default) or `new_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum OpenConfig {
    OsOpen,
    NewWindow,
}

/// `CloseTabMode`: `this` (the default), `other` or `right`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum CloseTabMode {
    This,
    Other,
    Right,
}

/// `CrashThread`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum CrashThread {
    Main,
    Io,
    Render,
}

/// Where an action runs (`Action.Scope`): app-wide, or on the terminal the key reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    App,
    Surface,
}

/// One keybind action: ghostty's `Binding.Action` union, every tag but `cursor_key` (which no config can set).
/// Integers keep Zig's widths (`usize`/`isize` as 64-bit); the name of each is [`ActionKind`]'s.
#[derive(Debug, Clone, PartialEq, EnumDiscriminants)]
#[strum_discriminants(
    name(ActionKind),
    derive(EnumString, IntoStaticStr, EnumIter, Hash),
    strum(serialize_all = "snake_case")
)]
pub enum Action {
    Ignore,
    Unbind,
    Csi(String),
    Esc(String),
    Text(String),
    Reset,
    CopyToClipboard(CopyFormat),
    PasteFromClipboard,
    PasteFromSelection,
    CopyUrlToClipboard,
    CopyTitleToClipboard,
    IncreaseFontSize(f32),
    DecreaseFontSize(f32),
    ResetFontSize,
    SetFontSize(f32),
    Search(String),
    SearchSelection,
    NavigateSearch(Direction),
    StartSearch,
    EndSearch,
    ClearScreen,
    SelectAll,
    ScrollToTop,
    ScrollToBottom,
    ScrollToSelection,
    ScrollToRow(u64),
    ScrollPageUp,
    ScrollPageDown,
    ScrollPageFractional(f32),
    ScrollPageLines(i16),
    AdjustSelection(SelectionAdjust),
    JumpToPrompt(i16),
    WriteScrollbackFile(WriteScreen),
    WriteScreenFile(WriteScreen),
    WriteSelectionFile(WriteScreen),
    NewWindow,
    NewTab,
    PreviousTab,
    NextTab,
    LastTab,
    GotoTab(u64),
    MoveTab(i64),
    MoveTabToNewWindow,
    ToggleTabOverview,
    PromptSurfaceTitle,
    PromptTabTitle,
    PromptWindowTitle,
    SetSurfaceTitle(String),
    SetTabTitle(String),
    SetWindowTitle(String),
    NewSplit(SplitDirection),
    GotoSplit(SplitFocus),
    GotoWindow(Direction),
    ToggleSplitZoom,
    ToggleReadonly,
    ResizeSplit(ResizeDirection, u16),
    EqualizeSplits,
    ResetWindowSize,
    Inspector(InspectorMode),
    ShowGtkInspector,
    ShowOnScreenKeyboard,
    OpenConfig(OpenConfig),
    ReloadConfig,
    CloseSurface,
    CloseTab(CloseTabMode),
    CloseWindow,
    CloseAllWindows,
    ToggleMaximize,
    ToggleFullscreen,
    ToggleWindowDecorations,
    ToggleWindowFloatOnTop,
    ToggleSecureInput,
    ToggleMouseReporting,
    ToggleCommandPalette,
    ToggleQuickTerminal,
    ToggleVisibility,
    ToggleBackgroundOpacity,
    CheckForUpdates,
    Undo,
    Redo,
    EndKeySequence,
    ActivateKeyTable(String),
    ActivateKeyTableOnce(String),
    DeactivateKeyTable,
    DeactivateAllKeyTables,
    Quit,
    Crash(CrashThread),
}

/// The actions without a parameter.
const UNIT_ACTIONS: &[Action] = &[
    Action::Ignore,
    Action::Unbind,
    Action::Reset,
    Action::PasteFromClipboard,
    Action::PasteFromSelection,
    Action::CopyUrlToClipboard,
    Action::CopyTitleToClipboard,
    Action::ResetFontSize,
    Action::SearchSelection,
    Action::StartSearch,
    Action::EndSearch,
    Action::ClearScreen,
    Action::SelectAll,
    Action::ScrollToTop,
    Action::ScrollToBottom,
    Action::ScrollToSelection,
    Action::ScrollPageUp,
    Action::ScrollPageDown,
    Action::NewWindow,
    Action::NewTab,
    Action::PreviousTab,
    Action::NextTab,
    Action::LastTab,
    Action::MoveTabToNewWindow,
    Action::ToggleTabOverview,
    Action::PromptSurfaceTitle,
    Action::PromptTabTitle,
    Action::PromptWindowTitle,
    Action::ToggleSplitZoom,
    Action::ToggleReadonly,
    Action::EqualizeSplits,
    Action::ResetWindowSize,
    Action::ShowGtkInspector,
    Action::ShowOnScreenKeyboard,
    Action::ReloadConfig,
    Action::CloseSurface,
    Action::CloseWindow,
    Action::CloseAllWindows,
    Action::ToggleMaximize,
    Action::ToggleFullscreen,
    Action::ToggleWindowDecorations,
    Action::ToggleWindowFloatOnTop,
    Action::ToggleSecureInput,
    Action::ToggleMouseReporting,
    Action::ToggleCommandPalette,
    Action::ToggleQuickTerminal,
    Action::ToggleVisibility,
    Action::ToggleBackgroundOpacity,
    Action::CheckForUpdates,
    Action::Undo,
    Action::Redo,
    Action::EndKeySequence,
    Action::DeactivateKeyTable,
    Action::DeactivateAllKeyTables,
    Action::Quit,
];

/// The most bytes `csi:` and `esc:` write, prefix included (`performBindingAction`'s buffer).
const SEQUENCE_BUFFER: usize = 128;

impl Action {
    /// The action's name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        ActionKind::from(self).into()
    }

    /// `Action.scope`: the app-wide actions, every other one runs on the terminal the key reached.
    #[must_use]
    pub fn scope(&self) -> Scope {
        match self {
            Self::Ignore
            | Self::Unbind
            | Self::OpenConfig(_)
            | Self::ReloadConfig
            | Self::CloseAllWindows
            | Self::Quit
            | Self::ToggleQuickTerminal
            | Self::ToggleVisibility
            | Self::CheckForUpdates
            | Self::ShowGtkInspector
            | Self::NewWindow
            | Self::Undo
            | Self::Redo => Scope::App,
            _ => Scope::Surface,
        }
    }

    /// Whether performing it closes the terminal (`closingAction`): key handling stops there.
    #[must_use]
    pub fn closes(&self) -> bool {
        matches!(self, Self::CloseSurface | Self::CloseWindow | Self::CloseTab(_))
    }

    /// The bytes `csi:`, `esc:` and `text:` write to the terminal, as `performBindingAction` builds them: `ESC [`
    /// or `ESC` before the parameter as written (at most 128 bytes in all), or the `text:` parameter decoded as a Zig
    /// string literal (`\xNN` a byte, `\n`, `\u{...}`). `None` for any other action, an over-long sequence or text
    /// that does not decode (ghostty writes nothing then).
    #[must_use]
    pub fn pty_bytes(&self) -> Option<Vec<u8>> {
        let sequence = |prefix: &str, data: &str| {
            let bytes = format!("{prefix}{data}").into_bytes();
            (bytes.len() <= SEQUENCE_BUFFER).then_some(bytes)
        };
        match self {
            Self::Csi(data) => sequence("\x1b[", data),
            Self::Esc(data) => sequence("\x1b", data),
            Self::Text(text) => validate::decode_string(text),
            _ => None,
        }
    }

    /// The action of `kind` with `param` (the text after the first `:`).
    fn build(kind: ActionKind, param: Option<&str>) -> Result<Self, BindError> {
        if let Some(action) = UNIT_ACTIONS.iter().find(|action| ActionKind::from(*action) == kind) {
            return match param {
                Some(_) => Err(BindError::InvalidFormat),
                None => Ok(action.clone()),
            };
        }
        if text_action(kind, "").is_some() {
            let param = param.ok_or(BindError::InvalidFormat)?;
            return text_action(kind, param).ok_or(BindError::InvalidFormat);
        }
        Self::build_typed(kind, param)
    }

    /// An action whose parameter has a type of its own; it may be left out only where the type has a default.
    fn build_typed(kind: ActionKind, param: Option<&str>) -> Result<Self, BindError> {
        use ActionKind as K;
        Ok(match kind {
            K::CopyToClipboard => Self::CopyToClipboard(named_or(param, CopyFormat::Mixed)?),
            K::IncreaseFontSize => Self::IncreaseFontSize(float(param)?),
            K::DecreaseFontSize => Self::DecreaseFontSize(float(param)?),
            K::SetFontSize => Self::SetFontSize(float(param)?),
            K::NavigateSearch => Self::NavigateSearch(named(param)?),
            K::ScrollToRow => Self::ScrollToRow(int(param, false, 64)?),
            K::ScrollPageFractional => Self::ScrollPageFractional(float(param)?),
            K::ScrollPageLines => Self::ScrollPageLines(int(param, true, 16)?),
            K::AdjustSelection => Self::AdjustSelection(named(param)?),
            K::JumpToPrompt => Self::JumpToPrompt(int(param, true, 16)?),
            K::WriteScrollbackFile => Self::WriteScrollbackFile(write_screen(param)?),
            K::WriteScreenFile => Self::WriteScreenFile(write_screen(param)?),
            K::WriteSelectionFile => Self::WriteSelectionFile(write_screen(param)?),
            K::GotoTab => Self::GotoTab(int(param, false, 64)?),
            K::MoveTab => Self::MoveTab(int(param, true, 64)?),
            K::NewSplit => Self::NewSplit(named_or(param, SplitDirection::Auto)?),
            K::GotoSplit => Self::GotoSplit(named(param)?),
            K::GotoWindow => Self::GotoWindow(named(param)?),
            K::ResizeSplit => resize_split(param)?,
            K::Inspector => Self::Inspector(named(param)?),
            K::OpenConfig => Self::OpenConfig(named_or(param, OpenConfig::OsOpen)?),
            K::CloseTab => Self::CloseTab(named_or(param, CloseTabMode::This)?),
            K::Crash => Self::Crash(named(param)?),
            _ => return Err(BindError::InvalidAction),
        })
    }

    /// The parameter as a config line writes it.
    fn param(&self) -> Option<String> {
        let name = |value: &'static str| Some(value.to_owned());
        match self {
            Self::Csi(text)
            | Self::Esc(text)
            | Self::Text(text)
            | Self::Search(text)
            | Self::SetSurfaceTitle(text)
            | Self::SetTabTitle(text)
            | Self::SetWindowTitle(text)
            | Self::ActivateKeyTable(text)
            | Self::ActivateKeyTableOnce(text) => Some(text.clone()),
            Self::CopyToClipboard(format) => name(format.into()),
            Self::IncreaseFontSize(size)
            | Self::DecreaseFontSize(size)
            | Self::SetFontSize(size)
            | Self::ScrollPageFractional(size) => Some(size.to_string()),
            Self::NavigateSearch(direction) | Self::GotoWindow(direction) => name(direction.into()),
            Self::ScrollToRow(number) | Self::GotoTab(number) => Some(number.to_string()),
            Self::ScrollPageLines(number) | Self::JumpToPrompt(number) => Some(number.to_string()),
            Self::MoveTab(number) => Some(number.to_string()),
            Self::AdjustSelection(adjust) => name(adjust.into()),
            Self::WriteScrollbackFile(write) | Self::WriteScreenFile(write) | Self::WriteSelectionFile(write) => {
                Some(format!("{},{}", <&str>::from(write.action), <&str>::from(write.format)))
            }
            Self::NewSplit(direction) => name(direction.into()),
            Self::GotoSplit(focus) => name(focus.into()),
            Self::ResizeSplit(direction, amount) => Some(format!("{},{amount}", <&str>::from(direction))),
            Self::Inspector(mode) => name(mode.into()),
            Self::OpenConfig(open) => name(open.into()),
            Self::CloseTab(mode) => name(mode.into()),
            Self::Crash(thread) => name(thread.into()),
            _ => None,
        }
    }
}

/// `Action.parse`: the name up to the first `:`, the parameter after it.
impl FromStr for Action {
    type Err = BindError;

    fn from_str(input: &str) -> Result<Self, BindError> {
        let (name, param) = match input.split_once(':') {
            Some((name, param)) => (name, Some(param)),
            None => (input, None),
        };
        if name.is_empty() {
            return Err(BindError::InvalidFormat);
        }
        let kind = ActionKind::from_str(name).map_err(|_| BindError::InvalidAction)?;
        Self::build(kind, param)
    }
}

/// `name` or `name:parameter`, as a config line writes it.
impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())?;
        match self.param() {
            Some(param) => write!(f, ":{param}"),
            None => Ok(()),
        }
    }
}

/// The action of a string-parameter `kind` carrying `text`; `None` for any other kind.
fn text_action(kind: ActionKind, text: &str) -> Option<Action> {
    use ActionKind as K;
    let text = text.to_owned();
    Some(match kind {
        K::Csi => Action::Csi(text),
        K::Esc => Action::Esc(text),
        K::Text => Action::Text(text),
        K::Search => Action::Search(text),
        K::SetSurfaceTitle => Action::SetSurfaceTitle(text),
        K::SetTabTitle => Action::SetTabTitle(text),
        K::SetWindowTitle => Action::SetWindowTitle(text),
        K::ActivateKeyTable => Action::ActivateKeyTable(text),
        K::ActivateKeyTableOnce => Action::ActivateKeyTableOnce(text),
        _ => return None,
    })
}

/// A required enum parameter, by its exact tag name.
fn named<T: FromStr>(param: Option<&str>) -> Result<T, BindError> {
    T::from_str(param.ok_or(BindError::InvalidFormat)?).map_err(|_| BindError::InvalidFormat)
}

/// An enum parameter with a default for a bare action.
fn named_or<T: FromStr>(param: Option<&str>, default: T) -> Result<T, BindError> {
    param.map_or(Ok(default), |param| named(Some(param)))
}

/// `std.fmt.parseFloat` into an `f32`.
fn float(param: Option<&str>) -> Result<f32, BindError> {
    let value = validate::parse_float(param.ok_or(BindError::InvalidFormat)?).map_err(|_| BindError::InvalidFormat)?;
    // f32 by Zig's own narrowing
    #[expect(clippy::cast_possible_truncation, reason = "ghostty's float parameters are f32")]
    let value = value as f32;
    Ok(value)
}

/// `std.fmt.parseInt(T, param, 10)` into an integer of `bits` width.
fn int<T: TryFrom<i128>>(param: Option<&str>, signed: bool, bits: u8) -> Result<T, BindError> {
    let value = validate::parse_int(param.ok_or(BindError::InvalidFormat)?, 10, signed, bits)
        .map_err(|_| BindError::InvalidFormat)?;
    T::try_from(value).map_err(|_| BindError::InvalidFormat)
}

/// `WriteScreen.parse`: `ACTION` or `ACTION,FORMAT`.
fn write_screen(param: Option<&str>) -> Result<WriteScreen, BindError> {
    let param = param.ok_or(BindError::InvalidFormat)?;
    let (action, format) = match param.split_once(',') {
        Some((action, format)) => (action, named(Some(format))?),
        None => (param, WriteFormat::Plain),
    };
    Ok(WriteScreen {
        action: named(Some(action))?,
        format,
    })
}

/// `resize_split:DIRECTION,AMOUNT`: a tuple, exactly two fields.
fn resize_split(param: Option<&str>) -> Result<Action, BindError> {
    let mut fields = param.ok_or(BindError::InvalidFormat)?.split(',');
    let direction = named(fields.next())?;
    let amount = int(fields.next(), false, 16)?;
    if fields.next().is_some() {
        return Err(BindError::InvalidFormat);
    }
    Ok(Action::ResizeSplit(direction, amount))
}
