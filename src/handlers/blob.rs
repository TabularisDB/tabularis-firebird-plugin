//! Bounded BLOB access by complete primary key, using the host's wire contract.
use super::{
    crud::{identifier, pk_values, primary_filter},
    metadata::{connect, object_name, respond},
};
use crate::{error::PluginError, rpc::ok_response};
use rsfbclient_core::SqlType;
use serde_json::{json, Value};

fn read(params: &Value) -> Result<Vec<u8>, PluginError> {
    let table = object_name(params, "table")?;
    let column = params
        .get("col_name")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::invalid_params("col_name is required"))?;
    let (quoted_table, quoted_column) = (identifier(table)?, identifier(column)?);
    let pk = pk_values(params)?;
    let mut client = connect(params)?;
    let (predicate, values) = primary_filter(&mut client, table, &pk)?;
    let column_type = client.query_objects(
        "SELECT f.RDB$FIELD_TYPE AS \"kind\", COALESCE(f.RDB$FIELD_SUB_TYPE,0) AS \"subtype\" FROM RDB$RELATION_FIELDS r JOIN RDB$FIELDS f ON f.RDB$FIELD_NAME=r.RDB$FIELD_SOURCE WHERE r.RDB$RELATION_NAME=? AND r.RDB$FIELD_NAME=?",
        vec![SqlType::Text(table.into()), SqlType::Text(column.into())])?;
    if column_type.len() != 1
        || column_type[0]["kind"] != 261
        || !matches!(column_type[0]["subtype"].as_i64(), Some(0 | 1))
    {
        return Err(PluginError::invalid_params(
            "Column must be a binary or text BLOB",
        ));
    }
    let binary = column_type[0]["subtype"] == 0;
    let rows = client.query_objects(
        &format!("SELECT {quoted_column} AS \"value\" FROM {quoted_table} WHERE {predicate}"),
        values,
    )?;
    if rows.len() != 1 {
        return Err(PluginError::invalid_params(
            "BLOB target must identify exactly one row",
        ));
    }
    match &rows[0]["value"] {
        Value::String(wire) if binary => crate::utils::blob::decode(wire),
        Value::String(text) => {
            if text.len() > crate::limits::current().blob_bytes {
                return Err(PluginError::invalid_params("Text exceeds max_blob_mib"));
            }
            Ok(text.as_bytes().to_vec())
        }
        Value::Null => Err(PluginError::invalid_params("The selected BLOB is NULL")),
        _ => Err(PluginError::invalid_params(
            "The selected value is not binary or text",
        )),
    }
}

pub fn fetch(id: Value, params: &Value) -> Value {
    respond(
        id,
        read(params).map(|bytes| json!(crate::utils::blob::encode(&bytes))),
    )
}

pub fn save(id: Value, params: &Value) -> Value {
    let result = (|| {
        let path = params
            .get("file_path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| PluginError::invalid_params("file_path is required"))?;
        let bytes = read(params)?;
        crate::control::current().check()?;
        // Refuse overwrite: a failed fetch must never truncate an existing file.
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| PluginError::internal(format!("Cannot create BLOB export: {e}")))?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.flush()) {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(PluginError::internal(format!(
                "Cannot write BLOB export: {error}"
            )));
        }
        Ok(Value::Null)
    })();
    match result {
        Ok(value) => ok_response(id, value),
        Err(error) => crate::rpc::error_response(id, error.code, &error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{client::Client, models::ConnectionParams};

    #[test]
    #[ignore = "mutates disposable fixture; run tests/live-firebird.ps1"]
    fn live_blob_preview_export_and_complete_primary_keys() {
        let config_path = std::env::var("FIREBIRD_TEST_CONFIG").unwrap();
        let config: Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        let table = format!("FB_BLOB_TEST_{}", std::process::id());
        let file = std::path::Path::new(&config_path)
            .parent()
            .unwrap()
            .join("blob-export.bin");
        struct Cleanup(Value, String, std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Ok(mut client) =
                    Client::connect(ConnectionParams::from_value(&self.0).unwrap())
                {
                    let _ =
                        client.execute(&format!("DROP TABLE {}", self.1), vec![], 0, None, None);
                }
                let _ = std::fs::remove_file(&self.2);
            }
        }
        client.execute(&format!("CREATE TABLE {table} (A INTEGER, B INTEGER, BIN BLOB SUB_TYPE BINARY, TXT BLOB SUB_TYPE TEXT CHARACTER SET UTF8, PRIMARY KEY(A,B))"), vec![], 0, None, None).unwrap();
        let _cleanup = Cleanup(config.clone(), table.clone(), file.clone());
        let mut bytes = vec![42; 20_000];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let literal = "BLOB:2:application/octet-stream:AP8= — literal text";
        client
            .execute(
                &format!("INSERT INTO {table} VALUES (1,2,?,?)"),
                vec![
                    SqlType::Binary(bytes.clone()),
                    SqlType::Text(literal.into()),
                ],
                0,
                None,
                None,
            )
            .unwrap();
        client
            .execute(
                &format!("INSERT INTO {table} VALUES (2,2,NULL,NULL)"),
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        let params = json!({"params":config,"table":table,"col_name":"BIN","pk_map":{"A":1,"B":2}});
        let preview = fetch(json!(7), &params);
        assert!(preview.get("error").is_none(), "{preview}");
        assert!(preview["result"]
            .as_str()
            .unwrap()
            .starts_with("BLOB:20000:image/png:"));
        assert_eq!(
            crate::utils::blob::decode(preview["result"].as_str().unwrap()).unwrap(),
            bytes
        );
        let mut text_params = params.clone();
        text_params["col_name"] = json!("TXT");
        let text = fetch(json!(8), &text_params);
        assert_eq!(
            crate::utils::blob::decode(text["result"].as_str().unwrap()).unwrap(),
            literal.as_bytes()
        );
        for change in [
            json!({"pk_map":{"A":1}}),
            json!({"pk_map":{"A":null,"B":2}}),
            json!({"pk_map":{"A":1,"B":2,"TXT":"x"}}),
            json!({"pk_map":{"A":9,"B":9}}),
            json!({"pk_map":{"A":2,"B":2}}),
            json!({"col_name":"A"}),
        ] {
            let mut invalid = params.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .extend(change.as_object().unwrap().clone());
            assert!(
                fetch(json!(1), &invalid).get("error").is_some(),
                "{invalid}"
            );
        }
        let mut export = params.clone();
        export["file_path"] = json!(file);
        assert!(save(json!(9), &export).get("error").is_none());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert!(save(json!(10), &export).get("error").is_some());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        let roundtrip = crate::handlers::crud::sql_parameter(&preview["result"]).unwrap();
        assert!(matches!(roundtrip, SqlType::Binary(value) if value == bytes));
        drop(client);
        Client::connect(ConnectionParams::from_value(&config).unwrap())
            .unwrap()
            .execute(&format!("DROP TABLE {table}"), vec![], 0, None, None)
            .unwrap();
    }
}
