//! Firebird plans and bounded, read-only SELECT execution measurements.
use crate::{
    client::Client,
    error::PluginError,
    models::{inner_params, ConnectionParams},
    rpc::{error_response, ok_response},
};
use serde_json::{json, Value};
use std::time::Instant;

pub fn explain_query(id: Value, params: &Value) -> Value {
    let result = (|| {
        let analyze = match params.get("analyze") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            _ => return Err(PluginError::invalid_params("analyze must be a boolean")),
        };
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| PluginError::invalid_params("query must be a non-empty string"))?;
        if analyze
            && !crate::utils::sql::top_level_words(query)
                .first()
                .is_some_and(|(_, word)| {
                    word.eq_ignore_ascii_case("SELECT") || word.eq_ignore_ascii_case("WITH")
                })
        {
            return Err(PluginError::invalid_params(
                "Runtime analysis executes SELECT queries only",
            ));
        }
        let connection = ConnectionParams::from_value(inner_params(params))?;
        let sql = crate::utils::sql::firebird_query(&crate::utils::sql::unqualify_console_query(
            query,
            connection.database.as_deref().unwrap_or_default(),
        ))?;
        let mut client = Client::connect(connection)?;
        let started = Instant::now();
        let plan = client.explain(&sql)?;
        let mut result = parsed_plan(query, &plan)?;
        result["planning_time_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        if analyze {
            let started = Instant::now();
            // Retain no rows; the cursor still scans the complete result under
            // the normal cancellation, query deadline and decoded-BLOB limits.
            let execution = client.query(&sql, 0, Some(0))?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            result["has_analyze_data"] = json!(true);
            result["execution_time_ms"] = json!(elapsed);
            result["root"]["actual_rows"] = execution["total_count"].clone();
            result["root"]["actual_time_ms"] = json!(elapsed);
            result["root"]["actual_loops"] = json!(1);
            result["root"]["extra"] = json!({"measurement_source":"client wall clock including network and fetch", "scope":"whole SELECT; per-node engine counters unavailable"});
        }
        Ok(result)
    })();
    match result {
        Ok(result) => ok_response(id, result),
        Err(error) => error_response(id, error.code, &error.message),
    }
}

fn node(id: usize, label: &str, children: Vec<Value>) -> Value {
    json!({"id":id.to_string(),"node_type":label,"relation":null,"startup_cost":null,"total_cost":null,
        "plan_rows":null,"actual_rows":null,"actual_time_ms":null,"actual_loops":null,"buffers_hit":null,
        "buffers_read":null,"filter":null,"index_condition":null,"join_type":null,"hash_condition":null,
        "extra":{},"children":children})
}

fn parsed_plan(query: &str, raw: &str) -> Result<Value, PluginError> {
    let lines = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            (
                line.len() - line.trim_start().len(),
                line.trim().trim_start_matches("->").trim(),
            )
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return Err(PluginError::invalid_params(
            "Firebird returned no execution plan for this statement",
        ));
    }
    fn children(
        lines: &[(usize, &str)],
        position: &mut usize,
        indent: usize,
        depth: usize,
    ) -> Result<Vec<Value>, PluginError> {
        if depth > 64 {
            return Err(PluginError::internal(
                "Firebird execution plan nesting exceeds 64 levels",
            ));
        }
        let mut nodes = vec![];
        while let Some(&(level, label)) = lines.get(*position) {
            if level < indent {
                break;
            }
            let id = *position + 1;
            *position += 1;
            let nested = match lines.get(*position) {
                Some(&(next, _)) if next > level => children(lines, position, next, depth + 1)?,
                _ => vec![],
            };
            nodes.push(node(id, label, nested));
        }
        Ok(nodes)
    }
    let roots = children(&lines, &mut 0, lines[0].0, 0)?;
    Ok(
        json!({"root":node(0,"Firebird execution plan", roots),"planning_time_ms":null,
        "execution_time_ms":null,"original_query":query,"driver":"firebird","has_analyze_data":false,"raw_output":raw}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_firebird_hierarchy_and_raw_output_without_inventing_costs() {
        let raw = "\nSelect Expression\n  -> Nested Loop Join (inner)\n    -> Table A Full Scan\n    -> Table B Access By ID\n      -> Bitmap\n";
        let plan = parsed_plan("SELECT ...", raw).unwrap();
        assert_eq!(
            plan["root"]["children"][0]["children"][0]["children"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            plan["root"]["children"][0]["children"][0]["children"][1]["children"][0]["node_type"],
            "Bitmap"
        );
        assert_eq!(plan["raw_output"], raw);
        assert!(plan["root"]["total_cost"].is_null());
        assert!(parsed_plan("SELECT ...", " \n").is_err());
        assert_eq!(
            explain_query(json!(1), &json!({"query":"DELETE FROM T","analyze":true}))["error"]
                ["code"],
            -32602
        );
    }

    #[test]
    #[ignore = "requires isolated fixture; run tests/live-firebird.ps1"]
    fn live_prepare_only_plans_and_recovery() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        for query in [
            "SELECT * FROM FB_CHILD WHERE LABEL = 'alpha'",
            "DELETE FROM FB_CHILD",
            "UPDATE FB_CHILD SET LABEL = ?",
        ] {
            let plan = client.explain(query).unwrap();
            assert!(plan.contains("FB_CHILD"), "{plan}");
            let parsed = parsed_plan(query, &plan).unwrap();
            assert_eq!(parsed["has_analyze_data"], false);
        }
        // Preparing DELETE/UPDATE must leave every fixture row unchanged.
        assert_eq!(
            client
                .query("SELECT COUNT(*) FROM FB_CHILD", 0, None)
                .unwrap()["rows"],
            json!([[2]])
        );
        assert_eq!(
            client
                .query("SELECT LABEL FROM FB_CHILD ORDER BY ID", 0, None)
                .unwrap()["rows"],
            json!([["alpha"], ["beta"]])
        );
        assert!(client.explain("SELECT * FROM MISSING_PLAN_TABLE").is_err());
        client.test_connection().unwrap();
        let response = explain_query(
            json!(2),
            &json!({"params":config,"query":"SELECT * FROM FB_CHILD","analyze":false}),
        );
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["driver"], "firebird");
        let analyzed = explain_query(
            json!(3),
            &json!({"params":config,"query":"SELECT * FROM FB_CHILD","analyze":true}),
        );
        assert!(analyzed.get("error").is_none(), "{analyzed}");
        assert_eq!(analyzed["result"]["has_analyze_data"], true);
        assert_eq!(analyzed["result"]["root"]["actual_rows"], 2);
        assert!(analyzed["result"]["execution_time_ms"].as_f64().unwrap() >= 0.0);
        assert!(analyzed["result"]["root"]["children"][0]["actual_rows"].is_null());
        assert!(explain_query(
            json!(4),
            &json!({"params":config,"query":"DELETE FROM FB_CHILD RETURNING ID","analyze":true})
        )
        .get("error")
        .is_some());
        assert_eq!(
            client
                .query("SELECT COUNT(*) FROM FB_CHILD", 0, None)
                .unwrap()["rows"],
            json!([[2]])
        );
        assert_eq!(
            client
                .execute(
                    "EXECUTE PROCEDURE FB_ADD(?, ?)",
                    vec![
                        rsfbclient_core::SqlType::Integer(2),
                        rsfbclient_core::SqlType::Integer(3)
                    ],
                    0,
                    None,
                    None
                )
                .unwrap()["rows"],
            json!([[5]])
        );
    }
}
