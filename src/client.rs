//! Dedicated Firebird attachments, bounded cursors and explicit transaction ownership.
use crate::control::{self, Control, Deadline};
use crate::{error::PluginError, models::ConnectionParams};
use rsfbclient_core::{
    charset, Dialect, FirebirdClientDbOps, FirebirdClientSqlOps, FreeStmtOp, Row, SqlType,
    StmtType, TrDataAccessMode, TrIsolationLevel, TrLockResolution, TrOp, TrRecordVersion,
    TransactionConfiguration,
};
use rsfbclient_rust::{DbHandle, RustFbClient, RustFbClientAttachmentConfig, TrHandle};
use serde_json::{json, Value};
use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

pub struct Client {
    wire: RustFbClient,
    database: DbHandle,
    control: Arc<Control>,
    query_timeout_secs: u32,
    batch_transaction: Option<TrHandle>,
    abort_socket: TcpStream,
}
impl Client {
    pub fn connect(params: ConnectionParams) -> Result<Self, PluginError> {
        let control = control::current();
        control.check()?;
        let _deadline = Deadline::connection(&control, params.connect_timeout_secs);
        let result = Self::connect_inner(params, control.clone());
        control.check()?;
        result
    }

    fn connect_inner(params: ConnectionParams, control: Arc<Control>) -> Result<Self, PluginError> {
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
        let socket = connect_socket(
            &config.host,
            config.port,
            params.connect_timeout_secs,
            &control,
        )?;
        control.attach(&socket)?;
        let abort_socket = socket
            .try_clone()
            .map_err(|e| PluginError::internal(e.to_string()))?;
        let mut wire = RustFbClient::from_socket_with_options(
            charset::UTF_8,
            socket,
            &config,
            params.wire_options,
        )
        .map_err(firebird_error)?;
        wire.set_blob_limit(crate::limits::current().blob_bytes);
        let database = wire
            .attach_database(&config, Dialect::D3, false)
            .map_err(firebird_error)?;
        Ok(Self {
            wire,
            database,
            control,
            query_timeout_secs: params
                .query_timeout_secs
                .unwrap_or(crate::limits::current().query_timeout_secs),
            batch_transaction: None,
            abort_socket,
        })
    }

    pub fn bind_request(&mut self, params: &ConnectionParams) -> Result<(), PluginError> {
        self.control = control::current();
        self.control.attach(&self.abort_socket)?;
        self.query_timeout_secs = params
            .query_timeout_secs
            .unwrap_or(crate::limits::current().query_timeout_secs);
        Ok(())
    }

    pub fn park(&self) -> Result<(), PluginError> {
        self.control.detach();
        self.control.check()
    }

    pub fn in_transaction(&self) -> bool {
        self.batch_transaction.is_some()
    }

    pub fn begin(&mut self, config: TransactionConfiguration) -> Result<(), PluginError> {
        if self.in_transaction() {
            return Err(PluginError::invalid_params(
                "A transaction is already open in this session",
            ));
        }
        self.control.check()?;
        let _deadline = Deadline::query(&self.control, self.query_timeout_secs);
        self.batch_transaction = Some(
            self.wire
                .begin_transaction(&mut self.database, config)
                .map_err(firebird_error)?,
        );
        self.control.check()
    }

    pub fn finish(&mut self, operation: TrOp) -> Result<(), PluginError> {
        self.control.check()?;
        let _deadline = Deadline::query(&self.control, self.query_timeout_secs);
        let mut transaction = self
            .batch_transaction
            .take()
            .ok_or_else(|| PluginError::invalid_params("No transaction is open in this session"))?;
        let result = self.wire.transaction_operation(&mut transaction, operation).map_err(|error| {
            if matches!(operation, TrOp::Commit | TrOp::CommitRetaining) {
                PluginError { code: -32002, message: format!("Commit failed; outcome is unknown. Check the data before retrying: {error}") }
            } else { firebird_error(error) }
        });
        if result.is_ok() && matches!(operation, TrOp::CommitRetaining | TrOp::RollbackRetaining) {
            self.batch_transaction = Some(transaction);
        }
        // On any error the caller discards this attachment; never replay a commit.
        result
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

    pub fn explain(&mut self, sql: &str) -> Result<String, PluginError> {
        self.control.check()?;
        let _deadline = Deadline::query(&self.control, self.query_timeout_secs);
        let mut transaction = self
            .wire
            .begin_transaction(
                &mut self.database,
                TransactionConfiguration {
                    data_access: TrDataAccessMode::ReadOnly,
                    isolation: TrIsolationLevel::ReadCommited(TrRecordVersion::RecordVersion),
                    lock_resolution: TrLockResolution::NoWait,
                },
            )
            .map_err(firebird_error)?;
        let result = (|| {
            let (kind, mut statement) = self
                .wire
                .prepare_statement(&mut self.database, &mut transaction, Dialect::D3, sql)
                .map_err(firebird_error)?;
            let plan = if matches!(
                kind,
                StmtType::Select
                    | StmtType::SelectForUpd
                    | StmtType::Insert
                    | StmtType::Update
                    | StmtType::Delete
                    | StmtType::ExecProcedure
            ) {
                self.wire.explain_plan(&statement).map_err(firebird_error)
            } else {
                Err(PluginError::invalid_params(
                    "Execution plans require a query or DML statement",
                ))
            };
            let closed = self
                .wire
                .free_statement(&mut statement, FreeStmtOp::Drop)
                .map_err(firebird_error);
            plan.and_then(|plan| closed.map(|_| plan))
        })();
        let rolled_back = self
            .wire
            .transaction_operation(&mut transaction, TrOp::Rollback)
            .map_err(firebird_error);
        self.control.check()?;
        result.and_then(|plan| rolled_back.map(|_| plan))
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
        self.control.check()?;
        let _deadline = Deadline::query(&self.control, self.query_timeout_secs);
        let result = self.query_inner(sql, params, offset, limit);
        self.control.check()?;
        result
    }

    fn query_inner(
        &mut self,
        sql: &str,
        params: Vec<SqlType>,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<Value, PluginError> {
        self.statement(sql, params, offset, limit, false, None)
    }

    pub fn execute(
        &mut self,
        sql: &str,
        params: Vec<SqlType>,
        offset: u64,
        limit: Option<u64>,
        max_affected: Option<usize>,
    ) -> Result<Value, PluginError> {
        self.control.check()?;
        let _deadline = Deadline::query(&self.control, self.query_timeout_secs);
        // Commit errors after a successful execute have an unknown outcome: never retry.
        let result = self.statement(sql, params, offset, limit, true, max_affected);
        if let Err(error) = &result {
            if error.code != -32002 {
                self.control.check()?;
            }
        }
        result
    }

    fn statement(
        &mut self,
        sql: &str,
        params: Vec<SqlType>,
        offset: u64,
        limit: Option<u64>,
        writable: bool,
        max_affected: Option<usize>,
    ) -> Result<Value, PluginError> {
        let pinned = self.batch_transaction.is_some();
        let mut transaction = match self.batch_transaction.take() {
            Some(transaction) => transaction,
            None => self
                .wire
                .begin_transaction(
                    &mut self.database,
                    TransactionConfiguration {
                        data_access: if writable {
                            TrDataAccessMode::ReadWrite
                        } else {
                            TrDataAccessMode::ReadOnly
                        },
                        lock_resolution: TrLockResolution::NoWait,
                        isolation: TrIsolationLevel::ReadCommited(TrRecordVersion::RecordVersion),
                    },
                )
                .map_err(firebird_error)?,
        };
        let result = (|| {
            let (kind, mut statement) = self
                .wire
                .prepare_statement(&mut self.database, &mut transaction, Dialect::D3, sql)
                .map_err(firebird_error)?;
            let result = (|| {
                if !writable && kind != StmtType::Select {
                    return Err(PluginError::invalid_params(
                        "Only read-only SELECT statements are supported",
                    ));
                }
                if matches!(
                    kind,
                    StmtType::StartTrans | StmtType::Commit | StmtType::Rollback
                ) {
                    return Err(PluginError::invalid_params("Explicit transaction statements are not supported by per-request execution"));
                }
                if kind == StmtType::Savepoint && !pinned {
                    return Err(PluginError::invalid_params(
                        "Savepoints require an open transaction",
                    ));
                }
                let columns = statement.column_names();
                let rows = matches!(kind, StmtType::Select | StmtType::SelectForUpd);
                let (mut result, affected) = if !rows && !columns.is_empty() {
                    let output = self
                        .wire
                        .execute2(&mut self.database, &mut transaction, &mut statement, params)
                        .map_err(firebird_error)?;
                    let affected = self
                        .wire
                        .affected_rows(&statement)
                        .map_err(firebird_error)?;
                    let output = if affected == 0
                        && (kind != StmtType::ExecProcedure || crate::utils::sql::is_dml(sql))
                    {
                        None
                    } else {
                        Some(Ok(Row { cols: output }))
                    };
                    let result = collect_rows(output.into_iter(), columns, offset, limit)?;
                    (result, affected)
                } else {
                    let affected = self
                        .wire
                        .execute(&mut self.database, &mut transaction, &mut statement, params)
                        .map_err(firebird_error)?;
                    let result = if rows {
                        let cursor = std::iter::from_fn(|| {
                            self.wire
                                .fetch(&mut self.database, &mut transaction, &mut statement)
                                .map(|row| row.map(|cols| Row { cols }))
                                .transpose()
                        });
                        collect_rows(cursor, columns, offset, limit)?
                    } else {
                        json!({"columns":[],"rows":[],"total_count":0})
                    };
                    (result, if rows { 0 } else { affected })
                };
                if max_affected.is_some_and(|max| affected > max) {
                    return Err(PluginError::invalid_params(
                        "Mutation matched multiple rows; all changes rolled back",
                    ));
                }
                if writable {
                    result["affected_rows"] = json!(affected);
                }
                self.control.check()?;
                Ok(result)
            })();
            let closed = self
                .wire
                .free_statement(&mut statement, FreeStmtOp::Drop)
                .map_err(firebird_error);
            result.and_then(|result| closed.map(|_| result))
        })();
        if pinned && result.is_ok() {
            self.batch_transaction = Some(transaction);
            return result;
        }
        // Failed execution/fetch/validation always rolls back; successful writes commit once.
        let operation = if writable && result.is_ok() {
            TrOp::Commit
        } else {
            TrOp::Rollback
        };
        let finalized = self.wire.transaction_operation(&mut transaction, operation)
            .map_err(|error| if writable && result.is_ok() { PluginError {code:-32002,message:format!("Commit failed; outcome is unknown. Check the data before retrying: {error}")} } else { firebird_error(error) });
        result.and_then(|result| finalized.map(|_| result))
    }

    pub fn execute_batch(
        &mut self,
        queries: &[String],
        offset: u64,
        limit: Option<u64>,
    ) -> Result<Vec<Value>, PluginError> {
        self.control.check()?;
        let _deadline = Deadline::query(&self.control, self.query_timeout_secs);
        self.batch_transaction = Some(
            self.wire
                .begin_transaction(
                    &mut self.database,
                    TransactionConfiguration {
                        data_access: TrDataAccessMode::ReadWrite,
                        isolation: TrIsolationLevel::ReadCommited(TrRecordVersion::RecordVersion),
                        lock_resolution: TrLockResolution::NoWait,
                    },
                )
                .map_err(firebird_error)?,
        );
        let bounds = crate::limits::current();
        let mut bytes = 0usize;
        let mut rows = 0usize;
        let result = (|| {
            let mut results = vec![];
            for (index, query) in queries.iter().enumerate() {
                let started = Instant::now();
                let result = self
                    .execute(query, vec![], offset, limit, None)
                    .map_err(|error| PluginError {
                        code: error.code,
                        message: format!(
                            "Batch statement {} failed; the batch was rolled back: {}",
                            index + 1,
                            error.message
                        ),
                    })?;
                bytes = bytes.saturating_add(serde_json::to_vec(&result).unwrap().len());
                rows = rows.saturating_add(result["rows"].as_array().unwrap().len());
                if bytes > bounds.bytes || rows > bounds.rows {
                    return Err(PluginError::internal(
                        "Combined batch results exceed configured limits; batch rolled back",
                    ));
                }
                results.push(json!({"result":result,"error":null,"execution_time_ms":started.elapsed().as_secs_f64()*1000.0}));
            }
            self.control.check()?;
            Ok(results)
        })();
        if let Some(mut transaction) = self.batch_transaction.take() {
            let commit = result.is_ok();
            self.wire.transaction_operation(&mut transaction,if commit{TrOp::Commit}else{TrOp::Rollback}).map_err(|error|if commit{PluginError{code:-32002,message:format!("Batch commit failed; outcome is unknown. Check the data before retrying: {error}")}}else{firebird_error(error)})?;
        }
        result
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
        // Cleanup is bounded too, including idle sessions whose request was disarmed.
        if self.control.attach(&self.abort_socket).is_err() {
            return;
        }
        let _deadline = Deadline::query(&self.control, 5);
        if let Some(mut transaction) = self.batch_transaction.take() {
            let _ = self
                .wire
                .transaction_operation(&mut transaction, TrOp::Rollback);
        }
        if self.control.check().is_ok() {
            let _ = self.wire.detach_database(&mut self.database);
        }
    }
}

fn connect_socket(
    host: &str,
    port: u16,
    seconds: u32,
    control: &Control,
) -> Result<TcpStream, PluginError> {
    let deadline = (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds.into()));
    let host = host.to_owned();
    let (sender, receiver) = mpsc::channel();
    // OS DNS resolution cannot be interrupted; stop waiting when the request ends.
    std::thread::spawn(move || {
        let _ = sender.send(
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|a| a.collect::<Vec<_>>()),
        );
    });
    let addresses = loop {
        control.check()?;
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => {
                break result.map_err(|e| {
                    PluginError::internal(format!("Firebird address resolution: {e}"))
                })?
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return Err(PluginError::internal("Firebird address resolver stopped")),
        }
    };
    let mut last_error = "No server addresses resolved".to_string();
    for address in addresses {
        control.check()?;
        let result = match deadline {
            Some(end) => TcpStream::connect_timeout(
                &address,
                end.saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ),
            None => TcpStream::connect(address),
        };
        match result {
            Ok(socket) => return Ok(socket),
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(PluginError::internal(format!(
        "Firebird connection: {last_error}"
    )))
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
    collect_rows_limited(cursor, columns, offset, limit, crate::limits::current())
}

fn collect_rows_limited(
    cursor: impl Iterator<Item = Result<Row, rsfbclient_core::FbError>>,
    columns: Vec<String>,
    offset: u64,
    limit: Option<u64>,
    bounds: crate::limits::Limits,
) -> Result<Value, PluginError> {
    let mut rows = Vec::new();
    let mut total_count = 0u64;
    let mut bytes = serde_json::to_vec(&columns).unwrap().len();
    if bytes > bounds.bytes {
        return Err(PluginError::internal(
            "Column headers exceed max_result_mib",
        ));
    }
    for row in cursor {
        let row = row.map_err(|e| PluginError::internal(format!("Firebird fetch failed: {e}")))?;

        if total_count >= offset && limit.is_none_or(|n| total_count - offset < n) {
            if rows.len() >= bounds.rows {
                return Err(PluginError::internal(
                    "Result exceeds max_result_rows; use a smaller page or a SQL row limit",
                ));
            }
            let values = row
                .cols
                .into_iter()
                .map(|c| sql_value(c.value))
                .collect::<Result<Vec<_>, _>>()?;
            bytes = bytes.saturating_add(serde_json::to_vec(&values).unwrap().len());
            if bytes > bounds.bytes {
                return Err(PluginError::internal(
                    "Result exceeds max_result_mib; reduce the page size or selected columns",
                ));
            }
            rows.push(values);
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
    #[test]
    fn limits_buffered_rows_and_bytes_without_returning_partial_results() {
        let bounds = crate::limits::Limits {
            wire_options: crate::limits::Limits::default().wire_options,
            query_timeout_secs: 0,
            rows: 1,
            bytes: 1024,
            blob_bytes: 3,
        };
        assert!(collect_rows_limited((1..=2).map(row), vec!["N".into()], 0, None, bounds).is_err());
        assert_eq!(
            collect_rows_limited((1..=3).map(row), vec!["N".into()], 0, Some(1), bounds).unwrap()
                ["total_count"],
            3
        );
        let bytes = crate::limits::Limits { bytes: 7, ..bounds };
        assert!(
            collect_rows_limited([row(1)].into_iter(), vec!["N".into()], 0, None, bytes).is_err()
        );
    }
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
    #[ignore = "requires isolated fixture; run tests/live-firebird.ps1"]
    fn live_wire_encryption_policies_and_cipher_selection() {
        use rsfbclient_rust::WireCrypt;
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let connection = ConnectionParams::from_value(&config).unwrap();
        let mut client = Client::connect(connection.clone()).unwrap();
        client.test_connection().unwrap();
        let cipher = client.wire.cipher_name();
        let expected = std::env::var("FIREBIRD_TEST_CIPHER").ok();
        if let Some(expected) = expected.as_deref() {
            assert_eq!(
                cipher,
                if expected == "Disabled" {
                    None
                } else {
                    Some(expected)
                }
            );
        }
        let mut required = connection.clone();
        required.wire_options.mode = WireCrypt::Required;
        let mut enabled = connection.clone();
        enabled.wire_options.mode = WireCrypt::Enabled;
        let mut disabled = connection.clone();
        disabled.wire_options.mode = WireCrypt::Disabled;
        let mut modern = required.clone();
        modern.wire_options.allow_arc4 = false;
        if cipher.is_none() {
            assert!(Client::connect(required).is_err());
            assert!(Client::connect(modern).is_err());
            let mut plain = Client::connect(enabled).unwrap();
            plain.test_connection().unwrap();
            assert!(plain.wire.cipher_name().is_none());
        } else {
            Client::connect(required)
                .unwrap()
                .test_connection()
                .unwrap();
            Client::connect(enabled).unwrap().test_connection().unwrap();
            assert!(Client::connect(disabled).is_err());
            if cipher == Some("Arc4") {
                assert!(Client::connect(modern).is_err());
            } else {
                let mut modern = Client::connect(modern).unwrap();
                modern.test_connection().unwrap();
                assert_eq!(modern.wire.cipher_name(), cipher);
            }
        }
    }

    #[test]
    #[ignore = "requires FIREBIRD_TEST_CONFIG; run tests/live-firebird.ps1"]
    fn live_cancellation_timeout_and_blob_limits() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let heavy = "WITH RECURSIVE n(x) AS (SELECT 1 FROM RDB$DATABASE UNION ALL SELECT x + 1 FROM n WHERE x < 1000) SELECT SUM(CAST(a.x AS BIGINT) * b.x * c.x) FROM n a CROSS JOIN n b CROSS JOIN n c";
        for timeout in [false, true] {
            let mut parameters = config.clone();
            parameters["query_timeout_secs"] = json!(if timeout { 1 } else { 0 });
            let mut client =
                Client::connect(ConnectionParams::from_value(&parameters).unwrap()).unwrap();
            let id = client
                .query("SELECT CURRENT_CONNECTION FROM RDB$DATABASE", 0, None)
                .unwrap()["rows"][0][0]
                .as_i64()
                .unwrap();
            let stop = client.control.clone();
            let cancel = (!timeout).then(|| {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    stop.cancel();
                })
            });
            let start = Instant::now();
            let error = client.query(heavy, 0, None).unwrap_err();
            assert_eq!(error.code, if timeout { -32001 } else { -32800 }, "{error}");
            assert!(start.elapsed() < Duration::from_secs(5));
            if let Some(cancel) = cancel {
                cancel.join().unwrap();
            }
            drop(client);
            let mut observer =
                Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let count = observer
                    .query(
                        &format!(
                            "SELECT COUNT(*) FROM MON$ATTACHMENTS WHERE MON$ATTACHMENT_ID = {id}"
                        ),
                        0,
                        None,
                    )
                    .unwrap();
                if count["rows"] == json!([[0]]) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "Cancelled attachment remains active on the server"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            observer.wire.set_blob_limit(3);
            assert_eq!(
                observer
                    .query(
                        "SELECT CAST('abc' AS BLOB SUB_TYPE TEXT) FROM RDB$DATABASE",
                        0,
                        None
                    )
                    .unwrap()["rows"],
                json!([["abc"]])
            );
            assert!(observer
                .query(
                    "SELECT CAST('abcd' AS BLOB SUB_TYPE TEXT) FROM RDB$DATABASE",
                    0,
                    None
                )
                .unwrap_err()
                .message
                .contains("BLOB exceeds"));
            observer.test_connection().unwrap();
        }
    }

    #[test]
    #[ignore = "requires Firebird 4+ and FIREBIRD_TEST_CONFIG; run tests/live-firebird.ps1"]
    fn live_temporal_and_decfloat_types_firebird4() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let mut client = Client::connect(ConnectionParams::from_value(&config).unwrap()).unwrap();
        let result = client
            .query(
                r#"SELECT
            DATE '2024-02-29' AS D,
            TIME '23:59:59.1234' AS T,
            TIMESTAMP '2024-02-29 23:59:59.1234' AS TS,
            TIMESTAMP '2024-07-01 12:34:56.7890 Europe/Berlin' AS SUMMER_TS,
            TIMESTAMP '2024-01-01 12:34:56.7890 Europe/Berlin' AS WINTER_TS,
            TIME '12:34:56.7890 +05:45' AS OFFSET_T,
            CAST('1.234567890123456' AS DECFLOAT(16)) AS DF16,
            CAST('1.234567890123456789012345678901234' AS DECFLOAT(34)) AS DF34,
            CAST('1E+6000' AS DECFLOAT(34)) AS HUGE_DF,
            CAST('1234567890123456789012345678901234.5678' AS DECIMAL(38,4)) AS WIDE_VALUE,
            CAST(NULL AS TIME WITH TIME ZONE) AS NULL_T,
            CAST(NULL AS DECFLOAT(34)) AS NULL_DF,
            CAST('hello' AS BLOB SUB_TYPE TEXT) AS TEXT_BLOB
            FROM RDB$DATABASE"#,
                0,
                None,
            )
            .unwrap();
        assert_eq!(
            result["rows"],
            json!([[
                "2024-02-29",
                "23:59:59.1234",
                "2024-02-29 23:59:59.1234",
                "2024-07-01 12:34:56.7890 Europe/Berlin",
                "2024-01-01 12:34:56.7890 Europe/Berlin",
                "12:34:56.7890 +05:45",
                "1.234567890123456",
                "1.234567890123456789012345678901234",
                "1E+6000",
                "1234567890123456789012345678901234.5678",
                null,
                null,
                "hello"
            ]])
        );
        let special = client.query("SELECT CAST('NaN' AS DECFLOAT(34)) AS N, CAST('Infinity' AS DECFLOAT(16)) AS I, CAST('-Infinity' AS DECFLOAT(34)) AS NEG FROM RDB$DATABASE", 0, None).unwrap();
        assert_eq!(special["rows"], json!([["NaN", "Infinity", "-Infinity"]]));
        client.test_connection().unwrap();
    }

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
                "9007199254740993",
                1.25,
                null
            ]])
        );
        let temporal = client.query("SELECT DATE '2024-02-29', TIME '23:59:59.1234', TIMESTAMP '2024-02-29 23:59:59.1234' FROM RDB$DATABASE", 0, None).unwrap();
        let engine = client
            .query(
                "SELECT RDB$GET_CONTEXT('SYSTEM', 'ENGINE_VERSION') FROM RDB$DATABASE",
                0,
                None,
            )
            .unwrap();
        let timestamp = if engine["rows"][0][0].as_str().unwrap().starts_with("3.") {
            "29-FEB-2024 23:59:59.1234"
        } else {
            "2024-02-29 23:59:59.1234"
        };
        assert_eq!(
            temporal["rows"],
            json!([["2024-02-29", "23:59:59.1234", timestamp]])
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
