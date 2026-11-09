//! Connection checks and read-only query execution.
use crate::{
    client::Client,
    error::PluginError,
    models::{inner_params, ConnectionParams},
    rpc::{error_response, not_implemented, ok_response},
    utils::identifiers::quote_identifier,
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
            let query = crate::utils::sql::firebird_query(query)?;
            let mut result = client.query(&query, offset, limit)?;
            result["affected_rows"] = json!(0);
            result["pagination"] = match limit {
                Some(size) => json!({
                    "page": offset / size + 1, "page_size": size,
                    "total_rows": result["total_count"],
                    "has_more": offset.saturating_add(result["rows"].as_array().unwrap().len() as u64) < result["total_count"].as_u64().unwrap()
                }),
                None => Value::Null,
            };
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
    let limit = number("limit")?.or(number("page_size")?);
    if page > u32::MAX as u64 || limit.is_some_and(|n| n > u32::MAX as u64) {
        return Err(PluginError::invalid_params(
            "page and limit must fit unsigned 32-bit integers",
        ));
    }
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

pub fn get_table_query_template(id: Value, params: &Value) -> Value {
    respond(id, table_query_template(params).map(Value::String))
}

fn table_query_template(params: &Value) -> Result<String, PluginError> {
    let request = params
        .get("request")
        .filter(|v| v.is_object())
        .ok_or_else(|| PluginError::invalid_params("request must be an object"))?;
    let identifier = |value: &Value| -> Result<String, PluginError> {
        let name = value
            .as_str()
            .filter(|s| !s.is_empty() && !s.contains('\0'))
            .ok_or_else(|| {
                PluginError::invalid_params(
                    "table and column names must be non-empty strings without NUL characters",
                )
            })?;
        Ok(quote_identifier(name, '"'))
    };
    let table = identifier(&request["table"])?;
    match request.get("schema") {
        None | Some(Value::Null) => {}
        Some(Value::String(schema)) if schema.is_empty() => {}
        _ => {
            return Err(PluginError::invalid_params(
                "Firebird does not support SQL schemas",
            ))
        }
    }
    let columns = match request.get("columns") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(values)) => values
            .iter()
            .map(identifier)
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(PluginError::invalid_params("columns must be an array")),
    };
    let limit = match request.get("limit") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| {
                    PluginError::invalid_params("limit must be an unsigned 32-bit integer")
                })?,
        ),
    };
    let kind = request
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::invalid_params("kind must be select, update or delete"))?;
    if kind != "select" && limit.is_some() {
        return Err(PluginError::invalid_params(
            "limit is only supported for SELECT templates",
        ));
    }
    match kind {
        "select" => {
            let first = limit.map(|n| format!(" FIRST {n}")).unwrap_or_default();
            let projection = if columns.is_empty() {
                "*".into()
            } else {
                columns.join(", ")
            };
            Ok(format!("SELECT{first} {projection} FROM {table};"))
        }
        "update" => {
            let assignments = if columns.is_empty() {
                "/* choose a column */ \"column_name\" = :value_1".into()
            } else {
                columns
                    .iter()
                    .enumerate()
                    .map(|(i, column)| format!("{column} = :value_{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            Ok(format!("UPDATE {table} SET {assignments} WHERE 1 = 0;"))
        }
        "delete" => Ok(format!("DELETE FROM {table} WHERE 1 = 0;")),
        _ => Err(PluginError::invalid_params(
            "kind must be select, update or delete",
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_firebird_templates_without_connecting() {
        let response = get_table_query_template(
            json!(8),
            &json!({"request":{"table":"order\"items", "kind":"select", "columns":["select", "mixedCase"], "limit":100}}),
        );
        assert_eq!(response["id"], json!(8));
        assert_eq!(
            response["result"],
            json!("SELECT FIRST 100 \"select\", \"mixedCase\" FROM \"order\"\"items\";")
        );
        assert_eq!(
            table_query_template(&json!({"request":{"table":"T", "kind":"select"}})).unwrap(),
            "SELECT * FROM \"T\";"
        );
        assert_eq!(
            table_query_template(&json!({"request":{"table":"T", "kind":"select", "limit":0}}))
                .unwrap(),
            "SELECT FIRST 0 * FROM \"T\";"
        );
        assert_eq!(
            table_query_template(
                &json!({"request":{"table":"T", "kind":"update", "columns":["A", "B"]}})
            )
            .unwrap(),
            "UPDATE \"T\" SET \"A\" = :value_1, \"B\" = :value_2 WHERE 1 = 0;"
        );
        assert!(
            table_query_template(&json!({"request":{"table":"T", "kind":"update"}}))
                .unwrap()
                .contains("WHERE 1 = 0")
        );
        assert_eq!(
            table_query_template(&json!({"request":{"table":"T", "kind":"delete"}})).unwrap(),
            "DELETE FROM \"T\" WHERE 1 = 0;"
        );
    }

    #[test]
    fn rejects_invalid_template_inputs() {
        for request in [
            json!(null),
            json!({}),
            json!({"table":"T", "kind":"drop"}),
            json!({"table":"T", "kind":"select", "columns":[""]}),
            json!({"table":"T", "kind":"select", "columns":"A"}),
            json!({"table":"T", "kind":"select", "schema":"PUBLIC"}),
            json!({"table":"T", "kind":"select", "limit":-1}),
            json!({"table":"T", "kind":"select", "limit":4294967296u64}),
            json!({"table":"T", "kind":"delete", "limit":0}),
        ] {
            assert_eq!(
                get_table_query_template(json!(8), &json!({"request":request}))["error"]["code"],
                json!(-32602)
            );
        }
    }
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
