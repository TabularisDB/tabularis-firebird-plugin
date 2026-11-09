//! Send real requests to the sibling plugin executable.
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Write},
    process::{Command, Stdio},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?
        .with_file_name(format!("firebird-plugin{}", std::env::consts::EXE_SUFFIX));
    let mut child = Command::new(executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!("Build the plugin first with cargo build --bin firebird-plugin: {e}")
        })?;
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    println!("Enter a method name or complete JSON-RPC request; help / exit.");
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut id = 0u64;
        for line in io::stdin().lock().lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if matches!(line, "exit" | "quit") {
                break;
            }
            if line == "help" {
                println!("A method name uses empty connection params; a JSON object sends your supplied params. Notifications have no response. Do not store real passwords in shell history.");
                continue;
            }
            id += 1;
            let request = if line.starts_with('{') {
                match serde_json::from_str::<Value>(line) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("Invalid JSON: {error}");
                        continue;
                    }
                }
            } else {
                json!({"jsonrpc":"2.0","id":id,"method":line,"params":{"params":{}}})
            };
            writeln!(input, "{request}")?;
            input.flush()?;
            if request.get("id").is_none() {
                continue;
            }
            let mut response = String::new();
            if output.read_line(&mut response)? == 0 {
                return Err("Plugin closed stdout".into());
            }
            let response: Value = serde_json::from_str(&response)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Ok(())
    })();
    drop(input);
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    result?;
    if !status.success() {
        return Err(format!("Plugin exited with {status}").into());
    }
    Ok(())
}
