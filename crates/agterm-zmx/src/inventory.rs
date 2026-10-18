//! The client side of the daemon lifecycle (upstream's `ZmxClient` kill paths): the forced batch kill behind the pane
//! finalizer, the launch reap and the reset's launch kill ([`agterm_host::SessionHost::kill_batch`]). The decisions,
//! the reap and prune policies included, are core's (`agterm_core::commands::live`, [`agterm_core::live::lifecycle`]);
//! it runs them over [`agterm_host::SessionHost`].

use std::collections::BTreeSet;
use std::time::Duration;

use crate::client::ZmxClient;

impl ZmxClient {
    /// One `zmx kill <names…> --force` for every distinct name, in order; `timeout` overrides this invocation's bound.
    /// Diagnostic only: zmx handles the names in turn, so a failure part-way has already reached some daemons, and a
    /// caller confirms each by its leader's exit.
    #[must_use]
    pub fn kill_batch(&self, names: &[String], timeout: Option<Duration>) -> bool {
        self.kill_names(names, None, timeout)
    }

    /// [`ZmxClient::kill_batch`] with the leader pids the caller already holds (`shells`, else listed for the
    /// sweep): each confirmed kill hangs up the foreground job its shell leaves behind.
    pub fn kill_names(&self, names: &[String], shells: Option<&[(String, i32)]>, timeout: Option<Duration>) -> bool {
        let mut seen = BTreeSet::new();
        let unique: Vec<&str> = names
            .iter()
            .filter(|name| seen.insert(name.as_str()))
            .map(String::as_str)
            .collect();
        if unique.is_empty() {
            return true;
        }
        let jobs = self.foreground_jobs(&unique, shells, timeout);
        let mut arguments = vec!["kill"];
        arguments.extend(&unique);
        arguments.push("--force");
        match self.invoke(&arguments, timeout, true, None) {
            Ok(output) => {
                self.hang_up_confirmed(&jobs, &output);
                true
            }
            Err(error) => {
                tracing::error!(names = unique.join(","), %error, "zmx kill failed");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use agterm_core::live::daemon_name;
    use uuid::Uuid;

    use super::*;
    use crate::client::{CommandError, Invocation, Runner};

    type Log = Arc<Mutex<Vec<Invocation>>>;

    fn recording(
        reply: impl Fn(&Invocation) -> Result<String, CommandError> + Send + Sync + 'static,
    ) -> (ZmxClient, Log) {
        let log = Log::default();
        let sink = Arc::clone(&log);
        let runner: Runner = Box::new(move |invocation| {
            sink.lock().unwrap().push(invocation.clone());
            reply(invocation)
        });
        (
            ZmxClient::with_runner("/tmp/zmx", "/tmp/zmx-dir", Duration::from_secs(3), runner),
            log,
        )
    }

    fn arguments(log: &Log) -> Vec<Vec<String>> {
        log.lock()
            .unwrap()
            .iter()
            .map(|invocation| invocation.arguments.clone())
            .collect()
    }

    #[test]
    fn a_batch_kill_forces_every_distinct_name_once() {
        let (client, log) = recording(|_| Ok(String::new()));
        let name = daemon_name(Uuid::from_u128(7));
        assert!(client.kill_batch(&[name.clone(), name.clone()], None));
        assert_eq!(arguments(&log), [vec!["kill".to_owned(), name, "--force".to_owned()]]);
        assert!(log.lock().unwrap()[0].merges_stderr);
        assert!(client.kill_batch(&[], None));
        assert_eq!(log.lock().unwrap().len(), 1, "nothing to kill runs nothing");
    }

    #[test]
    fn a_failed_batch_kill_reports_false() {
        let (client, _) = recording(|_| Err(CommandError::Failed(1, "boom".to_owned())));
        assert!(!client.kill_batch(&[daemon_name(Uuid::from_u128(2))], None));
    }
}
