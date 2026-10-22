//! Mesa's lavapipe for the GPUI tests that render: the ICD manifest `scripts/fetch-lavapipe.sh --print-icd` resolves
//! (`NGTERM_LAVAPIPE_ICD`, else the distribution's, else the vendored one; its driver checked), the conformance
//! harness's one rule. Nothing is fetched here: `scripts/fetch-lavapipe.sh` without arguments does that.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The lavapipe ICD manifest, or `None` with the script's reason on stderr.
pub fn lavapipe_icd() -> Option<PathBuf> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fetch-lavapipe.sh");
    match Command::new(&script).arg("--print-icd").output() {
        Ok(output) if output.status.success() => {
            let icd = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            (!icd.is_empty()).then(|| PathBuf::from(icd))
        }
        Ok(output) => {
            eprintln!("no lavapipe: {}", String::from_utf8_lossy(&output.stderr).trim());
            None
        }
        Err(error) => {
            eprintln!("no lavapipe: {}: {error}", script.display());
            None
        }
    }
}
