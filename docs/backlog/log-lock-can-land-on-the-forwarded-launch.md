---
worth: yes
where: crates/agterm-app/src/logging.rs:79
added: 2026-10-14
---
# The log's instance lock can land on the launch that is forwarded away

`open_log_file` takes the `flock` on `agterm.log` before `GApplication` decides which process stays, so with two
launches at once the one that is about to be forwarded to the running instance can win the lock (on a log within the
bound, or on the fresh file between a rotating launch's rename and its `try_lock`). The instance that keeps running then
writes unlocked, and a later launch may rotate its live log under it. Related, same root: an unlocked launch can append
just before the lock holder renames the file, so its lines land in `agterm.log.1`. Found in the revmux review of the
rotation inode fix (2026-10-14); both predate it. A fix would have the surviving instance take (or retake) the lock
after `GApplication` registration, or serialize launches on a separate lock file, keeping "a held lock never rotates".
