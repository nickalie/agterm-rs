//! One running agterm per application id, by `GApplication`'s own D-Bus protocol, so a GPUI launch and a GTK-build
//! launch forward to each other's primary (plan > Solution Overview > Single instance).
//!
//! The primary owns the app id's well-known name on the session bus and serves, at `GApplication`'s object path,
//! `org.gtk.Application` (`Activate` with platform data: the only call a default-flags GTK launch makes; `Open` the
//! same) and `org.gtk.Actions` (`DescribeAll`, which a non-primary `GApplication` calls while it registers and fails
//! without, and `Activate`). A launch that finds the name owned sends `Activate` with its own platform data and exits
//! before GPUI starts and before it reads or writes the model state (`windows.json`, the window files, the settings),
//! so the two never both write them; like the GTK build's, it only appends its lines to the shared `agterm.log`
//! (logging starts first, and rotates the file only under the running instance's lock). As `GApplication` does, a
//! launch with no session bus runs as a primary of its own.
//!
//! The activation token travels in the platform data: GPUI consumes `XDG_ACTIVATION_TOKEN` for its first toplevel
//! only and removes it from the environment when its platform is created, and a plain `GApplication` forwards no token
//! (`GtkApplication` does), so [`Activation::from_env`] reads the token and the startup id first thing in `main`. A
//! primary ends its own X11 startup sequence once its window exists (`activation::launched`), and `main` removes
//! `DESKTOP_STARTUP_ID` from the environment, as GDK does, so no child inherits it.

use std::collections::HashMap;
use std::path::Path;

use agterm_core::paths::Env;
use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::{Flags, Message};
use zbus::zvariant::{OwnedValue, Signature, Value};

/// The application id the Linux fork registered, kept so this build can take over its state and its launches.
pub const DEFAULT_APP_ID: &str = "io.github.melonamin.agterm";
/// The variable that gives a dev or test instance an id of its own (`scripts/run-gpui-dev.sh`, the conformance
/// harness).
pub const APP_ID_ENV: &str = "AGTERM_APP_ID";
/// The xdg-activation token a launcher hands a Wayland launch.
pub const ACTIVATION_TOKEN_ENV: &str = "XDG_ACTIVATION_TOKEN";
/// The startup-notification id a launcher hands an X11 launch (its `_TIME<n>` suffix is the launching event's time).
pub const STARTUP_ID_ENV: &str = "DESKTOP_STARTUP_ID";

/// The platform data keys `GtkApplication` sends and reads.
const TOKEN_KEY: &str = "activation-token";
const STARTUP_ID_KEY: &str = "desktop-startup-id";
const CWD_KEY: &str = "cwd";

/// `AGTERM_APP_ID` when `GApplication` would accept it, else the fork's id.
#[must_use]
pub fn app_id(env: &dyn Env) -> String {
    env.var(APP_ID_ENV)
        .and_then(|id| id.into_string().ok())
        .filter(|id| id_is_valid(id))
        .unwrap_or_else(|| DEFAULT_APP_ID.to_owned())
}

/// `GApplication`'s validity rule (`g_application_id_is_valid`): at most 255 bytes, at least two `.`-separated
/// elements, each non-empty, of `[A-Za-z0-9_-]` and not starting with a digit.
#[must_use]
pub fn id_is_valid(id: &str) -> bool {
    let element_is_valid = |element: &str| {
        element.bytes().next().is_some_and(|first| !first.is_ascii_digit())
            && element
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    };
    id.len() <= 255 && id.split('.').count() >= 2 && id.split('.').all(element_is_valid)
}

/// `GApplication`'s object path for `app_id` (`application_path_from_appid`): `/` + the id, `.` → `/`, `-` → `_`.
#[must_use]
pub fn object_path(app_id: &str) -> String {
    format!("/{}", app_id.replace('.', "/").replace('-', "_"))
}

/// What a launch hands the primary to raise its window with: the Wayland activation token and the X11 startup id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activation {
    pub token: Option<String>,
    pub startup_id: Option<String>,
}

impl Activation {
    /// This launch's token and startup id. Call it before GPUI's platform exists, which removes the token.
    #[must_use]
    pub fn from_env(env: &dyn Env) -> Self {
        let read = |name| {
            env.var(name)
                .and_then(|value| value.into_string().ok())
                .filter(|value| !value.is_empty())
        };
        Self {
            token: read(ACTIVATION_TOKEN_ENV),
            startup_id: read(STARTUP_ID_ENV),
        }
    }

    /// What a primary was handed in `Activate`'s platform data; values of another type count as absent.
    #[must_use]
    pub fn from_platform_data(data: &HashMap<String, OwnedValue>) -> Self {
        let string = |key: &str| {
            data.get(key)
                .and_then(|value| <&str>::try_from(&**value).ok())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self {
            token: string(TOKEN_KEY),
            startup_id: string(STARTUP_ID_KEY),
        }
    }

    /// The platform data a forwarding launch sends: `GApplication`'s `cwd` (a NUL-terminated bytestring) and
    /// `GtkApplication`'s two keys. Unlike `GtkApplication`, which sends one value under both keys, each key carries what
    /// this launch was given.
    #[must_use]
    pub fn platform_data(&self, cwd: Option<&Path>) -> HashMap<&'static str, Value<'static>> {
        let mut data = HashMap::new();
        if let Some(cwd) = cwd {
            let mut bytes = std::os::unix::ffi::OsStrExt::as_bytes(cwd.as_os_str()).to_vec();
            bytes.push(0);
            data.insert(CWD_KEY, Value::from(bytes));
        }
        if let Some(token) = &self.token {
            data.insert(TOKEN_KEY, Value::from(token.clone()));
        }
        if let Some(startup_id) = &self.startup_id {
            data.insert(STARTUP_ID_KEY, Value::from(startup_id.clone()));
        }
        data
    }
}

/// What another launch asked of the primary over the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusRequest {
    /// `org.gtk.Application.Activate` (or `Open`): raise the frontmost window.
    Activate(Activation),
}

/// Where the bus handlers hand requests: drained into the main loop once the app runs.
pub type BusRequests = async_channel::Sender<BusRequest>;

/// `GApplication`'s `org.gtk.Application` on the primary.
struct ApplicationIface {
    requests: BusRequests,
}

#[zbus::interface(name = "org.gtk.Application")]
impl ApplicationIface {
    #[expect(clippy::needless_pass_by_value, reason = "zbus hands a method its arguments owned")]
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        let activation = Activation::from_platform_data(&platform_data);
        let mut keys: Vec<&str> = platform_data.keys().map(String::as_str).collect();
        keys.sort_unstable();
        tracing::info!(
            ?keys,
            token = activation.token.as_deref(),
            startup_id = activation.startup_id.as_deref(),
            "instance: activation requested over the bus"
        );
        if self.requests.try_send(BusRequest::Activate(activation)).is_err() {
            tracing::debug!("instance: activation dropped, the app is gone");
        }
    }

    /// Files are not agterm's: an `Open` raises the window as an `Activate` does.
    #[expect(clippy::needless_pass_by_value, reason = "zbus hands a method its arguments owned")]
    fn open(&self, uris: Vec<String>, hint: String, platform_data: HashMap<String, OwnedValue>) {
        tracing::info!(uris = uris.len(), hint, "instance: open requested over the bus");
        self.activate(platform_data);
    }
}

/// `GApplication`'s `org.gtk.Actions` on the primary: no exported actions yet.
struct ActionsIface;

#[zbus::interface(name = "org.gtk.Actions")]
impl ActionsIface {
    #[expect(clippy::unused_self, reason = "a zbus method takes the interface")]
    fn list(&self) -> Vec<String> {
        Vec::new()
    }

    #[expect(clippy::unused_self, reason = "a zbus method takes the interface")]
    fn describe_all(&self) -> HashMap<String, (bool, Signature, Vec<OwnedValue>)> {
        HashMap::new()
    }

    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_value,
        reason = "a zbus method takes the interface and its arguments owned"
    )]
    fn activate(&self, action_name: String, parameter: Vec<OwnedValue>, platform_data: HashMap<String, OwnedValue>) {
        tracing::info!(
            action_name,
            parameters = parameter.len(),
            platform_data = platform_data.len(),
            "instance: unknown action activated over the bus"
        );
    }
}

/// How this launch ended up registered.
pub enum Registration {
    /// This process owns the app id: it runs the app. The connection must live as long as the process.
    Primary(Connection),
    /// Another process owns the app id: this launch forwards to it and exits.
    Remote(Remote),
    /// No session bus: the app runs without single-instance (`GApplication`'s behavior).
    Unique(zbus::Error),
}

/// The bus registration failed past the connection: the launch cannot tell whether another instance runs.
#[derive(Debug, thiserror::Error)]
#[error("failed to register {app_id} on the session bus: {source}")]
pub struct RegisterError {
    app_id: String,
    #[source]
    source: Box<zbus::Error>,
}

/// Registers `app_id` on the session bus the way `GApplication` does: the interfaces are exported first, so a
/// racing launch never meets the name without them, then the name is requested without queueing.
///
/// # Errors
///
/// The bus refused the name request or the export; a missing bus is [`Registration::Unique`] instead.
pub fn register(app_id: &str, requests: BusRequests) -> Result<Registration, RegisterError> {
    let failed = |source| RegisterError {
        app_id: app_id.to_owned(),
        source: Box::new(source),
    };
    let connection = match connect(app_id, requests) {
        Ok(connection) => connection,
        Err(Connect::NoBus(error)) => return Ok(Registration::Unique(error)),
        Err(Connect::Export(error)) => return Err(failed(error)),
    };
    let remote = |connection| {
        Registration::Remote(Remote {
            connection,
            app_id: app_id.to_owned(),
        })
    };
    match connection.request_name_with_flags(app_id, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Ok(Registration::Primary(connection)),
        // zbus reports the bus's `Exists` reply as this error
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => {
            Ok(remote(connection))
        }
        Err(error) => Err(failed(error)),
    }
}

enum Connect {
    NoBus(zbus::Error),
    Export(zbus::Error),
}

/// A session bus connection serving both interfaces at `app_id`'s object path, no name requested yet. A connection
/// that cannot be made at all means no bus; a failure after it is a registration failure.
fn connect(app_id: &str, requests: BusRequests) -> Result<Connection, Connect> {
    let connection = zbus::blocking::connection::Builder::session()
        .and_then(zbus::blocking::connection::Builder::build)
        .map_err(Connect::NoBus)?;
    let path = object_path(app_id);
    let server = connection.object_server();
    server
        .at(path.as_str(), ApplicationIface { requests })
        .and_then(|_| server.at(path.as_str(), ActionsIface))
        .map_err(Connect::Export)?;
    drop(server);
    Ok(connection)
}

/// The primary another process runs, as a forwarding launch reaches it.
pub struct Remote {
    connection: Connection,
    app_id: String,
}

impl Remote {
    /// Sends `org.gtk.Application.Activate` with this launch's platform data, expecting no reply (`GApplication`'s
    /// remote activation is fire-and-forget: the primary's answer never holds the launch up).
    ///
    /// # Errors
    ///
    /// The message could not be built or written to the bus.
    pub fn activate(&self, activation: &Activation, cwd: Option<&Path>) -> zbus::Result<()> {
        let path = object_path(&self.app_id);
        let message = Message::method_call(path.as_str(), "Activate")?
            .destination(self.app_id.as_str())?
            .interface("org.gtk.Application")?
            .with_flags(Flags::NoReplyExpected)?
            .build(&(activation.platform_data(cwd),))?;
        self.connection.send(&message)
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::paths::MapEnv;

    use super::*;

    #[test]
    fn the_app_id_honours_a_valid_override_only() {
        assert_eq!(app_id(&MapEnv::new()), DEFAULT_APP_ID);
        let env = MapEnv::new().with(APP_ID_ENV, "io.github.melonamin.agterm.dev.i1");
        assert_eq!(app_id(&env), "io.github.melonamin.agterm.dev.i1");
        for invalid in [
            "",
            "agterm",
            "io..agterm",
            "io.github.9agterm",
            ".io.agterm",
            "io.agterm.",
            "io.agt erm",
            "io.agtérm",
        ] {
            assert_eq!(
                app_id(&MapEnv::new().with(APP_ID_ENV, invalid)),
                DEFAULT_APP_ID,
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn validity_follows_gapplication() {
        assert!(id_is_valid("org.example.my-app"));
        assert!(id_is_valid("_a._b"));
        assert!(id_is_valid(&format!("a.{}", "b".repeat(253))));
        assert!(!id_is_valid(&format!("a.{}", "b".repeat(254))));
        assert!(!id_is_valid("a.1b"));
    }

    #[test]
    fn the_object_path_is_gapplications() {
        assert_eq!(object_path("io.github.melonamin.agterm"), "/io/github/melonamin/agterm");
        assert_eq!(object_path("org.example.my-app"), "/org/example/my_app");
    }

    #[test]
    fn a_launch_reads_its_token_and_startup_id_and_skips_empty_ones() {
        let env = MapEnv::new()
            .with(ACTIVATION_TOKEN_ENV, "tok")
            .with(STARTUP_ID_ENV, "id_TIME42");
        assert_eq!(
            Activation::from_env(&env),
            Activation {
                token: Some("tok".to_owned()),
                startup_id: Some("id_TIME42".to_owned()),
            }
        );
        let empty = MapEnv::new().with(ACTIVATION_TOKEN_ENV, "").with(STARTUP_ID_ENV, "");
        assert_eq!(Activation::from_env(&empty), Activation::default());
    }

    #[test]
    fn platform_data_round_trips_and_carries_the_cwd_as_a_bytestring() {
        let activation = Activation {
            token: Some("tok".to_owned()),
            startup_id: Some("sid".to_owned()),
        };
        let data = activation.platform_data(Some(Path::new("/srv")));
        assert_eq!(data[CWD_KEY], Value::from(b"/srv\0".to_vec()));
        let received: HashMap<String, OwnedValue> = data
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.try_into().unwrap()))
            .collect();
        assert_eq!(Activation::from_platform_data(&received), activation);
        assert!(Activation::default().platform_data(None).is_empty());
    }

    #[test]
    fn values_of_another_type_count_as_absent() {
        let mut data: HashMap<String, OwnedValue> = HashMap::new();
        data.insert(TOKEN_KEY.to_owned(), Value::from(7_u32).try_into().unwrap());
        data.insert(STARTUP_ID_KEY.to_owned(), Value::from("").try_into().unwrap());
        assert_eq!(Activation::from_platform_data(&data), Activation::default());
    }
}
