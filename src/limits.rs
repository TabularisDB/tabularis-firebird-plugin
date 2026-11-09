use crate::error::PluginError;
use serde_json::Value;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Copy)]
pub struct Limits {
    pub query_timeout_secs: u32,
    pub rows: usize,
    pub bytes: usize,
    pub blob_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            query_timeout_secs: 0,
            rows: 100_000,
            bytes: 64 * 1024 * 1024,
            blob_bytes: 8 * 1024 * 1024,
        }
    }
}

fn configured() -> &'static RwLock<Limits> {
    static LIMITS: OnceLock<RwLock<Limits>> = OnceLock::new();
    LIMITS.get_or_init(|| RwLock::new(Limits::default()))
}

pub fn current() -> Limits {
    *configured().read().unwrap()
}

pub fn initialize(params: &Value) -> Result<(), PluginError> {
    *configured().write().unwrap() = parse(params.get("settings").unwrap_or(&Value::Null))?;
    Ok(())
}

fn parse(settings: &Value) -> Result<Limits, PluginError> {
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
    Ok(Limits {
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
                .blob_bytes,
            3 * 1024 * 1024
        );
        for invalid in [
            json!([]),
            json!({"max_result_rows":0}),
            json!({"max_result_mib":1025}),
            json!({"max_blob_mib":"8"}),
        ] {
            assert!(parse(&invalid).is_err());
        }
        assert_eq!(parse(&Value::Null).unwrap().rows, 100_000);
    }
}
