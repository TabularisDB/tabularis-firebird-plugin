//! Parameterized grid mutations, requiring the complete declared primary key.
use crate::{
    client::Client,
    error::PluginError,
    models::{inner_params, ConnectionParams},
    rpc::{error_response, ok_response},
    utils::identifiers::quote_identifier,
};
use rsfbclient_core::SqlType;
use serde_json::{Map, Value};

pub fn identifier(name: &str) -> Result<String, PluginError> {
    if name.is_empty() || name.contains('\0') || name.trim_end() != name {
        return Err(PluginError::invalid_params(
            "Identifiers must be non-empty without NUL or trailing whitespace",
        ));
    }
    Ok(quote_identifier(name, '"'))
}

pub fn sql_parameter(value: &Value) -> Result<SqlType, PluginError> {
    Ok(match value {
        Value::Null => SqlType::Null,
        Value::Bool(v) => SqlType::Boolean(*v),
        Value::String(v) => SqlType::Text(v.clone()),
        Value::Number(v) if v.is_i64() => SqlType::Integer(v.as_i64().unwrap()),
        Value::Number(v) if v.is_u64() => SqlType::Text(v.to_string()),
        Value::Number(v) => SqlType::Floating(
            v.as_f64()
                .ok_or_else(|| PluginError::invalid_params("Invalid numeric parameter"))?,
        ),
        Value::Array(v) => {
            if v.len() > crate::limits::current().blob_bytes {
                return Err(PluginError::invalid_params(
                    "Binary parameter exceeds max_blob_mib",
                ));
            }
            SqlType::Binary(
                v.iter()
                    .map(|b| {
                        b.as_u64()
                            .and_then(|b| u8::try_from(b).ok())
                            .ok_or_else(|| {
                                PluginError::invalid_params(
                                    "Binary parameters must contain bytes between 0 and 255",
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        _ => {
            return Err(PluginError::invalid_params(
                "Parameters must be scalar values or byte arrays",
            ))
        }
    })
}

fn object<'a>(params: &'a Value, key: &str) -> Result<&'a Map<String, Value>, PluginError> {
    params
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| PluginError::invalid_params(format!("{key} must be an object")))
}

fn pk_values(params: &Value) -> Result<Map<String, Value>, PluginError> {
    let pk = if params.get("pk_map").is_some() {
        object(params, "pk_map")?.clone()
    } else {
        let name = params
            .get("pk_col")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::invalid_params("pk_map is required"))?;
        let value = params
            .get("pk_val")
            .ok_or_else(|| PluginError::invalid_params("pk_val is required"))?;
        Map::from_iter([(name.to_string(), value.clone())])
    };
    if pk.is_empty() || pk.values().any(Value::is_null) {
        return Err(PluginError::invalid_params(
            "A non-empty, non-NULL primary key is required",
        ));
    }
    for name in pk.keys() {
        identifier(name)?;
    }
    Ok(pk)
}

fn perform(params: &Value, operation: &str) -> Result<Value, PluginError> {
    let name = params
        .get("table")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::invalid_params("table is required"))?;
    let table = identifier(name)?;
    let mut values = vec![];
    let mut client;
    let sql = if operation == "insert" {
        let data = object(params, "data")?;
        let columns = data
            .keys()
            .map(|n| identifier(n))
            .collect::<Result<Vec<_>, _>>()?;
        values = data
            .values()
            .map(sql_parameter)
            .collect::<Result<Vec<_>, _>>()?;
        client = Client::connect(ConnectionParams::from_value(inner_params(params))?)?;
        if data.is_empty() {
            format!("INSERT INTO {table} DEFAULT VALUES")
        } else {
            format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                columns.join(", "),
                vec!["?"; values.len()].join(", ")
            )
        }
    } else {
        let pk = pk_values(params)?;
        let assignment = if operation == "update" {
            let column = params
                .get("col_name")
                .and_then(Value::as_str)
                .ok_or_else(|| PluginError::invalid_params("col_name is required"))?;
            let value = params
                .get("new_val")
                .ok_or_else(|| PluginError::invalid_params("new_val is required"))?;
            values.push(sql_parameter(value)?);
            Some(format!("{} = ?", identifier(column)?))
        } else {
            None
        };
        client = Client::connect(ConnectionParams::from_value(inner_params(params))?)?;
        let primary = client.query_objects("SELECT TRIM(s.RDB$FIELD_NAME) AS \"name\" FROM RDB$RELATION_CONSTRAINTS c JOIN RDB$INDEX_SEGMENTS s ON s.RDB$INDEX_NAME = c.RDB$INDEX_NAME WHERE c.RDB$RELATION_NAME = ? AND c.RDB$CONSTRAINT_TYPE = 'PRIMARY KEY' ORDER BY s.RDB$FIELD_POSITION", vec![SqlType::Text(name.into())])?;
        if primary.is_empty()
            || primary.len() != pk.len()
            || primary
                .iter()
                .any(|c| !pk.contains_key(c["name"].as_str().unwrap()))
        {
            return Err(PluginError::invalid_params(
                "pk_map must contain exactly the table's complete primary key",
            ));
        }
        let mut predicates = vec![];
        for (key, value) in &pk {
            predicates.push(format!("{} = ?", identifier(key)?));
            values.push(sql_parameter(value)?);
        }
        let predicate = predicates.join(" AND ");
        match assignment {
            Some(set) => format!("UPDATE {table} SET {set} WHERE {predicate}"),
            None => format!("DELETE FROM {table} WHERE {predicate}"),
        }
    };
    let result = client.execute(&sql, values, 0, None, Some(1))?;
    Ok(result["affected_rows"].clone())
}

fn respond(id: Value, params: &Value, operation: &str) -> Value {
    match perform(params, operation) {
        Ok(result) => ok_response(id, result),
        Err(error) => error_response(id, error.code, &error.message),
    }
}
pub fn insert_record(id: Value, params: &Value) -> Value {
    respond(id, params, "insert")
}
pub fn update_record(id: Value, params: &Value) -> Value {
    respond(id, params, "update")
}
pub fn delete_record(id: Value, params: &Value) -> Value {
    respond(id, params, "delete")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore = "mutates an isolated fixture; run tests/live-firebird.ps1"]
    fn live_parameterized_grid_writes_and_rollback() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let table = format!(
            "FB_W_{:X}_{:X}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u32
        );
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        client.execute(&format!("CREATE TABLE {table} (A INTEGER NOT NULL, B INTEGER NOT NULL, NOTE VARCHAR(100), AMOUNT DECIMAL(18,2), PRIMARY KEY(A,B))"),vec![],0,None,None).unwrap();
        struct Cleanup(Value, String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if self.1.is_empty() {
                    return;
                }
                if let Ok(mut c) = Client::connect(ConnectionParams::from_value(&self.0).unwrap()) {
                    let _ = c.execute(&format!("DROP TABLE {}", self.1), vec![], 0, None, None);
                }
            }
        }
        let mut cleanup = Cleanup(config.clone(), table.clone());
        let call = |operation: &str, mut params: Value| {
            params["params"] = config.clone();
            params["table"] = json!(table);
            perform(&params, operation)
        };
        assert_eq!(
            call(
                "insert",
                json!({"data":{"A":1,"B":1,"NOTE":"O'Reilly","AMOUNT":"90071992547409.93"}})
            )
            .unwrap(),
            1
        );
        assert_eq!(
            call("insert", json!({"data":{"A":1,"B":2,"NOTE":"second"}})).unwrap(),
            1
        );
        assert!(call(
            "update",
            json!({"pk_map":{"A":1},"col_name":"NOTE","new_val":"bad"})
        )
        .is_err());
        assert!(call("delete", json!({"pk_map":{"A":1,"B":1,"NOTE":"O'Reilly"}})).is_err());
        assert_eq!(call("update",json!({"pk_map":{"A":1,"B":1},"col_name":"NOTE","new_val":"'; DELETE FROM anything; --"})).unwrap(),1);
        assert!(call("insert", json!({"data":{"A":1,"B":1}})).is_err());
        assert!(client
            .execute(
                &format!("UPDATE {table} SET NOTE='bad'"),
                vec![],
                0,
                None,
                Some(1)
            )
            .is_err());
        assert_eq!(
            client
                .query(
                    &format!("SELECT NOTE,AMOUNT FROM {table} ORDER BY B"),
                    0,
                    None
                )
                .unwrap()["rows"],
            json!([
                ["'; DELETE FROM anything; --", "90071992547409.93"],
                ["second", null]
            ])
        );
        let returning = client
            .execute(
                &format!("INSERT INTO {table}(A,B,AMOUNT) VALUES (2,1,?) RETURNING A,B,AMOUNT"),
                vec![SqlType::Text("-90071992547409.93".into())],
                0,
                None,
                None,
            )
            .unwrap();
        assert_eq!(returning["rows"], json!([[2, 1, "-90071992547409.93"]]));
        assert_eq!(returning["affected_rows"], 1);
        let empty = client
            .execute(
                &format!("UPDATE {table} SET NOTE='none' WHERE A=-1 RETURNING A,B"),
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        assert_eq!(empty["rows"], json!([]));
        assert_eq!(empty["affected_rows"], 0);
        assert_eq!(call("delete", json!({"pk_map":{"A":1,"B":1}})).unwrap(), 1);
        assert_eq!(call("delete", json!({"pk_map":{"A":1,"B":1}})).unwrap(), 0);
        assert!(client.execute("COMMIT", vec![], 0, None, None).is_err());
        assert!(client
            .execute_batch(
                &[
                    format!("INSERT INTO {table}(A,B) VALUES(9,9)"),
                    format!("INSERT INTO {table}(A,B) VALUES(9,9)")
                ],
                0,
                None
            )
            .is_err());
        assert_eq!(
            client
                .query(&format!("SELECT COUNT(*) FROM {table} WHERE A=9"), 0, None)
                .unwrap()["rows"],
            json!([[0]])
        );
        let batch = client
            .execute_batch(
                &[
                    format!("INSERT INTO {table}(A,B) VALUES(9,9)"),
                    format!("SELECT A,B FROM {table} WHERE A=9"),
                ],
                0,
                None,
            )
            .unwrap();
        assert_eq!(batch[1]["result"]["rows"], json!([[9, 9]]));
        let rpc=crate::rpc::handle_line(&json!({"jsonrpc":"2.0","id":77,"method":"execute_query_batch","params":{"params":config,"queries":[format!("SELECT A,B FROM {table} WHERE A=9")]}}).to_string());
        assert!(rpc.get("error").is_none(), "{rpc}");
        crate::host_contract_tests::validate_response("execute_query_batch", &rpc["result"]);
        drop(client);
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        client
            .execute(&format!("DROP TABLE {table}"), vec![], 0, None, None)
            .unwrap();
        cleanup.1.clear();
    }
    #[test]
    fn rejects_unsafe_mutations_before_connecting() {
        for params in [
            json!({}),
            json!({"table":"T","pk_map":{}}),
            json!({"table":"T","pk_map":{"ID":null}}),
            json!({"table":"T","pk_map":{"ID":1},"col_name":"C"}),
        ] {
            assert_eq!(update_record(json!(1), &params)["error"]["code"], -32602);
        }
        assert!(sql_parameter(&json!({"nested":1})).is_err());
        assert!(sql_parameter(&json!([256])).is_err());
        assert_eq!(identifier("a\"b").unwrap(), "\"a\"\"b\"");
    }
}
