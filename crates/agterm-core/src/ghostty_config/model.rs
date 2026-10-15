//! The typed config for the subset of ghostty's keys the terminal honours, with ghostty's defaults at `GHOSTTY_REV`
//! (Linux side). Lines apply in order, the last one winning; a repeatable key collects its lines; an empty value
//! resets a key to its default (a list to empty), as `parseIntoField` does. Every other known key is ignored, logged
//! at debug.

use std::collections::BTreeMap;

use bitflags::bitflags;
use strum::{EnumIter, EnumString, IntoStaticStr};

use super::binds::Keybinds;
use super::parse::{Diagnostic, Setting};
use super::validate::{
    self, Accepted, Compat, ConfigPath, Padding, TerminalColor, ThemeName, parse_bool, parse_color, parse_float,
    parse_limit, parse_padding, parse_palette_entry, parse_terminal_color, parse_theme,
};
use crate::chrome::Rgb;

/// `cursor-style`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
pub enum CursorStyle {
    #[strum(serialize = "bar")]
    Bar,
    #[default]
    #[strum(serialize = "block")]
    Block,
    #[strum(serialize = "underline")]
    Underline,
    #[strum(serialize = "block_hollow")]
    BlockHollow,
}

/// `background-image-position`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum BackgroundImagePosition {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    CenterCenter,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
    /// Ghostty's default, drawn as `center-center`.
    #[default]
    Center,
}

/// `background-image-fit`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum BackgroundImageFit {
    #[default]
    Contain,
    Cover,
    Stretch,
    None,
}

/// `window-padding-balance`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum PaddingBalance {
    #[default]
    False,
    True,
    Equal,
}

/// `window-padding-color`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum PaddingColor {
    #[default]
    Background,
    Extend,
    ExtendAlways,
}

/// `copy-on-select`: where a selection made with the mouse or `select_all` is copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum CopyOnSelect {
    /// Not copied.
    False,
    /// To the selection clipboard (the primary selection on Linux).
    True,
    /// To the selection clipboard and the system clipboard.
    Clipboard,
}

impl Default for CopyOnSelect {
    /// Ghostty's default on Linux and macOS.
    fn default() -> Self {
        Self::True
    }
}

/// `clipboard-read` and `clipboard-write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum ClipboardAccess {
    Allow,
    Deny,
    Ask,
}

/// `right-click-action`: what a right click does when no program has the mouse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum RightClickAction {
    Ignore,
    /// Paste the clipboard (the selection dropped first).
    Paste,
    /// Copy the selection to the clipboard and drop it.
    Copy,
    /// Copy when there is a selection, else paste.
    CopyOrPaste,
    /// Select the word or link under the pointer for a context menu (ghostty's default).
    #[default]
    ContextMenu,
}

/// `middle-click-action`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum MiddleClickAction {
    /// Paste the primary selection (ghostty's default).
    #[default]
    PrimaryPaste,
    Ignore,
}

/// `mouse-shift-capture`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum MouseShiftCapture {
    #[default]
    False,
    True,
    Always,
    Never,
}

bitflags! {
    /// `shell-integration-features`, in ghostty's field order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ShellIntegrationFeatures: u8 {
        const CURSOR = 1;
        const SUDO = 1 << 1;
        const TITLE = 1 << 2;
        const SSH_ENV = 1 << 3;
        const SSH_TERMINFO = 1 << 4;
        const PATH = 1 << 5;
    }
}

impl ShellIntegrationFeatures {
    /// The names ghostty spells the flags with, in bit order.
    pub const NAMES: [&str; 6] = ["cursor", "sudo", "title", "ssh-env", "ssh-terminfo", "path"];
    /// Ghostty's default: `cursor`, `title` and `path` on.
    pub const DEFAULT: Self = Self::CURSOR.union(Self::TITLE).union(Self::PATH);
    /// The flags whose ssh wrappers call a `ghostty` CLI agterm does not ship (upstream #463): always off.
    pub const UNSUPPORTED: Self = Self::SSH_ENV.union(Self::SSH_TERMINFO);

    /// `GHOSTTY_SHELL_FEATURES` as libghostty sets it for a terminal's program (`shell_integration.setupFeatures`):
    /// the flags that are on, sorted by name, the cursor's spelled `cursor:blink` or `cursor:steady`; `None` with every
    /// flag off.
    #[must_use]
    pub fn env_value(self, cursor_blink: bool) -> Option<String> {
        // `all()` yields the flags in bit order, the order of `NAMES`
        let mut names: Vec<(&str, Self)> = Self::NAMES.into_iter().zip(Self::all().iter()).collect();
        names.sort_unstable_by_key(|(name, _)| *name);
        let on: Vec<String> = names
            .into_iter()
            .filter(|(_, flag)| self.contains(*flag))
            .map(|(name, flag)| match flag {
                Self::CURSOR if cursor_blink => "cursor:blink".to_owned(),
                Self::CURSOR => "cursor:steady".to_owned(),
                _ => name.to_owned(),
            })
            .collect();
        (!on.is_empty()).then(|| on.join(","))
    }
}

impl Default for ShellIntegrationFeatures {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `font-family*`, `font-size`, `font-feature`.
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    pub family: Vec<String>,
    pub family_bold: Vec<String>,
    pub family_italic: Vec<String>,
    pub family_bold_italic: Vec<String>,
    /// Points.
    pub size: f32,
    pub feature: Vec<String>,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: Vec::new(),
            family_bold: Vec::new(),
            family_italic: Vec::new(),
            family_bold_italic: Vec::new(),
            size: if cfg!(target_os = "macos") { 13.0 } else { 12.0 },
            feature: Vec::new(),
        }
    }
}

/// `background`, `foreground`, `palette`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colors {
    pub background: Rgb,
    pub foreground: Rgb,
    /// The palette slots the config sets; the rest keep the terminal's default palette.
    pub palette: BTreeMap<u8, Rgb>,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: Rgb::new(0x28, 0x2c, 0x34),
            foreground: Rgb::WHITE,
            palette: BTreeMap::new(),
        }
    }
}

/// `cursor-*`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    pub style: CursorStyle,
    /// Unset: the terminal's own blink mode decides.
    pub style_blink: Option<bool>,
    pub color: Option<TerminalColor>,
    pub text: Option<TerminalColor>,
    pub opacity: f64,
    pub click_to_move: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            style: CursorStyle::Block,
            style_blink: None,
            color: None,
            text: None,
            opacity: 1.0,
            click_to_move: true,
        }
    }
}

/// `selection-*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub foreground: Option<TerminalColor>,
    pub background: Option<TerminalColor>,
    pub clear_on_typing: bool,
    pub clear_on_copy: bool,
    /// `selection-word-chars`: the word boundaries, decoded (Zig string escapes); `None` is ghostty's default
    /// (`` \t '"│`|:;,()[]{}<>$``). U+0000 is always a boundary too and is not listed.
    pub word_chars: Option<Vec<char>>,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            foreground: None,
            background: None,
            clear_on_typing: true,
            clear_on_copy: false,
            word_chars: None,
        }
    }
}

/// `background-image*`: the watermark the terminal draws.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundImage {
    /// Made absolute against the file that set it.
    pub path: Option<ConfigPath>,
    pub opacity: f32,
    pub position: BackgroundImagePosition,
    pub fit: BackgroundImageFit,
    pub repeat: bool,
}

impl Default for BackgroundImage {
    fn default() -> Self {
        Self {
            path: None,
            opacity: 1.0,
            position: BackgroundImagePosition::Center,
            fit: BackgroundImageFit::Contain,
            repeat: false,
        }
    }
}

/// `window-padding-*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPadding {
    pub x: Padding,
    pub y: Padding,
    pub balance: PaddingBalance,
    pub color: PaddingColor,
}

impl Default for WindowPadding {
    fn default() -> Self {
        let both = Padding { start: 2, end: 2 };
        Self {
            x: both,
            y: both,
            balance: PaddingBalance::False,
            color: PaddingColor::Background,
        }
    }
}

/// `scrollback-limit-bytes` (formerly `scrollback-limit`) and `scrollback-limit-lines`; `None` is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scrollback {
    pub limit_bytes: Option<u64>,
    pub limit_lines: Option<u64>,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self {
            limit_bytes: Some(50_000_000),
            limit_lines: None,
        }
    }
}

/// `clipboard-*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    pub read: ClipboardAccess,
    pub write: ClipboardAccess,
    /// `None` is unlimited.
    pub write_limit_bytes: Option<u64>,
    pub trim_trailing_spaces: bool,
    pub paste_protection: bool,
    pub paste_bracketed_safe: bool,
    /// `clipboard-codepoint-map` lines as written.
    pub codepoint_map: Vec<String>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            read: ClipboardAccess::Ask,
            write: ClipboardAccess::Allow,
            write_limit_bytes: Some(64 * 1024 * 1024),
            trim_trailing_spaces: true,
            paste_protection: true,
            paste_bracketed_safe: true,
            codepoint_map: Vec::new(),
        }
    }
}

/// `mouse-*` and the other keys of the pointer: `right-click-action`, `middle-click-action`,
/// `click-repeat-interval`, `focus-follows-mouse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mouse {
    pub hide_while_typing: bool,
    pub shift_capture: MouseShiftCapture,
    pub reporting: bool,
    /// `mouse-scroll-multiplier` as written; unset is ghostty's `precision:1,discrete:3`.
    pub scroll_multiplier: Option<String>,
    pub right_click_action: RightClickAction,
    pub middle_click_action: MiddleClickAction,
    /// `click-repeat-interval` in milliseconds; 0 is the platform's default (500 ms off macOS).
    pub click_repeat_interval: u32,
    /// `focus-follows-mouse`: the pointer moving over a split pane focuses it.
    pub focus_follows_mouse: bool,
}

impl Default for Mouse {
    fn default() -> Self {
        Self {
            hide_while_typing: false,
            shift_capture: MouseShiftCapture::False,
            reporting: true,
            scroll_multiplier: None,
            right_click_action: RightClickAction::ContextMenu,
            middle_click_action: MiddleClickAction::PrimaryPaste,
            click_repeat_interval: 0,
            focus_follows_mouse: false,
        }
    }
}

/// `link-*`: what counts as a link besides OSC 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// `link-url`: URLs in the text are links.
    pub url: bool,
}

impl Default for Link {
    fn default() -> Self {
        Self { url: true }
    }
}

/// The terminal's config, as far as agterm draws and drives the terminal itself.
#[derive(Debug, Clone, PartialEq)]
pub struct GhosttyConfig {
    pub font: Font,
    /// `theme`, both sides.
    pub theme: Option<ThemeName>,
    pub colors: Colors,
    pub cursor: Cursor,
    pub selection: Selection,
    pub background_opacity: f64,
    pub background_image: BackgroundImage,
    pub window_padding: WindowPadding,
    pub scrollback: Scrollback,
    pub clipboard: Clipboard,
    pub copy_on_select: CopyOnSelect,
    pub mouse: Mouse,
    pub link: Link,
    pub shell_integration_features: ShellIntegrationFeatures,
    /// The child's `TERM`.
    pub term: String,
    /// `keybind`: ghostty's built-in binds with the lines over them.
    pub keybind: Keybinds,
    /// A static window title in place of the programs' own.
    pub title: Option<String>,
}

/// `term`'s default, which an empty value also restores.
pub const DEFAULT_TERM: &str = "xterm-ghostty";

impl Default for GhosttyConfig {
    fn default() -> Self {
        Self {
            font: Font::default(),
            theme: None,
            colors: Colors::default(),
            cursor: Cursor::default(),
            selection: Selection::default(),
            background_opacity: 1.0,
            background_image: BackgroundImage::default(),
            window_padding: WindowPadding::default(),
            scrollback: Scrollback::default(),
            clipboard: Clipboard::default(),
            copy_on_select: CopyOnSelect::default(),
            mouse: Mouse::default(),
            link: Link::default(),
            shell_integration_features: ShellIntegrationFeatures::DEFAULT,
            term: DEFAULT_TERM.to_owned(),
            keybind: Keybinds::defaults(),
            title: None,
        }
    }
}

/// The value of an accepted line: `None` resets the key (an empty value).
fn given(value: Option<&str>) -> Option<&str> {
    match value {
        Some("") => None,
        Some(value) => Some(value),
        None => Some(""),
    }
}

impl GhosttyConfig {
    /// Applies an accepted line; a refused one changes nothing, as in ghostty. A line its key refuses only in the
    /// context of the lines before it (a `keybind = chain=…` with no binding to extend) is the diagnostic returned.
    pub fn apply(&mut self, setting: &Setting) -> Option<Diagnostic> {
        if setting.key == "keybind" {
            return self.apply_keybind(setting);
        }
        match &setting.accepted {
            Ok(Accepted::Field(spec)) => {
                if !self.apply_key(spec.name, setting) {
                    tracing::debug!(key = spec.name, "ghostty config key not supported; ignored");
                }
            }
            Ok(Accepted::Compat(handler)) => self.apply_compat(*handler, setting),
            Err(_) => {}
        }
        None
    }

    /// A `keybind` line over the binds so far. A refused one is still applied, without a second diagnostic: ghostty
    /// defines the key table a refused `name/binding` names, and refuses any other line before it changes anything.
    fn apply_keybind(&mut self, setting: &Setting) -> Option<Diagnostic> {
        let value = setting.value.as_deref()?;
        let applied = self.keybind.parse_cli(value);
        if setting.accepted.is_err() {
            return None;
        }
        let error = applied.err()?;
        Some(Diagnostic {
            location: setting.location.clone(),
            key: setting.key.clone(),
            message: format!("unknown error {}", error.zig_name()),
        })
    }

    /// What `Config.finalize` derives: the bold and italic families default to `font-family`, an empty `term` is
    /// ghostty's own.
    pub fn finalize(&mut self) {
        let family = self.font.family.clone();
        for styled in [
            &mut self.font.family_bold,
            &mut self.font.family_italic,
            &mut self.font.family_bold_italic,
        ] {
            if styled.is_empty() {
                styled.clone_from(&family);
            }
        }
        if self.term.is_empty() {
            DEFAULT_TERM.clone_into(&mut self.term);
        }
    }

    fn apply_compat(&mut self, handler: Compat, setting: &Setting) {
        match (handler, setting.key.as_str()) {
            (Compat::Renamed(to), _) => {
                self.apply_key(to, setting);
            }
            // a false value leaves the colors alone, as ghostty's handlers do
            (Compat::Bool, "cursor-invert-fg-bg") => {
                if bool_of(setting) {
                    self.cursor.color = Some(TerminalColor::CellForeground);
                    self.cursor.text = Some(TerminalColor::CellBackground);
                }
            }
            (Compat::Bool, "selection-invert-fg-bg") => {
                if bool_of(setting) {
                    self.selection.foreground = Some(TerminalColor::CellBackground);
                    self.selection.background = Some(TerminalColor::CellForeground);
                }
            }
            _ => tracing::debug!(key = setting.key, "ghostty config key not supported; ignored"),
        }
    }

    /// Applies `setting` as `key`; false when the model does not carry the key.
    fn apply_key(&mut self, key: &str, setting: &Setting) -> bool {
        let value = given(setting.value.as_deref());
        if let Some(rest) = key.strip_prefix("font-") {
            return self.font.apply(rest, value);
        }
        if let Some(rest) = key.strip_prefix("cursor-") {
            return self.cursor.apply(rest, value);
        }
        if let Some(rest) = key.strip_prefix("selection-") {
            return self.selection.apply(rest, value);
        }
        if let Some(rest) = key.strip_prefix("background-image") {
            return self.background_image.apply(rest, value, setting.path.as_ref());
        }
        if let Some(rest) = key.strip_prefix("window-padding-") {
            return self.window_padding.apply(rest, value);
        }
        if let Some(rest) = key.strip_prefix("clipboard-") {
            return self.clipboard.apply(rest, value);
        }
        if let Some(rest) = key.strip_prefix("mouse-") {
            return self.mouse.apply(rest, value);
        }
        if self.mouse.apply_pointer_key(key, value) {
            return true;
        }
        self.apply_other(key, value)
    }

    fn apply_other(&mut self, key: &str, value: Option<&str>) -> bool {
        match key {
            "theme" => self.theme = value.and_then(|value| parse_theme(value).ok()),
            "background" => self.colors.background = color_or(value, Colors::default().background),
            "foreground" => self.colors.foreground = color_or(value, Colors::default().foreground),
            "palette" => match value.map(parse_palette_entry) {
                Some(Ok((index, color))) => {
                    self.colors.palette.insert(index, color);
                }
                Some(Err(_)) => {}
                None => self.colors.palette.clear(),
            },
            "background-opacity" => self.background_opacity = float_or(value, 1.0),
            "scrollback-limit-bytes" => {
                self.scrollback.limit_bytes = limit_or(value, Scrollback::default().limit_bytes);
            }
            "scrollback-limit-lines" => self.scrollback.limit_lines = limit_or(value, None),
            "shell-integration-features" => self.shell_integration_features = shell_features(value),
            "term" => value.unwrap_or(DEFAULT_TERM).clone_into(&mut self.term),
            "copy-on-select" => self.copy_on_select = enum_or(value, CopyOnSelect::default()),
            "title" => self.title = value.map(str::to_owned),
            "link-url" => self.link.url = bool_or(value, true),
            _ => return false,
        }
        true
    }
}

impl Font {
    fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        let list = match key {
            "family" => &mut self.family,
            "family-bold" => &mut self.family_bold,
            "family-italic" => &mut self.family_italic,
            "family-bold-italic" => &mut self.family_bold_italic,
            "feature" => &mut self.feature,
            "size" => {
                // f32 by Zig's own narrowing
                #[expect(clippy::cast_possible_truncation, reason = "ghostty's font-size is an f32")]
                let size = float_or(value, f64::from(Self::default().size)) as f32;
                self.size = size;
                return true;
            }
            _ => return false,
        };
        push_or_clear(list, value);
        true
    }
}

impl Cursor {
    fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        match key {
            "style" => self.style = enum_or(value, CursorStyle::Block),
            "style-blink" => self.style_blink = value.and_then(|value| bool_value(value).ok()),
            "color" => self.color = value.and_then(|value| parse_terminal_color(value).ok()),
            "text" => self.text = value.and_then(|value| parse_terminal_color(value).ok()),
            "opacity" => self.opacity = float_or(value, 1.0),
            "click-to-move" => self.click_to_move = bool_or(value, true),
            _ => return false,
        }
        true
    }
}

impl Selection {
    fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        match key {
            "foreground" => self.foreground = value.and_then(|value| parse_terminal_color(value).ok()),
            "background" => self.background = value.and_then(|value| parse_terminal_color(value).ok()),
            "clear-on-typing" => self.clear_on_typing = bool_or(value, true),
            "clear-on-copy" => self.clear_on_copy = bool_or(value, false),
            "word-chars" => match value {
                // a value that does not decode is refused, as `parseCLI` refuses it
                Some(value) => {
                    if let Some(chars) = validate::decode_codepoints(value) {
                        self.word_chars = Some(chars);
                    }
                }
                None => self.word_chars = None,
            },
            _ => return false,
        }
        true
    }
}

impl BackgroundImage {
    /// `key` is what follows `background-image`: empty for the path itself.
    fn apply(&mut self, key: &str, value: Option<&str>, path: Option<&ConfigPath>) -> bool {
        match key {
            "" => self.path = value.and(path.cloned()),
            "-opacity" => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "ghostty's background-image-opacity is an f32"
                )]
                let opacity = float_or(value, 1.0) as f32;
                self.opacity = opacity;
            }
            "-position" => self.position = enum_or(value, BackgroundImagePosition::Center),
            "-fit" => self.fit = enum_or(value, BackgroundImageFit::Contain),
            "-repeat" => self.repeat = bool_or(value, false),
            _ => return false,
        }
        true
    }
}

impl WindowPadding {
    fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        let reset = Self::default();
        match key {
            "x" => self.x = value.and_then(|value| parse_padding(value).ok()).unwrap_or(reset.x),
            "y" => self.y = value.and_then(|value| parse_padding(value).ok()).unwrap_or(reset.y),
            "balance" => self.balance = enum_or(value, PaddingBalance::False),
            "color" => self.color = enum_or(value, PaddingColor::Background),
            _ => return false,
        }
        true
    }
}

impl Clipboard {
    fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        match key {
            "read" => self.read = enum_or(value, ClipboardAccess::Ask),
            "write" => self.write = enum_or(value, ClipboardAccess::Allow),
            "write-limit-bytes" => self.write_limit_bytes = limit_or(value, Self::default().write_limit_bytes),
            "trim-trailing-spaces" => self.trim_trailing_spaces = bool_or(value, true),
            "paste-protection" => self.paste_protection = bool_or(value, true),
            "paste-bracketed-safe" => self.paste_bracketed_safe = bool_or(value, true),
            "codepoint-map" => push_or_clear(&mut self.codepoint_map, value),
            _ => return false,
        }
        true
    }
}

impl Mouse {
    fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        match key {
            "hide-while-typing" => self.hide_while_typing = bool_or(value, false),
            "shift-capture" => self.shift_capture = enum_or(value, MouseShiftCapture::False),
            "reporting" => self.reporting = bool_or(value, true),
            "scroll-multiplier" => self.scroll_multiplier = value.map(str::to_owned),
            _ => return false,
        }
        true
    }

    /// The pointer's keys outside `mouse-*`.
    fn apply_pointer_key(&mut self, key: &str, value: Option<&str>) -> bool {
        match key {
            "right-click-action" => self.right_click_action = enum_or(value, RightClickAction::ContextMenu),
            "middle-click-action" => self.middle_click_action = enum_or(value, MiddleClickAction::PrimaryPaste),
            "click-repeat-interval" => {
                self.click_repeat_interval = value.and_then(|value| value.parse().ok()).unwrap_or(0);
            }
            "focus-follows-mouse" => self.focus_follows_mouse = bool_or(value, false),
            _ => return false,
        }
        true
    }
}

/// A bool key's value: a bare key (`Some("")` from [`given`]) is true.
fn bool_value(value: &str) -> Result<bool, validate::ValueError> {
    parse_bool(if value.is_empty() { "t" } else { value })
}

fn bool_or(value: Option<&str>, default: bool) -> bool {
    value.map_or(default, |value| bool_value(value).unwrap_or(default))
}

/// The boolean of a compatibility line (a bare key is true).
fn bool_of(setting: &Setting) -> bool {
    parse_bool(setting.value.as_deref().unwrap_or("t")).unwrap_or(false)
}

fn float_or(value: Option<&str>, default: f64) -> f64 {
    value.and_then(|value| parse_float(value).ok()).unwrap_or(default)
}

fn color_or(value: Option<&str>, default: Rgb) -> Rgb {
    value.and_then(|value| parse_color(value).ok()).unwrap_or(default)
}

fn limit_or(value: Option<&str>, default: Option<u64>) -> Option<u64> {
    value.map_or(default, |value| parse_limit(value).unwrap_or(default))
}

fn enum_or<T: std::str::FromStr>(value: Option<&str>, default: T) -> T {
    value.and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// A repeatable string: one more entry, or cleared by an empty value.
fn push_or_clear(list: &mut Vec<String>, value: Option<&str>) {
    match value {
        Some(value) => list.push(value.to_owned()),
        None => list.clear(),
    }
}

/// `shell-integration-features`: a boolean for all, else the named flags over ghostty's defaults.
fn shell_features(value: Option<&str>) -> ShellIntegrationFeatures {
    let Some(flags) = value.and_then(|value| validate::parse_flags(value, &ShellIntegrationFeatures::NAMES).ok())
    else {
        return ShellIntegrationFeatures::DEFAULT;
    };
    let mut features = ShellIntegrationFeatures::DEFAULT;
    for (bit, flag) in flags.into_iter().enumerate() {
        if let Some(on) = flag {
            features.set(ShellIntegrationFeatures::from_bits_retain(1 << bit), on);
        }
    }
    features
}
