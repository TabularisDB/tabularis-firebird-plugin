//! Firebird single-row SQL inserts, with complete binary/text values.
use super::{
    crud::identifier,
    metadata::{connect, respond},
};
use crate::{error::PluginError, utils::blob};
use rsfbclient_core::SqlType;
use serde_json::{json, Value};

const MAX_STATEMENT_BYTES: usize = 8 * 1024 * 1024;

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

fn byte_expression(bytes: &[u8], binary: bool) -> String {
    let literal = |part: &[u8]| format!("{}x'{}'", if binary { "" } else { "_UTF8 " }, hex(part));
    if bytes.len() <= 16000 {
        return literal(bytes);
    }
    let mut start = 0;
    let mut pieces = Vec::new();
    while start < bytes.len() {
        let mut end = (start + 16000).min(bytes.len());
        // Text chunks must end between UTF-8 code points.
        if !binary {
            while end < bytes.len() && bytes[end] & 0xc0 == 0x80 {
                end -= 1;
            }
        }
        let part = literal(&bytes[start..end]);
        pieces.push(if start == 0 {
            format!(
                "CAST({part} AS BLOB SUB_TYPE {})",
                if binary {
                    "BINARY"
                } else {
                    "TEXT CHARACTER SET UTF8"
                }
            )
        } else {
            part
        });
        start = end;
    }
    pieces.join(" || ")
}

pub(super) fn literal(value: &Value, binary: bool) -> Result<String, PluginError> {
    if binary && !value.is_null() && !value.is_string() {
        return Err(PluginError::invalid_params(
            "Binary export values require complete BLOB strings",
        ));
    }
    match value {
        Value::Null => Ok("NULL".into()),
        Value::Bool(value) => Ok(if *value { "TRUE" } else { "FALSE" }.into()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) if binary => Ok(byte_expression(&blob::decode(value)?, true)),
        Value::String(value) => Ok(byte_expression(value.as_bytes(), false)),
        _ => Err(PluginError::invalid_params(
            "SQL exports require scalar values or complete BLOB strings",
        )),
    }
}

fn perform(params: &Value) -> Result<Value, PluginError> {
    let table = params["table"]
        .as_str()
        .ok_or_else(|| PluginError::invalid_params("table is required"))?;
    let table_sql = identifier(table)?;
    let columns = params["columns"]
        .as_array()
        .filter(|c| !c.is_empty() && c.len() <= 32767)
        .ok_or_else(|| PluginError::invalid_params("columns must be a non-empty array"))?;
    let rows = params["rows"]
        .as_array()
        .filter(|r| r.len() <= 1000)
        .ok_or_else(|| PluginError::invalid_params("rows must contain at most 1000 rows"))?;
    let mut names = Vec::new();
    for column in columns {
        let name = column
            .as_str()
            .ok_or_else(|| PluginError::invalid_params("Column names must be strings"))?;
        identifier(name)?;
        if names.contains(&name) {
            return Err(PluginError::invalid_params("Duplicate export column"));
        }
        names.push(name);
    }
    if rows
        .iter()
        .any(|row| row.as_array().is_none_or(|r| r.len() != columns.len()))
    {
        return Err(PluginError::invalid_params(
            "Each export row must match columns",
        ));
    }
    let mut client = connect(params)?;
    let metadata = client.query_objects("SELECT TRIM(r.RDB$FIELD_NAME) AS \"name\", f.RDB$FIELD_TYPE AS \"kind\", f.RDB$FIELD_SUB_TYPE AS \"subtype\", f.RDB$CHARACTER_SET_ID AS \"charset_id\", f.RDB$DIMENSIONS AS \"dimensions\", (f.RDB$COMPUTED_SOURCE IS NOT NULL) AS \"computed\", r.RDB$IDENTITY_TYPE AS \"identity\" FROM RDB$RELATION_FIELDS r JOIN RDB$FIELDS f ON f.RDB$FIELD_NAME=r.RDB$FIELD_SOURCE WHERE r.RDB$RELATION_NAME=?", vec![SqlType::Text(table.into())])?;
    let mut selected = Vec::new();
    let mut override_identity = false;
    for (index, name) in names.iter().enumerate() {
        let field = metadata
            .iter()
            .find(|field| field["name"].as_str() == Some(name))
            .ok_or_else(|| PluginError::invalid_params(format!("Unknown export column {name}")))?;
        if !field["dimensions"].is_null()
            || (field["kind"] == 261 && !matches!(field["subtype"].as_i64(), None | Some(0 | 1)))
        {
            return Err(PluginError::invalid_params(
                "Arrays and nonstandard BLOB subtypes cannot be exported",
            ));
        }
        if field["computed"] == true {
            continue;
        }
        override_identity |= field["identity"] == 0;
        selected.push((
            index,
            identifier(name)?,
            (field["kind"] == 261 && field["subtype"] != 1)
                || (matches!(field["kind"].as_i64(), Some(14 | 37)) && field["charset_id"] == 1),
        ));
    }
    let mut statements = Vec::new();
    for row in rows {
        let row = row.as_array().unwrap();
        let statement = if selected.is_empty() {
            format!("INSERT INTO {table_sql} DEFAULT VALUES;")
        } else {
            let values = selected
                .iter()
                .map(|(index, _, binary)| literal(&row[*index], *binary))
                .collect::<Result<Vec<_>, _>>()?;
            format!(
                "INSERT INTO {table_sql} ({}){} VALUES ({});",
                selected
                    .iter()
                    .map(|(_, name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if override_identity {
                    " OVERRIDING SYSTEM VALUE"
                } else {
                    ""
                },
                values.join(", ")
            )
        };
        // shortcut: one INSERT per row, reject oversized SQL before publishing a dump; use bound parameters for larger BLOBs.
        if statement.len() > MAX_STATEMENT_BYTES {
            return Err(PluginError::invalid_params("Export row exceeds the 8 MiB SQL statement limit; use a parameterized transfer for larger values"));
        }
        statements.push(statement);
    }
    Ok(json!(statements))
}

pub fn render(id: Value, params: &Value) -> Value {
    respond(id, perform(params))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literals_keep_binary_text_unicode_and_null_distinct() {
        assert_eq!(
            literal(&json!(blob::encode(&[0, 255])), true).unwrap(),
            "x'00ff'"
        );
        assert_eq!(
            literal(&json!("BLOB:"), false).unwrap(),
            "_UTF8 x'424c4f423a'"
        );
        assert_eq!(literal(&Value::Null, false).unwrap(), "NULL");
        assert_eq!(literal(&json!("a\0'"), false).unwrap(), "_UTF8 x'610027'");
        let text = "日".repeat(12000);
        let expression = byte_expression(text.as_bytes(), false);
        assert!(expression.starts_with("CAST(_UTF8 x'"));
        assert!(expression.contains("AS BLOB SUB_TYPE TEXT CHARACTER SET UTF8) || "));
        assert!(literal(&json!([]), false).is_err());
    }

    #[test]
    #[ignore = "mutates disposable fixture; run tests/live-firebird.ps1"]
    fn live_sql_export_roundtrip_keeps_blobs_unicode_identity_and_computed_columns() {
        use crate::{client::Client, models::ConnectionParams};
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let table = format!("FB_EXPORT_{}", std::process::id());
        let sql = identifier(&table).unwrap();
        struct Cleanup(Value, String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Ok(mut client) =
                    Client::connect(ConnectionParams::from_value(&self.0).unwrap())
                {
                    let _ =
                        client.execute(&format!("DROP TABLE {}", self.1), vec![], 0, None, None);
                }
            }
        }
        let _cleanup = Cleanup(config.clone(), sql.clone());
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        client.execute(&format!("CREATE TABLE {sql} (ID BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, BIN BLOB SUB_TYPE BINARY, TXT BLOB SUB_TYPE TEXT CHARACTER SET UTF8, N NUMERIC(18,4), DERIVED COMPUTED BY (ID + 1))"), vec![], 0, None, None).unwrap();
        let bytes = (0..70000).map(|n| (n % 256) as u8).collect::<Vec<_>>();
        let text = format!("BLOB:literal\0'{}", "日本語🎉".repeat(7000));
        client
            .execute(
                &format!("INSERT INTO {sql} (ID,BIN,TXT,N) VALUES (77,?,?,?)"),
                vec![
                    SqlType::Binary(bytes.clone()),
                    SqlType::Text(text.clone()),
                    SqlType::Text("-1234567890123.4567".into()),
                ],
                0,
                None,
                None,
            )
            .unwrap();
        client
            .execute(
                &format!("INSERT INTO {sql} (ID) VALUES (78)"),
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        let original = client
            .query(&format!("SELECT * FROM {sql} ORDER BY ID"), 0, None)
            .unwrap();
        let response = perform(&json!({"params":config,"table":table,"columns":original["columns"],"rows":original["rows"]})).unwrap();
        let statements = response.as_array().unwrap();
        assert_eq!(statements.len(), 2);
        assert!(!statements[0].as_str().unwrap().contains("DERIVED"));
        client
            .execute(&format!("DELETE FROM {sql}"), vec![], 0, None, None)
            .unwrap();
        for statement in statements {
            client
                .execute(statement.as_str().unwrap(), vec![], 0, None, None)
                .unwrap();
        }
        let restored = client
            .query(&format!("SELECT * FROM {sql} ORDER BY ID"), 0, None)
            .unwrap();
        assert_eq!(restored["rows"], original["rows"]);
        assert_eq!(
            blob::decode(restored["rows"][0][1].as_str().unwrap()).unwrap(),
            bytes
        );
        assert_eq!(restored["rows"][0][2], text);
        assert_eq!(restored["rows"][0][3], "-1234567890123.4567");
        drop(client);
        Client::connect(ConnectionParams::from_value(&config).unwrap())
            .unwrap()
            .execute(&format!("DROP TABLE {sql}"), vec![], 0, None, None)
            .unwrap();
    }
}
