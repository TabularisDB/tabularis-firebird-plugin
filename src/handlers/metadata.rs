//! Firebird system-catalog discovery. Identifiers used as filters are bound values.

use serde_json::{json, Value};

use crate::{
    client::Client,
    error::PluginError,
    models::{inner_params, ConnectionParams},
    rpc::{error_response, not_implemented, ok_response},
};
use rsfbclient::SqlType;

fn respond(id: Value, result: Result<Value, PluginError>) -> Value {
    match result {
        Ok(value) => ok_response(id, value),
        Err(error) => error_response(id, error.code, &error.message),
    }
}

fn connect(params: &Value) -> Result<Client, PluginError> {
    Client::connect(ConnectionParams::from_value(inner_params(params))?)
}

fn object_name<'a>(params: &'a Value, key: &str) -> Result<&'a str, PluginError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PluginError::invalid_params(format!("{key} must be a non-empty string")))
}

pub fn get_databases(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let config = ConnectionParams::from_value(inner_params(params))?;
            let database = config.database.clone();
            Client::connect(config)?.test_connection()?;
            // Firebird has no server-wide SQL database catalog; expose the configured database.
            Ok(json!([database]))
        })(),
    )
}

pub fn get_schemas(id: Value, _params: &Value) -> Value {
    // Only meaningful if `capabilities.schemas` is true in the `.tabularium` manifest.
    ok_response(id, json!([]))
}

pub fn get_tables(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let rows = connect(params)?.query_objects(
                r#"
            SELECT TRIM(RDB$RELATION_NAME) AS "name", RDB$DESCRIPTION AS "comment"
            FROM RDB$RELATIONS
            WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0 AND RDB$VIEW_BLR IS NULL
            ORDER BY RDB$RELATION_NAME"#,
                vec![],
            )?;
            Ok(Value::Array(
                rows.into_iter()
                    .map(|mut row| {
                        row["schema"] = Value::Null;
                        row
                    })
                    .collect(),
            ))
        })(),
    )
}

pub fn get_columns(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = object_name(params, "table")?;
            let rows =
                connect(params)?.query_objects(COLUMNS_SQL, vec![SqlType::Text(name.into())])?;
            Ok(Value::Array(rows))
        })(),
    )
}

pub fn get_foreign_keys(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = object_name(params, "table")?;
            Ok(Value::Array(connect(params)?.query_objects(
                FOREIGN_KEYS_SQL,
                vec![SqlType::Text(name.into())],
            )?))
        })(),
    )
}

pub fn get_indexes(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = object_name(params, "table")?;
            let rows =
                connect(params)?.query_objects(INDEXES_SQL, vec![SqlType::Text(name.into())])?;
            Ok(Value::Array(group_indexes(rows)))
        })(),
    )
}

fn group_indexes(rows: Vec<Value>) -> Vec<Value> {
    let mut indexes: Vec<Value> = Vec::new();
    for mut row in rows {
        let column = row
            .as_object_mut()
            .unwrap()
            .remove("column_name")
            .unwrap_or(Value::Null);
        if indexes
            .last()
            .is_none_or(|last| last["index_name"] != row["index_name"])
        {
            row["columns"] = json!([]);
            indexes.push(row);
        }
        if !column.is_null() {
            indexes.last_mut().unwrap()["columns"]
                .as_array_mut()
                .unwrap()
                .push(column);
        }
    }
    indexes
}

const COLUMNS_SQL: &str = r#"
SELECT TRIM(rf.RDB$FIELD_NAME) AS "name",
    TRIM(CASE
      WHEN f.RDB$FIELD_TYPE IN (7, 8, 16, 26) AND f.RDB$FIELD_SUB_TYPE IN (1, 2)
      THEN CASE f.RDB$FIELD_SUB_TYPE WHEN 1 THEN 'NUMERIC' ELSE 'DECIMAL' END
        || '(' || CAST(f.RDB$FIELD_PRECISION AS VARCHAR(3)) || ','
        || CAST(-f.RDB$FIELD_SCALE AS VARCHAR(3)) || ')'
      ELSE CASE f.RDB$FIELD_TYPE
        WHEN 7 THEN 'SMALLINT' WHEN 8 THEN 'INTEGER' WHEN 10 THEN 'FLOAT'
        WHEN 12 THEN 'DATE' WHEN 13 THEN 'TIME'
        WHEN 14 THEN 'CHAR(' || CAST(f.RDB$CHARACTER_LENGTH AS VARCHAR(5)) || ')'
        WHEN 16 THEN 'BIGINT' WHEN 23 THEN 'BOOLEAN' WHEN 24 THEN 'DECFLOAT(16)'
        WHEN 25 THEN 'DECFLOAT(34)' WHEN 26 THEN 'INT128' WHEN 27 THEN 'DOUBLE PRECISION'
        WHEN 28 THEN 'TIME WITH TIME ZONE' WHEN 29 THEN 'TIMESTAMP WITH TIME ZONE'
        WHEN 35 THEN 'TIMESTAMP'
        WHEN 37 THEN 'VARCHAR(' || CAST(f.RDB$CHARACTER_LENGTH AS VARCHAR(5)) || ')'
        WHEN 261 THEN 'BLOB SUB_TYPE ' || CAST(COALESCE(f.RDB$FIELD_SUB_TYPE, 0) AS VARCHAR(6))
        ELSE 'UNKNOWN' END
    END || CASE WHEN f.RDB$DIMENSIONS IS NOT NULL THEN ' ARRAY' ELSE '' END) AS "data_type",
    (COALESCE(rf.RDB$NULL_FLAG, 0) = 0 AND COALESCE(f.RDB$NULL_FLAG, 0) = 0) AS "is_nullable",
    COALESCE(rf.RDB$DEFAULT_SOURCE, f.RDB$DEFAULT_SOURCE) AS "default_value",
    EXISTS(SELECT 1 FROM RDB$RELATION_CONSTRAINTS rc
      JOIN RDB$INDEX_SEGMENTS s ON s.RDB$INDEX_NAME = rc.RDB$INDEX_NAME
      WHERE rc.RDB$RELATION_NAME = rf.RDB$RELATION_NAME AND rc.RDB$CONSTRAINT_TYPE = 'PRIMARY KEY'
        AND s.RDB$FIELD_NAME = rf.RDB$FIELD_NAME) AS "is_pk",
    (rf.RDB$IDENTITY_TYPE IS NOT NULL) AS "is_auto_increment",
    rf.RDB$DESCRIPTION AS "comment"
FROM RDB$RELATION_FIELDS rf JOIN RDB$FIELDS f ON f.RDB$FIELD_NAME = rf.RDB$FIELD_SOURCE
WHERE rf.RDB$RELATION_NAME = ? ORDER BY rf.RDB$FIELD_POSITION
"#;

const INDEXES_SQL: &str = r#"
SELECT TRIM(i.RDB$INDEX_NAME) AS "index_name", TRIM(s.RDB$FIELD_NAME) AS "column_name",
    (COALESCE(i.RDB$UNIQUE_FLAG, 0) = 1) AS "is_unique",
    EXISTS(SELECT 1 FROM RDB$RELATION_CONSTRAINTS rc
      WHERE rc.RDB$INDEX_NAME = i.RDB$INDEX_NAME AND rc.RDB$CONSTRAINT_TYPE = 'PRIMARY KEY') AS "is_primary",
    i.RDB$EXPRESSION_SOURCE AS "expression"
FROM RDB$INDICES i LEFT JOIN RDB$INDEX_SEGMENTS s ON s.RDB$INDEX_NAME = i.RDB$INDEX_NAME
WHERE i.RDB$RELATION_NAME = ? ORDER BY i.RDB$INDEX_NAME, s.RDB$FIELD_POSITION
"#;

const FOREIGN_KEYS_SQL: &str = r#"
SELECT TRIM(rc.RDB$CONSTRAINT_NAME) AS "constraint_name", TRIM(s.RDB$FIELD_NAME) AS "column_name",
    TRIM(target.RDB$RELATION_NAME) AS "referenced_table", TRIM(ts.RDB$FIELD_NAME) AS "referenced_column",
    TRIM(ref.RDB$UPDATE_RULE) AS "on_update", TRIM(ref.RDB$DELETE_RULE) AS "on_delete"
FROM RDB$RELATION_CONSTRAINTS rc
JOIN RDB$REF_CONSTRAINTS ref ON ref.RDB$CONSTRAINT_NAME = rc.RDB$CONSTRAINT_NAME
JOIN RDB$RELATION_CONSTRAINTS target ON target.RDB$CONSTRAINT_NAME = ref.RDB$CONST_NAME_UQ
JOIN RDB$INDEX_SEGMENTS s ON s.RDB$INDEX_NAME = rc.RDB$INDEX_NAME
JOIN RDB$INDEX_SEGMENTS ts ON ts.RDB$INDEX_NAME = target.RDB$INDEX_NAME AND ts.RDB$FIELD_POSITION = s.RDB$FIELD_POSITION
WHERE rc.RDB$RELATION_NAME = ? ORDER BY rc.RDB$CONSTRAINT_NAME, s.RDB$FIELD_POSITION
"#;

pub fn get_views(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let rows = connect(params)?.query_objects(
                r#"
            SELECT TRIM(RDB$RELATION_NAME) AS "name", RDB$DESCRIPTION AS "comment"
            FROM RDB$RELATIONS
            WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0 AND RDB$VIEW_BLR IS NOT NULL
            ORDER BY RDB$RELATION_NAME"#,
                vec![],
            )?;
            Ok(Value::Array(
                rows.into_iter()
                    .map(|mut row| {
                        row["schema"] = Value::Null;
                        row
                    })
                    .collect(),
            ))
        })(),
    )
}

pub fn get_view_definition(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = object_name(params, "view")?;
            let rows = connect(params)?.query_objects(
                r#"
            SELECT RDB$VIEW_SOURCE AS "definition" FROM RDB$RELATIONS
            WHERE RDB$RELATION_NAME = ? AND RDB$VIEW_BLR IS NOT NULL"#,
                vec![SqlType::Text(name.into())],
            )?;
            rows.first()
                .and_then(|row| row.get("definition"))
                .filter(|v| v.is_string())
                .cloned()
                .ok_or_else(|| {
                    PluginError::internal("View does not exist or its source is unavailable")
                })
        })(),
    )
}

pub fn get_view_columns(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = object_name(params, "view")?;
            Ok(Value::Array(connect(params)?.query_objects(
                COLUMNS_SQL,
                vec![SqlType::Text(name.into())],
            )?))
        })(),
    )
}

pub fn get_routines(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_routine_parameters(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_routine_definition(id: Value, _params: &Value) -> Value {
    ok_response(id, Value::String(String::new()))
}

pub fn get_schema_snapshot(id: Value, _params: &Value) -> Value {
    // Used for the ER diagram. Return
    // [{ name, columns: [...], foreign_keys: [...] }].
    not_implemented(id, "get_schema_snapshot")
}

pub fn get_all_columns_batch(id: Value, _params: &Value) -> Value {
    not_implemented(id, "get_all_columns_batch")
}

pub fn get_all_foreign_keys_batch(id: Value, _params: &Value) -> Value {
    not_implemented(id, "get_all_foreign_keys_batch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_index_segments_in_catalog_order() {
        let rows = vec![
            json!({"index_name": "COMPOSITE", "column_name": "Z", "is_unique": true, "is_primary": false}),
            json!({"index_name": "COMPOSITE", "column_name": "A", "is_unique": true, "is_primary": false}),
            json!({"index_name": "EXPRESSION", "column_name": null, "expression": "UPPER(NAME)"}),
        ];
        let grouped = group_indexes(rows);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0]["columns"], json!(["Z", "A"]));
        assert_eq!(grouped[1]["columns"], json!([]));
        assert_eq!(grouped[1]["expression"], json!("UPPER(NAME)"));
    }

    #[test]
    fn rejects_missing_table_before_connecting() {
        for value in [json!({}), json!({"table": ""}), json!({"table": 1})] {
            assert_eq!(
                get_columns(json!(7), &value)["error"]["code"],
                json!(-32602)
            );
            assert_eq!(
                get_indexes(json!(7), &value)["error"]["code"],
                json!(-32602)
            );
            assert_eq!(
                get_foreign_keys(json!(7), &value)["error"]["code"],
                json!(-32602)
            );
        }
    }

    #[test]
    fn rejects_missing_view_before_connecting() {
        for value in [json!({}), json!({"view": ""}), json!({"view": 1})] {
            for handler in [get_view_definition, get_view_columns] {
                let response = handler(json!(7), &value);
                assert_eq!(response["id"], json!(7));
                assert_eq!(response["error"]["code"], json!(-32602));
            }
        }
    }

    #[test]
    #[ignore = "requires tests/fixture.sql and FIREBIRD_TEST_CONFIG; run tests/live-firebird.ps1"]
    fn live_catalog_and_rpc() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::env::var("FIREBIRD_TEST_CONFIG").expect("set FIREBIRD_TEST_CONFIG"),
            )
            .unwrap(),
        )
        .unwrap();
        let call = |method: &str, extra: Value| {
            let mut params = extra;
            params["params"] = config.clone();
            let response = crate::rpc::handle_line(
                &json!({"jsonrpc": "2.0", "id": 8, "method": method, "params": params}).to_string(),
            );
            assert_eq!(response["id"], json!(8));
            assert!(response.get("error").is_none(), "{}", response["error"]);
            response["result"].clone()
        };
        assert_eq!(call("test_connection", json!({})), json!({"success": true}));
        assert_eq!(call("ping", json!({})), Value::Null);
        assert_eq!(
            call("get_databases", json!({})),
            json!([config["database"]])
        );
        let tables = call("get_tables", json!({}));
        let tables = tables.as_array().unwrap();
        assert_eq!(tables.len(), 3);
        assert!(tables
            .iter()
            .all(|t| !t["name"].as_str().unwrap().starts_with("RDB$") && t["schema"].is_null()));
        assert_eq!(
            tables.iter().find(|t| t["name"] == "FB_CHILD").unwrap()["comment"],
            json!("Fixture table")
        );
        let columns = call("get_columns", json!({"table": "FB_CHILD"}));
        let columns = columns.as_array().unwrap();
        assert_eq!(columns[0]["name"], json!("ID"));
        assert_eq!(columns[0]["is_pk"], json!(true));
        assert_eq!(columns[0]["is_auto_increment"], json!(true));
        assert_eq!(columns[0]["is_nullable"], json!(false));
        assert_eq!(columns[3]["data_type"], json!("VARCHAR(40)"));
        assert_eq!(columns[3]["is_nullable"], json!(false));
        assert!(columns[3]["default_value"]
            .as_str()
            .unwrap()
            .contains("fallback"));
        assert_eq!(columns[3]["comment"], json!("Label comment"));
        assert_eq!(columns[4]["data_type"], json!("DECIMAL(18,2)"));
        assert_eq!(columns[4]["is_nullable"], json!(true));
        let quoted = call("get_columns", json!({"table": "odd' table"}));
        assert_eq!(quoted[0]["name"], json!("mixedCase"));
        assert_eq!(
            call("get_columns", json!({"table": "' OR 1=1 --"})),
            json!([])
        );
        let indexes = call("get_indexes", json!({"table": "FB_CHILD"}));
        let indexes = indexes.as_array().unwrap();
        assert_eq!(
            indexes
                .iter()
                .find(|i| i["index_name"] == "IDX_FB_CHILD_LABEL")
                .unwrap()["columns"],
            json!(["LABEL", "ID"])
        );
        assert!(indexes
            .iter()
            .any(|i| i["is_primary"] == true && i["is_unique"] == true));
        let fks = call("get_foreign_keys", json!({"table": "FB_CHILD"}));
        assert_eq!(
            fks,
            json!([
                {"constraint_name":"FK_FB_CHILD_PARENT", "column_name":"TENANT_ID", "referenced_table":"FB_PARENT", "referenced_column":"TENANT_ID", "on_update":"CASCADE", "on_delete":"SET NULL"},
                {"constraint_name":"FK_FB_CHILD_PARENT", "column_name":"PARENT_ID", "referenced_table":"FB_PARENT", "referenced_column":"ID", "on_update":"CASCADE", "on_delete":"SET NULL"}
            ])
        );
        let result = call(
            "execute_query",
            json!({"query":"SELECT LABEL, FLAG, NOTES FROM FB_CHILD ORDER BY ID", "page":2, "page_size":1}),
        );
        assert_eq!(result["total_count"], json!(2));
        assert_eq!(result["rows"], json!([["beta", false, null]]));
        let views = call("get_views", json!({}));
        let views = views.as_array().unwrap();
        assert_eq!(views.len(), 2);
        assert!(views
            .iter()
            .any(|v| v["name"] == "FB_CHILD_VIEW" && v["schema"].is_null()));
        assert!(views.iter().any(|v| v["name"] == "odd' view"));
        assert_eq!(
            call("get_view_definition", json!({"view":"FB_CHILD_VIEW"})),
            json!("SELECT ID, LABEL FROM FB_CHILD")
        );
        let view_columns = call("get_view_columns", json!({"view":"FB_CHILD_VIEW"}));
        assert_eq!(view_columns[0]["name"], json!("ID"));
        assert_eq!(view_columns[1]["name"], json!("LABEL"));
        assert_eq!(view_columns[1]["data_type"], json!("VARCHAR(40)"));
        assert_eq!(
            call("get_view_columns", json!({"view":"odd' view"}))[0]["name"],
            json!("mixedCase")
        );
        assert!(call("get_view_definition", json!({"view":"odd' view"}))
            .as_str()
            .unwrap()
            .contains("mixedCase"));
        assert_eq!(
            call("get_view_columns", json!({"view":"' OR 1=1 --"})),
            json!([])
        );
        let unknown_view = get_view_definition(
            json!(3),
            &json!({"params": config.clone(), "view":"' OR 1=1 --"}),
        );
        assert!(unknown_view.get("error").is_some());
        assert_eq!(
            call(
                "execute_query",
                json!({"query":"SELECT LABEL FROM FB_CHILD_VIEW ORDER BY ID"})
            )["rows"],
            json!([["alpha"], ["beta"]])
        );
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        assert!(client
            .query(
                "UPDATE FB_CHILD SET LABEL = 'changed' RETURNING ID",
                0,
                None
            )
            .is_err());
        let unchanged = call(
            "execute_query",
            json!({"query":"SELECT LABEL FROM FB_CHILD ORDER BY ID"}),
        );
        assert_eq!(unchanged["rows"], json!([["alpha"], ["beta"]]));
        let mut invalid = config;
        invalid["password"] = json!("deliberately-invalid-fixture-password");
        assert!(Client::connect(ConnectionParams::from_value(&invalid).unwrap()).is_err());
    }
}
