use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stalled_handshake(cancel: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted, ready) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        accepted.send(()).unwrap();
        let mut bytes = [0; 2048];
        while matches!(socket.read(&mut bytes), Ok(n) if n > 0) {}
    });
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_firebird-plugin"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut input = process.0.stdin.take().unwrap();
    let output = process.0.stdout.take().unwrap();
    let (responses, received) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if responses.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let started = Instant::now();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"test_connection","params":{"params":{"host":"127.0.0.1","port":port,"username":"test","password":"test","database":"unused","connect_timeout_secs":if cancel {10}else{1}}}})).unwrap();
    input.flush().unwrap();
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    if cancel {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"cancel","params":{"id":1}})
        )
        .unwrap();
    }
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}})
    )
    .unwrap();
    drop(input);
    let first: Value = serde_json::from_str(
        &received
            .recv_timeout(Duration::from_secs(4))
            .expect("stalled operation was not interrupted"),
    )
    .unwrap();
    assert_eq!(first["id"], 1);
    assert_eq!(first["error"]["code"], if cancel { -32800 } else { -32001 });
    let second: Value =
        serde_json::from_str(&received.recv_timeout(Duration::from_secs(2)).unwrap()).unwrap();
    assert_eq!(second, json!({"jsonrpc":"2.0","id":2,"result":null}));
    assert!(started.elapsed() < Duration::from_secs(4));
    process.0.wait().unwrap();
    reader.join().unwrap();
    assert!(
        received.try_recv().is_err(),
        "cancel notifications must not emit responses"
    );
    server.join().unwrap();
}

#[test]
fn cancel_interrupts_a_stalled_handshake_and_keeps_stdio_usable() {
    stalled_handshake(true);
}

#[test]
fn connect_timeout_interrupts_a_stalled_handshake() {
    stalled_handshake(false);
}

#[test]
#[ignore = "mutates an isolated fixture; run tests/live-firebird.ps1"]
fn live_stdio_sessions_roll_back_on_eof() {
    struct Rpc {
        process: Process,
        input: Option<std::process::ChildStdin>,
        lines: mpsc::Receiver<String>,
    }
    impl Rpc {
        fn new() -> Self {
            let mut process = Process(
                Command::new(env!("CARGO_BIN_EXE_firebird-plugin"))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let input = process.0.stdin.take();
            let output = process.0.stdout.take().unwrap();
            let (sender, lines) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(output).lines() {
                    if sender.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
            Self {
                process,
                input,
                lines,
            }
        }
        fn query(&mut self, params: Value) -> Value {
            writeln!(
                self.input.as_mut().unwrap(),
                "{}",
                json!({"jsonrpc":"2.0","id":1,"method":"execute_query","params":params})
            )
            .unwrap();
            self.input.as_mut().unwrap().flush().unwrap();
            let response: Value =
                serde_json::from_str(&self.lines.recv_timeout(Duration::from_secs(7)).unwrap())
                    .unwrap();
            assert!(response.get("error").is_none(), "{response}");
            assert!(response["result"].get("error").is_none(), "{response}");
            response["result"].clone()
        }
    }
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
    )
    .unwrap();
    let table = format!(
        "TE_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let mut observer = Rpc::new();
    observer.query(json!({"params":config,"query":format!("CREATE TABLE {table}(ID INTEGER)")}));
    let mut owner = Rpc::new();
    owner.query(json!({"params":config,"query":"BEGIN","session_id":"editor"}));
    let first = owner.query(json!({"params":config,"query":"SELECT CURRENT_CONNECTION, CURRENT_TRANSACTION FROM RDB$DATABASE","session_id":"editor"}));
    assert_eq!(first["in_transaction"], true);
    owner.query(json!({"params":config,"query":format!("INSERT INTO {table} VALUES(1)"),"session_id":"editor"}));
    let second = owner.query(json!({"params":config,"query":"SELECT CURRENT_CONNECTION, CURRENT_TRANSACTION FROM RDB$DATABASE","session_id":"editor"}));
    assert_eq!(first["result"]["rows"], second["result"]["rows"]);
    assert_eq!(
        observer.query(json!({"params":config,"query":format!("SELECT COUNT(*) FROM {table}")}))
            ["rows"],
        json!([[0]])
    );
    owner.input.take();
    assert_eq!(
        owner
            .lines
            .recv_timeout(Duration::from_secs(7))
            .unwrap_err(),
        mpsc::RecvTimeoutError::Disconnected
    );
    assert!(owner.process.0.wait().unwrap().success());
    assert_eq!(
        observer.query(json!({"params":config,"query":format!("SELECT COUNT(*) FROM {table}")}))
            ["rows"],
        json!([[0]])
    );
    observer.query(json!({"params":config,"query":format!("DROP TABLE {table}")}));
}
