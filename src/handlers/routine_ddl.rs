//! Catalog-backed edit scripts preserve declarations instead of guessing from display types.
use super::{
    crud::identifier,
    metadata::{self, connect, respond, FIELD_TYPE_SQL},
    routines::{kind, Name},
};
use crate::{client::Client, error::PluginError};
use rsfbclient_core::{
    SqlType, TrDataAccessMode, TrIsolationLevel, TrLockResolution, TransactionConfiguration,
};
use serde_json::{json, Value};

pub fn routine_create_template(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let kind = kind(params)?
                .ok_or_else(|| PluginError::invalid_params("routine_type is required"))?;
            Ok(json!(if kind == "FUNCTION" {
                "CREATE FUNCTION \"NEW_FUNCTION\" (\"INPUT_VALUE\" INTEGER)\nRETURNS INTEGER\nAS\nBEGIN\n    RETURN \"INPUT_VALUE\";\nEND"
            } else {
                "CREATE PROCEDURE \"NEW_PROCEDURE\" (\"INPUT_VALUE\" INTEGER)\nRETURNS (\"RESULT_VALUE\" INTEGER)\nAS\nBEGIN\n    \"RESULT_VALUE\" = \"INPUT_VALUE\";\n    SUSPEND;\nEND"
            }))
        })(),
    )
}

pub(super) fn header(client: &mut Client, name: &Name, kind: &str) -> Result<Value, PluginError> {
    let (table, field, source, extra) = if kind == "FUNCTION" {
        (
            "RDB$FUNCTIONS",
            "RDB$FUNCTION_NAME",
            "RDB$FUNCTION_SOURCE",
            r#"(COALESCE(r.RDB$DETERMINISTIC_FLAG,0)=1) AS "deterministic", (COALESCE(r.RDB$LEGACY_FLAG,0)=1) AS "legacy", r.RDB$MODULE_NAME AS "module""#,
        )
    } else {
        (
            "RDB$PROCEDURES",
            "RDB$PROCEDURE_NAME",
            "RDB$PROCEDURE_SOURCE",
            r#"FALSE AS "deterministic", FALSE AS "legacy", CAST(NULL AS VARCHAR(255)) AS "module""#,
        )
    };
    let security_exists = client.query_objects("SELECT COUNT(*) AS N FROM RDB$RELATION_FIELDS WHERE RDB$RELATION_NAME=? AND RDB$FIELD_NAME='RDB$SQL_SECURITY'",vec![SqlType::Text(table.into())])?[0]["N"]==1;
    let security = if security_exists {
        "r.RDB$SQL_SECURITY"
    } else {
        "CAST(NULL AS BOOLEAN)"
    };
    client.query_objects(&format!(r#"SELECT r.{source} AS "source", r.RDB$DESCRIPTION AS "comment", TRIM(r.RDB$ENGINE_NAME) AS "engine", r.RDB$ENTRYPOINT AS "entrypoint", {security} AS "security", {extra} FROM {table} r WHERE r.{field}=? AND r.RDB$PACKAGE_NAME IS NULL AND COALESCE(r.RDB$SYSTEM_FLAG,0)=0"#),vec![SqlType::Text(name.name.clone())])?.into_iter().next().ok_or_else(||PluginError::invalid_params("Routine no longer exists"))
}

pub(super) fn declarations(
    client: &mut Client,
    name: &Name,
    kind: &str,
) -> Result<Vec<Value>, PluginError> {
    let (table, field, parameter, mechanism, mode, order) = if kind == "PROCEDURE" {
        (
            "RDB$PROCEDURE_PARAMETERS",
            "RDB$PROCEDURE_NAME",
            "RDB$PARAMETER_NAME",
            "RDB$PARAMETER_MECHANISM",
            "p.RDB$PARAMETER_TYPE",
            "p.RDB$PARAMETER_TYPE,p.RDB$PARAMETER_NUMBER",
        )
    } else {
        (
            "RDB$FUNCTION_ARGUMENTS",
            "RDB$FUNCTION_NAME",
            "RDB$ARGUMENT_NAME",
            "RDB$ARGUMENT_MECHANISM",
            "CASE WHEN p.RDB$ARGUMENT_POSITION=0 THEN 1 ELSE 0 END",
            "p.RDB$ARGUMENT_POSITION",
        )
    };
    client.query_objects(&format!(r#"SELECT TRIM(p.{parameter}) AS "name", {mode} AS "mode", {FIELD_TYPE_SQL} AS "data_type",
        TRIM(p.RDB$FIELD_SOURCE) AS "domain", COALESCE(p.{mechanism},0) AS "mechanism",
        TRIM(p.RDB$RELATION_NAME) AS "relation", TRIM(p.RDB$FIELD_NAME) AS "column",
        p.RDB$NULL_FLAG AS "not_null", p.RDB$DEFAULT_SOURCE AS "default", (p.RDB$DEFAULT_VALUE IS NOT NULL) AS "has_default",
        f.RDB$DIMENSIONS AS "dimensions", f.RDB$FIELD_TYPE AS "field_type", TRIM(cs.RDB$CHARACTER_SET_NAME) AS "charset",
        TRIM(c.RDB$COLLATION_NAME) AS "collation", p.RDB$COLLATION_ID AS "explicit_collation", p.RDB$DESCRIPTION AS "comment"
        FROM {table} p JOIN RDB$FIELDS f ON f.RDB$FIELD_NAME=p.RDB$FIELD_SOURCE
        LEFT JOIN RDB$CHARACTER_SETS cs ON cs.RDB$CHARACTER_SET_ID=f.RDB$CHARACTER_SET_ID
        LEFT JOIN RDB$COLLATIONS c ON c.RDB$CHARACTER_SET_ID=f.RDB$CHARACTER_SET_ID AND c.RDB$COLLATION_ID=COALESCE(p.RDB$COLLATION_ID,f.RDB$COLLATION_ID,0)
        WHERE p.{field}=? AND p.RDB$PACKAGE_NAME IS NULL ORDER BY {order}"#),vec![SqlType::Text(name.name.clone())])
}

pub(super) fn declaration(row: &Value, named: bool) -> Result<String, PluginError> {
    if !row["dimensions"].is_null() {
        return Err(PluginError::invalid_params(
            "Array parameter declarations cannot be regenerated",
        ));
    }
    let domain = row["domain"]
        .as_str()
        .ok_or_else(|| PluginError::internal("Missing parameter domain"))?;
    let mechanism = row["mechanism"].as_i64().unwrap_or(-1);
    let (mut sql, literal_type) = match mechanism {
        1 => (
            match (row["relation"].as_str(), row["column"].as_str()) {
                (Some(relation), Some(column)) => format!(
                    "TYPE OF COLUMN {}.{}",
                    identifier(relation)?,
                    identifier(column)?
                ),
                (None, None) => format!("TYPE OF {}", identifier(domain)?),
                _ => return Err(PluginError::internal("Incomplete TYPE OF COLUMN metadata")),
            },
            false,
        ),
        0 if !domain.starts_with("RDB$") => (identifier(domain)?, false),
        0 => (
            row["data_type"]
                .as_str()
                .filter(|t| !t.contains("UNKNOWN"))
                .ok_or_else(|| {
                    PluginError::invalid_params("Unknown parameter type cannot be regenerated")
                })?
                .into(),
            true,
        ),
        _ => {
            return Err(PluginError::invalid_params(
                "Unknown parameter declaration mechanism",
            ))
        }
    };
    let text = matches!(row["field_type"].as_i64(), Some(14 | 37 | 261));
    if literal_type && text {
        if let Some(charset) = row["charset"].as_str() {
            sql.push_str(&format!(" CHARACTER SET {}", identifier(charset)?));
        }
    }
    if row["not_null"] == 1 {
        sql.push_str(" NOT NULL");
    }
    if text && (!row["explicit_collation"].is_null() || literal_type) {
        if let Some(collation) = row["collation"].as_str() {
            sql.push_str(&format!(" COLLATE {}", identifier(collation)?));
        } else if !row["explicit_collation"].is_null() {
            return Err(PluginError::internal("Missing parameter collation"));
        }
    }
    if named {
        sql = format!(
            "{} {sql}",
            identifier(
                row["name"]
                    .as_str()
                    .ok_or_else(|| PluginError::internal("Missing parameter name"))?
            )?
        );
    }
    if let Some(default) = row["default"].as_str().filter(|s| !s.trim().is_empty()) {
        if row["mode"] != 0 {
            return Err(PluginError::invalid_params(
                "Unexpected default on an output parameter",
            ));
        }
        sql.push(' ');
        sql.push_str(default);
    } else if row["has_default"] == true {
        return Err(PluginError::invalid_params(
            "Parameter default source is unavailable; refusing to discard it",
        ));
    }
    Ok(sql)
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn body(source: &str) -> String {
    if crate::utils::sql::top_level_words(source)
        .first()
        .is_some_and(|(_, word)| word.eq_ignore_ascii_case("AS"))
    {
        source.into()
    } else {
        format!("AS\n{source}")
    }
}

pub fn get_routine_edit_script(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = Name::from_request(params)?;
            let kind = kind(params)?
                .ok_or_else(|| PluginError::invalid_params("routine_type is required"))?;
            let mut client = connect(params)?;
            // Read the body and declarations from the same catalog snapshot.
            client.begin(TransactionConfiguration {
                data_access: TrDataAccessMode::ReadOnly,
                isolation: TrIsolationLevel::Concurrency,
                lock_resolution: TrLockResolution::NoWait,
            })?;
            metadata::routine(&mut client, &name, Some(kind))?;
            if let Some(package) = &name.package {
                let rows = client.query_objects(r#"SELECT RDB$PACKAGE_BODY_SOURCE AS "source" FROM RDB$PACKAGES WHERE RDB$PACKAGE_NAME=?"#,vec![SqlType::Text(package.clone())])?;
                let source = rows
                    .first()
                    .and_then(|r| r["source"].as_str())
                    .ok_or_else(|| {
                        PluginError::invalid_params("Package body source is unavailable")
                    })?;
                return Ok(json!(format!(
                    "RECREATE PACKAGE BODY {}\n{}",
                    identifier(package)?,
                    body(source)
                )));
            }
            let header = header(&mut client, &name, kind)?;
            if header["legacy"] == true {
                let entry = header["entrypoint"].as_str().ok_or_else(|| {
                    PluginError::invalid_params("Legacy entry point is unavailable")
                })?;
                let module = header["module"].as_str().ok_or_else(|| {
                    PluginError::invalid_params("Legacy module name is unavailable")
                })?;
                return Ok(json!(format!(
                    "ALTER EXTERNAL FUNCTION {}\nENTRY_POINT {}\nMODULE_NAME {}",
                    name.sql(),
                    literal(entry),
                    literal(module)
                )));
            }
            if !header["engine"].is_null() {
                return Err(PluginError::invalid_params("External UDR declarations require engine-specific editing; no PSQL script was generated"));
            }
            let source = header["source"]
                .as_str()
                .ok_or_else(|| PluginError::invalid_params("Routine PSQL source is unavailable"))?;
            let parameters = declarations(&mut client, &name, kind)?;
            let inputs = parameters
                .iter()
                .filter(|r| r["mode"] == 0)
                .map(|r| declaration(r, true))
                .collect::<Result<Vec<_>, _>>()?;
            let outputs = parameters
                .iter()
                .filter(|r| r["mode"] == 1)
                .collect::<Vec<_>>();
            let mut sql = format!("ALTER {kind} {}", name.sql());
            if !inputs.is_empty() {
                sql.push_str(&format!(" (\n    {}\n)", inputs.join(",\n    ")));
            }
            if kind == "FUNCTION" {
                if outputs.len() != 1 {
                    return Err(PluginError::internal(
                        "Function must have one return declaration",
                    ));
                }
                sql.push_str(&format!("\nRETURNS {}", declaration(outputs[0], false)?));
                if header["deterministic"] == true {
                    sql.push_str("\nDETERMINISTIC");
                }
            } else if !outputs.is_empty() {
                sql.push_str(&format!(
                    "\nRETURNS (\n    {}\n)",
                    outputs
                        .into_iter()
                        .map(|r| declaration(r, true))
                        .collect::<Result<Vec<_>, _>>()?
                        .join(",\n    ")
                ));
            }
            if let Some(definer) = header["security"].as_bool() {
                sql.push_str(if definer {
                    "\nSQL SECURITY DEFINER"
                } else {
                    "\nSQL SECURITY INVOKER"
                });
            }
            sql.push('\n');
            sql.push_str(&body(source));
            Ok(json!(sql))
        })(),
    )
}

pub fn drop_routine(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = Name::from_request(params)?;
            let kind = kind(params)?
                .ok_or_else(|| PluginError::invalid_params("routine_type is required"))?;
            if name.package.is_some() {
                return Err(PluginError::invalid_params("Firebird cannot drop one package member; edit the package header and body explicitly"));
            }
            let mut client = connect(params)?;
            metadata::routine(&mut client, &name, Some(kind))?;
            let legacy = kind=="FUNCTION" && client.query_objects(r#"SELECT COALESCE(RDB$LEGACY_FLAG,0) AS "legacy" FROM RDB$FUNCTIONS WHERE RDB$FUNCTION_NAME=? AND RDB$PACKAGE_NAME IS NULL"#,vec![SqlType::Text(name.name.clone())])?[0]["legacy"]==1;
            let keyword = if legacy { "EXTERNAL FUNCTION" } else { kind };
            client.execute(
                &format!("DROP {keyword} {}", name.sql()),
                vec![],
                0,
                None,
                None,
            )?;
            Ok(Value::Null)
        })(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_declaration_mechanisms_and_refuses_missing_information() {
        let row = json!({"name":"P","domain":"USER_DOMAIN","mechanism":1,"not_null":1,"mode":0,"has_default":true,"default":"DEFAULT 'O''Brien'"});
        assert_eq!(
            declaration(&row, true).unwrap(),
            "\"P\" TYPE OF \"USER_DOMAIN\" NOT NULL DEFAULT 'O''Brien'"
        );
        let mut column = row.clone();
        column["relation"] = json!("table\"name");
        column["column"] = json!("C");
        assert!(declaration(&column, true)
            .unwrap()
            .contains("TYPE OF COLUMN \"table\"\"name\".\"C\""));
        for (key, value) in [
            ("dimensions", json!(1)),
            ("mechanism", json!(99)),
            ("default", Value::Null),
        ] {
            let mut invalid = row.clone();
            invalid[key] = value;
            assert!(declaration(&invalid, true).is_err());
        }
        assert_eq!(
            routine_create_template(json!(1), &json!({"routine_type":"UNKNOWN"}))["error"]["code"],
            -32602
        );
    }

    #[test]
    #[ignore = "mutates an isolated fixture; run tests/live-firebird.ps1"]
    fn live_routine_edit_templates_and_drop_preserve_catalog() {
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
            response["result"].clone()
        };
        let execute = |sql: &str| {
            connect(&json!({"params":config}))
                .unwrap_or_else(|error| panic!("{error}\nSQL: {sql}"))
                .execute(sql, vec![], 0, None, None)
                .unwrap_or_else(|error| panic!("{error}\nSQL: {sql}"))
        };
        execute("GRANT EXECUTE ON PACKAGE FB_PACKAGE TO PUBLIC");
        for (name, kind) in [
            ("FB_ADD", "PROCEDURE"),
            ("FB_NO_ARGS", "PROCEDURE"),
            ("FB_TAX", "FUNCTION"),
            ("odd' routine", "FUNCTION"),
            ("FB_SHARED", "PROCEDURE"),
            ("FB_SHARED", "FUNCTION"),
            ("\"FB_PACKAGE\".\"FB_ECHO\"", "PROCEDURE"),
            ("\"odd.package\".\"odd\"\"member\"", "FUNCTION"),
            ("FB_LEGACY_FIXTURE", "FUNCTION"),
        ] {
            let parameters = call(
                "get_routine_parameters",
                json!({"routine_name":name,"routine_type":kind}),
            );
            let sql = call(
                "get_routine_edit_script",
                json!({"routine_name":name,"routine_type":kind}),
            );
            execute(sql.as_str().unwrap());
            assert_eq!(
                call(
                    "get_routine_parameters",
                    json!({"routine_name":name,"routine_type":kind})
                ),
                parameters,
                "{name}"
            );
        }
        assert_eq!(execute("SELECT COUNT(*) FROM RDB$USER_PRIVILEGES WHERE RDB$RELATION_NAME='FB_PACKAGE' AND RDB$USER='PUBLIC' AND RDB$PRIVILEGE='X'")["rows"],json!([[1]]));
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let proc = format!("RM_P_{suffix}");
        let function = format!("RM_F_{suffix}");
        let blobs = format!("RM_B_{suffix}");
        let templates = format!("RM_T_{suffix}");
        let legacy = format!("RM_U_{suffix}");
        struct Cleanup {
            config: Value,
            names: Vec<(String, &'static str)>,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for (name, kind) in &self.names {
                    if let Ok(mut client) = connect(&json!({"params":self.config})) {
                        let _ = client.execute(
                            &format!("DROP {kind} \"{name}\""),
                            vec![],
                            0,
                            None,
                            None,
                        );
                    }
                }
            }
        }
        let _cleanup = Cleanup {
            config: config.clone(),
            names: vec![
                (proc.clone(), "PROCEDURE"),
                (function.clone(), "FUNCTION"),
                (blobs.clone(), "PROCEDURE"),
                (templates.clone(), "PROCEDURE"),
                (templates.clone(), "FUNCTION"),
                (legacy.clone(), "EXTERNAL FUNCTION"),
            ],
        };
        let has_security = execute("SELECT COUNT(*) FROM RDB$RELATION_FIELDS WHERE RDB$RELATION_NAME='RDB$FUNCTIONS' AND RDB$FIELD_NAME='RDB$SQL_SECURITY'")["rows"][0][0]==1;
        execute(&format!("CREATE PROCEDURE {proc} (A FB_LABEL, B TYPE OF FB_LABEL, C TYPE OF COLUMN FB_CHILD.LABEL, D VARCHAR(25) CHARACTER SET UTF8 NOT NULL COLLATE UNICODE_CI DEFAULT 'O''Brien') RETURNS(O FB_LABEL) {} AS BEGIN O=A; SUSPEND; END",if has_security{"SQL SECURITY INVOKER"}else{""}));
        execute(&format!("CREATE FUNCTION {function} (A TYPE OF FB_LABEL, B INTEGER NOT NULL DEFAULT 2) RETURNS VARCHAR(40) CHARACTER SET UTF8 NOT NULL COLLATE UNICODE_CI DETERMINISTIC {} AS BEGIN RETURN A||B; END",if has_security{"SQL SECURITY DEFINER"}else{""}));
        let modern = if has_security {
            ", D DECFLOAT(34), Z TIMESTAMP WITH TIME ZONE, I NUMERIC(38,4)"
        } else {
            ""
        };
        execute(&format!("CREATE PROCEDURE {blobs}(TXT BLOB SUB_TYPE TEXT CHARACTER SET UTF8, BIN BLOB SUB_TYPE BINARY, N NUMERIC(18,0) NOT NULL, FLAG BOOLEAN, T TIME {modern}) RETURNS (RESULT_VALUE BLOB SUB_TYPE TEXT CHARACTER SET UTF8) AS BEGIN RESULT_VALUE=TXT; SUSPEND; END"));
        execute(&format!(
            "COMMENT ON PROCEDURE {proc} IS 'Procedure comment'"
        ));
        execute(&format!("COMMENT ON PARAMETER {proc}.A IS 'Input comment'"));
        execute(&format!(
            "COMMENT ON FUNCTION {function} IS 'Function comment'"
        ));
        execute(&format!(
            "COMMENT ON PARAMETER {function}.A IS 'Function argument comment'"
        ));
        execute(&format!("GRANT EXECUTE ON PROCEDURE {proc} TO PUBLIC"));
        let snapshot = |name: &str, kind: &str| {
            let name = Name {
                package: None,
                name: name.into(),
            };
            let mut client = connect(&json!({"params":config})).unwrap();
            let mut parameters = declarations(&mut client, &name, kind).unwrap();
            for parameter in &mut parameters {
                if parameter["domain"].as_str().unwrap().starts_with("RDB$") {
                    parameter["domain"] = Value::Null;
                }
                parameter
                    .as_object_mut()
                    .unwrap()
                    .remove("explicit_collation");
            }
            (header(&mut client, &name, kind).unwrap(), parameters)
        };
        for (name, kind) in [
            (&proc, "PROCEDURE"),
            (&function, "FUNCTION"),
            (&blobs, "PROCEDURE"),
        ] {
            let before = snapshot(name, kind);
            let sql = call(
                "get_routine_edit_script",
                json!({"routine_name":name,"routine_type":kind}),
            );
            execute(sql.as_str().unwrap());
            assert_eq!(snapshot(name, kind), before, "{sql}");
        }
        assert_eq!(
            execute(&format!("SELECT O FROM {proc}('label','b','c')"))["rows"],
            json!([["label"]])
        );
        assert_eq!(
            execute(&format!("SELECT {function}('a') FROM RDB$DATABASE"))["rows"],
            json!([["a2"]])
        );
        let privilege = execute(&format!("SELECT COUNT(*) FROM RDB$USER_PRIVILEGES WHERE RDB$RELATION_NAME='{proc}' AND RDB$USER='PUBLIC' AND RDB$PRIVILEGE='X'"));
        assert_eq!(privilege["rows"], json!([[1]]));
        for kind in ["FUNCTION", "PROCEDURE"] {
            let script = call("routine_create_template", json!({"routine_type":kind}));
            execute(
                &script
                    .as_str()
                    .unwrap()
                    .replace(&format!("NEW_{kind}"), &templates),
            );
        }
        call(
            "drop_routine",
            json!({"routine_name":templates,"routine_type":"FUNCTION"}),
        );
        execute(&format!("DECLARE EXTERNAL FUNCTION {legacy} CSTRING(40) RETURNS DOUBLE PRECISION ENTRY_POINT 'missing_entry' MODULE_NAME 'missing_module'"));
        call(
            "drop_routine",
            json!({"routine_name":legacy,"routine_type":"FUNCTION"}),
        );
        assert_eq!(
            execute(&format!("SELECT * FROM {templates}(42)"))["rows"],
            json!([[42]])
        );
        call(
            "drop_routine",
            json!({"routine_name":templates,"routine_type":"PROCEDURE"}),
        );
        let denied = drop_routine(
            json!(1),
            &json!({"params":config,"routine_name":"\"FB_PACKAGE\".\"FB_ECHO\"","routine_type":"PROCEDURE"}),
        );
        assert_eq!(denied["error"]["code"], -32602);
    }
}
