//! Required JSON fields consumed directly by Tabularis 0.31/0.32's
//! src-tauri/src/models.rs and plugins/driver.rs (not the guide's older examples).
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Column {
    name: String,
    data_type: String,
    is_pk: bool,
    is_nullable: bool,
    is_auto_increment: bool,
}
#[derive(Deserialize)]
struct ForeignKey {
    name: String,
    column_name: String,
    ref_table: String,
    ref_column: String,
    seq_in_fk: Option<i32>,
}
#[derive(Deserialize)]
struct Index {
    name: String,
    column_name: String,
    is_unique: bool,
    is_primary: bool,
    seq_in_index: i32,
}
#[derive(Deserialize)]
struct Routine {
    name: String,
    routine_type: String,
    identity_args: Option<String>,
}
#[derive(Deserialize)]
struct Trigger {
    name: String,
    table_name: String,
    event: String,
    timing: String,
    definition: Option<String>,
}
#[derive(Deserialize)]
struct Parameter {
    name: String,
    data_type: String,
    mode: String,
    ordinal_position: i32,
}
#[derive(Deserialize)]
struct Pagination {
    page: u32,
    page_size: u32,
    total_rows: Option<u64>,
    has_more: bool,
}
#[derive(Deserialize)]
struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    affected_rows: u64,
    pagination: Option<Pagination>,
}
#[derive(Deserialize)]
struct TableSchema {
    name: String,
    columns: Vec<Column>,
    foreign_keys: Vec<ForeignKey>,
}

#[derive(Deserialize)]
struct BatchResult {
    result: Option<QueryResult>,
    error: Option<String>,
    execution_time_ms: Option<f64>,
}
#[derive(Deserialize)]
struct SessionQuery {
    result: QueryResult,
    in_transaction: bool,
}
#[derive(Deserialize)]
struct SessionBatch {
    results: Vec<BatchResult>,
    in_transaction: bool,
}
#[derive(Deserialize)]
struct SessionError {
    error: String,
    in_transaction: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionMetadata {
    data_types: Vec<DataType>,
}
#[derive(Deserialize)]
struct DataType {
    name: String,
    category: DataTypeCategory,
    requires_length: bool,
    requires_precision: bool,
    default_length: Option<String>,
    #[serde(default)]
    supports_auto_increment: bool,
}

// ConnectionMetadataOverrides::resolve accepts these categories in the host.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum DataTypeCategory {
    Numeric,
    String,
    Date,
    Binary,
    Json,
    Spatial,
    Other,
}

#[test]
fn manifest_data_types_satisfy_connection_metadata_contract() {
    let manifest: Value = serde_json::from_str(include_str!("../.tabularium")).unwrap();
    validate_response(
        "get_connection_metadata",
        &serde_json::json!({"data_types": manifest["data_types"]}),
    );
}

pub fn validate_response(method: &str, value: &Value) {
    fn decode<T: serde::de::DeserializeOwned>(value: &Value) {
        serde_json::from_value::<T>(value.clone())
            .unwrap_or_else(|e| panic!("Tabularis response contract: {e}"));
    }
    match method {
        "execute_query" if value.get("in_transaction").is_some() => {
            if value.get("error").is_some() {
                decode::<SessionError>(value)
            } else {
                decode::<SessionQuery>(value)
            }
        }
        "execute_query_batch" if value.get("in_transaction").is_some() => {
            decode::<SessionBatch>(value)
        }
        "execute_query" => decode::<QueryResult>(value),
        "execute_query_batch" => decode::<Vec<BatchResult>>(value),
        "get_connection_metadata" => decode::<ConnectionMetadata>(value),
        "get_columns" | "get_view_columns" => decode::<Vec<Column>>(value),
        "get_foreign_keys" => decode::<Vec<ForeignKey>>(value),
        "get_indexes" => decode::<Vec<Index>>(value),
        "get_routines" => decode::<Vec<Routine>>(value),
        "get_triggers" => decode::<Vec<Trigger>>(value),
        "get_routine_parameters" => decode::<Vec<Parameter>>(value),
        "get_schema_snapshot" => decode::<Vec<TableSchema>>(value),
        "get_all_columns_batch" => decode::<std::collections::HashMap<String, Vec<Column>>>(value),
        "get_all_foreign_keys_batch" => {
            decode::<std::collections::HashMap<String, Vec<ForeignKey>>>(value)
        }
        _ => {}
    }
}

#[test]
fn guide_only_shapes_do_not_satisfy_the_host() {
    assert!(serde_json::from_value::<QueryResult>(
        serde_json::json!({"columns":[],"rows":[],"total_count":0})
    )
    .is_err());
    assert!(serde_json::from_value::<Index>(
        serde_json::json!({"index_name":"I","columns":["A"],"is_unique":false,"is_primary":false})
    )
    .is_err());
    assert!(serde_json::from_value::<ForeignKey>(serde_json::json!({"constraint_name":"FK","column_name":"A","referenced_table":"T","referenced_column":"ID"})).is_err());
    assert!(serde_json::from_value::<Parameter>(
        serde_json::json!({"name":"A","data_type":"INTEGER","mode":"IN"})
    )
    .is_err());
}
