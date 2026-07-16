use std::time::Duration;

use zbus::{
    blocking::{connection::Builder as ConnectionBuilder, Connection, Proxy},
    zvariant::{OwnedValue, Value},
};

use crate::{
    core::{ImeGuess, ImeSnapshot},
    platform::ImeStateProvider,
};

const DBUS_TIMEOUT: Duration = Duration::from_millis(150);
const FCITX_SERVICE: &str = "org.fcitx.Fcitx5";
const FCITX_PATH: &str = "/controller";
const FCITX_INTERFACE: &str = "org.fcitx.Fcitx.Controller1";
const DBUS_SERVICE: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";
const IBUS_SERVICE: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const IBUS_INTERFACE: &str = "org.freedesktop.IBus";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImeFramework {
    Fcitx5,
    IBus,
    Unavailable,
}

#[derive(Debug, Clone)]
pub(crate) struct ImeProbe {
    pub(crate) framework: ImeFramework,
    pub(crate) snapshot: ImeSnapshot,
    pub(crate) error: Option<String>,
}

pub(crate) struct LinuxImeStateProvider {
    fcitx_connection: Option<Connection>,
    ibus_connection: Option<Connection>,
    fcitx_connection_error: Option<String>,
    ibus_connection_error: Option<String>,
}

impl LinuxImeStateProvider {
    pub(crate) fn new() -> Self {
        let (fcitx_connection, fcitx_connection_error) = match ConnectionBuilder::session()
            .map(|builder| builder.method_timeout(DBUS_TIMEOUT))
            .and_then(ConnectionBuilder::build)
        {
            Ok(connection) => (Some(connection), None),
            Err(error) => (None, Some(format!("session D-Bus unavailable: {error}"))),
        };
        let (ibus_connection, ibus_connection_error) = match ConnectionBuilder::ibus()
            .map(|builder| builder.method_timeout(DBUS_TIMEOUT))
            .and_then(ConnectionBuilder::build)
        {
            Ok(connection) => (Some(connection), None),
            Err(error) => (None, Some(format!("IBus D-Bus unavailable: {error}"))),
        };

        Self {
            fcitx_connection,
            ibus_connection,
            fcitx_connection_error,
            ibus_connection_error,
        }
    }

    pub(crate) fn probe(&mut self) -> ImeProbe {
        if let Some(connection) = self.fcitx_connection.as_ref() {
            match fcitx_is_present(connection) {
                Ok(true) => {
                    return match fcitx_snapshot(connection) {
                        Ok(snapshot) => ImeProbe {
                            framework: ImeFramework::Fcitx5,
                            snapshot,
                            error: None,
                        },
                        Err(error) => unknown_probe(
                            ImeFramework::Fcitx5,
                            format!("fcitx5 state query failed: {error}"),
                        ),
                    };
                }
                Ok(false) => {}
                Err(error) => {
                    return unknown_probe(
                        ImeFramework::Fcitx5,
                        format!("could not determine whether fcitx5 is present: {error}"),
                    );
                }
            }
        }

        if let Some(connection) = self.ibus_connection.as_ref() {
            return match ibus_snapshot(connection) {
                Ok(snapshot) => ImeProbe {
                    framework: ImeFramework::IBus,
                    snapshot,
                    error: None,
                },
                Err(error) => unknown_probe(
                    ImeFramework::IBus,
                    format!("IBus global engine query failed: {error}"),
                ),
            };
        }

        let error = [
            self.fcitx_connection_error.as_deref(),
            self.ibus_connection_error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ");
        unknown_probe(
            ImeFramework::Unavailable,
            if error.is_empty() {
                "no supported IM framework detected".to_owned()
            } else {
                error
            },
        )
    }
}

impl Default for LinuxImeStateProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ImeStateProvider for LinuxImeStateProvider {
    fn snapshot(&mut self) -> ImeSnapshot {
        let probe = self.probe();
        if let Some(error) = probe.error.as_deref() {
            log::debug!("IME snapshot is Unknown: {error}");
        }
        probe.snapshot
    }
}

fn fcitx_is_present(connection: &Connection) -> zbus::Result<bool> {
    let proxy = Proxy::new(connection, DBUS_SERVICE, DBUS_PATH, DBUS_INTERFACE)?;
    proxy.call("NameHasOwner", &(FCITX_SERVICE,))
}

fn fcitx_snapshot(connection: &Connection) -> zbus::Result<ImeSnapshot> {
    let proxy = Proxy::new(connection, FCITX_SERVICE, FCITX_PATH, FCITX_INTERFACE)?;
    let state: i32 = proxy.call("State", &())?;
    let ime_id = proxy
        .call::<_, _, String>("CurrentInputMethod", &())
        .ok()
        .filter(|name| !name.is_empty());
    Ok(ImeSnapshot {
        active: fcitx_state_to_guess(state),
        ime_id,
    })
}

fn ibus_snapshot(connection: &Connection) -> zbus::Result<ImeSnapshot> {
    let proxy = Proxy::new(connection, IBUS_SERVICE, IBUS_PATH, IBUS_INTERFACE)?;
    let value: OwnedValue = proxy.get_property("GlobalEngine")?;
    let Some(engine_name) = ibus_engine_name_from_value(&value) else {
        return Ok(ImeSnapshot {
            active: ImeGuess::Unknown,
            ime_id: None,
        });
    };
    Ok(ImeSnapshot {
        active: ibus_engine_to_guess(&engine_name),
        ime_id: Some(engine_name),
    })
}

const fn fcitx_state_to_guess(state: i32) -> ImeGuess {
    // fcitx5 Instance::state(): 2=active, 1=inactive, 0=no recent
    // input context. No other value is documented, so it is fail-safe Unknown.
    match state {
        2 => ImeGuess::Yes,
        1 => ImeGuess::No,
        _ => ImeGuess::Unknown,
    }
}

fn ibus_engine_to_guess(engine_name: &str) -> ImeGuess {
    let normalized = engine_name.to_ascii_lowercase();
    if normalized.contains("mozc") {
        ImeGuess::Yes
    } else if normalized.starts_with("xkb:") {
        ImeGuess::No
    } else {
        // The v1 specification only verifies mozc. Treating arbitrary IBus
        // engines as active could inject Enter into an unrelated input mode.
        ImeGuess::Unknown
    }
}

fn ibus_engine_name_from_value(value: &Value<'_>) -> Option<String> {
    let mut value = value;
    while let Value::Value(inner) = value {
        value = inner.as_ref();
    }
    let Value::Structure(structure) = value else {
        return None;
    };
    let fields = structure.fields();
    if value_as_string(fields.first()?)? != "IBusEngineDesc" {
        return None;
    }
    let name = value_as_string(fields.get(2)?)?;
    (!name.is_empty()).then_some(name)
}

fn value_as_string(value: &Value<'_>) -> Option<String> {
    match value {
        Value::Str(value) => Some(value.as_str().to_owned()),
        Value::Value(inner) => value_as_string(inner),
        _ => None,
    }
}

fn unknown_probe(framework: ImeFramework, error: String) -> ImeProbe {
    ImeProbe {
        framework,
        snapshot: ImeSnapshot {
            active: ImeGuess::Unknown,
            ime_id: None,
        },
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zbus::zvariant::Structure;

    use super::*;

    #[test]
    fn fcitx_state_mapping_matches_upstream_source() {
        assert_eq!(fcitx_state_to_guess(2), ImeGuess::Yes);
        assert_eq!(fcitx_state_to_guess(1), ImeGuess::No);
        assert_eq!(fcitx_state_to_guess(0), ImeGuess::Unknown);
        assert_eq!(fcitx_state_to_guess(-1), ImeGuess::Unknown);
        assert_eq!(fcitx_state_to_guess(3), ImeGuess::Unknown);
    }

    #[test]
    fn ibus_engine_classification_is_conservative() {
        assert_eq!(ibus_engine_to_guess("mozc-jp"), ImeGuess::Yes);
        assert_eq!(ibus_engine_to_guess("ibus-mozc"), ImeGuess::Yes);
        assert_eq!(ibus_engine_to_guess("xkb:us::eng"), ImeGuess::No);
        assert_eq!(ibus_engine_to_guess("anthy"), ImeGuess::Unknown);
        assert_eq!(ibus_engine_to_guess(""), ImeGuess::Unknown);
    }

    #[test]
    fn ibus_serialized_engine_desc_name_is_field_two() {
        let attachments = HashMap::<String, OwnedValue>::new();
        let value = Value::Structure(Structure::from(("IBusEngineDesc", attachments, "mozc-jp")));
        assert_eq!(
            ibus_engine_name_from_value(&value),
            Some("mozc-jp".to_owned())
        );
    }

    #[test]
    fn malformed_ibus_engine_desc_fails_safe() {
        let value = Value::Structure(Structure::from(("WrongType", "ignored", "mozc-jp")));
        assert_eq!(ibus_engine_name_from_value(&value), None);
        assert_eq!(ibus_engine_name_from_value(&Value::from("mozc-jp")), None);
    }
}
