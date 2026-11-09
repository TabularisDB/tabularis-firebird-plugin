//! Connection checks and read-only query execution.
use crate::{
    client::Client,
    error::PluginError,
    models::{inner_params, ConnectionParams},
    rpc::{error_response, not_implemented, ok_response},
};
use serde_json::{json, Value};
use std::time::Instant;
fn respond(id: Value, result: Result<Value, PluginError>) -> Value {
    match result {
        Ok(value) => ok_response(id, value),
        Err(error) => error_response(id, error.code, &error.message),
    }
}
pub fn test_connection(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            Client::connect(ConnectionParams::from_value(inner_params(params))?)?
                .test_connection()?;
            Ok(json!({"success": true}))
        })(),
    )
}
pub fn ping(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            Client::connect(ConnectionParams::from_value(inner_params(params))?)?
                .test_connection()?;
            Ok(Value::Null)
        })(),
    )
}
pub fn execute_query(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let start = Instant::now();
            let query = params
                .get("query")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| PluginError::invalid_params("query must be a non-empty string"))?;
            let (offset, limit) = pagination(params)?;
            let mut client = Client::connect(ConnectionParams::from_value(inner_params(params))?)?;
            let mut result = client.query(query, offset, limit)?;
            result["execution_time_ms"] = json!(start.elapsed().as_millis() as u64);
            Ok(result)
        })(),
    )
}
fn pagination(params: &Value) -> Result<(u64, Option<u64>), PluginError> {
    let number = |key: &str| -> Result<Option<u64>, PluginError> {
        match params.get(key).filter(|v| !v.is_null()) {
            None => Ok(None),
            Some(value) => value.as_u64().filter(|n| *n > 0).map(Some).ok_or_else(|| {
                PluginError::invalid_params(format!("{key} must be a positive integer"))
            }),
        }
    };
    let page = number("page")?.unwrap_or(1);
    let limit = number("page_size")?;
    if page > 1 && limit.is_none() {
        return Err(PluginError::invalid_params(
            "page_size is required when page is greater than 1",
        ));
    }
    let offset = (page - 1)
        .checked_mul(limit.unwrap_or(0))
        .ok_or_else(|| PluginError::invalid_params("pagination offset is too large"))?;
    Ok((offset, limit))
}
pub fn explain_query(id: Value, _params: &Value) -> Value {
    not_implemented(id, "explain_query")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_pagination_before_connecting() {
        assert_eq!(pagination(&json!({})).unwrap(), (0, None));
        assert_eq!(
            pagination(&json!({"page": 3, "page_size": 25})).unwrap(),
            (50, Some(25))
        );
        for params in [
            json!({"page": 0}),
            json!({"page_size": -1}),
            json!({"page": 2}),
            json!({"page": u64::MAX, "page_size": 2}),
        ] {
            assert!(pagination(&params).is_err());
        }
    }
    #[test]
    fn invalid_requests_fail_and_preserve_rpc_id() {
        let response = test_connection(json!(42), &json!({"params": {}}));
        assert_eq!(response["id"], json!(42));
        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(ping(json!(1), &Value::Null)["error"]["code"], json!(-32602));
        assert_eq!(
            execute_query(json!(2), &json!({"query": " "}))["error"]["code"],
            json!(-32602)
        );
    }
}
