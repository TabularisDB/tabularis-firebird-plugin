use crate::error::PluginError;
use rsfbclient_rust::{WireCrypt, WireOptions};
use serde_json::Value;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Copy)]
pub struct Limits {
    pub wire_options: WireOptions,
    pub query_timeout_secs: u32,
    pub rows: usize,
    pub bytes: usize,
    pub blob_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wire_options: WireOptions {
                mode: WireCrypt::Required,
                allow_arc4: true,
            },
            query_timeout_secs: 0,
            rows: 100_000,
            bytes: 64 * 1024 * 1024,
            blob_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Default)]
struct Settings {
    limits: Limits,
    connection: ConnectionDefaults,
}

#[derive(Clone)]
pub struct ConnectionDefaults {
    pub role: Option<String>,
    pub charset: &'static str,
}

impl Default for ConnectionDefaults {
    fn default() -> Self {
        Self {
            role: None,
            charset: "UTF8",
        }
    }
}

pub fn role(value: &str) -> Result<Option<String>, PluginError> {
    if value.contains('\0') || value.len() > 255 {
        return Err(PluginError::invalid_params(
            "role must contain at most 255 UTF-8 bytes without NUL",
        ));
    }
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

pub fn charset(value: &str) -> Result<rsfbclient_core::Charset, PluginError> {
    let value = match value.trim().to_ascii_uppercase().as_str() {
        "EUCJ_0208" => "EUCJP",
        "BIG_5" => "BIG52003",
        _ => value,
    };
    value
        .parse::<rsfbclient_core::Charset>()
        .map_err(|error| PluginError::invalid_params(error.to_string()))
}

fn configured() -> &'static RwLock<Settings> {
    static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();
    SETTINGS.get_or_init(|| RwLock::new(Settings::default()))
}

pub fn current() -> Limits {
    configured().read().unwrap().limits
}

pub fn connection_defaults() -> ConnectionDefaults {
    configured().read().unwrap().connection.clone()
}

pub fn initialize(params: &Value) -> Result<(), PluginError> {
    *configured().write().unwrap() = parse(params.get("settings").unwrap_or(&Value::Null))?;
    Ok(())
}

fn parse(settings: &Value) -> Result<Settings, PluginError> {
    if !settings.is_null() && !settings.is_object() {
        return Err(PluginError::invalid_params("settings must be an object"));
    }
    let number = |name: &str, default: usize, max: u64| -> Result<usize, PluginError> {
        match settings.get(name).filter(|v| !v.is_null()) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .filter(|n| *n > 0 && *n <= max)
                .map(|n| n as usize)
                .ok_or_else(|| {
                    PluginError::invalid_params(format!(
                        "{name} must be an integer between 1 and {max}"
                    ))
                }),
        }
    };
    let string = |key: &str, default: &str| -> Result<String, PluginError> {
        match settings.get(key).filter(|value| !value.is_null()) {
            None => Ok(default.to_owned()),
            Some(Value::String(value)) => Ok(value.clone()),
            _ => Err(PluginError::invalid_params(format!(
                "{key} must be a string"
            ))),
        }
    };
    Ok(Settings {
        connection: ConnectionDefaults {
            role: role(&string("role", "")?)?,
            charset: charset(&string("charset", "UTF8")?)?.on_firebird,
        },
        limits: Limits {
            wire_options: WireOptions {
                mode: match settings.get("wire_crypt").filter(|v| !v.is_null()) {
                    None => WireCrypt::Required,
                    Some(Value::String(value)) => {
                        value.parse().map_err(|error: rsfbclient_core::FbError| {
                            PluginError::invalid_params(error.to_string())
                        })?
                    }
                    _ => return Err(PluginError::invalid_params("wire_crypt must be a string")),
                },
                allow_arc4: match settings.get("allow_arc4").filter(|v| !v.is_null()) {
                    None => true,
                    Some(Value::Bool(value)) => *value,
                    _ => return Err(PluginError::invalid_params("allow_arc4 must be boolean")),
                },
            },
            query_timeout_secs: match settings.get("query_timeout_secs").filter(|v| !v.is_null()) {
                None => 0,
                Some(value) => value
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| {
                        PluginError::invalid_params(
                            "query_timeout_secs must be a non-negative 32-bit integer",
                        )
                    })?,
            },
            rows: number("max_result_rows", 100_000, 1_000_000)?,
            bytes: number("max_result_mib", 64, 1024)? * 1024 * 1024,
            blob_bytes: number("max_blob_mib", 8, 256)? * 1024 * 1024,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_settings_without_mutating_global_state() {
        assert_eq!(
            parse(&json!({"max_result_rows":2,"max_result_mib":1,"max_blob_mib":3}))
                .unwrap()
                .limits
                .blob_bytes,
            3 * 1024 * 1024
        );
        for invalid in [
            json!([]),
            json!({"max_result_rows":0}),
            json!({"max_result_mib":1025}),
            json!({"max_blob_mib":"8"}),
            json!({"wire_crypt":"optional"}),
            json!({"allow_arc4":"false"}),
            json!({"role": false}),
            json!({"role": "a\0b"}),
            json!({"charset": "NONE"}),
            json!({"charset": 1252}),
        ] {
            assert!(parse(&invalid).is_err());
        }
        assert_eq!(parse(&Value::Null).unwrap().limits.rows, 100_000);
        let connection = parse(&json!({"role":"\"Leser\"","charset":"win-1252"}))
            .unwrap()
            .connection;
        assert_eq!(connection.role.as_deref(), Some("\"Leser\""));
        assert_eq!(connection.charset, "WIN1252");
    }
}
