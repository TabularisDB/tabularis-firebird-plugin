//! The RPC worker owns sessions; only explicitly open transactions retain attachments.
use crate::{
    client::Client,
    control::{self, Deadline},
    error::PluginError,
    models::ConnectionParams,
};
use rsfbclient_core::{
    SqlType, TrDataAccessMode, TrIsolationLevel, TrLockResolution, TrOp, TrRecordVersion,
    TransactionConfiguration,
};
use serde_json::{json, Value};
use std::{cell::RefCell, collections::HashMap};

struct Session {
    connection: ConnectionParams,
    client: Client,
}
thread_local! { static SESSIONS: RefCell<HashMap<String, Session>> = RefCell::new(HashMap::new()); }
const MAX_SESSIONS: usize = 32;

pub fn id(params: &Value) -> Result<Option<&str>, PluginError> {
    match params.get("session_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if !id.is_empty() && id.len() <= 256 && !id.contains('\0') => {
            Ok(Some(id))
        }
        _ => Err(PluginError::invalid_params(
            "session_id must be a non-empty string of at most 256 bytes without NUL",
        )),
    }
}

fn same_attachment(a: &ConnectionParams, b: &ConnectionParams) -> bool {
    a.host == b.host
        && a.port == b.port
        && a.database == b.database
        && a.username == b.username
        && a.password == b.password
        && a.ssl_mode == b.ssl_mode
}

pub fn run(
    connection: ConnectionParams,
    session_id: Option<&str>,
    action: impl FnOnce(&mut Client) -> Result<Value, PluginError>,
) -> (Result<Value, PluginError>, bool) {
    let saved = SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        if let Some(id) = session_id {
            if sessions.get(id).is_some_and(|s| !same_attachment(&s.connection, &connection)) {
                return Err(PluginError::invalid_params("Connection changed while a transaction is open; finish or release the original session first"));
            }
            Ok(sessions.remove(id))
        } else { Ok(None) }
    });
    let saved = match saved {
        Ok(saved) => saved,
        Err(error) => return (Err(error), true),
    };
    let client = match saved {
        Some(session) => Ok(session.client),
        None => Client::connect(connection.clone()),
    };
    let mut client = match client {
        Ok(client) => client,
        Err(error) => return (Err(error), false),
    };
    if let Err(error) = client.bind_request(&connection) {
        return (Err(error), false);
    }
    let deadline = Deadline::query(
        &control::current(),
        connection
            .query_timeout_secs
            .unwrap_or(crate::limits::current().query_timeout_secs),
    );
    let was_open = client.in_transaction();
    let mut result = action(&mut client);
    // Stop and join the timer before removing its ability to abort the attachment.
    drop(deadline);
    if result.is_ok() && client.in_transaction() {
        if let Some(id) = session_id {
            let full = SESSIONS.with(|s| s.borrow().len() >= MAX_SESSIONS);
            if full {
                result = Err(PluginError::invalid_params(
                    "At most 32 transactions may be open; release another session first",
                ));
            } else if let Err(error) = client.park() {
                result = Err(error);
            } else {
                SESSIONS.with(|s| {
                    s.borrow_mut()
                        .insert(id.into(), Session { connection, client })
                });
                return (result, true);
            }
        } else {
            result = Err(PluginError::invalid_params(
                "Explicit transactions require session_id",
            ));
        }
    }
    // Drop rolls back any remaining transaction, also after parsing/validation failures.
    if let Err(error) = &mut result {
        if error.code != -32002 && (was_open || client.in_transaction()) {
            error.message.push_str(
                "; the session transaction was rolled back (earlier commits remain committed)",
            );
        }
    }
    drop(client);
    (result, false)
}

pub fn release(params: &Value) -> Result<Value, PluginError> {
    let id = id(params)?.ok_or_else(|| PluginError::invalid_params("session_id is required"))?;
    if let Some(mut session) = SESSIONS.with(|s| s.borrow_mut().remove(id)) {
        session.client.bind_request(&session.connection)?;
        let _deadline = Deadline::query(&control::current(), 5);
        session.client.finish(TrOp::Rollback)?;
    }
    Ok(Value::Null)
}

#[derive(Debug)]
enum Command {
    Begin(TransactionConfiguration),
    Finish(TrOp),
}

// Control statements use a deliberately small grammar. Never discard unknown options.
fn command(sql: &str) -> Result<Option<Command>, PluginError> {
    let keywords = crate::utils::sql::top_level_words(sql);
    let first = keywords
        .first()
        .map(|(_, word)| word.to_ascii_uppercase())
        .unwrap_or_default();
    let second = keywords
        .get(1)
        .map(|(_, word)| word.to_ascii_uppercase())
        .unwrap_or_default();
    if !matches!(first.as_str(), "BEGIN" | "START" | "COMMIT" | "ROLLBACK")
        && !(first == "SET" && second == "TRANSACTION")
    {
        return Ok(None);
    }
    if first == "ROLLBACK" && second == "TO" {
        return Ok(None);
    }
    let invalid = || {
        PluginError::invalid_params("Unsupported transaction syntax; use BEGIN/START TRANSACTION/SET TRANSACTION with READ ONLY/WRITE, ISOLATION LEVEL SNAPSHOT or READ COMMITTED, NO WAIT, or COMMIT/ROLLBACK [WORK] [RETAINING]")
    };
    let bytes = sql.as_bytes();
    let mut words = vec![];
    let mut i = 0;
    let mut ended = false;
    while i < bytes.len() {
        match bytes[i] {
            b if b.is_ascii_whitespace() => i += 1,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut depth = 1;
                while i < bytes.len() && depth > 0 {
                    if bytes.get(i..i + 2) == Some(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if bytes.get(i..i + 2) == Some(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if depth != 0 {
                    return Err(invalid());
                }
            }
            b';' if !ended => {
                ended = true;
                i += 1;
            }
            b if b.is_ascii_alphabetic() && !ended => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
                    i += 1;
                }
                words.push(sql[start..i].to_ascii_uppercase());
            }
            _ => return Err(invalid()),
        }
    }
    let tokens = words.iter().map(String::as_str).collect::<Vec<_>>();
    if matches!(first.as_str(), "COMMIT" | "ROLLBACK") {
        let mut rest = &tokens[1..];
        if rest.first() == Some(&"WORK") {
            rest = &rest[1..];
        }
        let retaining = match rest {
            [] => false,
            ["RETAINING"] | ["RETAIN"] | ["RETAIN", "SNAPSHOT"] => true,
            _ => return Err(invalid()),
        };
        return Ok(Some(Command::Finish(match (first.as_str(), retaining) {
            ("COMMIT", false) => TrOp::Commit,
            ("COMMIT", true) => TrOp::CommitRetaining,
            (_, false) => TrOp::Rollback,
            (_, true) => TrOp::RollbackRetaining,
        })));
    }
    let mut rest = match tokens.as_slice() {
        ["BEGIN", "TRANSACTION" | "WORK", rest @ ..]
        | ["START", "TRANSACTION", rest @ ..]
        | ["SET", "TRANSACTION", rest @ ..] => rest,
        ["BEGIN", rest @ ..] => rest,
        _ => return Err(invalid()),
    };
    let mut config = TransactionConfiguration {
        lock_resolution: TrLockResolution::NoWait,
        isolation: TrIsolationLevel::ReadCommited(TrRecordVersion::RecordVersion),
        ..TransactionConfiguration::default()
    };
    let (mut access, mut isolation, mut lock) = (false, false, false);
    while !rest.is_empty() {
        match rest {
            ["READ", mode @ ("ONLY" | "WRITE"), tail @ ..] if !access => {
                config.data_access = if *mode == "ONLY" {
                    TrDataAccessMode::ReadOnly
                } else {
                    TrDataAccessMode::ReadWrite
                };
                access = true;
                rest = tail;
            }
            ["ISOLATION", "LEVEL", "SNAPSHOT", tail @ ..] if !isolation => {
                config.isolation = TrIsolationLevel::Concurrency;
                isolation = true;
                rest = tail;
            }
            ["ISOLATION", "LEVEL", "READ", "COMMITTED", tail @ ..] if !isolation => {
                let (version, tail) = match tail {
                    ["RECORD_VERSION", tail @ ..] => (TrRecordVersion::RecordVersion, tail),
                    ["NO", "RECORD_VERSION", tail @ ..] => (TrRecordVersion::NoRecordVersion, tail),
                    tail => (TrRecordVersion::RecordVersion, tail),
                };
                config.isolation = TrIsolationLevel::ReadCommited(version);
                isolation = true;
                rest = tail;
            }
            ["NO", "WAIT", tail @ ..] if !lock => {
                lock = true;
                rest = tail;
            }
            _ => return Err(invalid()),
        }
    }
    Ok(Some(Command::Begin(config)))
}

pub fn execute(
    client: &mut Client,
    sql: &str,
    values: Vec<SqlType>,
    offset: u64,
    limit: Option<u64>,
) -> Result<Value, PluginError> {
    match command(sql)? {
        None => return client.execute(sql, values, offset, limit, None),
        Some(_) if !values.is_empty() => {
            return Err(PluginError::invalid_params(
                "Transaction commands do not accept values",
            ))
        }
        Some(Command::Begin(config)) => client.begin(config)?,
        Some(Command::Finish(operation)) => client.finish(operation)?,
    }
    Ok(json!({"columns":[],"rows":[],"total_count":0,"affected_rows":0}))
}

pub fn batch(
    client: &mut Client,
    queries: &[String],
    offset: u64,
    limit: Option<u64>,
) -> Result<Value, PluginError> {
    let controls = queries
        .iter()
        .map(|q| command(q).map(|c| c.is_some()))
        .collect::<Result<Vec<_>, _>>()?;
    if !client.in_transaction() && !controls.contains(&true) {
        return client
            .execute_batch(queries, offset, limit)
            .map(Value::Array);
    }
    let mut results = vec![];
    let bounds = crate::limits::current();
    let (mut bytes, mut rows) = (0usize, 0usize);
    for (index, query) in queries.iter().enumerate() {
        let start = std::time::Instant::now();
        let result = execute(client,query,vec![],offset,limit).and_then(|result| {
            bytes = bytes.saturating_add(serde_json::to_vec(&result).unwrap().len());
            rows = rows.saturating_add(result["rows"].as_array().unwrap().len());
            if bytes > bounds.bytes || rows > bounds.rows { Err(PluginError::internal("Combined batch results exceed configured limits")) } else { Ok(result) }
        }).map_err(|error| PluginError {code:error.code, message:format!("Batch statement {} failed; the open transaction will be rolled back. Earlier explicit commits or autocommit statements remain committed: {}",index+1,error.message)})?;
        results.push(json!({"result":result,"error":null,"execution_time_ms":start.elapsed().as_secs_f64()*1000.0}));
    }
    Ok(Value::Array(results))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_session_ids_and_attachment_identity() {
        for value in [json!(false), json!(""), json!("\0"), json!("a".repeat(257))] {
            assert!(id(&json!({"session_id":value})).is_err());
        }
        assert_eq!(id(&json!({})).unwrap(), None);
        let connection =
            ConnectionParams::from_value(&json!({"database":"a","username":"u","password":"p"}))
                .unwrap();
        let mut other = connection.clone();
        other.query_timeout_secs = Some(1);
        other.connect_timeout_secs = 3;
        assert!(same_attachment(&connection, &other));
        other.database = Some("b".into());
        assert!(!same_attachment(&connection, &other));
    }
    #[test]
    fn parses_only_complete_transaction_commands() {
        for sql in [
            "BEGIN",
            "/* comment */ BEGIN TRANSACTION; -- done",
            "SET TRANSACTION READ ONLY ISOLATION LEVEL SNAPSHOT NO WAIT",
            "START TRANSACTION READ WRITE ISOLATION LEVEL READ COMMITTED RECORD_VERSION",
            "COMMIT WORK RETAIN SNAPSHOT",
            "ROLLBACK RETAINING",
        ] {
            assert!(command(sql).unwrap().is_some(), "{sql}");
        }
        for sql in [
            "SELECT 'COMMIT' FROM T",
            "EXECUTE BLOCK AS BEGIN END",
            "ROLLBACK TO SAVEPOINT A",
            "SET ROLE R",
        ] {
            assert!(command(sql).unwrap().is_none(), "{sql}");
        }
        for sql in [
            "BEGIN; DELETE FROM T",
            "COMMIT AND CHAIN",
            "COMMIT 'WORK'",
            "BEGIN READ ONLY READ WRITE",
            "SET TRANSACTION WAIT",
            "BEGIN /*",
            "BEGIN;;",
        ] {
            assert!(command(sql).is_err(), "{sql}");
        }
        assert!(matches!(
            command("BEGIN READ ONLY").unwrap(),
            Some(Command::Begin(TransactionConfiguration {
                data_access: TrDataAccessMode::ReadOnly,
                ..
            }))
        ));
    }

    #[test]
    #[ignore = "mutates an isolated fixture; run tests/live-firebird.ps1"]
    fn live_pinned_transactions_are_isolated_and_released() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let connection = ConnectionParams::from_value(&config).unwrap();
        let table = format!(
            "TS_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        struct Cleanup {
            connection: ConnectionParams,
            table: String,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                SESSIONS.with(|s| s.borrow_mut().clear());
                if let Ok(mut client) = Client::connect(self.connection.clone()) {
                    let _ = client.execute(
                        &format!("DROP TABLE {}", self.table),
                        vec![],
                        0,
                        None,
                        None,
                    );
                }
            }
        }
        Client::connect(connection.clone())
            .unwrap()
            .execute(
                &format!("CREATE TABLE {table}(ID INTEGER PRIMARY KEY)"),
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        let _cleanup = Cleanup {
            connection: connection.clone(),
            table: table.clone(),
        };
        let call = |query: &str, session: &str, config: &Value| {
            let current = std::sync::Arc::new(control::Control::default());
            let result = control::run(current.clone(), || {
                crate::handlers::query::execute_query(
                    json!(1),
                    &json!({"params":config,"query":query,"session_id":session}),
                )
            });
            assert!(result.get("error").is_none(), "{result}");
            crate::host_contract_tests::validate_response("execute_query", &result["result"]);
            (result["result"].clone(), current)
        };
        let ok = |query: &str, session: &str| {
            let (result, _) = call(query, session, &config);
            assert!(result.get("error").is_none(), "{query}: {result}");
            result
        };
        let batch = |queries: Vec<String>| {
            control::run(std::sync::Arc::new(control::Control::default()), || {
                crate::handlers::query::execute_query_batch(
                    json!(1),
                    &json!({"params":config,"queries":queries,"session_id":"A"}),
                )
            })["result"]
                .clone()
        };
        let count = || {
            Client::connect(connection.clone())
                .unwrap()
                .query(&format!("SELECT COUNT(*) FROM {table}"), 0, None)
                .unwrap()["rows"][0][0]
                .as_i64()
                .unwrap()
        };
        assert_eq!(ok("BEGIN", "A")["in_transaction"], true);
        let identity = ok(
            "SELECT CURRENT_CONNECTION, CURRENT_TRANSACTION FROM RDB$DATABASE",
            "A",
        )["result"]["rows"]
            .clone();
        let (_, old_request) = call(&format!("INSERT INTO {table} VALUES(1)"), "A", &config);
        old_request.cancel(); // A completed request cannot abort the parked attachment.
        assert_eq!(
            ok(
                "SELECT CURRENT_CONNECTION, CURRENT_TRANSACTION FROM RDB$DATABASE",
                "A"
            )["result"]["rows"],
            identity
        );
        assert_eq!(ok("BEGIN READ ONLY", "B")["in_transaction"], true);
        assert_ne!(
            ok(
                "SELECT CURRENT_CONNECTION, CURRENT_TRANSACTION FROM RDB$DATABASE",
                "B"
            )["result"]["rows"],
            identity
        );
        assert_eq!(count(), 0);
        let mut changed = config.clone();
        changed["database"] = json!("another_database");
        let mismatch = call("COMMIT", "A", &changed).0;
        assert_eq!(mismatch["in_transaction"], true);
        assert_eq!(mismatch["error_code"], -32602);
        let duplicate = call(&format!("INSERT INTO {table} VALUES(1)"), "A", &config).0;
        assert!(duplicate["error"].is_string());
        assert_eq!(duplicate["in_transaction"], false);
        assert_eq!(count(), 0); // Both writes rolled back; no automatic replay.

        ok("BEGIN", "A");
        ok(&format!("INSERT INTO {table} VALUES(1)"), "A");
        ok("SAVEPOINT TEST_SP", "A");
        ok(&format!("INSERT INTO {table} VALUES(2)"), "A");
        ok("ROLLBACK TO SAVEPOINT TEST_SP", "A");
        ok("RELEASE SAVEPOINT TEST_SP", "A");
        assert_eq!(
            ok(&format!("SELECT ID FROM {table}"), "A")["result"]["rows"],
            json!([[1]])
        );
        assert_eq!(ok("COMMIT RETAINING", "A")["in_transaction"], true);
        assert_eq!(count(), 1);
        ok(&format!("INSERT INTO {table} VALUES(3)"), "A");
        assert_eq!(ok("ROLLBACK RETAINING", "A")["in_transaction"], true);
        assert_eq!(ok("COMMIT", "A")["in_transaction"], false);
        assert_eq!(count(), 1);
        for _ in 0..2 {
            control::run(std::sync::Arc::new(control::Control::default()), || {
                release(&json!({"session_id":"B"}))
            })
            .unwrap();
        }

        let opened = batch(vec![
            "BEGIN".into(),
            format!("INSERT INTO {table} VALUES(2)"),
            format!("SELECT ID FROM {table} ORDER BY ID"),
        ]);
        assert_eq!(opened["in_transaction"], true);
        assert_eq!(opened["results"][2]["result"]["rows"], json!([[1], [2]]));
        let closed = batch(vec![
            format!("INSERT INTO {table} VALUES(3)"),
            "COMMIT".into(),
        ]);
        assert_eq!(closed["in_transaction"], false);
        assert_eq!(count(), 3);
        let failed = batch(vec![
            "BEGIN".into(),
            format!("INSERT INTO {table} VALUES(4)"),
            format!("INSERT INTO {table} VALUES(1)"),
        ]);
        assert_eq!(failed["in_transaction"], false);
        assert!(failed["results"][2]["error"].is_string());
        assert_eq!(count(), 3);
        ok("BEGIN READ ONLY", "A");
        let readonly = call(&format!("INSERT INTO {table} VALUES(4)"), "A", &config).0;
        assert!(readonly["error"].is_string());
        assert_eq!(readonly["in_transaction"], false);

        let heavy = "WITH RECURSIVE N(I) AS (SELECT 1 FROM RDB$DATABASE UNION ALL SELECT I+1 FROM N WHERE I<1000) SELECT SUM(CAST(A.I AS BIGINT)*B.I*C.I) FROM N A CROSS JOIN N B CROSS JOIN N C";
        ok("BEGIN", "A");
        ok(&format!("INSERT INTO {table} VALUES(4)"), "A");
        let mut timed = config.clone();
        timed["query_timeout_secs"] = json!(1);
        let timeout = call(heavy, "A", &timed).0;
        assert_eq!(timeout["error_code"], -32001);
        assert_eq!(timeout["in_transaction"], false);
        assert_eq!(count(), 3);
        ok("BEGIN", "A");
        ok(&format!("INSERT INTO {table} VALUES(4)"), "A");
        let cancellation = std::sync::Arc::new(control::Control::default());
        let cancel = cancellation.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            cancel.cancel();
        });
        let cancelled = control::run(cancellation, || {
            crate::handlers::query::execute_query(
                json!(1),
                &json!({"params":config,"query":heavy,"session_id":"A"}),
            )
        });
        thread.join().unwrap();
        assert_eq!(cancelled["result"]["error_code"], -32800);
        assert_eq!(cancelled["result"]["in_transaction"], false);
        assert_eq!(count(), 3);

        ok("BEGIN", "A");
        ok(&format!("INSERT INTO {table} VALUES(4)"), "A");
        control::run(std::sync::Arc::new(control::Control::default()), || {
            release(&json!({"session_id":"A"}))
        })
        .unwrap();
        assert_eq!(count(), 3);
        ok("BEGIN", "A");
        ok(&format!("INSERT INTO {table} VALUES(4)"), "A");
        let broken = std::sync::Arc::new(control::Control::default());
        control::run(broken.clone(), || {
            SESSIONS.with(|s| {
                s.borrow_mut()
                    .get_mut("A")
                    .unwrap()
                    .client
                    .bind_request(&connection)
            })
        })
        .unwrap();
        broken.cancel();
        let commit = call("COMMIT", "A", &config).0;
        assert_eq!(commit["error_code"], -32002);
        assert_eq!(commit["in_transaction"], false);
        assert_eq!(count(), 3);
        assert!(SESSIONS.with(|s| s.borrow().is_empty()));
        for index in 0..MAX_SESSIONS {
            assert_eq!(
                ok("BEGIN READ ONLY", &format!("capacity-{index}"))["in_transaction"],
                true
            );
        }
        let full = call("BEGIN", "overflow", &config).0;
        assert_eq!(full["error_code"], -32602);
        assert_eq!(full["in_transaction"], false);
        assert_eq!(SESSIONS.with(|s| s.borrow().len()), MAX_SESSIONS);
        for index in 0..MAX_SESSIONS {
            control::run(std::sync::Arc::new(control::Control::default()), || {
                release(&json!({"session_id":format!("capacity-{index}")}))
            })
            .unwrap();
        }
        assert!(SESSIONS.with(|s| s.borrow().is_empty()));
    }
}
