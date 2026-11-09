//! Trigger metadata and Firebird-native create/edit/drop scripts.
use super::{
    crud::identifier,
    metadata::{connect, object_name, respond},
};
use crate::error::PluginError;
use rsfbclient_core::SqlType;
use serde_json::{json, Value};

const SQL: &str = r#"SELECT TRIM(RDB$TRIGGER_NAME) AS "name", COALESCE(TRIM(RDB$RELATION_NAME),'') AS "table_name",
    RDB$TRIGGER_TYPE AS "type_code", RDB$TRIGGER_SOURCE AS "definition", RDB$TRIGGER_SEQUENCE AS "position",
    (COALESCE(RDB$TRIGGER_INACTIVE,0)=0) AS "active"
    FROM RDB$TRIGGERS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0"#;

fn event(code: i64) -> (String, String) {
    if (8192..=8196).contains(&code) {
        return (
            "ON".into(),
            [
                "CONNECT",
                "DISCONNECT",
                "TRANSACTION START",
                "TRANSACTION COMMIT",
                "TRANSACTION ROLLBACK",
            ][(code - 8192) as usize]
                .into(),
        );
    }
    if code & 0x4000 != 0 {
        let timing = if code & 1 == 0 { "BEFORE" } else { "AFTER" };
        if code & !1 == 0x7fff_ffff_ffff_dffe_i64 {
            return (timing.into(), "ANY DDL STATEMENT".into());
        }
        let entries = [
            (1, "CREATE TABLE"),
            (2, "ALTER TABLE"),
            (3, "DROP TABLE"),
            (4, "CREATE PROCEDURE"),
            (5, "ALTER PROCEDURE"),
            (6, "DROP PROCEDURE"),
            (7, "CREATE FUNCTION"),
            (8, "ALTER FUNCTION"),
            (9, "DROP FUNCTION"),
            (10, "CREATE TRIGGER"),
            (11, "ALTER TRIGGER"),
            (12, "DROP TRIGGER"),
            (16, "CREATE EXCEPTION"),
            (17, "ALTER EXCEPTION"),
            (18, "DROP EXCEPTION"),
            (19, "CREATE VIEW"),
            (20, "ALTER VIEW"),
            (21, "DROP VIEW"),
            (22, "CREATE DOMAIN"),
            (23, "ALTER DOMAIN"),
            (24, "DROP DOMAIN"),
            (25, "CREATE ROLE"),
            (26, "ALTER ROLE"),
            (27, "DROP ROLE"),
            (28, "CREATE INDEX"),
            (29, "ALTER INDEX"),
            (30, "DROP INDEX"),
            (31, "CREATE SEQUENCE"),
            (32, "ALTER SEQUENCE"),
            (33, "DROP SEQUENCE"),
            (34, "CREATE USER"),
            (35, "ALTER USER"),
            (36, "DROP USER"),
            (37, "CREATE COLLATION"),
            (38, "DROP COLLATION"),
            (39, "ALTER CHARACTER SET"),
            (40, "CREATE PACKAGE"),
            (41, "ALTER PACKAGE"),
            (42, "DROP PACKAGE"),
            (43, "CREATE PACKAGE BODY"),
            (44, "DROP PACKAGE BODY"),
            (45, "CREATE MAPPING"),
            (46, "ALTER MAPPING"),
            (47, "DROP MAPPING"),
        ];
        let mut remaining = code & !0x4001;
        let mut events = vec![];
        for (bit, label) in entries {
            if remaining & (1_i64 << bit) != 0 {
                events.push(label.to_string());
                remaining &= !(1_i64 << bit);
            }
        }
        if remaining != 0 || events.is_empty() {
            events.push(format!("UNKNOWN DDL BITS {remaining:#x}"));
        }
        return (timing.into(), events.join(" OR "));
    }
    if (1..=114).contains(&code) {
        let bits = code + 1;
        let mut events = vec![];
        let mut ended = false;
        for shift in [1, 3, 5] {
            let action = (bits >> shift) & 3;
            if action == 0 {
                ended = true;
            } else if ended {
                return ("UNKNOWN".into(), format!("UNKNOWN TYPE {code}"));
            }
            match action {
                1 => events.push("INSERT"),
                2 => events.push("UPDATE"),
                3 => events.push("DELETE"),
                _ => {}
            }
        }
        let mut unique = events.clone();
        unique.sort_unstable();
        unique.dedup();
        if events.is_empty() || unique.len() != events.len() {
            return ("UNKNOWN".into(), format!("UNKNOWN TYPE {code}"));
        }
        return (
            if bits & 1 == 0 { "BEFORE" } else { "AFTER" }.into(),
            events.join(" OR "),
        );
    }
    ("UNKNOWN".into(), format!("UNKNOWN TYPE {code}"))
}

fn decode(mut row: Value) -> Result<Value, PluginError> {
    let code = row.as_object_mut().unwrap().remove("type_code").unwrap();
    let code = code
        .as_i64()
        .or_else(|| code.as_str()?.parse().ok())
        .ok_or_else(|| PluginError::internal("Invalid trigger type in catalog"))?;
    let (timing, event) = event(code);
    row["timing"] = json!(timing);
    row["event"] = json!(event);
    Ok(row)
}

pub fn get_triggers(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            connect(params)?
                .query_objects(&format!("{SQL} ORDER BY RDB$TRIGGER_NAME"), vec![])?
                .into_iter()
                .map(decode)
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        })(),
    )
}

pub fn get_trigger_definition(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = object_name(params, "trigger")?;
            let rows = connect(params)?.query_objects(
                &format!("{SQL} AND RDB$TRIGGER_NAME = ?"),
                vec![SqlType::Text(name.into())],
            )?;
            let row = decode(
                rows.into_iter()
                    .next()
                    .ok_or_else(|| PluginError::invalid_params("Trigger does not exist"))?,
            )?;
            let source = row["definition"].as_str().ok_or_else(|| {
                PluginError::invalid_params(
                    "Trigger PSQL source is unavailable (for example, an external trigger)",
                )
            })?;
            let timing = row["timing"].as_str().unwrap();
            let event = row["event"].as_str().unwrap();
            if timing == "UNKNOWN" || event.contains("UNKNOWN") {
                return Err(PluginError::invalid_params(
                    "Cannot reconstruct an unknown trigger event",
                ));
            }
            let table = row["table_name"].as_str().unwrap();
            let table = if table.is_empty() {
                String::new()
            } else {
                format!(" FOR {}", identifier(table)?)
            };
            Ok(json!(format!(
                "CREATE OR ALTER TRIGGER {}{} {} {} {} POSITION {}\n{}",
                identifier(name)?,
                table,
                if row["active"] == true {
                    "ACTIVE"
                } else {
                    "INACTIVE"
                },
                timing,
                event,
                row["position"].as_i64().unwrap_or(0),
                source
            )))
        })(),
    )
}

pub fn mutate(id: Value, params: &Value, drop: bool) -> Value {
    respond(
        id,
        (|| {
            let sql = if drop {
                format!(
                    "DROP TRIGGER {}",
                    identifier(object_name(params, "trigger")?)?
                )
            } else {
                let sql = params
                    .get("trigger_sql")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| PluginError::invalid_params("trigger_sql is required"))?;
                let words = crate::utils::sql::top_level_words(sql)
                    .into_iter()
                    .take(4)
                    .map(|(_, w)| w.to_ascii_uppercase())
                    .collect::<Vec<_>>();
                let words = words.iter().map(String::as_str).collect::<Vec<_>>();
                if !matches!(
                    words.as_slice(),
                    ["CREATE" | "ALTER" | "RECREATE", "TRIGGER", ..]
                        | ["CREATE", "OR", "ALTER", "TRIGGER"]
                ) {
                    return Err(PluginError::invalid_params(
                        "trigger_sql must create or alter a trigger",
                    ));
                }
                sql.into()
            };
            connect(params)?.execute(&sql, vec![], 0, None, None)?;
            Ok(Value::Null)
        })(),
    )
}

pub fn get_sequences(id: Value, params: &Value) -> Value {
    // Catalog only: never call GEN_ID/NEXT VALUE FOR while browsing metadata.
    respond(
        id,
        (|| {
            Ok(json!(connect(params)?.query_objects(r#"SELECT TRIM(RDB$GENERATOR_NAME) AS "name", RDB$INITIAL_VALUE AS "initial_value", RDB$GENERATOR_INCREMENT AS "increment", RDB$DESCRIPTION AS "comment" FROM RDB$GENERATORS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$GENERATOR_NAME"#,vec![])?))
        })(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_trigger_permutations_database_and_ddl_events() {
        assert_eq!(event(1), ("BEFORE".into(), "INSERT".into()));
        assert_eq!(
            event(114),
            ("AFTER".into(), "INSERT OR UPDATE OR DELETE".into())
        );
        assert_eq!(event(11), ("BEFORE".into(), "UPDATE OR INSERT".into()));
        assert_eq!(event(8195), ("ON".into(), "TRANSACTION COMMIT".into()));
        assert_eq!(
            event(0x400b),
            ("AFTER".into(), "CREATE TABLE OR DROP TABLE".into())
        );
        assert_eq!(
            event(0x7fff_ffff_ffff_dfff),
            ("AFTER".into(), "ANY DDL STATEMENT".into())
        );
        assert!(event(0x4000 | (1 << 55)).1.contains("UNKNOWN"));
        assert!(event(0).1.contains("UNKNOWN"));
        assert!(event(7).1.contains("UNKNOWN"));
    }

    #[test]
    #[ignore = "mutates an isolated fixture; run tests/live-firebird.ps1"]
    fn live_trigger_roundtrips_and_sequence_metadata() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let call = |method: &str, mut params: Value| {
            params["params"] = config.clone();
            let response = crate::rpc::handle_line(
                &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
            );
            assert!(response.get("error").is_none(), "{method}: {response}");
            crate::host_contract_tests::validate_response(method, &response["result"]);
            response["result"].clone()
        };
        let rows = call("get_triggers", json!({}));
        assert_eq!(rows.as_array().unwrap().len(), 4);
        let table = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "FB_CHILD_TOUCH")
            .unwrap();
        assert_eq!(table["table_name"], "FB_CHILD");
        assert_eq!(table["event"], "INSERT OR UPDATE");
        assert_eq!(table["timing"], "BEFORE");
        assert_eq!(table["position"], 7);
        assert_eq!(table["active"], true);
        let ddl = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "FB_DDL_OBSERVER")
            .unwrap();
        assert_eq!(ddl["event"], "CREATE TABLE OR DROP TABLE");
        assert_eq!(ddl["active"], false);
        let database = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "FB_CONNECT_NOTE")
            .unwrap();
        assert_eq!(database["timing"], "ON");
        assert_eq!(database["event"], "CONNECT");
        assert_eq!(database["table_name"], "");
        for trigger in rows.as_array().unwrap() {
            let sql = call(
                "get_trigger_definition",
                json!({"trigger_name":trigger["name"]}),
            );
            assert!(sql.as_str().unwrap().starts_with("CREATE OR ALTER TRIGGER"));
            call("create_trigger", json!({"trigger_sql":sql}));
        }
        assert_eq!(call("get_triggers", json!({})), rows);
        call(
            "create_trigger",
            json!({"trigger_sql":"CREATE TRIGGER FB_RPC_DROP FOR FB_CHILD INACTIVE AFTER UPDATE AS BEGIN END"}),
        );
        call("drop_trigger", json!({"trigger_name":"FB_RPC_DROP"}));
        let missing = get_trigger_definition(
            json!(1),
            &json!({"params":config,"trigger_name":"' OR 1=1 --"}),
        );
        assert_eq!(missing["error"]["code"], -32602);
        assert_eq!(call("get_triggers", json!({})), rows);
        let mut client = connect(&json!({"params":config})).unwrap();
        let before = client
            .query("SELECT GEN_ID(FB_SEQUENCE,0) FROM RDB$DATABASE", 0, None)
            .unwrap();
        let sequences = call("get_sequences", json!({}));
        let sequence = sequences
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "FB_SEQUENCE")
            .unwrap();
        assert_eq!(sequence["initial_value"], 42);
        assert_eq!(sequence["increment"], 3);
        assert_eq!(sequence["comment"], "Sequence fixture");
        assert_eq!(
            client
                .query("SELECT GEN_ID(FB_SEQUENCE,0) FROM RDB$DATABASE", 0, None)
                .unwrap(),
            before
        );
    }
}
