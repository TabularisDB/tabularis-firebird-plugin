//! JSON-RPC dispatch and response helpers.

use serde_json::{json, Value};

use crate::handlers;

/// Parse one JSON-RPC line and return the response value (serialised
/// downstream by `main.rs`). Never panics — parse errors and method
/// failures are surfaced as JSON-RPC error responses.
pub fn handle_line(line: &str) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(err) => return error_response(Value::Null, -32700, &format!("parse error: {err}")),
    };

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    match method.as_str() {
        "initialize" => match crate::limits::initialize(&params) {
            Ok(()) => ok_response(id, Value::Null),
            Err(error) => error_response(id, error.code, &error.message),
        },
        "ping" => handlers::query::ping(id, &params),
        "test_connection" => handlers::query::test_connection(id, &params),

        // Metadata.
        "get_databases" => handlers::metadata::get_databases(id, &params),
        "get_connection_metadata" => handlers::metadata::get_connection_metadata(id, &params),
        "get_schemas" => handlers::metadata::get_schemas(id, &params),
        "get_tables" => handlers::metadata::get_tables(id, &params),
        "get_columns" => handlers::metadata::get_columns(id, &params),
        "get_foreign_keys" => handlers::metadata::get_foreign_keys(id, &params),
        "get_indexes" => handlers::metadata::get_indexes(id, &params),
        "get_views" => handlers::metadata::get_views(id, &params),
        "get_triggers" => handlers::triggers::get_triggers(id, &params),
        "get_trigger_definition" => handlers::triggers::get_trigger_definition(id, &params),
        "create_trigger" => handlers::triggers::mutate(id, &params, false),
        "drop_trigger" => handlers::triggers::mutate(id, &params, true),
        "get_sequences" => handlers::triggers::get_sequences(id, &params),
        "get_view_definition" => handlers::metadata::get_view_definition(id, &params),
        "get_view_columns" => handlers::metadata::get_view_columns(id, &params),
        "get_routines" => handlers::metadata::get_routines(id, &params),
        "get_routine_parameters" => handlers::metadata::get_routine_parameters(id, &params),
        "get_routine_definition" => handlers::metadata::get_routine_definition(id, &params),
        "build_routine_call_sql" => handlers::routines::build_routine_call_sql(id, &params),
        "get_schema_snapshot" => handlers::metadata::get_schema_snapshot(id, &params),
        "get_all_columns_batch" => handlers::metadata::get_all_columns_batch(id, &params),
        "get_all_foreign_keys_batch" => handlers::metadata::get_all_foreign_keys_batch(id, &params),

        // View mutation.
        "create_view" | "alter_view" | "drop_view" => handlers::ddl::mutate(id, &params, &method),

        // Query execution and SQL previews.
        "execute_query" => handlers::query::execute_query(id, &params),
        "execute_query_batch" => handlers::query::execute_query_batch(id, &params),
        "release_session" => handlers::query::release_session(id, &params),
        "get_table_query_template" => handlers::query::get_table_query_template(id, &params),
        "explain_query" => handlers::explain::explain_query(id, &params),

        // CRUD.
        "insert_record" => handlers::crud::insert_record(id, &params),
        "update_record" => handlers::crud::update_record(id, &params),
        "delete_record" => handlers::crud::delete_record(id, &params),

        // DDL.
        "get_create_table_sql" => handlers::ddl::get_create_table_sql(id, &params),
        "get_drop_table_sql" => handlers::ddl::drop_table_sql(id, &params),
        "get_add_column_sql" => handlers::ddl::get_add_column_sql(id, &params),
        "get_alter_column_sql" => handlers::ddl::get_alter_column_sql(id, &params),
        "get_create_index_sql" => handlers::ddl::get_create_index_sql(id, &params),
        "get_create_foreign_key_sql" => handlers::ddl::get_create_foreign_key_sql(id, &params),
        "drop_index" => handlers::ddl::drop_index(id, &params),
        "drop_foreign_key" => handlers::ddl::drop_foreign_key(id, &params),

        other => not_implemented(id, other),
    }
}

pub fn ok_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "result": result,
        "id": id,
    })
}

pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "error": { "code": code, "message": message },
        "id": id,
    })
}

/// Keep code -32601 and "Method not found" in the message: most host fallbacks
/// for optional methods (batch execution, materialized views, BLOBs, ...)
/// match on the message text, a few on the code.
pub fn not_implemented(id: Value, method: &str) -> Value {
    error_response(id, -32601, &format!("Method not found: {method}"))
}
