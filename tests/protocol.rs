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
