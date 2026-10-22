//! Which link clicks a terminal opens and how (upstream's `LinkPolicy.swift`).
//!
//! A terminal renders untrusted program output, so an escape-sequence link (`GHOSTTY_ACTION_OPEN_URL`) can carry any
//! scheme. [`disposition`] maps a raw link to opening a web or mail URL with the desktop's handler, revealing a
//! local `file://` link in the file manager, or ignoring it. A `file://` link is revealed, never opened: opening
//! hands it to the default application for its type, which for a script or a `.desktop` file can run it, while a
//! reveal only selects it. A `file://` whose host is not this machine is ignored, as is a path under an automount
//! root, where even looking at the path can mount a network share. [`route`] adds the link-open setting on top: a
//! web link clicked in a pane or the scratch terminal may go to a session overlay instead of the browser.
//!
//! Host-free: the local host names are injected (the `*_for_hosts` variants), and the frontend only carries out the
//! route. URLs are parsed here as RFC 3986 references, strictly (a space, a control character or a broken
//! percent-escape makes a link unparseable, so it is ignored), in place of Foundation's `URL(string:)`.

use std::collections::HashSet;
use std::hash::BuildHasher;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::OnceLock;

use uuid::Uuid;

use crate::persist::settings::LinkOpenMode;

/// The schemes safe to hand to the desktop's opener: web and mail only, none that hands off to a local executable or
/// handler.
pub const PERMITTED_SCHEMES: [&str; 4] = ["http", "https", "mailto", "ftp"];

/// The automount roots where revealing a path can mount a network share: autofs's `/net` (the `-hosts` map) and
/// `/misc` (`auto.misc`). Upstream's macOS list (`/Network`, `auto_home`'s `/home` and their `/System/Volumes/Data`
/// twins) names no automount on Linux, where `/home` holds the real home directories.
const AUTOMOUNT_ROOTS: [&str; 2] = ["/net", "/misc"];

/// What a link click does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkDisposition {
    /// Open the link, as given, with the desktop's handler for its scheme.
    Open(String),
    /// Reveal this local path, host-stripped and dot-normalized, in the file manager.
    Reveal(PathBuf),
    Ignore,
}

/// Where a clicked link came from. Only a pane and the scratch terminal have an owning session a page could open on;
/// a HUD link stays in the browser so the click never replaces the HUD it sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickOrigin {
    Pane(Uuid),
    Scratch(Uuid),
    Hud,
    ProgramOverlay,
    Quick,
}

/// What a link click does once the link-open setting applies. `Browser` is the desktop's opener, whatever handler the
/// scheme maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Browser(String),
    Overlay { url: String, session: Uuid },
    Reveal(PathBuf),
    Ignore,
}

/// The lowercased host names counting as this machine for a `file://` link: `localhost` and the kernel's host name
/// (what GNU `ls --hyperlink` emits as `file://<host>/…`; `eza` leaves the host empty, which always counts), with the
/// `.local`-stripped short forms. Read once, from `/proc` (no resolver, no network).
#[must_use]
pub fn local_host_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let hostname = ["/proc/sys/kernel/hostname", "/etc/hostname"]
            .iter()
            .find_map(|path| std::fs::read_to_string(path).ok())
            .map(|name| name.trim().to_owned())
            .unwrap_or_default();
        expanded_host_names(["localhost", hostname.as_str()])
    })
}

/// Normalizes each raw host name and adds the `.local`-stripped short form beside the full one; empty names (and a
/// bare `.local`'s empty short form) are skipped.
#[must_use]
pub fn expanded_host_names<'a>(raw: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    let mut out = HashSet::new();
    for name in raw {
        let normalized = normalized_host(name);
        if normalized.is_empty() {
            continue;
        }
        if let Some(short) = normalized.strip_suffix(".local")
            && !short.is_empty()
        {
            out.insert(short.to_owned());
        }
        out.insert(normalized);
    }
    out
}

/// A host lowercased, without a trailing FQDN dot, so matching is stable.
#[must_use]
pub fn normalized_host(host: &str) -> String {
    let lower = host.to_lowercase();
    match lower.strip_suffix('.') {
        Some(stripped) => stripped.to_owned(),
        None => lower,
    }
}

/// [`disposition_for_hosts`] with this machine's names.
#[must_use]
pub fn disposition(raw: &str) -> LinkDisposition {
    disposition_for_hosts(raw, local_host_names())
}

/// Maps a raw terminal link to what a click does: a permitted web or mail scheme opens; a local `file://` link (an
/// empty host or one in `local_hosts`) reveals its host-stripped, dot-normalized path; anything else is ignored: a
/// `file://` with another host, an empty or relative path, a UNC-style `//` path or an automount path (checked after
/// the `..` normalization, so `/tmp/../net/x` cannot sneak through), any other scheme, no scheme, or a link that does
/// not parse.
#[must_use]
pub fn disposition_for_hosts<S: BuildHasher>(raw: &str, local_hosts: &HashSet<String, S>) -> LinkDisposition {
    let Some(link) = Link::parse(raw) else {
        return LinkDisposition::Ignore;
    };
    if PERMITTED_SCHEMES.contains(&link.scheme.as_str()) {
        return LinkDisposition::Open(raw.to_owned());
    }
    if link.scheme != "file" {
        return LinkDisposition::Ignore;
    }
    let host = normalized_host(link.host.as_deref().unwrap_or_default());
    if !host.is_empty() && !local_hosts.contains(&host) {
        return LinkDisposition::Ignore;
    }
    // decoded first, so an encoded slash or dot segment is seen for what it is
    let Some(path) = percent_decode(link.path) else {
        return LinkDisposition::Ignore;
    };
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\0') {
        return LinkDisposition::Ignore;
    }
    // lexical only: never stat (or resolve a symlink into) the automount path about to be denied
    let normalized = lexically_normalized_absolute_path(&path);
    if is_automount_path(&normalized) {
        return LinkDisposition::Ignore;
    }
    LinkDisposition::Reveal(PathBuf::from(normalized))
}

/// [`route_for_hosts`] with this machine's names.
#[must_use]
pub fn route(raw: &str, mode: LinkOpenMode, origin: ClickOrigin) -> Route {
    route_for_hosts(raw, mode, origin, local_host_names())
}

/// The route of a raw link: [`disposition_for_hosts`] decides open, reveal or ignore, then a web link an overlay can
/// show, clicked in a pane or the scratch terminal, goes to that session's overlay under [`LinkOpenMode::Overlay`].
/// Whether the overlay is free right now is the caller's check.
#[must_use]
pub fn route_for_hosts<S: BuildHasher>(
    raw: &str,
    mode: LinkOpenMode,
    origin: ClickOrigin,
    local_hosts: &HashSet<String, S>,
) -> Route {
    match disposition_for_hosts(raw, local_hosts) {
        LinkDisposition::Ignore => Route::Ignore,
        LinkDisposition::Reveal(path) => Route::Reveal(path),
        LinkDisposition::Open(url) => {
            if mode != LinkOpenMode::Overlay || !overlay_can_show(&url) {
                return Route::Browser(url);
            }
            match origin {
                ClickOrigin::Pane(session) | ClickOrigin::Scratch(session) => Route::Overlay { url, session },
                ClickOrigin::Hud | ClickOrigin::ProgramOverlay | ClickOrigin::Quick => Route::Browser(url),
            }
        }
    }
}

/// Whether a session web overlay can show `url`: https, or plain http from a host [`loads_plain_http`] accepts.
/// `mailto`, `ftp` and other plain http go to the desktop's handler.
fn overlay_can_show(url: &str) -> bool {
    let Some(link) = Link::parse(url) else {
        return false;
    };
    match link.scheme.as_str() {
        "https" => true,
        "http" => loads_plain_http(link.host.as_deref().unwrap_or_default()),
        _ => false,
    }
}

/// Whether the app's web views load plain http from `host`: local networking only, read off the host's syntax, never
/// off where it resolves (upstream's App Transport Security rule): an unqualified name, a `.local` name, or an IP
/// literal, a public one included.
fn loads_plain_http(host: &str) -> bool {
    let host = normalized_host(host);
    if host.is_empty() {
        return false;
    }
    let local_name = host.rsplit_once('.').is_some_and(|(_, last)| last == "local");
    if local_name || !host.contains(['.', ':']) {
        return true;
    }
    host.parse::<Ipv4Addr>().is_ok() || host.parse::<Ipv6Addr>().is_ok()
}

/// Whether a dot-normalized absolute path is an automount root or under one: exact or a `<root>/…` child, so
/// `/networkx` is not caught.
fn is_automount_path(path: &str) -> bool {
    AUTOMOUNT_ROOTS.iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Collapses `.` and `..` in an absolute path lexically: no filesystem access, no symlink resolution. A `..` at the
/// root is dropped.
fn lexically_normalized_absolute_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for component in path.split('/').filter(|component| !component.is_empty()) {
        match component {
            "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(component),
        }
    }
    format!("/{}", out.join("/"))
}

/// The parts of a URL the policy reads.
#[derive(Debug, PartialEq, Eq)]
struct Link<'a> {
    /// Lowercased.
    scheme: String,
    /// The authority's host, percent-decoded, without an IPv6 literal's brackets; `None` without an authority.
    host: Option<String>,
    /// The path, still percent-encoded.
    path: &'a str,
}

impl<'a> Link<'a> {
    /// Parses an absolute RFC 3986 URI; `None` for a relative reference, a character a URI cannot hold (see
    /// [`uri_char`]), a broken percent-escape, or a malformed authority.
    fn parse(raw: &'a str) -> Option<Self> {
        if !raw.chars().all(uri_char) || !valid_escapes(raw) {
            return None;
        }
        let (scheme, rest) = raw.split_once(':')?;
        let mut chars = scheme.chars();
        if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return None;
        }
        let hier = rest.split(['?', '#']).next().unwrap_or_default();
        let (host, path) = match hier.strip_prefix("//") {
            Some(after) => {
                let end = after.find('/').unwrap_or(after.len());
                (Some(authority_host(&after[..end])?), &after[end..])
            }
            None => (None, hier),
        };
        Some(Self {
            scheme: scheme.to_ascii_lowercase(),
            host,
            path,
        })
    }
}

/// The host of an authority (`[userinfo@]host[:port]`), percent-decoded; `None` for a port that is not a number or
/// an unterminated IPv6 literal.
fn authority_host(authority: &str) -> Option<String> {
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host_port)| host_port);
    let (host, port) = if let Some(literal) = host_port.strip_prefix('[') {
        let (host, after) = literal.split_once(']')?;
        let port = if after.is_empty() { "" } else { after.strip_prefix(':')? };
        (host, port)
    } else {
        host_port.rsplit_once(':').unwrap_or((host_port, ""))
    };
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    percent_decode(host)
}

/// Whether `c` may appear in a URI as written: no space, no control character, none of the characters RFC 3986
/// excludes (the double quote, angle brackets, backslash, caret, grave accent, braces and bar). Non-ASCII passes (an
/// IRI).
fn uri_char(c: char) -> bool {
    !c.is_control() && !c.is_whitespace() && !matches!(c, '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}')
}

/// Whether every `%` starts a two-hex-digit escape.
fn valid_escapes(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.iter().enumerate().filter(|(_, b)| **b == b'%').all(|(index, _)| {
        bytes
            .get(index + 1..index + 3)
            .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    })
}

/// Decodes percent-escapes; `None` when the result is not UTF-8.
pub(crate) fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let escaped = (byte == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(decoded) = escaped {
            out.push(decoded);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed set, so host matching does not depend on the machine.
    fn hosts() -> HashSet<String> {
        ["myhost", "localhost"].into_iter().map(str::to_owned).collect()
    }

    fn disposition_of(raw: &str) -> LinkDisposition {
        disposition_for_hosts(raw, &hosts())
    }

    fn reveal(path: &str) -> LinkDisposition {
        LinkDisposition::Reveal(PathBuf::from(path))
    }

    // disposition (open / reveal / ignore)

    /// The garbage suffixes model a C string over-read past the link's length: they leave the URL unparseable.
    #[test]
    fn trailing_garbage_does_not_yield_a_web_url() {
        assert_eq!(
            disposition_of("https://example.com\0/etc/other"),
            LinkDisposition::Ignore
        );
        assert_eq!(
            disposition_of("https://example.com extra junk"),
            LinkDisposition::Ignore
        );
    }

    #[test]
    fn web_schemes_open() {
        for raw in [
            "http://example.com",
            "https://example.com/path?q=1#frag",
            "HTTPS://EXAMPLE.COM",
            "mailto:someone@example.com",
            "ftp://host/file.txt",
        ] {
            assert_eq!(disposition_of(raw), LinkDisposition::Open(raw.to_owned()), "{raw}");
        }
    }

    /// A local `file://` reveals the host-stripped, dot-normalized path, so every host form of `/tmp/x.md` resolves
    /// to the same plain path.
    #[test]
    fn local_file_links_reveal() {
        for (raw, expected) in [
            ("file:///tmp/x.md", "/tmp/x.md"),
            ("file:/tmp/x.md", "/tmp/x.md"),
            ("file://localhost/tmp/x.md", "/tmp/x.md"),
            ("file://myhost/tmp/x.md", "/tmp/x.md"),
            ("file://MYHOST/tmp/x.md", "/tmp/x.md"),
            ("file://myhost./tmp/x.md", "/tmp/x.md"),
            ("FILE:///tmp/x.md", "/tmp/x.md"),
            ("file:///tmp/../tmp/x.md", "/tmp/x.md"),
            ("file://myhost/tmp/a%20b.md", "/tmp/a b.md"),
            (
                "file:///usr/share/applications/x.desktop",
                "/usr/share/applications/x.desktop",
            ),
            ("file://user@myhost:22/tmp/x.md", "/tmp/x.md"),
        ] {
            assert_eq!(disposition_of(raw), reveal(expected), "{raw}");
        }
    }

    #[test]
    fn automount_paths_are_ignored() {
        for raw in [
            "file:///net/server/share/x.md",
            "file:///misc/cd/x",
            "file://localhost/net/server/share",
            "file:///tmp/../net/server/share",
            "file:///tmp/%2E%2E/net/server/share",
            "file:///net",
            "file:///misc",
        ] {
            assert_eq!(disposition_of(raw), LinkDisposition::Ignore, "{raw}");
        }
    }

    /// Siblings that merely start with an automount root are revealed, and so are the real home directories (macOS's
    /// `auto_home` root is no automount on Linux).
    #[test]
    fn automount_lookalikes_and_homes_still_reveal() {
        for path in [
            "/networkx/x.md",
            "/nettools/x.md",
            "/miscellany/x.md",
            "/home/someone/x.md",
            "/home",
        ] {
            assert_eq!(disposition_of(&format!("file://{path}")), reveal(path), "{path}");
        }
    }

    /// The classifier never resolves symlinks: a link inside a symlinked directory keeps the link's path. Unix only: a
    /// Windows temporary path (`C:\...`) is not a `file://` path as written, and its symlink needs a privilege.
    #[cfg(unix)]
    #[test]
    fn reveal_does_not_resolve_symlinks() -> std::io::Result<()> {
        let base = tempfile::tempdir()?;
        let target = base.path().join("target");
        std::fs::create_dir_all(&target)?;
        // the leaf must exist, else a symlink-resolving regression would return the link's path too
        std::fs::write(target.join("x.md"), b"")?;
        std::os::unix::fs::symlink(&target, base.path().join("link"))?;
        let leaf = base.path().join("link").join("x.md");
        let raw = format!("file://{}", leaf.display());
        assert_eq!(disposition_of(&raw), LinkDisposition::Reveal(leaf));
        Ok(())
    }

    /// An empty or relative `file://` path would reveal the working directory: ignored.
    #[test]
    fn empty_or_relative_file_paths_are_ignored() {
        for raw in ["file://localhost", "file://myhost", "file:relative"] {
            assert_eq!(disposition_of(raw), LinkDisposition::Ignore, "{raw}");
        }
    }

    #[test]
    fn non_local_file_links_are_ignored() {
        for raw in ["file://otherhost/tmp/x.md", "file://remote.example.com/share/x.md"] {
            assert_eq!(disposition_of(raw), LinkDisposition::Ignore, "{raw}");
        }
    }

    #[test]
    fn other_schemes_are_ignored() {
        for raw in [
            "javascript:alert(1)",
            "vscode://file/x",
            "tel:+15550100",
            "example.com",
            "",
            "   ",
            "1http://example.com",
            "https://example.com/%zz",
            "http://example.com:port/",
        ] {
            assert_eq!(disposition_of(raw), LinkDisposition::Ignore, "{raw}");
        }
    }

    #[test]
    fn unc_paths_are_ignored() {
        for raw in [
            "file:////server/share/x.md",
            "file://localhost//server/share/x.md",
            "file://localhost/%2Fserver/share",
        ] {
            assert_eq!(disposition_of(raw), LinkDisposition::Ignore, "{raw}");
        }
    }

    #[test]
    fn an_encoded_nul_or_invalid_utf8_path_is_ignored() {
        assert_eq!(disposition_of("file:///tmp/a%00b"), LinkDisposition::Ignore);
        assert_eq!(disposition_of("file:///tmp/%ff"), LinkDisposition::Ignore);
    }

    // local_host_names / expanded_host_names

    #[test]
    fn expanded_host_names_normalize_and_add_the_local_short_form() {
        let out = expanded_host_names(["MyMac.local", "Box.", "localhost", "", ".local"]);
        assert!(out.contains("mymac.local"));
        assert!(out.contains("mymac"));
        assert!(out.contains("box"));
        assert!(out.contains("localhost"));
        assert!(!out.contains(""));
    }

    #[test]
    fn local_host_names_always_have_localhost() {
        assert!(local_host_names().contains("localhost"));
    }

    /// The default host set reveals an empty-host or localhost file link.
    #[test]
    fn default_local_hosts_reveal_local_files() {
        for raw in ["file:///tmp/x.md", "file://localhost/tmp/x.md"] {
            assert_eq!(disposition(raw), reveal("/tmp/x.md"), "{raw}");
        }
    }

    // route (browser / overlay / reveal / ignore)

    fn origins(session: Uuid) -> [ClickOrigin; 5] {
        [
            ClickOrigin::Pane(session),
            ClickOrigin::Scratch(session),
            ClickOrigin::Hud,
            ClickOrigin::ProgramOverlay,
            ClickOrigin::Quick,
        ]
    }

    fn route_of(raw: &str, mode: LinkOpenMode, origin: ClickOrigin) -> Route {
        route_for_hosts(raw, mode, origin, &hosts())
    }

    #[test]
    fn browser_mode_sends_web_links_to_the_browser() {
        for raw in ["http://example.com/a", "https://example.com/a?q=1"] {
            for origin in origins(Uuid::new_v4()) {
                assert_eq!(
                    route_of(raw, LinkOpenMode::Browser, origin),
                    Route::Browser(raw.to_owned())
                );
            }
        }
    }

    #[test]
    fn overlay_mode_opens_pane_and_scratch_web_links_on_the_owning_session() {
        let session = Uuid::new_v4();
        for raw in ["http://localhost:5173/a", "HTTPS://example.com/a"] {
            for origin in [ClickOrigin::Pane(session), ClickOrigin::Scratch(session)] {
                let url = raw.to_owned();
                assert_eq!(
                    route_of(raw, LinkOpenMode::Overlay, origin),
                    Route::Overlay { url, session }
                );
            }
        }
    }

    /// Regression upstream: a plain-http page the app's web views cannot load was routed to the overlay.
    #[test]
    fn overlay_mode_takes_plain_http_only_from_hosts_a_web_view_may_load() {
        let session = Uuid::new_v4();
        for (raw, overlay) in [
            ("http://localhost/", true),
            ("http://LOCALHOST:8080/x", true),
            ("http://devbox/", true),
            ("http://mac.local:3000/", true),
            ("http://127.0.0.1:5173/", true),
            ("http://93.184.216.34/", true),
            ("http://[::1]:8080/", true),
            ("http://[2001:db8::1]/", true),
            ("http://example.com/", false),
            ("http://www.example.com:8080/a", false),
            ("http://local/", true),
            ("http://notlocal.localx/", false),
            ("http://1.2.3.example/", false),
            ("http://999.1.1.1/", false),
            ("https://example.com/", true),
        ] {
            let url = raw.to_owned();
            let expected = if overlay {
                Route::Overlay { url, session }
            } else {
                Route::Browser(url)
            };
            let routed = route_of(raw, LinkOpenMode::Overlay, ClickOrigin::Pane(session));
            assert_eq!(routed, expected, "{raw}");
        }
    }

    #[test]
    fn overlay_mode_keeps_sessionless_origins_in_the_browser() {
        for origin in [ClickOrigin::Hud, ClickOrigin::ProgramOverlay, ClickOrigin::Quick] {
            assert_eq!(
                route_of("https://example.com", LinkOpenMode::Overlay, origin),
                Route::Browser("https://example.com".to_owned())
            );
        }
    }

    #[test]
    fn non_web_schemes_always_go_to_the_system_handler() {
        for raw in ["mailto:someone@example.com", "ftp://host/file.txt"] {
            for mode in LinkOpenMode::ALL {
                let routed = route_of(raw, *mode, ClickOrigin::Pane(Uuid::new_v4()));
                assert_eq!(routed, Route::Browser(raw.to_owned()), "{raw}");
            }
        }
    }

    #[test]
    fn file_links_reveal_and_other_schemes_stay_ignored_in_every_mode() {
        for mode in LinkOpenMode::ALL {
            for origin in origins(Uuid::new_v4()) {
                assert_eq!(
                    route_of("file:///tmp/x.md", *mode, origin),
                    Route::Reveal(PathBuf::from("/tmp/x.md"))
                );
                assert_eq!(route_of("x-custom://run", *mode, origin), Route::Ignore);
                assert_eq!(route_of("file://elsewhere/tmp/x.md", *mode, origin), Route::Ignore);
            }
        }
    }
}
