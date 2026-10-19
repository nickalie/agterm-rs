//! What a live launch reads of this host: [`agterm_session::launch`]'s inputs, re-exported here for the callers that
//! still reach them through this crate, and the zmx client over them.

pub use agterm_session::launch::{
    BUILD_ID_PATH, EXECUTABLE_FILE, EXECUTABLE_OVERRIDE as ZMX_PATH_OVERRIDE, Runtime, bundled_build_id, login_shell,
};

use agterm_core::live::socket_directory;

use crate::client::ZmxClient;

/// A client of the daemons of `runtime`'s instance, hanging up what a confirmed kill leaves running.
#[must_use]
pub fn client(runtime: &Runtime) -> ZmxClient {
    ZmxClient::new(runtime.executable.clone(), socket_directory(&runtime.state_dir))
        .with_sweeper(crate::sweep::Sweeper::live())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use agterm_core::paths::MapEnv;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn the_client_talks_to_the_namespace_the_wrap_attaches_in() {
        let root = tempfile::tempdir().unwrap();
        let integration = root.path().join("share/ghostty/shell-integration/zsh");
        std::fs::create_dir_all(&integration).unwrap();
        std::fs::write(integration.join(".zshenv"), "").unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let zmx = bin.join(EXECUTABLE_FILE);
        std::fs::write(&zmx, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&zmx, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = MapEnv::new().with("AGTERM_STATE_DIR", "/tmp/agterm-state");
        let runtime = Runtime::resolve(&env, Some(&bin), Some("/usr/bin/zsh".to_owned()), false);
        let configuration = runtime.configuration(Uuid::new_v4(), BTreeMap::new(), None).unwrap();
        assert_eq!(client(&runtime).socket_directory(), configuration.socket_directory);
        assert_eq!(
            configuration.socket_directory,
            socket_directory(Path::new("/tmp/agterm-state"))
        );
    }
}
