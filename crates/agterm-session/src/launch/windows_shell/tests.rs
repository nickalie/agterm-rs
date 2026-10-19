//! herdr: the `default_windows_pane_shell` and image tests of `src/pane.rs` at `HERDR_REV`, licensed under the Apache
//! License 2.0, modified: each test owns a temporary directory; the hosts are named.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// A structurally valid PE32+ image whose fields tests make malformed one way at a time.
fn image(machine: u16, characteristics: u16, sections: u16) -> Vec<u8> {
    const PE_OFFSET: u32 = 0x80;
    const OPTIONAL_HEADER_LEN: u16 = 0x70;
    let pe = PE_OFFSET as usize;
    let mut bytes = vec![0u8; pe + PE_HEADERS_LEN + usize::from(OPTIONAL_HEADER_LEN) + usize::from(sections) * 40];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[PE_OFFSET_FIELD..PE_OFFSET_FIELD + 4].copy_from_slice(&PE_OFFSET.to_le_bytes());
    bytes[pe..pe + 4].copy_from_slice(PE_SIGNATURE);
    let coff = pe + 4;
    bytes[coff..coff + 2].copy_from_slice(&machine.to_le_bytes());
    bytes[coff + 2..coff + 4].copy_from_slice(&sections.to_le_bytes());
    bytes[coff + 16..coff + 18].copy_from_slice(&OPTIONAL_HEADER_LEN.to_le_bytes());
    bytes[coff + 18..coff + 20].copy_from_slice(&characteristics.to_le_bytes());
    bytes[pe + PE_HEADERS_LEN..pe + PE_HEADERS_LEN + 2].copy_from_slice(&PE32_PLUS_MAGIC.to_le_bytes());
    bytes
}

fn x64_image() -> Vec<u8> {
    image(MACHINE_AMD64, IMAGE_FILE_EXECUTABLE_IMAGE, 1)
}

fn path_of(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).unwrap()
}

/// The shell an x64 host picks under `PATH` = `dirs`.
fn shell_under(dirs: &[&Path]) -> String {
    shell_on_path(Some(&path_of(dirs)), MACHINE_AMD64)
}

fn display(path: PathBuf) -> String {
    path.into_os_string().into_string().unwrap()
}

#[test]
fn pwsh_on_path_is_preferred_as_a_full_path() {
    let dir = tempfile::tempdir().unwrap();
    let pwsh = dir.path().join(PWSH);
    fs::write(&pwsh, x64_image()).unwrap();
    assert_eq!(shell_under(&[dir.path()]), display(pwsh));
}

#[test]
fn without_a_launchable_pwsh_the_inbox_powershell_runs() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(PWSH), b"not a PE image").unwrap();
    assert_eq!(shell_under(&[dir.path()]), INBOX_POWERSHELL);
    assert_eq!(shell_on_path(None, MACHINE_AMD64), INBOX_POWERSHELL);
    // an empty PATH, and a relative directory that would resolve against the working directory
    assert_eq!(shell_on_path(Some(OsStr::new("")), MACHINE_AMD64), INBOX_POWERSHELL);
    assert_eq!(shell_under(&[Path::new("relative")]), INBOX_POWERSHELL);
}

#[test]
fn malformed_or_foreign_images_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let pwsh = dir.path().join(PWSH);
    let truncated_header = {
        let mut bytes = b"MZ".to_vec();
        bytes.resize(DOS_HEADER_LEN, 0);
        bytes[PE_OFFSET_FIELD..PE_OFFSET_FIELD + 4].copy_from_slice(&0x50u32.to_le_bytes());
        bytes
    };
    // one declared section, but the file ends before its 40-byte header
    let truncated_section_table = {
        let mut bytes = x64_image();
        bytes.truncate(bytes.len() - 1);
        bytes
    };
    let wrong_magic = {
        let mut bytes = x64_image();
        let at = 0x80 + PE_HEADERS_LEN;
        bytes[at..at + 2].copy_from_slice(&0x0107u16.to_le_bytes());
        bytes
    };
    let cases: [(&str, Vec<u8>); 9] = [
        ("empty", Vec::new()),
        ("dos signature only", b"MZ".to_vec()),
        ("truncated pe header", truncated_header),
        ("truncated section table", truncated_section_table),
        (
            "dll",
            image(MACHINE_AMD64, IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL, 1),
        ),
        ("no executable bit", image(MACHINE_AMD64, 0, 1)),
        ("foreign machine", image(0x0200, IMAGE_FILE_EXECUTABLE_IMAGE, 1)),
        ("zero sections", image(MACHINE_AMD64, IMAGE_FILE_EXECUTABLE_IMAGE, 0)),
        ("not PE32 or PE32+", wrong_magic),
    ];
    for (label, bytes) in cases {
        fs::write(&pwsh, &bytes).unwrap();
        assert_eq!(shell_under(&[dir.path()]), INBOX_POWERSHELL, "case {label:?}");
    }
    fs::write(&pwsh, x64_image()).unwrap();
    assert_eq!(shell_under(&[dir.path()]), display(pwsh), "the valid image");
}

#[test]
fn machine_compatibility_tracks_the_native_host() {
    assert!(machine_is_compatible(MACHINE_I386, MACHINE_AMD64));
    assert!(machine_is_compatible(MACHINE_AMD64, MACHINE_AMD64));
    assert!(!machine_is_compatible(MACHINE_ARM64, MACHINE_AMD64));
    assert!(machine_is_compatible(MACHINE_I386, MACHINE_ARM64));
    assert!(machine_is_compatible(MACHINE_AMD64, MACHINE_ARM64));
    assert!(machine_is_compatible(MACHINE_ARM64, MACHINE_ARM64));
    assert!(machine_is_compatible(MACHINE_I386, MACHINE_I386));
    assert!(!machine_is_compatible(MACHINE_AMD64, MACHINE_I386));
    assert!(!machine_is_compatible(0x0200, MACHINE_AMD64));
    // an unknown host counts as x64
    assert!(machine_is_compatible(MACHINE_AMD64, 0));
    assert!(!machine_is_compatible(MACHINE_ARM64, 0));
}

#[test]
fn an_arm64_image_launches_only_on_an_arm64_host() {
    let dir = tempfile::tempdir().unwrap();
    let pwsh = dir.path().join(PWSH);
    fs::write(&pwsh, image(MACHINE_ARM64, IMAGE_FILE_EXECUTABLE_IMAGE, 1)).unwrap();
    assert!(
        !is_launchable(&pwsh, MACHINE_AMD64),
        "x64 Windows cannot launch a pure ARM64 image"
    );
    assert!(is_launchable(&pwsh, MACHINE_ARM64));
    assert!(!is_launchable(&pwsh, MACHINE_I386));
}

#[test]
fn invalid_candidates_are_skipped_for_a_later_directory() {
    let base = tempfile::tempdir().unwrap();
    let (invalid, valid) = (base.path().join("invalid"), base.path().join("valid"));
    fs::create_dir_all(&invalid).unwrap();
    fs::create_dir_all(&valid).unwrap();
    fs::write(invalid.join(PWSH), b"MZ").unwrap();
    let pwsh = valid.join(PWSH);
    fs::write(&pwsh, x64_image()).unwrap();
    // a directory named pwsh.exe is no candidate either
    let directory = base.path().join("directory");
    fs::create_dir_all(directory.join(PWSH)).unwrap();
    assert_eq!(shell_under(&[&directory, &invalid, &valid]), display(pwsh));
}
