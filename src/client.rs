//! One read-only Firebird connection per request.
use crate::{error::PluginError, models::ConnectionParams};
use rsfbclient_core::{
    charset, Dialect, FirebirdClientDbOps, FirebirdClientSqlOps, FreeStmtOp, Row, SqlType,
    StmtType, TrDataAccessMode, TrLockResolution, TrOp, TransactionConfiguration,
};
use rsfbclient_rust::{DbHandle, RustFbClient, RustFbClientAttachmentConfig};
use serde_json::{json, Value};

pub struct Client {
    wire: RustFbClient,
    database: DbHandle,
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
        let mut wire = RustFbClient::new(charset::UTF_8);
        let config = RustFbClientAttachmentConfig {
            host: params
                .host
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("localhost")
                .into(),
            port: params.port.unwrap_or(3050),
            db_name: database.into(),
            user: username.into(),
            pass: password.into(),
            role_name: None,
        };
        let database = wire
            .attach_database(&config, Dialect::D3, false)
            .map_err(firebird_error)?;
        Ok(Self { wire, database })
    }

    pub fn test_connection(&mut self) -> Result<(), PluginError> {
        let result = self.query("SELECT 1 FROM RDB$DATABASE", 0, None)?;
        if result["rows"] != json!([[1]]) {
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
        self.query_with_params(sql, vec![], offset, limit)
    }

    fn query_with_params(
        &mut self,
        sql: &str,
        params: Vec<SqlType>,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<Value, PluginError> {
        let mut transaction = self
            .wire
            .begin_transaction(
                &mut self.database,
                TransactionConfiguration {
                    data_access: TrDataAccessMode::ReadOnly,
                    lock_resolution: TrLockResolution::NoWait,
                    ..TransactionConfiguration::default()
                },
            )
            .map_err(firebird_error)?;
        let result = (|| {
            let (kind, mut statement) = self
                .wire
                .prepare_statement(&mut self.database, &mut transaction, Dialect::D3, sql)
                .map_err(firebird_error)?;
            let result = (|| {
                if kind != StmtType::Select {
                    return Err(PluginError::invalid_params(
                        "Only read-only SELECT statements are supported",
                    ));
                }
                let columns = statement.column_names();
                self.wire
                    .execute(&mut self.database, &mut transaction, &mut statement, params)
                    .map_err(firebird_error)?;
                let cursor = std::iter::from_fn(|| {
                    self.wire
                        .fetch(&mut self.database, &mut transaction, &mut statement)
                        .map(|row| row.map(|cols| Row { cols }))
                        .transpose()
                });
                collect_rows(cursor, columns, offset, limit)
            })();
            let closed = self
                .wire
                .free_statement(&mut statement, FreeStmtOp::Drop)
                .map_err(firebird_error);
            result.and_then(|result| closed.map(|_| result))
        })();
        // Release resources on both successful queries and prepare/execute/fetch failures.
        let rolled_back = self
            .wire
            .transaction_operation(&mut transaction, TrOp::Rollback)
            .map_err(firebird_error);
        result.and_then(|result| rolled_back.map(|_| result))
    }

    pub fn query_objects(
        &mut self,
        sql: &str,
        params: Vec<SqlType>,
    ) -> Result<Vec<Value>, PluginError> {
        let result = self.query_with_params(sql, params, 0, None)?;
        let names = result["columns"].as_array().unwrap();
        Ok(result["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                Value::Object(
                    names
                        .iter()
                        .zip(row.as_array().unwrap())
                        .map(|(name, value)| (name.as_str().unwrap().to_string(), value.clone()))
                        .collect(),
                )
            })
            .collect())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.wire.detach_database(&mut self.database);
    }
}

fn firebird_error(error: rsfbclient_core::FbError) -> PluginError {
    PluginError::internal(format!("Firebird: {error}"))
}
fn collect_rows(
    cursor: impl Iterator<Item = Result<Row, rsfbclient_core::FbError>>,
    columns: Vec<String>,
    offset: u64,
    limit: Option<u64>,
) -> Result<Value, PluginError> {
    let mut rows = Vec::new();
    let mut total_count = 0u64;
    for row in cursor {
        let row = row.map_err(|e| PluginError::internal(format!("Firebird fetch failed: {e}")))?;

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
    fn row(n: i64) -> Result<Row, rsfbclient_core::FbError> {
        Ok(Row {
            cols: vec![rsfbclient_core::Column::new("N", 0, SqlType::Integer(n))],
        })
    }
    #[test]
    fn paginates_and_counts_without_losing_column_names() {
        assert_eq!(
            collect_rows((1..=5).map(row), vec!["N".into()], 2, Some(2)).unwrap(),
            json!({"columns": ["N"], "rows": [[3], [4]], "total_count": 5})
        );
        let beyond = collect_rows((1..=2).map(row), vec!["N".into()], 5, Some(2)).unwrap();
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
            vec!["N".into()],
            0,
            None
        )
        .is_err());
    }

    #[test]
    fn empty_cursor_keeps_prepared_headers() {
        assert_eq!(
            collect_rows(std::iter::empty(), vec!["A".into(), "B".into()], 0, None).unwrap(),
            json!({"columns":["A", "B"], "rows":[], "total_count":0})
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    #[ignore = "requires FIREBIRD_TEST_CONFIG; run tests/live-firebird.ps1"]
    fn live_exact_numbers_empty_headers_and_recovery() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        let empty = client
            .query(
                "SELECT 1 AS A, CAST(NULL AS DECIMAL(18,2)) AS B FROM RDB$DATABASE WHERE 1 = 0",
                0,
                None,
            )
            .unwrap();
        assert_eq!(
            empty,
            json!({"columns":["A", "B"], "rows":[], "total_count":0})
        );
        let numbers = client
            .query(
                r#"SELECT
            CAST('90071992547409.93' AS NUMERIC(18,2)) AS N,
            CAST('-90071992547409.93' AS DECIMAL(18,2)) AS D,
            CAST('1234567890123456789012345678901234.5678' AS DECIMAL(38,4)) AS WIDE_VALUE,
            CAST('9007199254740993' AS NUMERIC(18,0)) AS WHOLE_VALUE,
            CAST(1.25 AS DOUBLE PRECISION) AS FLOAT_VALUE,
            CAST(NULL AS DECIMAL(18,2)) AS NULL_VALUE
            FROM RDB$DATABASE"#,
                0,
                None,
            )
            .unwrap();
        assert_eq!(
            numbers["rows"],
            json!([[
                "90071992547409.93",
                "-90071992547409.93",
                "1234567890123456789012345678901234.5678",
                "9007199254740993",
                1.25,
                null
            ]])
        );
        assert!(client
            .query("SELECT 1/0 FROM RDB$DATABASE", 0, None)
            .is_err());
        client.test_connection().unwrap();
        assert!(client
            .query("SELECT * FROM MISSING_TABLE_9182", 0, None)
            .is_err());
        client.test_connection().unwrap();
        assert!(client.query("COMMIT", 0, None).is_err());
        client.test_connection().unwrap();
    }

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
