//! A pane's working-directory report as libghostty handed it to the GTK build: [`PaneEvent::Pwd`] carries the raw
//! OSC 7 value, and [`reported_path`] turns it into the path Ghostty's `StreamHandler.reportPwd` (`termio/
//! stream_handler.zig` at `LIBGHOSTTY_VT_REV`) reported as `pwd_change`.
//!
//! - An empty value resets the pwd: the report is the empty path.
//! - Otherwise the value must parse as a `file://` or `kitty-shell-cwd://` URI (the scheme as written, case and all)
//!   with a host (Ghostty parses with `mac_address` on Linux, so a MAC address host is a host, not a host and a
//!   port), and the host must be local:
//!   `localhost` or this machine's hostname exactly (`os/hostname.zig` `isLocal`). An empty host (`file:///tmp`) is
//!   no match for either, so Ghostty ignores it, and so does this.
//! - The path is percent-decoded and ends at a query or fragment, except for `kitty-shell-cwd://`, whose path is
//!   everything after the host, raw (Ghostty's `raw_path`).
//!
//! [`PaneEvent::Pwd`]: crate::PaneEvent::Pwd

/// The schemes OSC 7 accepts.
const FILE: &str = "file";
const KITTY: &str = "kitty-shell-cwd";

/// The path Ghostty reports for the raw OSC 7 value `raw` on a machine named `hostname`; `None` when Ghostty ignores
/// the report (an invalid URI, another scheme, no host, a host that is not local).
#[must_use]
pub fn reported_path(raw: &str, hostname: &str) -> Option<String> {
    if raw.is_empty() {
        return Some(String::new());
    }
    let (scheme, rest) = raw.split_once("://")?;
    if scheme != FILE && scheme != KITTY {
        tracing::debug!(scheme, "OSC 7 scheme must be file or kitty-shell-cwd");
        return None;
    }
    let path_start = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(path_start);
    let host = decode(&host_of(authority));
    if host != "localhost" && host != hostname {
        tracing::debug!(host, "OSC 7 host must be local");
        return None;
    }
    if scheme == KITTY {
        return Some(path.to_owned());
    }
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(decode(&path[..end]))
}

/// This machine's hostname, as `gethostname(2)` gives it (on Windows `GetComputerNameW`'s, the name Ghostty's
/// `isLocal` compares against there).
#[cfg(unix)]
#[must_use]
pub fn local_hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is valid for its length, and `gethostname` writes at most that many bytes.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return None;
    }
    let len = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
    std::str::from_utf8(&buffer[..len]).ok().map(str::to_owned)
}

/// This machine's hostname, as `GetComputerNameW` gives it (Ghostty's `isLocal` on Windows).
#[cfg(windows)]
#[must_use]
pub fn local_hostname() -> Option<String> {
    use windows_sys::Win32::System::WindowsProgramming::GetComputerNameW;

    let mut buffer = [0_u16; 256];
    let mut len = u32::try_from(buffer.len()).ok()?;
    // SAFETY: the buffer is valid for `len` UTF-16 units, and `GetComputerNameW` writes at most that many (and the
    // name's length, without the NUL, into `len` on success).
    if unsafe { GetComputerNameW(buffer.as_mut_ptr(), &raw mut len) } == 0 {
        return None;
    }
    let len = usize::try_from(len).ok()?.min(buffer.len());
    String::from_utf16(&buffer[..len]).ok()
}

/// The host of a URI's authority: past any user info, without a port, a MAC address kept whole.
fn host_of(authority: &str) -> String {
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    if is_mac_address(host) {
        return host.to_owned();
    }
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.bytes().all(|byte| byte.is_ascii_digit()) => name.to_owned(),
        _ => host.to_owned(),
    }
}

/// Six pairs of hex digits separated by colons (Ghostty's `isValidMacAddress`).
fn is_mac_address(text: &str) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Percent-decodes `text`; a `%` not followed by two hex digits stays as it is.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(index + 1).and_then(|byte| hex(*byte)),
                bytes.get(index + 2).and_then(|byte| hex(*byte)),
            )
        {
            out.push(high << 4 | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).and_then(|digit| u8::try_from(digit).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "box";

    #[test]
    fn a_local_file_uri_reports_its_decoded_path() {
        assert_eq!(
            reported_path("file://box/home/u/my%20dir", HOST).as_deref(),
            Some("/home/u/my dir")
        );
        assert_eq!(reported_path("file://localhost/tmp", HOST).as_deref(), Some("/tmp"));
        assert_eq!(
            reported_path("file://box/tmp?x=1#top", HOST).as_deref(),
            Some("/tmp"),
            "a query and a fragment end the path"
        );
        // Ghostty compares the scheme as written (`std.mem.eql`, a case-sensitive `startsWith` for kitty's)
        assert_eq!(reported_path("FILE://box/tmp", HOST), None);
        assert_eq!(reported_path("KITTY-SHELL-CWD://box/tmp", HOST), None);
        assert_eq!(
            reported_path("file://user@box:22/srv", HOST).as_deref(),
            Some("/srv"),
            "user info and a port are not the host"
        );
    }

    #[test]
    fn kitty_shell_cwd_keeps_its_path_raw() {
        assert_eq!(
            reported_path("kitty-shell-cwd://box/a%20b?c#d", HOST).as_deref(),
            Some("/a%20b?c#d")
        );
    }

    #[test]
    fn an_empty_value_resets_the_pwd() {
        assert_eq!(reported_path("", HOST).as_deref(), Some(""));
    }

    #[test]
    fn a_foreign_host_another_scheme_or_no_host_is_ignored() {
        assert_eq!(reported_path("file://elsewhere/tmp", HOST), None);
        assert_eq!(reported_path("file:///tmp", HOST), None, "an empty host is not local");
        assert_eq!(reported_path("http://box/tmp", HOST), None);
        assert_eq!(reported_path("/tmp", HOST), None, "a bare path is no URI");
        assert_eq!(
            reported_path("file://12:34:56:78:90:ab/tmp", HOST),
            None,
            "a MAC address host is a host"
        );
        assert_eq!(
            reported_path("file://12:34:56:78:90:ab/tmp", "12:34:56:78:90:ab").as_deref(),
            Some("/tmp")
        );
    }

    #[test]
    fn this_machine_has_a_hostname() {
        assert!(local_hostname().is_some_and(|name| !name.is_empty()));
    }
}
