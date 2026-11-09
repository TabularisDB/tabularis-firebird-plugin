//! Failure injection and bounded workloads against the disposable live fixture.
use crate::{client::Client, control, models::ConnectionParams};
use rsfbclient_core::{
    SqlType, TrDataAccessMode, TrIsolationLevel, TrLockResolution, TrOp, TrRecordVersion,
    TransactionConfiguration,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

fn configuration() -> (Value, ConnectionParams) {
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
    )
    .unwrap();
    let connection = ConnectionParams::from_value(&config).unwrap();
    (config, connection)
}

fn transaction() -> TransactionConfiguration {
    TransactionConfiguration {
        data_access: TrDataAccessMode::ReadWrite,
        isolation: TrIsolationLevel::ReadCommited(TrRecordVersion::RecordVersion),
        lock_resolution: TrLockResolution::NoWait,
    }
}

fn rpc(config: &Value, query: &str, session: &str) -> Value {
    let response = control::run(Arc::new(control::Control::default()), || {
        crate::handlers::query::execute_query(
            json!(1),
            &json!({"params":config,"query":query,"session_id":session}),
        )
    });
    assert!(response.get("error").is_none(), "{response}");
    response["result"].clone()
}

fn successful(config: &Value, query: &str, session: &str) -> Value {
    let result = rpc(config, query, session);
    assert!(result.get("error").is_none(), "{query}: {result}");
    result
}

/// Forward encrypted bytes unchanged; optionally suppress replies to simulate a lost commit reply.
struct Relay {
    port: u16,
    hold_replies: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    sockets: Arc<Mutex<Vec<TcpStream>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Relay {
    fn new(connection: &ConnectionParams) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let destination = format!(
            "{}:{}",
            connection.host.as_deref().unwrap(),
            connection.port.unwrap()
        );
        let hold_replies = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let sockets = Arc::new(Mutex::new(Vec::new()));
        let (held, stopped, shared) = (hold_replies.clone(), stop.clone(), sockets.clone());
        let worker = thread::spawn(move || {
            let mut downstream = loop {
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("relay accept failed: {error}"),
                }
            };
            downstream.set_nonblocking(false).unwrap();
            let mut upstream = TcpStream::connect(destination).unwrap();
            *shared.lock().unwrap() = vec![
                downstream.try_clone().unwrap(),
                upstream.try_clone().unwrap(),
            ];
            let mut from_client = downstream.try_clone().unwrap();
            let mut to_server = upstream.try_clone().unwrap();
            let forward = thread::spawn(move || {
                let _ = std::io::copy(&mut from_client, &mut to_server);
                let _ = from_client.shutdown(Shutdown::Both);
                let _ = to_server.shutdown(Shutdown::Both);
            });
            let mut buffer = [0; 16 * 1024];
            while let Ok(count) = upstream.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                if !held.load(Ordering::SeqCst) && downstream.write_all(&buffer[..count]).is_err() {
                    break;
                }
            }
            let _ = downstream.shutdown(Shutdown::Both);
            let _ = upstream.shutdown(Shutdown::Both);
            forward.join().unwrap();
        });
        Self {
            port,
            hold_replies,
            stop,
            sockets,
            worker: Some(worker),
        }
    }

    fn disconnect(&self) {
        for socket in self.sockets.lock().unwrap().iter() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.disconnect();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
#[ignore = "requires disposable Firebird fixture; run tests/live-firebird.ps1"]
fn live_lost_commit_reply_and_server_disconnect_do_not_replay_writes() {
    let (config, connection) = configuration();
    let mut admin = Client::connect(connection.clone()).unwrap();
    admin
        .execute(
            "CREATE TABLE FB_FAILURE_TEST (ID INTEGER PRIMARY KEY)",
            vec![],
            0,
            None,
            None,
        )
        .unwrap();
    let relay = Relay::new(&connection);
    let mut proxied = config.clone();
    proxied["port"] = json!(relay.port);
    proxied["query_timeout_secs"] = json!(10);
    let (inserted, ready) = mpsc::channel();
    let (commit, go) = mpsc::channel();
    let owner = thread::spawn(move || {
        successful(&proxied, "BEGIN", "lost-reply");
        successful(
            &proxied,
            "INSERT INTO FB_FAILURE_TEST VALUES(1)",
            "lost-reply",
        );
        inserted.send(()).unwrap();
        go.recv_timeout(Duration::from_secs(10)).unwrap();
        rpc(&proxied, "COMMIT", "lost-reply")
    });
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    relay.hold_replies.store(true, Ordering::SeqCst);
    commit.send(()).unwrap();
    // Confirm durability independently before severing the connection: the reply,
    // rather than the COMMIT request, must be what is lost.
    let deadline = Instant::now() + Duration::from_secs(7);
    let committed = loop {
        if admin
            .query("SELECT COUNT(*) FROM FB_FAILURE_TEST WHERE ID = 1", 0, None)
            .unwrap()["rows"]
            == json!([[1]])
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    relay.disconnect();
    let lost = owner.join().unwrap();
    assert!(committed, "COMMIT did not reach the server");
    assert_eq!(lost["error_code"], -32002, "{lost}");
    assert_eq!(lost["in_transaction"], false);
    drop(relay);
    let replay = rpc(&config, "COMMIT", "lost-reply");
    assert_eq!(replay["error_code"], -32602);
    assert_eq!(
        admin
            .query("SELECT ID FROM FB_FAILURE_TEST", 0, None)
            .unwrap()["rows"],
        json!([[1]])
    );

    successful(&config, "BEGIN", "server-cut");
    successful(
        &config,
        "INSERT INTO FB_FAILURE_TEST VALUES(2)",
        "server-cut",
    );
    let attachment = successful(
        &config,
        "SELECT CURRENT_CONNECTION FROM RDB$DATABASE",
        "server-cut",
    )["result"]["rows"][0][0]
        .as_i64()
        .unwrap();
    admin
        .execute(
            "DELETE FROM MON$ATTACHMENTS WHERE MON$ATTACHMENT_ID = ?",
            vec![SqlType::Integer(attachment)],
            0,
            None,
            None,
        )
        .unwrap();
    let failed = rpc(&config, "SELECT 1 FROM RDB$DATABASE", "server-cut");
    assert!(failed["error"].is_string());
    assert_eq!(failed["in_transaction"], false);
    assert_eq!(rpc(&config, "COMMIT", "server-cut")["error_code"], -32602);
    assert_eq!(
        admin
            .query("SELECT ID FROM FB_FAILURE_TEST", 0, None)
            .unwrap()["rows"],
        json!([[1]])
    );
    successful(&config, "BEGIN", "server-cut");
    successful(&config, "ROLLBACK", "server-cut");
    drop(admin);
    Client::connect(connection)
        .unwrap()
        .execute("DROP TABLE FB_FAILURE_TEST", vec![], 0, None, None)
        .unwrap();
}

#[test]
#[ignore = "requires disposable Firebird fixture; run tests/live-firebird.ps1"]
fn live_large_results_and_concurrent_transactions_remain_bounded_and_atomic() {
    let (_, connection) = configuration();
    let mut client = Client::connect(connection.clone()).unwrap();
    client.execute("CREATE TABLE FB_WORKLOAD_TEST (ID INTEGER PRIMARY KEY, N INTEGER, B BLOB SUB_TYPE TEXT CHARACTER SET UTF8)", vec![], 0, None, None).unwrap();
    let rows = "WITH RECURSIVE N(I) AS (SELECT 1 FROM RDB$DATABASE UNION ALL SELECT I+1 FROM N WHERE I<1000) SELECT FIRST 100001 A.I FROM N A CROSS JOIN N B";
    assert!(client
        .query(rows, 0, None)
        .unwrap_err()
        .message
        .contains("max_result_rows"));
    let page = client.query(rows, 0, Some(10)).unwrap();
    assert_eq!(page["rows"].as_array().unwrap().len(), 10);
    assert_eq!(page["total_count"], 100001);
    let partial = rows.replace("100001", "60000");
    let error = client
        .execute_batch(
            &[
                "INSERT INTO FB_WORKLOAD_TEST(ID,N) VALUES(-1,0)".into(),
                partial.clone(),
                partial,
            ],
            0,
            None,
        )
        .unwrap_err();
    assert!(
        error.message.contains("Combined batch results"),
        "{error:?}"
    );
    assert_eq!(
        client
            .query("SELECT COUNT(*) FROM FB_WORKLOAD_TEST", 0, None)
            .unwrap()["rows"],
        json!([[0]])
    );

    let large_blob = "é".repeat(4 * 1024 * 1024 + 1);
    client
        .execute(
            "INSERT INTO FB_WORKLOAD_TEST(ID,B) VALUES(-2,?)",
            vec![SqlType::Text(large_blob)],
            0,
            None,
            None,
        )
        .unwrap();
    assert!(client
        .query("SELECT B FROM FB_WORKLOAD_TEST WHERE ID = -2", 0, None)
        .unwrap_err()
        .message
        .contains("BLOB exceeds"));
    client
        .execute(
            "DELETE FROM FB_WORKLOAD_TEST WHERE ID = -2",
            vec![],
            0,
            None,
            None,
        )
        .unwrap();

    client
        .execute(
            "INSERT INTO FB_WORKLOAD_TEST(ID,N) VALUES(0,0)",
            vec![],
            0,
            None,
            None,
        )
        .unwrap();
    client.begin(transaction()).unwrap();
    client
        .execute(
            "UPDATE FB_WORKLOAD_TEST SET N = N + 1 WHERE ID = 0",
            vec![],
            0,
            None,
            None,
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let workers = (0..8)
        .map(|index| {
            let (connection, barrier) = (connection.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                let mut writer = Client::connect(connection).unwrap();
                assert!(writer
                    .execute(
                        "UPDATE FB_WORKLOAD_TEST SET N = N + 1 WHERE ID = 0",
                        vec![],
                        0,
                        None,
                        None
                    )
                    .is_err());
                let batch = (1..=20)
                    .map(|id| {
                        format!(
                            "INSERT INTO FB_WORKLOAD_TEST(ID,N) VALUES({},1)",
                            index * 20 + id
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(writer.execute_batch(&batch, 0, None).unwrap().len(), 20);
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join())
        .collect::<Vec<_>>();
    client.finish(TrOp::Rollback).unwrap();
    for result in results {
        result.unwrap();
    }
    assert_eq!(
        client
            .query("SELECT COUNT(*), SUM(N) FROM FB_WORKLOAD_TEST", 0, None)
            .unwrap()["rows"],
        json!([[161, 160]])
    );
    drop(client);
    Client::connect(connection)
        .unwrap()
        .execute("DROP TABLE FB_WORKLOAD_TEST", vec![], 0, None, None)
        .unwrap();
}
