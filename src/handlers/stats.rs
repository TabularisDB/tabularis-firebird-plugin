//! Exact, read-only column statistics; masked parts are never selected.
use super::{
    crud::identifier,
    metadata::{connect, respond},
};
use crate::error::PluginError;
use rsfbclient_core::{
    SqlType, TrDataAccessMode, TrIsolationLevel, TrLockResolution, TrOp, TransactionConfiguration,
};
use serde_json::{json, Value};

fn count(value: &Value) -> Result<u64, PluginError> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| PluginError::internal("Invalid column statistics count"))
}

fn perform(params: &Value) -> Result<Value, PluginError> {
    let request = params
        .get("request")
        .ok_or_else(|| PluginError::invalid_params("request is required"))?;
    let name = |key| {
        request
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::invalid_params(format!("{key} is required")))
    };
    let (table, column) = (name("table")?, name("column")?);
    let (table_sql, column_sql) = (identifier(table)?, identifier(column)?);
    let include = request
        .get("include")
        .ok_or_else(|| PluginError::invalid_params("include is required"))?;
    let part = |key| {
        include
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(|| PluginError::invalid_params(format!("include.{key} must be a boolean")))
    };
    let (distinct, min_max, top) = (part("distinct")?, part("min_max")?, part("top_values")?);
    let limit = request
        .get("top_values_limit")
        .and_then(Value::as_u64)
        .filter(|n| *n <= 100 && (!top || *n > 0))
        .ok_or_else(|| {
            PluginError::invalid_params(
                "top_values_limit must be 1..100 (or 0 when top_values is false)",
            )
        })?;
    let filter = match request.get("filter") {
        None | Some(Value::Null) => "",
        Some(Value::String(s)) if s.len() <= 65536 && !s.contains('\0') => s.trim(),
        _ => {
            return Err(PluginError::invalid_params(
                "filter must be a SQL expression up to 65536 bytes",
            ))
        }
    };
    let predicate = if filter.is_empty() {
        "1=1".to_string()
    } else {
        format!("({filter})")
    };
    let mut client = connect(params)?;
    client.begin(TransactionConfiguration {
        data_access: TrDataAccessMode::ReadOnly,
        isolation: TrIsolationLevel::Concurrency,
        lock_resolution: TrLockResolution::NoWait,
    })?;
    let metadata = client.query_objects(
        "SELECT f.RDB$FIELD_TYPE AS \"kind\", f.RDB$DIMENSIONS AS \"dimensions\" FROM RDB$RELATION_FIELDS r JOIN RDB$FIELDS f ON f.RDB$FIELD_NAME=r.RDB$FIELD_SOURCE WHERE r.RDB$RELATION_NAME=? AND r.RDB$FIELD_NAME=?",
        vec![SqlType::Text(table.into()), SqlType::Text(column.into())])?;
    if metadata.len() != 1 || !metadata[0]["dimensions"].is_null() {
        return Err(PluginError::invalid_params(
            "Statistics require an existing scalar column",
        ));
    }
    if metadata[0]["kind"] == 261 && (distinct || min_max || top) {
        return Err(PluginError::invalid_params(
            "BLOB statistics support row/NULL counts only",
        ));
    }
    let mut projections = vec![
        "COUNT(*) AS \"row_count\"".to_string(),
        format!("COUNT(*) - COUNT({column_sql}) AS \"null_count\""),
    ];
    if distinct {
        projections.push(format!("COUNT(DISTINCT {column_sql}) AS \"distinct\""));
    }
    if min_max {
        projections.push(format!(
            "MIN({column_sql}) AS \"min\", MAX({column_sql}) AS \"max\""
        ));
    }
    let rows = client.query_objects(
        &format!(
            "SELECT {} FROM {table_sql} WHERE {predicate}",
            projections.join(", ")
        ),
        vec![],
    )?;
    let row = rows
        .first()
        .ok_or_else(|| PluginError::internal("Missing column statistics"))?;
    let mut result = json!({"row_count":count(&row["row_count"])?,"null_count":count(&row["null_count"])?,
        "distinct_count":null,"min":null,"max":null,"top_values":null});
    if distinct {
        result["distinct_count"] = json!(count(&row["distinct"])?);
    }
    if min_max {
        result["min"] = row["min"].clone();
        result["max"] = row["max"].clone();
    }
    if top {
        let rows = client.query_objects(&format!("SELECT FIRST {limit} {column_sql} AS \"value\", COUNT(*) AS \"count\" FROM {table_sql} WHERE {predicate} AND {column_sql} IS NOT NULL GROUP BY {column_sql} ORDER BY COUNT(*) DESC, {column_sql}"),vec![])?;
        result["top_values"] = Value::Array(
            rows.into_iter()
                .map(|row| Ok(json!({"value":row["value"],"count":count(&row["count"])?})))
                .collect::<Result<Vec<_>, PluginError>>()?,
        );
    }
    client.finish(TrOp::Rollback)?;
    Ok(result)
}

pub fn get(id: Value, params: &Value) -> Value {
    respond(id, perform(params))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_statistics_requests_fail_without_connecting() {
        for request in [
            json!({}),
            json!({"table":"T","column":"C","include":{}}),
            json!({"table":"T","column":"C","include":{"distinct":false,"min_max":false,"top_values":true},"top_values_limit":101}),
        ] {
            assert_eq!(
                get(json!(1), &json!({"request":request}))["error"]["code"],
                -32602
            );
        }
    }
    #[test]
    #[ignore = "requires disposable fixture; run tests/live-firebird.ps1"]
    fn live_column_statistics_exact_filtered_and_masked() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let mut params = json!({"params":config,"request":{"table":"FB_CHILD","column":"AMOUNT","top_values_limit":5,
            "include":{"distinct":true,"min_max":true,"top_values":true}}});
        let result = get(json!(1), &params);
        assert!(result.get("error").is_none(), "{result}");
        let stats = &result["result"];
        assert_eq!(stats["row_count"], 2);
        assert_eq!(stats["null_count"], 1);
        assert_eq!(stats["distinct_count"], 1);
        assert_eq!(stats["min"], "12.34");
        assert_eq!(stats["top_values"], json!([{"value":"12.34","count":1}]));
        crate::host_contract_tests::validate_response("get_column_stats", stats);
        params["request"]["filter"] = json!("LABEL = 'alpha'");
        assert_eq!(get(json!(1), &params)["result"]["row_count"], 1);
        params["request"]["filter"] = json!("1=0");
        assert_eq!(get(json!(1), &params)["result"]["top_values"], json!([]));
        params["request"]["filter"] = Value::Null;
        params["request"]["column"] = json!("NOTES");
        assert!(get(json!(1), &params).get("error").is_some());
        params["request"]["include"] = json!({"distinct":false,"min_max":false,"top_values":false});
        let masked = get(json!(1), &params);
        assert_eq!(masked["result"]["row_count"], 2);
        assert_eq!(masked["result"]["null_count"], 1);
        for key in ["distinct_count", "min", "max", "top_values"] {
            assert!(masked["result"][key].is_null());
        }
    }
}
