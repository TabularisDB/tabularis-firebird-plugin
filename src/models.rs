//! Shared request/response shapes.
//!
//! These mirror the `ConnectionParams` struct the host sends. Keep fields
//! optional — different database types leave different fields blank.

use crate::error::PluginError;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ConnectionParams {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ssl_mode: Option<String>,
    pub connect_timeout_secs: u32,
    pub query_timeout_secs: Option<u32>,
}

impl ConnectionParams {
    pub fn from_value(value: &Value) -> Result<Self, PluginError> {
        if !value.is_object() {
            return Err(PluginError::invalid_params(
                "params must contain a connection object",
            ));
        }
        for key in [
            "driver", "host", "database", "username", "password", "ssl_mode",
        ] {
            if value
                .get(key)
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                return Err(PluginError::invalid_params(format!(
                    "{key} must be a string"
                )));
            }
        }
        let obj = value.as_object();
        let get_str = |k: &str| {
            obj.and_then(|o| o.get(k))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let port = obj
            .and_then(|o| o.get("port"))
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok());

        if value.get("port").is_some_and(|v| !v.is_null()) && !port.is_some_and(|p| p > 0) {
            return Err(PluginError::invalid_params(
                "port must be an integer between 1 and 65535",
            ));
        }
        Ok(Self {
            host: get_str("host"),
            port,
            database: get_str("database"),
            username: get_str("username"),
            password: get_str("password"),
            ssl_mode: get_str("ssl_mode"),
            connect_timeout_secs: seconds(value, "connect_timeout_secs", 10)?,
            query_timeout_secs: value
                .get("query_timeout_secs")
                .filter(|v| !v.is_null())
                .map(|_| seconds(value, "query_timeout_secs", 0))
                .transpose()?,
        })
    }
}

fn seconds(value: &Value, key: &str, default: u32) -> Result<u32, PluginError> {
    match value.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                PluginError::invalid_params(format!("{key} must be a non-negative 32-bit integer"))
            }),
    }
}

/// Extract the nested `params` object every RPC method receives.
/// Tabularis wraps connection params in `params.params`.
pub fn inner_params(value: &Value) -> &Value {
    value.get("params").unwrap_or(&Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_invalid_connection_fields() {
        for value in [
            json!(null),
            json!([]),
            json!({"port": 0}),
            json!({"port": 65536}),
            json!({"port": "3050"}),
            json!({"password": 123}),
            json!({"database": false}),
            json!({"connect_timeout_secs": -1}),
            json!({"query_timeout_secs": "1"}),
            json!({"query_timeout_secs": 4294967296u64}),
        ] {
            assert!(ConnectionParams::from_value(&value).is_err());
        }
        assert_eq!(
            ConnectionParams::from_value(&json!({"port": 3050}))
                .unwrap()
                .port,
            Some(3050)
        );
        assert_eq!(
            ConnectionParams::from_value(&json!({"port": null}))
                .unwrap()
                .port,
            None
        );
    }

    #[test]
    fn parses_host_timeouts_and_defaults() {
        let defaults = ConnectionParams::from_value(&json!({})).unwrap();
        assert_eq!(defaults.connect_timeout_secs, 10);
        assert_eq!(defaults.query_timeout_secs, None);
        let explicit =
            ConnectionParams::from_value(&json!({"connect_timeout_secs":0,"query_timeout_secs":2}))
                .unwrap();
        assert_eq!(explicit.connect_timeout_secs, 0);
        assert_eq!(explicit.query_timeout_secs, Some(2));
    }
}
