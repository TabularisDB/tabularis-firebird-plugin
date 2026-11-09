//! Entry point: read JSON-RPC lines from stdin, dispatch, write responses.

use std::io::{self, BufRead, Write};
use std::{
    collections::HashMap,
    sync::{mpsc, Arc, Mutex},
};

mod client;
mod control;
mod error;
mod handlers;
mod limits;
mod models;
mod rpc;
mod utils;

#[cfg(test)]
#[allow(dead_code)] // These structs validate deserialization, rather than consume fields.
mod host_contract_tests;

fn main() {
    let active = Arc::new(Mutex::new(HashMap::<String, Arc<control::Control>>::new()));
    let worker_active = active.clone();
    let (sender, receiver) =
        mpsc::sync_channel::<(String, Option<String>, Arc<control::Control>)>(32);
    let worker = std::thread::spawn(move || {
        for (line, key, control) in receiver {
            let response = control::run(control.clone(), || match control.check() {
                Ok(()) => rpc::handle_line(&line),
                Err(error) => rpc::error_response(
                    serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"].clone(),
                    error.code,
                    &error.message,
                ),
            });
            if let Some(key) = key {
                worker_active.lock().unwrap().remove(&key);
            }
            write_response(&response);
        }
    });
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<serde_json::Value>(&line);
        if let Ok(request) = &parsed {
            if request.get("method").and_then(|v| v.as_str()) == Some("cancel") {
                if let Some(id) = request.get("params").and_then(|p| p.get("id")) {
                    if let Some(control) = active.lock().unwrap().get(&id.to_string()) {
                        control.cancel();
                    }
                }
                continue;
            }
            if request.is_object() && request.get("id").is_none() {
                continue;
            }
        }
        let key = parsed
            .as_ref()
            .ok()
            .and_then(|r| r.get("id"))
            .map(|id| id.to_string());
        let control = Arc::new(control::Control::default());
        if let Some(key) = &key {
            let mut requests = active.lock().unwrap();
            if requests.contains_key(key) {
                write_response(&rpc::error_response(
                    parsed.as_ref().unwrap()["id"].clone(),
                    -32600,
                    "Request id is already in use",
                ));
                continue;
            }
            requests.insert(key.clone(), control.clone());
        }
        if let Err(error) = sender.try_send((line, key, control)) {
            let (mpsc::TrySendError::Full((line, key, _))
            | mpsc::TrySendError::Disconnected((line, key, _))) = error;
            if let Some(key) = key {
                active.lock().unwrap().remove(&key);
            }
            let id = serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .and_then(|r| r.get("id").cloned())
                .unwrap_or_default();
            write_response(&rpc::error_response(
                id,
                -32000,
                "Plugin request queue is full",
            ));
        }
    }
    drop(sender);
    let _ = worker.join();
}

fn write_response(response: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{response}");
    let _ = out.flush();
}
