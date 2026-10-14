//! GApplication's D-Bus side, as much as a default-flags second launch uses: the app id's bus name at GApplication's
//! object path, `org.gtk.Actions.DescribeAll` (a non-primary calls it while registering) and
//! `org.gtk.Application.Activate` (its platform data carries the second launch's `activation-token`).

use std::collections::HashMap;

use futures::channel::mpsc::UnboundedSender;
use zbus::zvariant::{OwnedValue, Signature};

/// What the primary is asked to do.
#[derive(Debug)]
pub enum Request {
    /// `org.gtk.Application.Activate`: the token in its platform data, and the platform data's keys.
    Activate { token: Option<String>, keys: Vec<String> },
    /// `org.gtk.Actions.Activate`: the action's name (the drive script's controls).
    Action(String),
}

struct Application {
    requests: UnboundedSender<Request>,
}

#[zbus::interface(name = "org.gtk.Application")]
impl Application {
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        let token = platform_data
            .get("activation-token")
            .or_else(|| platform_data.get("desktop-startup-id"))
            .and_then(|value| String::try_from(value.try_clone().ok()?).ok());
        let mut keys: Vec<String> = platform_data.keys().cloned().collect();
        keys.sort();
        let _ = self.requests.unbounded_send(Request::Activate { token, keys });
    }

    fn open(&self, _uris: Vec<String>, _hint: String, platform_data: HashMap<String, OwnedValue>) {
        self.activate(platform_data);
    }
}

struct Actions {
    requests: UnboundedSender<Request>,
}

#[zbus::interface(name = "org.gtk.Actions")]
impl Actions {
    fn list(&self) -> Vec<String> {
        vec!["raise-without-token".into(), "quit".into()]
    }

    fn describe_all(&self) -> HashMap<String, (bool, Signature, Vec<OwnedValue>)> {
        HashMap::new()
    }

    fn activate(&self, action_name: String, _parameter: Vec<OwnedValue>, _platform_data: HashMap<String, OwnedValue>) {
        let _ = self.requests.unbounded_send(Request::Action(action_name));
    }
}

/// GApplication's object path for `app_id`: `/` + the id with `.` → `/` (and `-` → `_`).
pub fn object_path(app_id: &str) -> String {
    format!("/{}", app_id.replace('.', "/").replace('-', "_"))
}

/// Owns `app_id` on the session bus and serves the two interfaces; the connection lives as long as it is kept.
pub fn serve(app_id: &str, requests: UnboundedSender<Request>) -> zbus::Result<zbus::blocking::Connection> {
    let path = object_path(app_id);
    zbus::blocking::connection::Builder::session()?
        .name(app_id.to_owned())?
        .serve_at(path.as_str(), Application { requests: requests.clone() })?
        .serve_at(path.as_str(), Actions { requests })?
        .build()
}

#[cfg(test)]
mod tests {
    use super::object_path;

    #[test]
    fn the_object_path_is_gapplications() {
        assert_eq!(object_path("io.github.melonamin.agterm"), "/io/github/melonamin/agterm");
        assert_eq!(object_path("org.example.my-app"), "/org/example/my_app");
    }
}
