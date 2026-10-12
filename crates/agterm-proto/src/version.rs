//! The `version` result: the serving app plus the Rust build's conformance profile.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

/// The upstream version whose behavior this build claims.
pub const PARITY_LEVEL: &str = "0.36.0";

/// Which agterm serves the control socket. `tree` carries the same value as `tree.app`.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppIdentity {
    /// The comparable version number.
    pub version: String,
    /// The build's git commit, diagnostics only; absent for a build that recorded none.
    pub commit: Option<String>,
}

impl AppIdentity {
    /// Builds from raw build values, dropping a commit that is empty or the literal `unknown`.
    #[must_use]
    pub fn from_recorded(version: impl Into<String>, recorded_commit: Option<&str>) -> Self {
        let commit = recorded_commit
            .filter(|c| !c.is_empty() && *c != "unknown")
            .map(str::to_owned);
        Self {
            version: version.into(),
            commit,
        }
    }
}

/// The `conformance` block only the Rust build reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceProfile {
    /// A dotted upstream version, see [`PARITY_LEVEL`].
    pub parity_level: String,
    /// Capability names from the suite's vocabulary (`conformance/docs/rust-build.md`).
    pub capabilities: Vec<String>,
}

/// The `version` result: exactly `app` and `conformance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionResult {
    pub app: AppIdentity,
    pub conformance: ConformanceProfile,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::response::Response;

    fn sample() -> VersionResult {
        VersionResult {
            app: AppIdentity::from_recorded("0.1.0", Some("abc1234")),
            conformance: ConformanceProfile {
                parity_level: PARITY_LEVEL.to_owned(),
                capabilities: vec!["posix-shell".to_owned(), "live-sessions".to_owned()],
            },
        }
    }

    #[test]
    fn serializes_to_the_documented_shape() {
        let json = serde_json::to_value(sample()).unwrap();
        assert_eq!(
            json,
            json!({"app": {"version": "0.1.0", "commit": "abc1234"},
                   "conformance": {"parityLevel": "0.36.0", "capabilities": ["posix-shell", "live-sessions"]}})
        );
    }

    #[test]
    fn round_trips_inside_a_response() {
        let response = Response::with_result(sample());
        let line = serde_json::to_string(&response).unwrap();
        let back: Response<VersionResult> = serde_json::from_str(&line).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn commit_is_omitted_when_not_recorded() {
        for recorded in [None, Some(""), Some("unknown")] {
            let app = AppIdentity::from_recorded("0.1.0", recorded);
            assert_eq!(app.commit, None);
            assert_eq!(serde_json::to_value(&app).unwrap(), json!({"version": "0.1.0"}));
        }
    }
}
