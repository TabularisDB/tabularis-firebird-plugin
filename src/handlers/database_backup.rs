//! Transportable engine backups streamed through the encrypted Services API.
use super::metadata::respond;
use crate::{
    client::connect_socket,
    control::{self, Deadline},
    error::PluginError,
    models::{inner_params, ConnectionParams},
};
use rsfbclient_rust::{RustFbClientAttachmentConfig, RustFbService};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
    time::{SystemTime, UNIX_EPOCH},
};

fn invalid(message: &str) -> PluginError {
    PluginError::invalid_params(message)
}
fn io(error: std::io::Error) -> PluginError {
    PluginError::internal(format!("Firebird backup file: {error}"))
}
fn engine(error: rsfbclient_core::FbError) -> PluginError {
    control::current()
        .check()
        .err()
        .unwrap_or_else(|| PluginError::internal(format!("Firebird backup service: {error}")))
}

struct RemoveFile(PathBuf);
impl Drop for RemoveFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
struct PrivateFile {
    file: File,
    cleanup: RemoveFile,
}
impl PrivateFile {
    fn beside(path: &Path) -> Result<Self, PluginError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| PluginError::internal(e.to_string()))?
            .as_nanos();
        let path = path.with_file_name(format!(
            ".tabularis-firebird-backup-{}-{nonce}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(io)?;
        Ok(Self {
            file,
            cleanup: RemoveFile(path),
        })
    }
}

fn connect(params: &ConnectionParams) -> Result<RustFbService, PluginError> {
    let user = params
        .username
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid("username is required"))?;
    let pass = params
        .password
        .as_deref()
        .ok_or_else(|| invalid("password is required"))?;
    if params
        .ssl_mode
        .as_deref()
        .is_some_and(|s| !matches!(s, "" | "disable"))
    {
        return Err(invalid("TLS ssl_mode is not supported by this driver"));
    }
    let control = control::current();
    let _deadline = Deadline::connection(&control, params.connect_timeout_secs);
    let config = RustFbClientAttachmentConfig {
        host: params
            .host
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("localhost")
            .into(),
        port: params.port.unwrap_or(3050),
        db_name: "service_mgr".into(),
        user: user.into(),
        pass: pass.into(),
        role_name: params.role.clone(),
    };
    let socket = connect_socket(
        &config.host,
        config.port,
        params.connect_timeout_secs,
        &control,
    )?;
    let cancelled = Arc::new(AtomicBool::new(false));
    control.attach(&socket, &cancelled)?;
    RustFbService::from_socket(socket, &config, params.wire_options, Some(cancelled))
        .map_err(engine)
}

fn transfer(params: &Value, restore: bool) -> Result<Value, PluginError> {
    let connection = ConnectionParams::from_value(inner_params(params))?;
    let database = connection
        .database
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid("database must be a server-side path or alias"))?;
    let path = Path::new(
        params["file_path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("file_path is required"))?,
    );
    if restore && params["create_database"] != true {
        return Err(invalid("Native restore requires create_database=true and a new target path; existing databases are never replaced"));
    }
    let control = control::current();
    control.check()?;
    let mut private = PrivateFile::beside(path)?;
    let mut buffer = vec![0; 32768];
    let mut bytes = 0u64;
    if restore {
        // Read from one open handle into an owned snapshot before starting the engine.
        let mut source = File::open(path).map_err(io)?;
        if !source.metadata().map_err(io)?.is_file() {
            return Err(invalid("Backup input must be a regular file"));
        }
        loop {
            control.check()?;
            let length = source.read(&mut buffer).map_err(io)?;
            if length == 0 {
                break;
            }
            private.file.write_all(&buffer[..length]).map_err(io)?;
            bytes += length as u64;
        }
        if bytes == 0 {
            return Err(invalid("Backup input is empty"));
        }
        private.file.seek(SeekFrom::Start(0)).map_err(io)?;
    }
    let mut service = connect(&connection)?;
    let _deadline = Deadline::query(
        &control,
        connection
            .query_timeout_secs
            .unwrap_or(crate::limits::current().query_timeout_secs),
    );
    let result = (|| {
        if restore {
            service.start_restore(database)
        } else {
            service.start_backup(database)
        }
        .map_err(engine)?;
        let mut stdin = 0;
        let mut eof_sent = false;
        let mut sent = 0u64;
        loop {
            control.check()?;
            let input = if restore && stdin > 0 {
                if eof_sent {
                    return Err(invalid(
                        "Firebird requested more backup data after end of input",
                    ));
                }
                let size = stdin.min(buffer.len());
                let length = private.file.read(&mut buffer[..size]).map_err(io)?;
                sent += length as u64;
                eof_sent = length == 0;
                Some(&buffer[..length])
            } else {
                None
            };
            let output = service.query(restore, input).map_err(engine)?;
            if !restore {
                private.file.write_all(&output.data).map_err(io)?;
                bytes += output.data.len() as u64;
            }
            stdin = output.stdin;
            if !output.more {
                break;
            }
        }
        if restore && sent != bytes {
            return Err(invalid(
                "Firebird finished before consuming the complete backup",
            ));
        }
        if !restore && bytes == 0 {
            return Err(invalid("Firebird returned an empty backup"));
        }
        service.detach().map_err(engine)?;
        control.check()?;
        Ok(())
    })();
    if let Err(error) = result {
        if restore {
            return Err(PluginError {code:error.code, message:format!("{}; native restore may have left a partial new database. Check the target before retrying. No database was automatically deleted.", error.message)});
        }
        return Err(error);
    }
    if !restore {
        private.file.sync_all().map_err(io)?;
        control.check()?;
        // Close before rename/removal on Windows as well as Unix.
        let PrivateFile { cleanup, file } = private;
        drop(file);
        let result = fs::rename(&cleanup.0, path).map_err(io);
        result?;
    }
    Ok(
        json!({"format":"firebird", "bytes":bytes, "created_database":restore,"rows":0,"tables":0,"statements":0}),
    )
}

pub fn backup(id: Value, params: &Value) -> Value {
    respond(id, transfer(params, false))
}
pub fn restore(id: Value, params: &Value) -> Value {
    respond(id, transfer(params, true))
}
pub fn capabilities(id: Value) -> Value {
    respond(
        id,
        Ok(
            json!({"version":1,"format":"firebird","backup":true,"restore":true,"create_database":true,"replace_database":false,"local_client_required":false}),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_requires_explicit_new_database_before_connecting() {
        let result = transfer(
            &json!({"params":{"database":"unused"},"file_path":"missing.fbk"}),
            true,
        )
        .unwrap_err();
        assert!(result.message.contains("create_database=true"));
    }

    #[test]
    #[ignore = "requires an isolated Firebird server"]
    fn live_native_backup_restore_preserves_arrays_metadata_and_target() {
        use crate::{client::Client, models::ConnectionParams};
        let config: Value = serde_json::from_slice(
            &fs::read(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let base = config["database"].as_str().unwrap();
        let mut original = config.clone();
        original["database"] = json!(format!("{base}.native-source.fdb"));
        let mut target = config.clone();
        target["database"] = json!(format!("{base}.native-restored.fdb"));
        let file =
            PathBuf::from(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).with_extension("fbk");
        struct Cleanup(Vec<Value>, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for config in &self.0 {
                    if let Ok(client) = Client::connect_without_database_triggers(
                        ConnectionParams::from_value(config).unwrap(),
                    ) {
                        let _ = client.drop_created_database();
                    }
                }
                let _ = fs::remove_file(&self.1);
            }
        }
        let mut cleanup = Cleanup(vec![], file.clone());
        let mut source =
            Client::create_database(ConnectionParams::from_value(&original).unwrap(), None)
                .unwrap();
        cleanup.0.push(original.clone());
        for sql in [
            "CREATE TABLE NATIVE_DATA(ID INTEGER PRIMARY KEY, VALUES_ARRAY INTEGER[-1:1,2:3], CUSTOM BLOB SUB_TYPE -123)",
            "INSERT INTO NATIVE_DATA(ID) VALUES(42)",
            "COMMENT ON TABLE NATIVE_DATA IS 'native array roundtrip'",
            "CREATE PROCEDURE NATIVE_P RETURNS(N INTEGER) AS BEGIN N=42; SUSPEND; END",
        ] {source.execute(sql, vec![], 0, None, None).unwrap();}
        let password = config["password"].as_str().unwrap();
        source
            .execute(
                &format!(
                    "CREATE USER NATIVE_OWNER PASSWORD '{}'",
                    password.replace('\'', "''")
                ),
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        source
            .execute(
                "GRANT CREATE TABLE TO USER NATIVE_OWNER",
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        let mut owner_config = original.clone();
        owner_config["username"] = json!("NATIVE_OWNER");
        let mut owner =
            Client::connect(ConnectionParams::from_value(&owner_config).unwrap()).unwrap();
        owner
            .execute(
                "CREATE TABLE NATIVE_OTHER_OWNER(N INTEGER)",
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        owner
            .execute(
                "INSERT INTO NATIVE_OTHER_OWNER VALUES(9)",
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        drop(owner);
        // Exceed the SQL record and decoded-BLOB limits without buffering the backup.
        let mut state = 0x12345678u32;
        let bytes = (0..9 * 1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect::<Vec<_>>();
        source
            .execute(
                "UPDATE NATIVE_DATA SET CUSTOM=? WHERE ID=42",
                vec![rsfbclient_core::SqlType::Binary(bytes)],
                0,
                None,
                None,
            )
            .unwrap();
        #[cfg(windows)]
        if std::env::var_os("FIREBIRD_TEST_LIBRARY").is_some() {
            let mut config_file = PrivateFile::beside(&file).unwrap();
            serde_json::to_writer(&mut config_file.file, &original).unwrap();
            config_file.file.flush().unwrap();
            let status = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    "tests/native-array-fixture.ps1",
                    "-ConfigurationFile",
                ])
                .arg(&config_file.cleanup.0)
                .status()
                .unwrap();
            assert!(status.success(), "Native array fixture failed");
            let external = Path::new(base).with_extension("native-external.dat");
            source
                .execute(
                    &format!(
                        "CREATE TABLE NATIVE_EXTERNAL EXTERNAL FILE '{}' (N INTEGER)",
                        external.to_str().unwrap().replace('\'', "''")
                    ),
                    vec![],
                    0,
                    None,
                    None,
                )
                .unwrap();
            source
                .execute(
                    "INSERT INTO NATIVE_EXTERNAL VALUES(73)",
                    vec![],
                    0,
                    None,
                    None,
                )
                .unwrap();
        }
        source
            .execute(
                "CREATE EXCEPTION NATIVE_BLOCKED 'backup must suppress database triggers'",
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        source.execute("CREATE TRIGGER NATIVE_CONNECT ACTIVE ON CONNECT AS BEGIN EXCEPTION NATIVE_BLOCKED; END",vec![],0,None,None).unwrap();
        drop(source);
        let exported = crate::handlers::database_dump::export_sql(
            json!(1),
            &json!({"params":original,"file_path":file,"options":{"format":"firebird","structure":true,"data":true}}),
        );
        assert!(exported.get("error").is_none(), "{}", exported["error"]);
        let result = &exported["result"];
        assert!(result["bytes"].as_u64().unwrap() > 8 * 1024 * 1024);
        let response = crate::handlers::database_dump::import_sql(
            json!(2),
            &json!({"params":target,"file_path":file,"create_database":true}),
        );
        assert!(response.get("error").is_none(), "{}", response["error"]);
        let restored = &response["result"];
        assert_eq!(restored["created_database"], true);
        assert_eq!(fs::read(&file).unwrap()[0], 0);
        cleanup.0.push(target.clone());
        let mut copy = Client::connect_without_database_triggers(
            ConnectionParams::from_value(&target).unwrap(),
        )
        .unwrap();
        assert_eq!(
            copy.query(
                "SELECT ID, VALUES_ARRAY[-1,2] FROM NATIVE_DATA WHERE ID=42",
                0,
                None
            )
            .unwrap()["rows"],
            json!([[42, null]])
        );
        #[cfg(windows)]
        if std::env::var_os("FIREBIRD_TEST_LIBRARY").is_some() {
            assert_eq!(
                copy.query("SELECT N FROM NATIVE_EXTERNAL", 0, None)
                    .unwrap()["rows"],
                json!([[73]])
            );
            assert_eq!(copy.query("SELECT RDB$EXTERNAL_FILE FROM RDB$RELATIONS WHERE RDB$RELATION_NAME='NATIVE_EXTERNAL'",0,None).unwrap()["rows"],json!([[null]]));
            assert_eq!(copy.query("SELECT VALUES_ARRAY[-1,2],VALUES_ARRAY[-1,3],VALUES_ARRAY[0,2],VALUES_ARRAY[0,3],VALUES_ARRAY[1,2],VALUES_ARRAY[1,3] FROM NATIVE_DATA WHERE ID=43",0,None).unwrap()["rows"],json!([[i32::MIN,-1,0,42,i32::MAX,1234567]]));
        }
        assert_eq!(
            copy.query(
                "SELECT OCTET_LENGTH(CUSTOM) FROM NATIVE_DATA WHERE ID=42",
                0,
                None
            )
            .unwrap()["rows"],
            json!([[9 * 1024 * 1024]])
        );
        assert_eq!(copy.query("SELECT TRIM(RDB$OWNER_NAME) FROM RDB$RELATIONS WHERE RDB$RELATION_NAME='NATIVE_OTHER_OWNER'",0,None).unwrap()["rows"],json!([["NATIVE_OWNER"]]));
        assert_eq!(
            copy.query("SELECT N FROM NATIVE_OTHER_OWNER", 0, None)
                .unwrap()["rows"],
            json!([[9]])
        );
        assert_eq!(
            copy.query("SELECT N FROM NATIVE_P", 0, None).unwrap()["rows"],
            json!([[42]])
        );
        let failure = transfer(
            &json!({"params":target,"file_path":file,"create_database":true}),
            true,
        )
        .unwrap_err();
        assert!(failure.message.contains("Firebird"));
        assert_eq!(
            copy.query("SELECT ID FROM NATIVE_DATA WHERE ID=42", 0, None)
                .unwrap()["rows"],
            json!([[42]])
        );
        fs::write(&file, b"previous backup").unwrap();
        let mut missing = original;
        missing["database"] = json!(format!("{base}.does-not-exist.fdb"));
        assert!(transfer(&json!({"params":missing,"file_path":file}), false).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"previous backup");
    }
}
