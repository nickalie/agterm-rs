//! Actions libghostty asks the host to perform (`ghostty_action_s`), decoded into owned values.
//!
//! An action's strings and config are valid only for the callback that delivers it, so decoding copies them out.
//! The tags agterm does not act on decode to [`Action::Other`], which the host answers "not handled".

use std::ffi::{CStr, c_char};
use std::num::NonZeroUsize;

use agterm_ghostty_sys as sys;

use crate::config::{Color, Config};

/// The host's name for one surface, set as its libghostty userdata and handed back with every callback about it.
/// A plain number rather than a pointer: a callback naming a surface the host already dropped resolves to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceKey(NonZeroUsize);

impl SurfaceKey {
    #[must_use]
    pub const fn new(value: NonZeroUsize) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }

    pub(crate) fn as_userdata(self) -> *mut std::ffi::c_void {
        std::ptr::without_provenance_mut(self.0.get())
    }

    pub(crate) fn from_userdata(userdata: *mut std::ffi::c_void) -> Option<Self> {
        NonZeroUsize::new(userdata.addr()).map(Self)
    }
}

/// What an action is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    App,
    Surface(SurfaceKey),
}

/// The pointer shape libghostty wants over a surface. The variants follow ghostty's `terminal.MouseShape`, which is
/// the CSS cursor vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseShape {
    Default,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
}

impl MouseShape {
    /// Every shape in `ghostty_action_mouse_shape_e` order: the index is the C value.
    const ALL: [Self; 34] = [
        Self::Default,
        Self::ContextMenu,
        Self::Help,
        Self::Pointer,
        Self::Progress,
        Self::Wait,
        Self::Cell,
        Self::Crosshair,
        Self::Text,
        Self::VerticalText,
        Self::Alias,
        Self::Copy,
        Self::Move,
        Self::NoDrop,
        Self::NotAllowed,
        Self::Grab,
        Self::Grabbing,
        Self::AllScroll,
        Self::ColResize,
        Self::RowResize,
        Self::NResize,
        Self::EResize,
        Self::SResize,
        Self::WResize,
        Self::NeResize,
        Self::NwResize,
        Self::SeResize,
        Self::SwResize,
        Self::EwResize,
        Self::NsResize,
        Self::NeswResize,
        Self::NwseResize,
        Self::ZoomIn,
        Self::ZoomOut,
    ];

    /// The CSS cursor names, in [`Self::ALL`] order.
    const CSS_NAMES: [&'static str; 34] = [
        "default",
        "context-menu",
        "help",
        "pointer",
        "progress",
        "wait",
        "cell",
        "crosshair",
        "text",
        "vertical-text",
        "alias",
        "copy",
        "move",
        "no-drop",
        "not-allowed",
        "grab",
        "grabbing",
        "all-scroll",
        "col-resize",
        "row-resize",
        "n-resize",
        "e-resize",
        "s-resize",
        "w-resize",
        "ne-resize",
        "nw-resize",
        "se-resize",
        "sw-resize",
        "ew-resize",
        "ns-resize",
        "nesw-resize",
        "nwse-resize",
        "zoom-in",
        "zoom-out",
    ];

    fn from_raw(raw: sys::ghostty_action_mouse_shape_e) -> Option<Self> {
        usize::try_from(raw)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
    }

    /// The shape's CSS cursor name (`text`, `pointer`, `ew-resize`).
    #[must_use]
    pub fn css_name(self) -> &'static str {
        // the discriminants count up from 0 in `ALL` order
        Self::CSS_NAMES.get(self as usize).copied().unwrap_or("default")
    }
}

/// Which dynamic color an OSC 10/11/12 (or its reset) changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorKind {
    Foreground,
    Background,
    Cursor,
    /// A palette entry.
    Palette(u8),
}

/// Where a URL to open came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenUrlKind {
    Unknown,
    Text,
    Html,
    /// An OSC 8 hyperlink.
    Osc8,
}

/// One action, decoded. Only the tags agterm acts on are spelled out; the rest arrive as [`Action::Other`].
#[derive(Debug)]
pub enum Action {
    /// Draw the surface now (OpenGL builds draw on the host's thread).
    Render,
    /// An OSC 0/1/2 title.
    SetTitle(String),
    /// An OSC 7 working directory.
    Pwd(String),
    /// The cell size changed (a font size change or a new DPI).
    CellSize {
        width: u32,
        height: u32,
    },
    /// An OSC 9 / 777 desktop notification.
    DesktopNotification {
        title: String,
        body: String,
    },
    /// The child exited; answering "handled" suppresses libghostty's "Process exited" prompt.
    ShowChildExited {
        exit_code: u32,
        runtime_ms: u64,
    },
    StartSearch {
        needle: Option<String>,
    },
    EndSearch,
    /// The match count; `None` when there is no query.
    SearchTotal(Option<usize>),
    /// The selected match's 1-based index; `None` when nothing is selected.
    SearchSelected(Option<usize>),
    /// The config libghostty applied in answer to an update, copied out (`None` when it could not be copied).
    ConfigChange(Option<Config>),
    MouseShape(MouseShape),
    /// `true` hides the pointer until it moves (mouse-hide-while-typing).
    MouseVisibility {
        hidden: bool,
    },
    /// A link was clicked.
    OpenUrl {
        kind: OpenUrlKind,
        url: String,
    },
    /// A dynamic color was set or reset, already applied by libghostty.
    ColorChange {
        kind: ColorKind,
        color: Color,
    },
    RingBell,
    /// Any tag agterm does not act on.
    Other(u32),
}

impl Action {
    /// Decode `raw`, copying out every string and config it carries.
    ///
    /// # Safety
    ///
    /// `raw` must be an action libghostty is delivering right now: its union member matches its tag and every
    /// pointer it carries is null or valid (strings NUL-terminated, `open_url` for `len` bytes) for the duration of
    /// the call.
    pub(crate) unsafe fn decode(raw: &sys::ghostty_action_s) -> Self {
        let union = &raw.action;
        match raw.tag {
            sys::GHOSTTY_ACTION_RENDER => Self::Render,
            sys::GHOSTTY_ACTION_END_SEARCH => Self::EndSearch,
            sys::GHOSTTY_ACTION_RING_BELL => Self::RingBell,
            tag @ (sys::GHOSTTY_ACTION_SET_TITLE
            | sys::GHOSTTY_ACTION_PWD
            | sys::GHOSTTY_ACTION_DESKTOP_NOTIFICATION
            | sys::GHOSTTY_ACTION_START_SEARCH
            | sys::GHOSTTY_ACTION_OPEN_URL) => {
                // SAFETY: the caller's contract, passed on unchanged.
                unsafe { decode_strings(tag, union) }
            }
            tag => {
                // SAFETY: the caller's contract, passed on unchanged.
                unsafe { decode_values(tag, union) }
            }
        }
    }
}

/// The actions that carry strings.
///
/// # Safety
///
/// As [`Action::decode`], for `tag` and `union`.
unsafe fn decode_strings(tag: sys::ghostty_action_tag_e, union: &sys::ghostty_action_u) -> Action {
    match tag {
        sys::GHOSTTY_ACTION_SET_TITLE => {
            // SAFETY: the tag selects this member.
            let title = unsafe { union.set_title }.title;
            // SAFETY: null or a string valid for the callback.
            Action::SetTitle(unsafe { owned(title) }.unwrap_or_default())
        }
        sys::GHOSTTY_ACTION_PWD => {
            // SAFETY: the tag selects this member.
            let pwd = unsafe { union.pwd }.pwd;
            // SAFETY: null or a string valid for the callback.
            Action::Pwd(unsafe { owned(pwd) }.unwrap_or_default())
        }
        sys::GHOSTTY_ACTION_DESKTOP_NOTIFICATION => {
            // SAFETY: the tag selects this member.
            let note = unsafe { union.desktop_notification };
            // SAFETY: null or a string valid for the callback.
            let title = unsafe { owned(note.title) }.unwrap_or_default();
            // SAFETY: null or a string valid for the callback.
            let body = unsafe { owned(note.body) }.unwrap_or_default();
            Action::DesktopNotification { title, body }
        }
        sys::GHOSTTY_ACTION_START_SEARCH => {
            // SAFETY: the tag selects this member.
            let needle = unsafe { union.start_search }.needle;
            // SAFETY: null or a string valid for the callback.
            let needle = unsafe { owned(needle) };
            Action::StartSearch { needle }
        }
        sys::GHOSTTY_ACTION_OPEN_URL => {
            // SAFETY: the tag selects this member.
            let open = unsafe { union.open_url };
            // SAFETY: null or valid for `len` bytes for the callback.
            let url = unsafe { owned_slice(open.url, open.len) };
            Action::OpenUrl {
                kind: open_url_kind(open.kind),
                url,
            }
        }
        tag => Action::Other(tag),
    }
}

/// The actions that carry plain values, and the config change, which carries a config to copy.
///
/// # Safety
///
/// As [`Action::decode`], for `tag` and `union`.
unsafe fn decode_values(tag: sys::ghostty_action_tag_e, union: &sys::ghostty_action_u) -> Action {
    match tag {
        sys::GHOSTTY_ACTION_CELL_SIZE => {
            // SAFETY: the tag selects this member.
            let size = unsafe { union.cell_size };
            Action::CellSize {
                width: size.width,
                height: size.height,
            }
        }
        sys::GHOSTTY_ACTION_SHOW_CHILD_EXITED => {
            // SAFETY: the tag selects this member.
            let exited = unsafe { union.child_exited };
            Action::ShowChildExited {
                exit_code: exited.exit_code,
                runtime_ms: exited.timetime_ms,
            }
        }
        sys::GHOSTTY_ACTION_SEARCH_TOTAL => {
            // SAFETY: the tag selects this member.
            Action::SearchTotal(usize::try_from(unsafe { union.search_total }.total).ok())
        }
        sys::GHOSTTY_ACTION_SEARCH_SELECTED => {
            // SAFETY: the tag selects this member.
            Action::SearchSelected(usize::try_from(unsafe { union.search_selected }.selected).ok())
        }
        sys::GHOSTTY_ACTION_CONFIG_CHANGE => {
            // SAFETY: the tag selects this member.
            let config = unsafe { union.config_change }.config;
            // SAFETY: null or a config libghostty keeps alive for the callback.
            Action::ConfigChange(unsafe { Config::clone_raw(config) })
        }
        sys::GHOSTTY_ACTION_MOUSE_SHAPE => {
            // SAFETY: the tag selects this member.
            MouseShape::from_raw(unsafe { union.mouse_shape }).map_or(Action::Other(tag), Action::MouseShape)
        }
        sys::GHOSTTY_ACTION_MOUSE_VISIBILITY => {
            // SAFETY: the tag selects this member.
            let visibility = unsafe { union.mouse_visibility };
            Action::MouseVisibility {
                hidden: visibility == sys::GHOSTTY_MOUSE_HIDDEN,
            }
        }
        sys::GHOSTTY_ACTION_COLOR_CHANGE => {
            // SAFETY: the tag selects this member.
            decode_color_change(unsafe { union.color_change })
        }
        tag => Action::Other(tag),
    }
}

fn decode_color_change(change: sys::ghostty_action_color_change_s) -> Action {
    let kind = match change.kind {
        sys::GHOSTTY_ACTION_COLOR_KIND_FOREGROUND => ColorKind::Foreground,
        sys::GHOSTTY_ACTION_COLOR_KIND_BACKGROUND => ColorKind::Background,
        sys::GHOSTTY_ACTION_COLOR_KIND_CURSOR => ColorKind::Cursor,
        index => match u8::try_from(index) {
            Ok(index) => ColorKind::Palette(index),
            Err(_) => return Action::Other(sys::GHOSTTY_ACTION_COLOR_CHANGE),
        },
    };
    Action::ColorChange {
        kind,
        color: Color {
            r: change.r,
            g: change.g,
            b: change.b,
        },
    }
}

fn open_url_kind(raw: sys::ghostty_action_open_url_kind_e) -> OpenUrlKind {
    match raw {
        sys::GHOSTTY_ACTION_OPEN_URL_KIND_TEXT => OpenUrlKind::Text,
        sys::GHOSTTY_ACTION_OPEN_URL_KIND_HTML => OpenUrlKind::Html,
        sys::GHOSTTY_ACTION_OPEN_URL_KIND_OSC8 => OpenUrlKind::Osc8,
        _ => OpenUrlKind::Unknown,
    }
}

/// A copy of a NUL-terminated string, `None` for null.
///
/// # Safety
///
/// `ptr` is null or a NUL-terminated string valid for the call.
pub(crate) unsafe fn owned(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null and NUL-terminated per the caller's contract.
    Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

/// A copy of `len` bytes at `ptr`, which need not be NUL-terminated (`open_url.url` is a Zig slice: reading to a NUL
/// would run into the neighbouring hyperlink storage).
///
/// # Safety
///
/// `ptr` is null or valid for `len` bytes for the call.
pub(crate) unsafe fn owned_slice(ptr: *const c_char, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: non-null and valid for len bytes per the caller's contract.
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::*;

    fn action(tag: sys::ghostty_action_tag_e, build: impl FnOnce(&mut sys::ghostty_action_u)) -> sys::ghostty_action_s {
        // SAFETY: the union is plain old data; all-zero is a valid value of every member.
        let mut union: sys::ghostty_action_u = unsafe { std::mem::zeroed() };
        build(&mut union);
        sys::ghostty_action_s { tag, action: union }
    }

    fn decode(raw: &sys::ghostty_action_s) -> Action {
        // SAFETY: every test builds the member matching its tag and keeps its strings alive across the call.
        unsafe { Action::decode(raw) }
    }

    #[test]
    fn a_title_is_copied_out() {
        let title = CString::new("vim — main.rs").expect("no NUL");
        let raw = action(sys::GHOSTTY_ACTION_SET_TITLE, |u| u.set_title.title = title.as_ptr());
        let decoded = decode(&raw);
        drop(title);
        assert!(
            matches!(decoded, Action::SetTitle(ref t) if t == "vim — main.rs"),
            "{decoded:?}"
        );
    }

    #[test]
    fn a_null_string_decodes_empty() {
        let raw = action(sys::GHOSTTY_ACTION_PWD, |u| u.pwd.pwd = std::ptr::null());
        assert!(matches!(decode(&raw), Action::Pwd(ref p) if p.is_empty()));
    }

    #[test]
    fn an_open_url_honours_its_length_not_a_nul() {
        let buffer = b"https://example.com/aXY";
        let raw = action(sys::GHOSTTY_ACTION_OPEN_URL, |u| {
            u.open_url.kind = sys::GHOSTTY_ACTION_OPEN_URL_KIND_OSC8;
            u.open_url.url = buffer.as_ptr().cast();
            u.open_url.len = buffer.len() - 2;
        });
        let decoded = decode(&raw);
        assert!(
            matches!(decoded, Action::OpenUrl { kind: OpenUrlKind::Osc8, ref url } if url == "https://example.com/a"),
            "{decoded:?}"
        );
    }

    #[test]
    fn search_counts_map_negative_to_none() {
        let none = action(sys::GHOSTTY_ACTION_SEARCH_TOTAL, |u| u.search_total.total = -1);
        let some = action(sys::GHOSTTY_ACTION_SEARCH_SELECTED, |u| u.search_selected.selected = 3);
        assert!(matches!(decode(&none), Action::SearchTotal(None)));
        assert!(matches!(decode(&some), Action::SearchSelected(Some(3))));
    }

    #[test]
    fn notification_and_search_needle_strings_decode() {
        let title = CString::new("build").expect("no NUL");
        let raw = action(sys::GHOSTTY_ACTION_DESKTOP_NOTIFICATION, |u| {
            u.desktop_notification.title = title.as_ptr();
            u.desktop_notification.body = std::ptr::null();
        });
        assert!(matches!(
            decode(&raw),
            Action::DesktopNotification { ref title, ref body } if title == "build" && body.is_empty()
        ));
        let start = action(sys::GHOSTTY_ACTION_START_SEARCH, |u| {
            u.start_search.needle = std::ptr::null();
        });
        assert!(matches!(decode(&start), Action::StartSearch { needle: None }));
    }

    #[test]
    fn mouse_actions_decode() {
        let shape = action(sys::GHOSTTY_ACTION_MOUSE_SHAPE, |u| {
            u.mouse_shape = sys::GHOSTTY_MOUSE_SHAPE_TEXT;
        });
        assert!(matches!(decode(&shape), Action::MouseShape(MouseShape::Text)));
        let unknown = action(sys::GHOSTTY_ACTION_MOUSE_SHAPE, |u| u.mouse_shape = 999);
        assert!(matches!(
            decode(&unknown),
            Action::Other(sys::GHOSTTY_ACTION_MOUSE_SHAPE)
        ));
        let hidden = action(sys::GHOSTTY_ACTION_MOUSE_VISIBILITY, |u| {
            u.mouse_visibility = sys::GHOSTTY_MOUSE_HIDDEN;
        });
        assert!(matches!(decode(&hidden), Action::MouseVisibility { hidden: true }));
        let visible = action(sys::GHOSTTY_ACTION_MOUSE_VISIBILITY, |u| {
            u.mouse_visibility = sys::GHOSTTY_MOUSE_VISIBLE;
        });
        assert!(matches!(decode(&visible), Action::MouseVisibility { hidden: false }));
    }

    #[test]
    fn color_changes_decode_their_kind() {
        let background = action(sys::GHOSTTY_ACTION_COLOR_CHANGE, |u| {
            u.color_change = sys::ghostty_action_color_change_s {
                kind: sys::GHOSTTY_ACTION_COLOR_KIND_BACKGROUND,
                r: 0x10,
                g: 0x20,
                b: 0x30,
            };
        });
        assert!(matches!(
            decode(&background),
            Action::ColorChange {
                kind: ColorKind::Background,
                color: Color {
                    r: 0x10,
                    g: 0x20,
                    b: 0x30
                }
            }
        ));
        let palette = action(sys::GHOSTTY_ACTION_COLOR_CHANGE, |u| u.color_change.kind = 7);
        assert!(matches!(
            decode(&palette),
            Action::ColorChange {
                kind: ColorKind::Palette(7),
                ..
            }
        ));
    }

    #[test]
    fn plain_tags_and_unknown_ones_decode() {
        assert!(matches!(
            decode(&action(sys::GHOSTTY_ACTION_RENDER, |_| {})),
            Action::Render
        ));
        assert!(matches!(
            decode(&action(sys::GHOSTTY_ACTION_RING_BELL, |_| {})),
            Action::RingBell
        ));
        assert!(matches!(
            decode(&action(sys::GHOSTTY_ACTION_END_SEARCH, |_| {})),
            Action::EndSearch
        ));
        assert!(matches!(
            decode(&action(sys::GHOSTTY_ACTION_NEW_TAB, |_| {})),
            Action::Other(sys::GHOSTTY_ACTION_NEW_TAB)
        ));
        let cell = action(sys::GHOSTTY_ACTION_CELL_SIZE, |u| {
            u.cell_size = sys::ghostty_action_cell_size_s { width: 9, height: 18 };
        });
        assert!(matches!(decode(&cell), Action::CellSize { width: 9, height: 18 }));
        let exited = action(sys::GHOSTTY_ACTION_SHOW_CHILD_EXITED, |u| {
            u.child_exited = sys::ghostty_surface_message_childexited_s {
                exit_code: 2,
                timetime_ms: 40,
            };
        });
        assert!(matches!(
            decode(&exited),
            Action::ShowChildExited {
                exit_code: 2,
                runtime_ms: 40
            }
        ));
    }

    #[test]
    fn a_null_config_change_decodes_to_none() {
        let raw = action(sys::GHOSTTY_ACTION_CONFIG_CHANGE, |u| {
            u.config_change.config = std::ptr::null_mut();
        });
        assert!(matches!(decode(&raw), Action::ConfigChange(None)));
    }

    #[test]
    fn every_mouse_shape_has_its_css_name() {
        assert_eq!(MouseShape::ALL.len(), MouseShape::CSS_NAMES.len());
        assert_eq!(MouseShape::Text.css_name(), "text");
        assert_eq!(MouseShape::Pointer.css_name(), "pointer");
        assert_eq!(MouseShape::ZoomOut.css_name(), "zoom-out");
        assert_eq!(
            MouseShape::from_raw(sys::GHOSTTY_MOUSE_SHAPE_NWSE_RESIZE),
            Some(MouseShape::NwseResize)
        );
        assert_eq!(MouseShape::NwseResize.css_name(), "nwse-resize");
    }

    #[test]
    fn surface_keys_round_trip_through_userdata() {
        let key = SurfaceKey::new(NonZeroUsize::new(42).expect("non-zero"));
        assert_eq!(SurfaceKey::from_userdata(key.as_userdata()), Some(key));
        assert_eq!(SurfaceKey::from_userdata(std::ptr::null_mut()), None);
    }
}
