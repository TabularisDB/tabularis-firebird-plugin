//! One read-only Firebird connection per request.
use crate::{error::PluginError, models::ConnectionParams};
use rsfbclient::{Connection, Queryable, Row, RustFbClient, SqlType};
use serde_json::{json, Value};

pub struct Client {
    connection: Connection<RustFbClient>,
}
impl Client {
    pub fn connect(params: ConnectionParams) -> Result<Self, PluginError> {
        let database = params
            .database
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                PluginError::invalid_params("database must be a server-side path or alias")
            })?;
        let username = params
            .username
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| PluginError::invalid_params("username is required"))?;
        let password = params
            .password
            .as_deref()
            .ok_or_else(|| PluginError::invalid_params("password is required"))?;
        if params
            .ssl_mode
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "" | "disable"))
        {
            return Err(PluginError::invalid_params(
                "TLS ssl_mode is not supported by this driver",
            ));
        }
        let connection = rsfbclient::builder_pure_rust()
            .host(
                params
                    .host
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("localhost"),
            )
            .port(params.port.unwrap_or(3050))
            .db_name(database)
            .user(username)
            .pass(password)
            .with_transaction(|tr| tr.read_only().no_wait())
            .connect()
            .map_err(|e| PluginError::internal(format!("Firebird connection failed: {e}")))?;
        Ok(Self { connection })
    }
    pub fn test_connection(&mut self) -> Result<(), PluginError> {
        let row: Option<(i64,)> = self
            .connection
            .query_first("SELECT 1 FROM RDB$DATABASE", ())
            .map_err(|e| PluginError::internal(format!("Firebird connection check failed: {e}")))?;
        if row != Some((1,)) {
            return Err(PluginError::internal(
                "Unexpected Firebird connection check result",
            ));
        }
        Ok(())
    }
    pub fn query(
        &mut self,
        sql: &str,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<Value, PluginError> {
        let cursor = self
            .connection
            .query_iter::<_, Row>(sql, ())
            .map_err(|e| PluginError::internal(format!("Firebird query failed: {e}")))?;
        collect_rows(cursor, offset, limit)
    }
}
fn collect_rows(
    cursor: impl Iterator<Item = Result<Row, rsfbclient::FbError>>,
    offset: u64,
    limit: Option<u64>,
) -> Result<Value, PluginError> {
    let mut columns: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    let mut total_count = 0u64;
    for row in cursor {
        let row = row.map_err(|e| PluginError::internal(format!("Firebird fetch failed: {e}")))?;
        if total_count == 0 {
            columns = row.cols.iter().map(|c| c.name.to_string()).collect();
        }
        if total_count >= offset && limit.is_none_or(|n| total_count - offset < n) {
            rows.push(
                row.cols
                    .into_iter()
                    .map(|c| sql_value(c.value))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        total_count += 1;
    }
    // shortcut: rsfbclient exposes names only on rows; add statement metadata when its API supports it.
    Ok(json!({"columns": columns, "rows": rows, "total_count": total_count}))
}
fn sql_value(value: SqlType) -> Result<Value, PluginError> {
    Ok(match value {
        SqlType::Null => Value::Null,
        SqlType::Text(s) => json!(s),
        SqlType::Boolean(b) => json!(b),
        SqlType::Integer(n) if n.unsigned_abs() <= 9_007_199_254_740_991 => json!(n),
        SqlType::Integer(n) => json!(n.to_string()),
        SqlType::Int128(n) => json!(n.to_string()),
        SqlType::Floating(n) => {
            Value::Number(serde_json::Number::from_f64(n).ok_or_else(|| {
                PluginError::internal("Non-finite Firebird number cannot be represented as JSON")
            })?)
        }
        SqlType::Timestamp(t) => json!(t.to_string()),
        SqlType::Binary(bytes) => json!(bytes),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(n: i64) -> Result<Row, rsfbclient::FbError> {
        Ok(Row {
            cols: vec![rsfbclient::Column::new("N", 0, SqlType::Integer(n))],
        })
    }
    #[test]
    fn paginates_and_counts_without_losing_column_names() {
        assert_eq!(
            collect_rows((1..=5).map(row), 2, Some(2)).unwrap(),
            json!({"columns": ["N"], "rows": [[3], [4]], "total_count": 5})
        );
        let beyond = collect_rows((1..=2).map(row), 5, Some(2)).unwrap();
        assert_eq!(beyond["columns"], json!(["N"]));
        assert_eq!(beyond["rows"], json!([]));
    }
    #[test]
    fn preserves_null_binary_and_large_integers() {
        assert_eq!(sql_value(SqlType::Null).unwrap(), Value::Null);
        assert_eq!(
            sql_value(SqlType::Binary(vec![0, 255])).unwrap(),
            json!([0, 255])
        );
        assert_eq!(
            sql_value(SqlType::Integer(i64::MAX)).unwrap(),
            json!(i64::MAX.to_string())
        );
        assert_eq!(
            sql_value(SqlType::Int128(i128::MAX)).unwrap(),
            json!(i128::MAX.to_string())
        );
        assert!(sql_value(SqlType::Floating(f64::NAN)).is_err());
    }
    #[test]
    fn fetch_errors_do_not_return_partial_results() {
        assert!(collect_rows(
            vec![row(1), Err("fetch failed".into())].into_iter(),
            0,
            None
        )
        .is_err());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    #[ignore = "requires FIREBIRD_TEST_CONFIG pointing to a JSON connection file"]
    fn live_connection_select_and_read_only_transaction() {
        let path = std::env::var("FIREBIRD_TEST_CONFIG").expect("set FIREBIRD_TEST_CONFIG");
        let config: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        client.test_connection().unwrap();
        let result = client
            .query("SELECT 42 AS ANSWER FROM RDB$DATABASE", 0, None)
            .unwrap();
        assert_eq!(
            result,
            json!({"columns": ["ANSWER"], "rows": [[42]], "total_count": 1})
        );
        let read_only = client.query("SELECT MON$READ_ONLY FROM MON$TRANSACTIONS WHERE MON$TRANSACTION_ID = CURRENT_TRANSACTION", 0, None).unwrap();
        assert_eq!(read_only["rows"], json!([[1]]));
        assert!(client
            .query("SELECT * FROM THIS_TABLE_MUST_NOT_EXIST_9182", 0, None)
            .is_err());
    }
}
