//! Logical database scripts: one source snapshot, explicit restore records and atomic files.
use super::{
    crud::identifier,
    export::literal,
    metadata::{respond, FIELD_TYPE_SQL},
    routine_ddl,
    routines::Name,
    triggers,
};
use crate::{
    client::Client,
    error::PluginError,
    models::{inner_params, ConnectionParams},
};
use rsfbclient_core::{
    SqlType, TrDataAccessMode, TrIsolationLevel, TrLockResolution, TrOp, TransactionConfiguration,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_STATEMENT: usize = 8 * 1024 * 1024;
const HEADER: &str = "-- Tabularis Firebird SQL 1 ";

fn error(message: impl Into<String>) -> PluginError {
    PluginError::invalid_params(message)
}
fn io(error: std::io::Error) -> PluginError {
    PluginError::internal(format!("Database SQL file: {error}"))
}
fn text<'a>(row: &'a Value, key: &str) -> Result<&'a str, PluginError> {
    row[key]
        .as_str()
        .ok_or_else(|| error(format!("Missing catalog source: {key}")))
}
fn integer(row: &Value, key: &str) -> Result<i64, PluginError> {
    row[key]
        .as_i64()
        .or_else(|| row[key].as_str().and_then(|n| n.parse().ok()))
        .ok_or_else(|| error(format!("Missing or invalid catalog integer: {key}")))
}
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn name(row: &Value) -> Result<String, PluginError> {
    identifier(text(row, "name")?)
}
fn source(row: &Value, key: &str) -> Option<String> {
    row[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn security_clause(
    client: &mut Client,
    table: &str,
    key: &str,
    value: &str,
) -> Result<String, PluginError> {
    if !has_column(client, table, "RDB$SQL_SECURITY")? {
        return Ok(String::new());
    }
    let rows = client.query_objects(
        &format!("SELECT RDB$SQL_SECURITY AS S FROM {table} WHERE {key}=?"),
        vec![SqlType::Text(value.into())],
    )?;
    Ok(match rows.first().and_then(|row| row["S"].as_bool()) {
        Some(true) => " SQL SECURITY DEFINER",
        Some(false) => " SQL SECURITY INVOKER",
        None => "",
    }
    .into())
}
fn has_column(client: &mut Client, table: &str, column: &str) -> Result<bool, PluginError> {
    Ok(client.query_objects("SELECT COUNT(*) AS N FROM RDB$RELATION_FIELDS WHERE RDB$RELATION_NAME=? AND RDB$FIELD_NAME=?",vec![SqlType::Text(table.into()),SqlType::Text(column.into())])?[0]["N"]==1)
}

fn field_type(row: &Value) -> Result<String, PluginError> {
    let mut sql = text(row, "data_type")?.to_string();
    if sql.contains("UNKNOWN") || sql.contains(" ARRAY") {
        return Err(error(
            "Unknown/array field cannot be exported as SQL literals",
        ));
    }
    if matches!(row["field_type"].as_i64(), Some(14 | 37 | 261)) {
        if row["field_type"] == 261 && !matches!(row["subtype"].as_i64(), None | Some(0 | 1)) {
            return Err(error("Nonstandard BLOB subtypes cannot be exported"));
        }
        if let Some(charset) = source(row, "charset") {
            sql.push_str(&format!(" CHARACTER SET {}", identifier(&charset)?));
        }
    }
    Ok(sql)
}

struct Table {
    name: String,
    columns: Vec<Value>,
}
struct Schema {
    before: Vec<String>,
    after: Vec<String>,
    tables: Vec<Table>,
    header: Value,
}

fn comment(statements: &mut Vec<String>, kind: &str, target: &str, value: &Value) {
    if let Some(value) = value.as_str() {
        statements.push(format!("COMMENT ON {kind} {target} IS {}", quoted(value)));
    }
}

fn role_privileges(client: &mut Client, role: &Value) -> Result<String, PluginError> {
    if !has_column(client, "RDB$ROLES", "RDB$SYSTEM_PRIVILEGES")? {
        return Ok(String::new());
    }
    let fields = (1..=8)
        .map(|n| format!("ASCII_VAL(SUBSTRING(RDB$SYSTEM_PRIVILEGES FROM {n} FOR 1)) AS B{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = client.query_objects(
        &format!("SELECT {fields} FROM RDB$ROLES WHERE RDB$ROLE_NAME=?"),
        vec![SqlType::Text(text(role, "name")?.into())],
    )?;
    let mut bits = 0u64;
    for n in 0..8 {
        bits |= rows[0][format!("B{}", n + 1)].as_u64().unwrap_or(0) << (n * 8);
    }
    let names = [
        "USER_MANAGEMENT",
        "READ_RAW_PAGES",
        "CREATE_USER_TYPES",
        "USE_NBACKUP_UTILITY",
        "CHANGE_SHUTDOWN_MODE",
        "TRACE_ANY_ATTACHMENT",
        "MONITOR_ANY_ATTACHMENT",
        "ACCESS_SHUTDOWN_DATABASE",
        "CREATE_DATABASE",
        "DROP_DATABASE",
        "USE_GBAK_UTILITY",
        "USE_GSTAT_UTILITY",
        "USE_GFIX_UTILITY",
        "IGNORE_DB_TRIGGERS",
        "CHANGE_HEADER_SETTINGS",
        "SELECT_ANY_OBJECT_IN_DATABASE",
        "ACCESS_ANY_OBJECT_IN_DATABASE",
        "MODIFY_ANY_OBJECT_IN_DATABASE",
        "CHANGE_MAPPING_RULES",
        "USE_GRANTED_BY_CLAUSE",
        "GRANT_REVOKE_ON_ANY_OBJECT",
        "GRANT_REVOKE_ANY_DDL_RIGHT",
        "CREATE_PRIVILEGED_ROLES",
        "GET_DBCRYPT_INFO",
        "MODIFY_EXT_CONN_POOL",
        "REPLICATE_INTO_DATABASE",
        "PROFILE_ANY_ATTACHMENT",
    ];
    let mut privileges = vec![];
    for (index, name) in names.iter().enumerate() {
        let bit = 1u64 << (index + 1);
        if bits & bit != 0 {
            privileges.push(*name);
            bits &= !bit;
        }
    }
    if bits != 0 {
        return Err(error(
            "Unknown role system privileges cannot be exported losslessly",
        ));
    }
    Ok(if privileges.is_empty() {
        String::new()
    } else {
        format!(" SET SYSTEM PRIVILEGES TO {}", privileges.join(", "))
    })
}

fn routine(client: &mut Client, row: &Value, stub: bool) -> Result<String, PluginError> {
    let routine_name = Name {
        package: None,
        name: text(row, "name")?.into(),
    };
    let kind = text(row, "kind")?.trim();
    let header = routine_ddl::header(client, &routine_name, kind)?;
    if header["legacy"] == true {
        return if stub {
            legacy_function(client, row, &header)
        } else {
            Ok(format!(
                "ALTER EXTERNAL FUNCTION {} ENTRY_POINT {} MODULE_NAME {}",
                name(row)?,
                quoted(text(&header, "entrypoint")?),
                quoted(text(&header, "module")?)
            ))
        };
    }
    let parameters = routine_ddl::declarations(client, &routine_name, kind)?;
    let inputs = parameters
        .iter()
        .filter(|p| p["mode"] == 0)
        .map(|p| routine_ddl::declaration(p, true))
        .collect::<Result<Vec<_>, _>>()?;
    let outputs = parameters
        .iter()
        .filter(|p| p["mode"] == 1)
        .collect::<Vec<_>>();
    let mut sql = format!("CREATE OR ALTER {kind} {}", name(row)?);
    if !inputs.is_empty() {
        sql.push_str(&format!(" ({})", inputs.join(", ")));
    }
    if kind == "FUNCTION" {
        if outputs.len() != 1 {
            return Err(error("Function return metadata is incomplete"));
        }
        sql.push_str(&format!(
            " RETURNS {}",
            routine_ddl::declaration(outputs[0], false)?
        ));
        if header["deterministic"] == true {
            sql.push_str(" DETERMINISTIC");
        }
    } else if !outputs.is_empty() {
        sql.push_str(&format!(
            " RETURNS ({})",
            outputs
                .iter()
                .map(|p| routine_ddl::declaration(p, true))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ));
    }
    if let Some(security) = header["security"].as_bool() {
        sql.push_str(if security {
            " SQL SECURITY DEFINER"
        } else {
            " SQL SECURITY INVOKER"
        });
    }
    if let Some(engine) = source(&header, "engine") {
        return Ok(format!(
            "{sql} EXTERNAL NAME {} ENGINE {}{}",
            quoted(text(&header, "entrypoint")?),
            identifier(&engine)?,
            source(&header, "source")
                .map(|body| format!("\nAS\n{body}"))
                .unwrap_or_default()
        ));
    }
    if stub {
        sql.push_str(if kind == "FUNCTION" {
            " AS BEGIN RETURN NULL; END"
        } else if row["selectable"] == 1 {
            " AS BEGIN SUSPEND; END"
        } else {
            " AS BEGIN EXIT; END"
        });
    } else {
        let body = text(&header, "source")?;
        sql.push_str(
            if crate::utils::sql::top_level_words(body)
                .first()
                .is_some_and(|(_, word)| word.eq_ignore_ascii_case("AS"))
            {
                "\n"
            } else {
                "\nAS\n"
            },
        );
        sql.push_str(body);
    }
    Ok(sql)
}

fn legacy_function(
    client: &mut Client,
    row: &Value,
    header: &Value,
) -> Result<String, PluginError> {
    let types=FIELD_TYPE_SQL.replace("f.","a.").replace("a.RDB$DIMENSIONS","CAST(NULL AS SMALLINT)").replace("WHEN 261 THEN", "WHEN 40 THEN 'CSTRING(' || CAST(COALESCE(a.RDB$CHARACTER_LENGTH,a.RDB$FIELD_LENGTH) AS VARCHAR(6)) || ')' WHEN 45 THEN 'BLOB_ID' WHEN 261 THEN");
    let arguments=client.query_objects(&format!("SELECT {types} AS \"data_type\", a.RDB$FIELD_TYPE AS \"field_type\", a.RDB$FIELD_SUB_TYPE AS \"subtype\", TRIM(c.RDB$CHARACTER_SET_NAME) AS \"charset\", a.RDB$ARGUMENT_POSITION AS \"position\", a.RDB$MECHANISM AS \"mechanism\" FROM RDB$FUNCTION_ARGUMENTS a LEFT JOIN RDB$CHARACTER_SETS c ON c.RDB$CHARACTER_SET_ID=a.RDB$CHARACTER_SET_ID WHERE a.RDB$FUNCTION_NAME=? AND a.RDB$PACKAGE_NAME IS NULL ORDER BY a.RDB$ARGUMENT_POSITION"),vec![SqlType::Text(text(row,"name")?.into())])?;
    let returns=client.query_objects("SELECT RDB$RETURN_ARGUMENT AS N FROM RDB$FUNCTIONS WHERE RDB$FUNCTION_NAME=? AND RDB$PACKAGE_NAME IS NULL",vec![SqlType::Text(text(row,"name")?.into())])?;
    let returns = integer(&returns[0], "N")?;
    let mut inputs = vec![];
    let mut output = None;
    for argument in arguments {
        let position = integer(&argument, "position")?;
        let mechanism = integer(&argument, "mechanism")?;
        let mut declaration = field_type(&argument)?;
        if argument["field_type"] == 40 {
            if let Some(charset) = source(&argument, "charset") {
                declaration.push_str(&format!(" CHARACTER SET {}", identifier(&charset)?));
            }
        }
        if position == returns {
            let mut result = if returns > 0 {
                format!("PARAMETER {returns}")
            } else {
                match mechanism.abs() {
                    0 => format!("{declaration} BY VALUE"),
                    1 | 3 => declaration.clone(),
                    2 => format!("{declaration} BY DESCRIPTOR"),
                    _ => return Err(error("Unknown legacy return mechanism")),
                }
            };
            if mechanism < 0 {
                result.push_str(" FREE_IT");
            }
            output = Some(result);
        }
        if position > 0 {
            declaration.push_str(match mechanism.abs() {
                1 | 3 => "",
                2 => " BY DESCRIPTOR",
                4 => " BY SCALAR_ARRAY",
                5 => " NULL",
                _ => return Err(error("Unknown legacy argument mechanism")),
            });
            inputs.push(declaration);
        }
    }
    Ok(format!(
        "DECLARE EXTERNAL FUNCTION {} {} RETURNS {} ENTRY_POINT {} MODULE_NAME {}",
        name(row)?,
        inputs.join(", "),
        output.ok_or_else(|| error("Missing legacy return argument"))?,
        quoted(text(header, "entrypoint")?),
        quoted(text(header, "module")?)
    ))
}

fn ordered_views(client: &mut Client) -> Result<Vec<Value>, PluginError> {
    let mut pending=client.query_objects("SELECT TRIM(RDB$RELATION_NAME) AS \"name\", RDB$VIEW_SOURCE AS \"source\", RDB$DESCRIPTION AS \"comment\" FROM RDB$RELATIONS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 AND RDB$VIEW_BLR IS NOT NULL ORDER BY RDB$RELATION_ID",vec![])?;
    let dependencies=client.query_objects("SELECT DISTINCT TRIM(d.RDB$DEPENDENT_NAME) AS \"dependent\", TRIM(d.RDB$DEPENDED_ON_NAME) AS \"dependency\" FROM RDB$DEPENDENCIES d JOIN RDB$RELATIONS r ON r.RDB$RELATION_NAME=d.RDB$DEPENDED_ON_NAME WHERE r.RDB$VIEW_BLR IS NOT NULL AND d.RDB$DEPENDENT_TYPE IN (0,1)",vec![])?;
    let mut ordered = vec![];
    while !pending.is_empty() {
        let index = pending
            .iter()
            .position(|view| {
                !dependencies.iter().any(|edge| {
                    edge["dependent"] == view["name"]
                        && pending
                            .iter()
                            .any(|other| other["name"] == edge["dependency"])
                })
            })
            .ok_or_else(|| error("Cyclic view dependencies cannot be restored"))?;
        ordered.push(pending.remove(index));
    }
    Ok(ordered)
}

fn mappings(client: &mut Client, schema: &mut Schema) -> Result<(), PluginError> {
    for row in client.query_objects("SELECT TRIM(RDB$MAP_NAME) AS \"name\", TRIM(RDB$MAP_USING) AS \"using\", TRIM(RDB$MAP_PLUGIN) AS \"plugin\", TRIM(RDB$MAP_DB) AS \"database\", TRIM(RDB$MAP_FROM_TYPE) AS \"from_type\", TRIM(RDB$MAP_FROM) AS \"from\", RDB$MAP_TO_TYPE AS \"to_type\", TRIM(RDB$MAP_TO) AS \"to\", RDB$DESCRIPTION AS \"comment\" FROM RDB$AUTH_MAPPING WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$MAP_NAME",vec![])? {
        let mut using=match text(&row,"using")? {
            "P"=>if let Some(plugin)=source(&row,"plugin"){format!("PLUGIN {}",identifier(&plugin)?)}else{"ANY PLUGIN".into()},
            "S"=>"ANY PLUGIN SERVERWIDE".into(),"M"=>"MAPPING".into(),"*"=>"'*'".into(),_=>return Err(error("Unknown authentication mapping source")),
        };
        if let Some(database)=source(&row,"database"){using.push_str(&format!(" IN {}",identifier(&database)?));}
        let kind=text(&row,"from_type")?;
        let kind=if kind=="*"{"'*'".into()}else{identifier(kind)?};
        let from=match source(&row,"from").as_deref(){Some("*")|None=>format!("ANY {kind}"),Some(value)=>format!("{kind} {}",identifier(value)?)};
        let to=match integer(&row,"to_type")? {0=>"USER",1=>"ROLE",_=>return Err(error("Unknown authentication mapping target"))};
        let target=source(&row,"to").map(|to|identifier(&to).map(|to|format!(" {to}"))).transpose()?.unwrap_or_default();
        schema.after.push(format!("CREATE MAPPING {} USING {using} FROM {from} TO {to}{target}",name(&row)?));
        if !row["comment"].is_null(){return Err(error("Authentication mapping descriptions have no portable COMMENT syntax"));}
    }
    Ok(())
}

fn catalog(client: &mut Client) -> Result<Schema, PluginError> {
    let database=client.query_objects("SELECT TRIM(RDB$CHARACTER_SET_NAME) AS \"charset\", RDB$DESCRIPTION AS \"comment\", CURRENT_USER AS \"owner\", RDB$GET_CONTEXT('SYSTEM','ENGINE_VERSION') AS \"version\" FROM RDB$DATABASE",vec![])?;
    let major = text(&database[0], "version")?
        .split('.')
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .ok_or_else(|| error("Unknown engine version"))?;
    let mut schema = Schema {
        before: vec![],
        after: vec![],
        tables: vec![],
        header: json!({"engine_major":major,"charset":database[0]["charset"],"owner":database[0]["owner"]}),
    };
    if has_column(client, "RDB$DATABASE", "RDB$SQL_SECURITY")? {
        let security =
            client.query_objects("SELECT RDB$SQL_SECURITY AS S FROM RDB$DATABASE", vec![])?;
        if let Some(security) = security[0]["S"].as_bool() {
            schema.before.push(format!(
                "ALTER DATABASE SET SQL SECURITY {}",
                if security { "DEFINER" } else { "INVOKER" }
            ));
        }
    }
    for catalog in [
        "RDB$RELATIONS",
        "RDB$PROCEDURES",
        "RDB$FUNCTIONS",
        "RDB$PACKAGES",
        "RDB$GENERATORS",
        "RDB$EXCEPTIONS",
        "RDB$ROLES",
        "RDB$FIELDS",
        "RDB$COLLATIONS",
    ] {
        if has_column(client,catalog,"RDB$OWNER_NAME")? && client.query_objects(&format!("SELECT COUNT(*) AS N FROM {catalog} WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 AND RDB$OWNER_NAME<>CURRENT_USER"),vec![])?[0]["N"]!=0 {
            return Err(error("SQL export cannot preserve objects owned by another user; export as their owner or use an engine backup"));
        }
    }
    for (catalog, blr, source) in [
        ("RDB$FIELDS", "RDB$DEFAULT_VALUE", "RDB$DEFAULT_SOURCE"),
        ("RDB$FIELDS", "RDB$COMPUTED_BLR", "RDB$COMPUTED_SOURCE"),
        ("RDB$FIELDS", "RDB$VALIDATION_BLR", "RDB$VALIDATION_SOURCE"),
    ] {
        if client.query_objects(&format!("SELECT COUNT(*) AS N FROM {catalog} f WHERE COALESCE(f.RDB$SYSTEM_FLAG,0)=0 AND {blr} IS NOT NULL AND {source} IS NULL AND (f.RDB$FIELD_NAME NOT STARTING WITH 'RDB$' OR EXISTS(SELECT 1 FROM RDB$RELATION_FIELDS r JOIN RDB$RELATIONS t ON t.RDB$RELATION_NAME=r.RDB$RELATION_NAME WHERE r.RDB$FIELD_SOURCE=f.RDB$FIELD_NAME AND t.RDB$VIEW_BLR IS NULL AND COALESCE(t.RDB$SYSTEM_FLAG,0)=0))"),vec![])?[0]["N"]!=0 {return Err(error("Compiled expressions without their SQL source cannot be exported losslessly"));}
    }
    comment(&mut schema.after, "DATABASE", "", &database[0]["comment"]);
    for role in client.query_objects("SELECT TRIM(RDB$ROLE_NAME) AS \"name\", RDB$DESCRIPTION AS \"comment\" FROM RDB$ROLES WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$ROLE_NAME",vec![])? {
        schema.before.push(format!("CREATE ROLE {}{}",name(&role)?,role_privileges(client,&role)?));
        comment(&mut schema.after,"ROLE",&name(&role)?,&role["comment"]);
    }
    for collation in client.query_objects("SELECT TRIM(c.RDB$COLLATION_NAME) AS \"name\", TRIM(cs.RDB$CHARACTER_SET_NAME) AS \"charset\", TRIM(c.RDB$BASE_COLLATION_NAME) AS \"base\", c.RDB$COLLATION_ATTRIBUTES AS \"attributes\", c.RDB$SPECIFIC_ATTRIBUTES AS \"specific\", c.RDB$DESCRIPTION AS \"comment\" FROM RDB$COLLATIONS c JOIN RDB$CHARACTER_SETS cs ON cs.RDB$CHARACTER_SET_ID=c.RDB$CHARACTER_SET_ID WHERE COALESCE(c.RDB$SYSTEM_FLAG,0)=0 ORDER BY c.RDB$COLLATION_ID",vec![])? {
        let attributes=collation["attributes"].as_u64().ok_or_else(||error("Missing collation attributes"))?;
        if attributes&!7!=0{return Err(error("Unknown collation attributes"));}
        let mut sql=format!("CREATE COLLATION {} FOR {} FROM EXTERNAL ({}) {} {} {}",name(&collation)?,identifier(text(&collation,"charset")?)?,quoted(text(&collation,"base")?),if attributes&1!=0{"PAD SPACE"}else{"NO PAD"},if attributes&2!=0{"CASE INSENSITIVE"}else{"CASE SENSITIVE"},if attributes&4!=0{"ACCENT INSENSITIVE"}else{"ACCENT SENSITIVE"});
        if let Some(specific)=source(&collation,"specific"){sql.push_str(&format!(" {}",quoted(&specific)));}
        schema.before.push(sql);
        comment(&mut schema.after,"COLLATION",&name(&collation)?,&collation["comment"]);
    }
    for charset in client.query_objects("SELECT TRIM(RDB$CHARACTER_SET_NAME) AS \"name\", TRIM(RDB$DEFAULT_COLLATE_NAME) AS \"collation\", RDB$DESCRIPTION AS \"comment\" FROM RDB$CHARACTER_SETS ORDER BY RDB$CHARACTER_SET_ID",vec![])? {
        if charset["collation"]!=charset["name"]{schema.before.push(format!("ALTER CHARACTER SET {} SET DEFAULT COLLATION {}",name(&charset)?,identifier(text(&charset,"collation")?)?));}
        comment(&mut schema.after,"CHARACTER SET",&name(&charset)?,&charset["comment"]);
    }
    let types = format!(
        r#"SELECT TRIM(f.RDB$FIELD_NAME) AS "name", {FIELD_TYPE_SQL} AS "data_type", f.RDB$FIELD_TYPE AS "field_type", f.RDB$FIELD_SUB_TYPE AS "subtype", TRIM(cs.RDB$CHARACTER_SET_NAME) AS "charset", TRIM(c.RDB$COLLATION_NAME) AS "collation", f.RDB$NULL_FLAG AS "not_null", f.RDB$DEFAULT_SOURCE AS "default", f.RDB$VALIDATION_SOURCE AS "check", f.RDB$DESCRIPTION AS "comment" FROM RDB$FIELDS f LEFT JOIN RDB$CHARACTER_SETS cs ON cs.RDB$CHARACTER_SET_ID=f.RDB$CHARACTER_SET_ID LEFT JOIN RDB$COLLATIONS c ON c.RDB$CHARACTER_SET_ID=f.RDB$CHARACTER_SET_ID AND c.RDB$COLLATION_ID=COALESCE(f.RDB$COLLATION_ID,0) WHERE COALESCE(f.RDB$SYSTEM_FLAG,0)=0 AND f.RDB$FIELD_NAME NOT STARTING WITH 'RDB$' ORDER BY f.RDB$FIELD_NAME"#
    );
    for domain in client.query_objects(&types, vec![])? {
        let mut sql = format!(
            "CREATE DOMAIN {} AS {}",
            name(&domain)?,
            field_type(&domain)?
        );
        if let Some(default) = source(&domain, "default") {
            sql.push_str(&format!(" {default}"));
        }
        if domain["not_null"] == 1 {
            sql.push_str(" NOT NULL");
        }
        if let Some(collation) = source(&domain, "collation") {
            sql.push_str(&format!(" COLLATE {}", identifier(&collation)?));
        }
        schema.before.push(sql);
        if let Some(check) = source(&domain, "check") {
            schema
                .after
                .push(format!("ALTER DOMAIN {} ADD {check}", name(&domain)?));
        }
        comment(
            &mut schema.after,
            "DOMAIN",
            &name(&domain)?,
            &domain["comment"],
        );
    }
    for exception in client.query_objects("SELECT TRIM(RDB$EXCEPTION_NAME) AS \"name\", RDB$MESSAGE AS \"message\", RDB$DESCRIPTION AS \"comment\" FROM RDB$EXCEPTIONS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$EXCEPTION_NAME",vec![])? {
        schema.before.push(format!("CREATE EXCEPTION {} {}",name(&exception)?,quoted(text(&exception,"message")?)));
        comment(&mut schema.after,"EXCEPTION",&name(&exception)?,&exception["comment"]);
    }
    for filter in client.query_objects("SELECT TRIM(RDB$FUNCTION_NAME) AS \"name\", RDB$INPUT_SUB_TYPE AS \"input\", RDB$OUTPUT_SUB_TYPE AS \"output\", RDB$ENTRYPOINT AS \"entrypoint\", RDB$MODULE_NAME AS \"module\", RDB$DESCRIPTION AS \"comment\" FROM RDB$FILTERS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$FUNCTION_NAME",vec![])? {
        schema.before.push(format!("DECLARE FILTER {} INPUT_TYPE {} OUTPUT_TYPE {} ENTRY_POINT {} MODULE_NAME {}",name(&filter)?,integer(&filter,"input")?,integer(&filter,"output")?,quoted(text(&filter,"entrypoint")?),quoted(text(&filter,"module")?)));
        comment(&mut schema.after,"FILTER",&name(&filter)?,&filter["comment"]);
    }
    for sequence in client.query_objects("SELECT TRIM(RDB$GENERATOR_NAME) AS \"name\", RDB$DESCRIPTION AS \"comment\" FROM RDB$GENERATORS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$GENERATOR_NAME",vec![])? {
        let increment=if has_column(client,"RDB$GENERATORS","RDB$GENERATOR_INCREMENT")? {client.query_objects("SELECT RDB$GENERATOR_INCREMENT AS N FROM RDB$GENERATORS WHERE RDB$GENERATOR_NAME=?",vec![SqlType::Text(text(&sequence,"name")?.into())])?[0]["N"].as_i64().unwrap_or(1)} else {1};
        let initial=integer(&client.query_objects("SELECT RDB$INITIAL_VALUE AS N FROM RDB$GENERATORS WHERE RDB$GENERATOR_NAME=?",vec![SqlType::Text(text(&sequence,"name")?.into())])?[0],"N")?;
        schema.before.push(format!("CREATE SEQUENCE {} START WITH {initial} INCREMENT BY {increment}",name(&sequence)?));
        comment(&mut schema.after,"SEQUENCE",&name(&sequence)?,&sequence["comment"]);
    }
    let mut deferred_columns = vec![];
    let mut deferred_defaults = vec![];
    let mut table_statements = HashMap::new();
    let relations=client.query_objects("SELECT TRIM(RDB$RELATION_NAME) AS \"name\", RDB$DESCRIPTION AS \"comment\", RDB$RELATION_TYPE AS \"type\", RDB$EXTERNAL_FILE AS \"external\" FROM RDB$RELATIONS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 AND RDB$VIEW_BLR IS NULL ORDER BY RDB$RELATION_NAME",vec![])?;
    for relation in relations {
        if !relation["external"].is_null()
            || !matches!(relation["type"].as_i64(), None | Some(0 | 4 | 5))
        {
            return Err(error(
                "External tables need explicit target file mappings to avoid overwriting server files",
            ));
        }
        let table_name = text(&relation, "name")?.to_string();
        let columns=client.query_objects(&format!(r#"SELECT TRIM(r.RDB$FIELD_NAME) AS "name", TRIM(r.RDB$FIELD_SOURCE) AS "domain", {FIELD_TYPE_SQL} AS "data_type", f.RDB$FIELD_TYPE AS "field_type", f.RDB$FIELD_SUB_TYPE AS "subtype", f.RDB$CHARACTER_SET_ID AS "charset_id", TRIM(cs.RDB$CHARACTER_SET_NAME) AS "charset", TRIM(c.RDB$COLLATION_NAME) AS "collation", r.RDB$NULL_FLAG AS "not_null", f.RDB$NULL_FLAG AS "domain_not_null", EXISTS(SELECT 1 FROM RDB$RELATION_CONSTRAINTS p JOIN RDB$INDEX_SEGMENTS s ON s.RDB$INDEX_NAME=p.RDB$INDEX_NAME WHERE p.RDB$RELATION_NAME=r.RDB$RELATION_NAME AND p.RDB$CONSTRAINT_TYPE='PRIMARY KEY' AND s.RDB$FIELD_NAME=r.RDB$FIELD_NAME) AS "primary_key", (SELECT TRIM(c.RDB$CONSTRAINT_NAME) FROM RDB$RELATION_CONSTRAINTS c JOIN RDB$CHECK_CONSTRAINTS n ON n.RDB$CONSTRAINT_NAME=c.RDB$CONSTRAINT_NAME WHERE c.RDB$RELATION_NAME=r.RDB$RELATION_NAME AND c.RDB$CONSTRAINT_TYPE='NOT NULL' AND n.RDB$TRIGGER_NAME=r.RDB$FIELD_NAME) AS "null_constraint", r.RDB$DEFAULT_SOURCE AS "default", f.RDB$COMPUTED_SOURCE AS "computed", r.RDB$IDENTITY_TYPE AS "identity", TRIM(r.RDB$GENERATOR_NAME) AS "generator", r.RDB$DESCRIPTION AS "comment" FROM RDB$RELATION_FIELDS r JOIN RDB$FIELDS f ON f.RDB$FIELD_NAME=r.RDB$FIELD_SOURCE LEFT JOIN RDB$CHARACTER_SETS cs ON cs.RDB$CHARACTER_SET_ID=f.RDB$CHARACTER_SET_ID LEFT JOIN RDB$COLLATIONS c ON c.RDB$CHARACTER_SET_ID=f.RDB$CHARACTER_SET_ID AND c.RDB$COLLATION_ID=COALESCE(r.RDB$COLLATION_ID,f.RDB$COLLATION_ID,0) WHERE r.RDB$RELATION_NAME=? ORDER BY r.RDB$FIELD_POSITION"#),vec![SqlType::Text(table_name.clone())])?;
        let mut definitions = vec![];
        for column in &columns {
            let mut definition = format!("{} ", name(column)?);
            if let Some(computed) = source(column, "computed") {
                definition.push_str(&format!("COMPUTED BY {computed}"));
            } else {
                let data_type = if !text(column, "domain")?.starts_with("RDB$") {
                    identifier(text(column, "domain")?)?
                } else {
                    field_type(column)?
                };
                definition.push_str(&data_type);
                if let Some(identity) = column["identity"].as_i64() {
                    definition.push_str(if identity == 0 {
                        " GENERATED ALWAYS AS IDENTITY"
                    } else {
                        " GENERATED BY DEFAULT AS IDENTITY"
                    });
                    let generator = text(column, "generator")?;
                    let state=client.query_objects("SELECT RDB$INITIAL_VALUE AS \"initial\", RDB$GENERATOR_INCREMENT AS \"increment\" FROM RDB$GENERATORS WHERE RDB$GENERATOR_NAME=?",vec![SqlType::Text(generator.into())])?;
                    let initial = integer(&state[0], "initial")?;
                    let increment = state[0]["increment"]
                        .as_i64()
                        .ok_or_else(|| error("Missing identity increment"))?;
                    definition.push_str(&format!(" (START WITH {initial}"));
                    if major >= 4 {
                        definition.push_str(&format!(" INCREMENT BY {increment}"));
                    }
                    definition.push(')');
                }
                if let Some(default) = source(column, "default") {
                    deferred_defaults.push(format!(
                        "ALTER TABLE {} ALTER {} SET {default}",
                        name(&relation)?,
                        name(column)?
                    ));
                }
                // Identity, domain and primary-key nullability needs no new named constraint.
                if column["not_null"] == 1
                    && (column["null_constraint"].is_string()
                        || (column["identity"].is_null()
                            && column["primary_key"] != true
                            && column["domain_not_null"] != 1))
                {
                    if let Some(constraint) = source(column, "null_constraint") {
                        definition.push_str(&format!(" CONSTRAINT {}", identifier(&constraint)?));
                    }
                    definition.push_str(" NOT NULL");
                }
                if let Some(collation) = source(column, "collation") {
                    definition.push_str(&format!(" COLLATE {}", identifier(&collation)?));
                }
            }
            if source(column, "computed").is_some() {
                deferred_columns.push(format!("ALTER TABLE {} ADD {definition}", name(&relation)?));
            } else {
                definitions.push(definition);
            }
            comment(
                &mut schema.after,
                "COLUMN",
                &format!("{}.{}", name(&relation)?, name(column)?),
                &column["comment"],
            );
        }
        if definitions.is_empty() {
            return Err(error("Catalog table has no stored columns"));
        }
        table_statements.insert(table_name.clone(), schema.before.len());
        schema.before.push(format!(
            "CREATE {}TABLE {} (\n{}\n){}{}",
            if matches!(relation["type"].as_i64(), Some(4 | 5)) {
                "GLOBAL TEMPORARY "
            } else {
                ""
            },
            name(&relation)?,
            definitions.join(",\n"),
            match relation["type"].as_i64() {
                Some(4) => " ON COMMIT PRESERVE ROWS",
                Some(5) => " ON COMMIT DELETE ROWS",
                _ => "",
            },
            security_clause(client, "RDB$RELATIONS", "RDB$RELATION_NAME", &table_name)?
        ));
        comment(
            &mut schema.after,
            "TABLE",
            &name(&relation)?,
            &relation["comment"],
        );
        schema.tables.push(Table {
            name: table_name,
            columns,
        });
    }
    let constraints=client.query_objects(r#"SELECT TRIM(r.RDB$RELATION_NAME) AS "table", TRIM(r.RDB$CONSTRAINT_NAME) AS "name", TRIM(r.RDB$CONSTRAINT_TYPE) AS "kind", TRIM(s.RDB$FIELD_NAME) AS "column", s.RDB$FIELD_POSITION AS "position", TRIM(target.RDB$RELATION_NAME) AS "ref_table", TRIM(ts.RDB$FIELD_NAME) AS "ref_column", TRIM(ref.RDB$UPDATE_RULE) AS "on_update", TRIM(ref.RDB$DELETE_RULE) AS "on_delete" FROM RDB$RELATION_CONSTRAINTS r JOIN RDB$RELATIONS rel ON rel.RDB$RELATION_NAME=r.RDB$RELATION_NAME LEFT JOIN RDB$INDEX_SEGMENTS s ON s.RDB$INDEX_NAME=r.RDB$INDEX_NAME LEFT JOIN RDB$REF_CONSTRAINTS ref ON ref.RDB$CONSTRAINT_NAME=r.RDB$CONSTRAINT_NAME LEFT JOIN RDB$RELATION_CONSTRAINTS target ON target.RDB$CONSTRAINT_NAME=ref.RDB$CONST_NAME_UQ LEFT JOIN RDB$INDEX_SEGMENTS ts ON ts.RDB$INDEX_NAME=target.RDB$INDEX_NAME AND ts.RDB$FIELD_POSITION=s.RDB$FIELD_POSITION WHERE COALESCE(rel.RDB$SYSTEM_FLAG,0)=0 AND r.RDB$CONSTRAINT_TYPE IN ('PRIMARY KEY','UNIQUE','FOREIGN KEY') ORDER BY r.RDB$CONSTRAINT_NAME,s.RDB$FIELD_POSITION"#,vec![])?;
    let mut index = 0;
    while index < constraints.len() {
        let row = &constraints[index];
        let mut end = index + 1;
        while end < constraints.len() && constraints[end]["name"] == row["name"] {
            end += 1;
        }
        let columns = constraints[index..end]
            .iter()
            .map(|r| identifier(text(r, "column")?))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        let mut sql = format!(
            "ALTER TABLE {} ADD CONSTRAINT {} {} ({columns})",
            identifier(text(row, "table")?)?,
            name(row)?,
            text(row, "kind")?
        );
        if row["kind"] == "FOREIGN KEY" {
            let referenced = constraints[index..end]
                .iter()
                .map(|r| identifier(text(r, "ref_column")?))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            sql.push_str(&format!(
                " REFERENCES {} ({referenced}) ON UPDATE {} ON DELETE {}",
                identifier(text(row, "ref_table")?)?,
                text(row, "on_update")?,
                text(row, "on_delete")?
            ));
        }
        let backing=client.query_objects("SELECT TRIM(c.RDB$INDEX_NAME) AS \"name\", i.RDB$INDEX_TYPE AS \"descending\", i.RDB$DESCRIPTION AS \"comment\" FROM RDB$RELATION_CONSTRAINTS c JOIN RDB$INDICES i ON i.RDB$INDEX_NAME=c.RDB$INDEX_NAME WHERE c.RDB$CONSTRAINT_NAME=?",vec![SqlType::Text(text(row,"name")?.into())])?;
        if let Some(backing) = backing.first() {
            sql.push_str(&format!(
                " USING {} INDEX {}",
                if backing["descending"] == 1 {
                    "DESCENDING"
                } else {
                    "ASCENDING"
                },
                name(backing)?
            ));
        }
        if row["kind"] == "FOREIGN KEY" {
            schema.after.push(sql);
        } else {
            // Inline keys retain the engine's implicit NOT NULL semantics.
            let prefix = format!("ALTER TABLE {} ADD ", identifier(text(row, "table")?)?);
            let clause = sql
                .strip_prefix(&prefix)
                .ok_or_else(|| error("Invalid key declaration"))?;
            let create = &mut schema.before[table_statements[text(row, "table")?]];
            let position = create
                .rfind("\n)")
                .ok_or_else(|| error("Invalid table declaration"))?;
            create.insert_str(position, &format!(",\n{clause}"));
        }
        if let Some(backing) = backing.first() {
            comment(
                &mut schema.after,
                "INDEX",
                &name(backing)?,
                &backing["comment"],
            );
        }
        index = end;
    }
    for check in client.query_objects(r#"SELECT TRIM(r.RDB$RELATION_NAME) AS "table", TRIM(r.RDB$CONSTRAINT_NAME) AS "name", t.RDB$TRIGGER_SOURCE AS "source" FROM RDB$RELATION_CONSTRAINTS r JOIN RDB$CHECK_CONSTRAINTS c ON c.RDB$CONSTRAINT_NAME=r.RDB$CONSTRAINT_NAME JOIN RDB$TRIGGERS t ON t.RDB$TRIGGER_NAME=c.RDB$TRIGGER_NAME WHERE r.RDB$CONSTRAINT_TYPE='CHECK' AND c.RDB$TRIGGER_NAME=(SELECT MIN(c2.RDB$TRIGGER_NAME) FROM RDB$CHECK_CONSTRAINTS c2 WHERE c2.RDB$CONSTRAINT_NAME=c.RDB$CONSTRAINT_NAME) ORDER BY 1,2"#,vec![])? {
        schema.after.push(format!("ALTER TABLE {} ADD CONSTRAINT {} {}",identifier(text(&check,"table")?)?,name(&check)?,text(&check,"source")?));
    }
    let routines=client.query_objects(r#"SELECT TRIM(RDB$PROCEDURE_NAME) AS "name", 'PROCEDURE' AS "kind", RDB$PROCEDURE_TYPE AS "selectable", RDB$DESCRIPTION AS "comment" FROM RDB$PROCEDURES WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 AND RDB$PACKAGE_NAME IS NULL UNION ALL SELECT TRIM(RDB$FUNCTION_NAME), 'FUNCTION', 0, RDB$DESCRIPTION FROM RDB$FUNCTIONS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 AND RDB$PACKAGE_NAME IS NULL ORDER BY 2,1"#,vec![])?;
    for row in &routines {
        schema.before.push(routine(client, row, true)?);
    }
    let mut package_bodies = vec![];
    for package in client.query_objects("SELECT TRIM(RDB$PACKAGE_NAME) AS \"name\", RDB$PACKAGE_HEADER_SOURCE AS \"header\", RDB$PACKAGE_BODY_SOURCE AS \"body\", RDB$DESCRIPTION AS \"comment\" FROM RDB$PACKAGES WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$PACKAGE_NAME",vec![])? {
        schema.before.push(format!("CREATE PACKAGE {}{} AS\n{}",name(&package)?,security_clause(client,"RDB$PACKAGES","RDB$PACKAGE_NAME",text(&package,"name")?)?,text(&package,"header")?));
        if let Some(body)=source(&package,"body") {package_bodies.push(format!("CREATE PACKAGE BODY {} AS\n{body}",name(&package)?));}
        comment(&mut schema.after,"PACKAGE",&name(&package)?,&package["comment"]);
    }
    schema.before.extend(deferred_columns);
    for table in &schema.tables {
        for (position, column) in table.columns.iter().enumerate() {
            schema.before.push(format!(
                "ALTER TABLE {} ALTER {} POSITION {}",
                identifier(&table.name)?,
                name(column)?,
                position + 1
            ));
        }
    }
    for view in ordered_views(client)? {
        let columns=client.query_objects("SELECT TRIM(RDB$FIELD_NAME) AS \"name\", RDB$DESCRIPTION AS \"comment\" FROM RDB$RELATION_FIELDS WHERE RDB$RELATION_NAME=? ORDER BY RDB$FIELD_POSITION",vec![SqlType::Text(text(&view,"name")?.into())])?;
        let names = columns
            .iter()
            .map(name)
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        schema.before.push(format!(
            "CREATE VIEW {} ({names}) AS\n{}",
            name(&view)?,
            text(&view, "source")?
        ));
        for column in columns {
            comment(
                &mut schema.after,
                "COLUMN",
                &format!("{}.{}", name(&view)?, name(&column)?),
                &column["comment"],
            );
        }
        comment(&mut schema.after, "VIEW", &name(&view)?, &view["comment"]);
    }
    for row in &routines {
        schema.before.push(routine(client, row, false)?);
        comment(
            &mut schema.after,
            text(row, "kind")?,
            &name(row)?,
            &row["comment"],
        );
        for parameter in routine_ddl::declarations(
            client,
            &Name {
                package: None,
                name: text(row, "name")?.into(),
            },
            text(row, "kind")?.trim(),
        )? {
            if parameter["name"].is_string() {
                comment(
                    &mut schema.after,
                    "PARAMETER",
                    &format!("{}.{}", name(row)?, name(&parameter)?),
                    &parameter["comment"],
                );
            }
        }
    }
    schema.before.extend(package_bodies);
    schema.before.extend(deferred_defaults);
    indexes(client, &mut schema)?;
    for trigger in client.query_objects("SELECT TRIM(RDB$TRIGGER_NAME) AS \"name\", TRIM(RDB$RELATION_NAME) AS \"table\", RDB$TRIGGER_TYPE AS \"type\", RDB$TRIGGER_SOURCE AS \"source\", RDB$TRIGGER_SEQUENCE AS \"position\", RDB$TRIGGER_INACTIVE AS \"inactive\", RDB$DESCRIPTION AS \"comment\" FROM RDB$TRIGGERS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$TRIGGER_NAME",vec![])? {
        let (timing,event)=triggers::event(integer(&trigger,"type")?);
        if timing=="UNKNOWN"||event.contains("UNKNOWN") {return Err(error("Unknown trigger event cannot be exported"));}
        let table=source(&trigger,"table").map(|t|identifier(&t).map(|t|format!(" FOR {t}"))).transpose()?.unwrap_or_default();
        schema.after.push(format!("CREATE TRIGGER {}{table} {} {timing} {event} POSITION {}{}\n{}",name(&trigger)?,if trigger["inactive"]==1 {"INACTIVE"}else{"ACTIVE"},trigger["position"].as_i64().unwrap_or(0),security_clause(client,"RDB$TRIGGERS","RDB$TRIGGER_NAME",text(&trigger,"name")?)?,text(&trigger,"source")?));
        comment(&mut schema.after,"TRIGGER",&name(&trigger)?,&trigger["comment"]);
    }
    permissions(client, &mut schema)?;
    mappings(client, &mut schema)?;
    Ok(schema)
}

fn indexes(client: &mut Client, schema: &mut Schema) -> Result<(), PluginError> {
    let condition = if has_column(client, "RDB$INDICES", "RDB$CONDITION_SOURCE")? {
        "i.RDB$CONDITION_SOURCE"
    } else {
        "CAST(NULL AS VARCHAR(1))"
    };
    let rows=client.query_objects(&format!(r#"SELECT TRIM(i.RDB$INDEX_NAME) AS "name", TRIM(i.RDB$RELATION_NAME) AS "table", i.RDB$UNIQUE_FLAG AS "unique", i.RDB$INDEX_TYPE AS "descending", i.RDB$INDEX_INACTIVE AS "inactive", i.RDB$EXPRESSION_SOURCE AS "expression", i.RDB$DESCRIPTION AS "comment", {condition} AS "condition", TRIM(s.RDB$FIELD_NAME) AS "column" FROM RDB$INDICES i LEFT JOIN RDB$INDEX_SEGMENTS s ON s.RDB$INDEX_NAME=i.RDB$INDEX_NAME WHERE COALESCE(i.RDB$SYSTEM_FLAG,0)=0 AND NOT EXISTS(SELECT 1 FROM RDB$RELATION_CONSTRAINTS c WHERE c.RDB$INDEX_NAME=i.RDB$INDEX_NAME) ORDER BY i.RDB$INDEX_NAME,s.RDB$FIELD_POSITION"#),vec![])?;
    let mut index = 0;
    while index < rows.len() {
        let row = &rows[index];
        let mut end = index + 1;
        while end < rows.len() && rows[end]["name"] == row["name"] {
            end += 1;
        }
        let expression = if let Some(expression) = source(row, "expression") {
            format!("COMPUTED BY {expression}")
        } else {
            format!(
                "({})",
                rows[index..end]
                    .iter()
                    .map(|r| identifier(text(r, "column")?))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            )
        };
        let mut sql = format!(
            "CREATE {}{}INDEX {} ON {} {expression}",
            if row["unique"] == 1 { "UNIQUE " } else { "" },
            if row["descending"] == 1 {
                "DESCENDING "
            } else {
                ""
            },
            name(row)?,
            identifier(text(row, "table")?)?
        );
        if let Some(condition) = source(row, "condition") {
            sql.push_str(&format!(" {condition}"));
        }
        schema.after.push(sql);
        if row["inactive"] == 1 {
            schema
                .after
                .push(format!("ALTER INDEX {} INACTIVE", name(row)?));
        }
        comment(&mut schema.after, "INDEX", &name(row)?, &row["comment"]);
        index = end;
    }
    Ok(())
}

fn object_type(code: i64) -> Result<&'static str, PluginError> {
    Ok(match code {
        0 | 1 => "TABLE",
        2 => "TRIGGER",
        5 => "PROCEDURE",
        7 => "EXCEPTION",
        8 => "USER",
        9 => "DOMAIN",
        11 => "CHARACTER SET",
        13 => "ROLE",
        14 => "SEQUENCE",
        15 => "FUNCTION",
        16 => "FILTER",
        17 => "COLLATION",
        18 => "PACKAGE",
        _ => return Err(error(format!("Unknown privilege object type {code}"))),
    })
}

fn permissions(client: &mut Client, schema: &mut Schema) -> Result<(), PluginError> {
    let rows=client.query_objects(r#"SELECT TRIM(RDB$USER) AS "grantee", TRIM(RDB$GRANTOR) AS "grantor", TRIM(RDB$PRIVILEGE) AS "privilege", RDB$GRANT_OPTION AS "grant_option", TRIM(RDB$RELATION_NAME) AS "target", TRIM(RDB$FIELD_NAME) AS "column", RDB$USER_TYPE AS "user_type", RDB$OBJECT_TYPE AS "object_type" FROM RDB$USER_PRIVILEGES ORDER BY RDB$OBJECT_TYPE,RDB$RELATION_NAME,RDB$USER,RDB$PRIVILEGE,RDB$FIELD_NAME"#,vec![])?;
    for row in rows {
        // Engine-created implicit privileges have no SQL grantor and are recreated by DDL.
        if row["grantor"].is_null() {
            continue;
        }
        let grantee = text(&row, "grantee")?;
        let grantor = text(&row, "grantor")?;
        if grantee == text(&schema.header, "owner")? && grantor == grantee {
            continue;
        }
        let user_type = row["user_type"]
            .as_i64()
            .ok_or_else(|| error("Missing privilege grantee type"))?;
        let recipient = if grantee == "PUBLIC" {
            "PUBLIC".to_string()
        } else {
            format!("{} {}", object_type(user_type)?, identifier(grantee)?)
        };
        let object = row["object_type"]
            .as_i64()
            .ok_or_else(|| error("Missing privilege object type"))?;
        let target = text(&row, "target")?;
        let option = row["grant_option"].as_u64().unwrap_or(0);
        let mut sql = if row["privilege"] == "M" {
            format!(
                "GRANT {}{} TO {recipient}{}",
                if row["column"] == "D" { "DEFAULT " } else { "" },
                identifier(target)?,
                if option > 0 { " WITH ADMIN OPTION" } else { "" }
            )
        } else {
            let privilege = match text(&row, "privilege")? {
                "A" => "ALL",
                "S" => "SELECT",
                "I" => "INSERT",
                "D" => "DELETE",
                "R" => "REFERENCES",
                "U" => "UPDATE",
                "X" => "EXECUTE",
                "G" => "USAGE",
                "C" => "CREATE",
                "L" => "ALTER",
                "O" => "DROP",
                unknown => return Err(error(format!("Unknown privilege {unknown}"))),
            };
            let column = source(&row, "column")
                .map(|c| identifier(&c).map(|c| format!(" ({c})")))
                .transpose()?
                .unwrap_or_default();
            let clause = if privilege == "CREATE" {
                object_type(object)?.to_string()
            } else if target.is_empty() {
                format!("ANY {}", object_type(object)?)
            } else if matches!(object, 0 | 1) {
                format!("ON {}", identifier(target)?)
            } else {
                format!("ON {} {}", object_type(object)?, identifier(target)?)
            };
            format!(
                "GRANT {privilege}{column} {clause} TO {recipient}{}",
                if option > 0 { " WITH GRANT OPTION" } else { "" }
            )
        };
        sql.push_str(&format!(" GRANTED BY {}", identifier(grantor)?));
        schema.after.push(sql);
    }
    Ok(())
}

fn generator_states(client: &mut Client, schema: &Schema) -> Result<Vec<String>, PluginError> {
    let mut statements = vec![];
    for row in client.query_objects("SELECT TRIM(RDB$GENERATOR_NAME) AS \"name\" FROM RDB$GENERATORS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 ORDER BY RDB$GENERATOR_NAME",vec![])? {
        let value=client.query(&format!("SELECT GEN_ID({},0) FROM RDB$DATABASE",name(&row)?),0,None)?["rows"][0][0].clone();
        let value=value.as_str().map(str::to_string).unwrap_or_else(||value.to_string());
        value.parse::<i64>().map_err(|_|error("Invalid sequence counter"))?;
        statements.push(format!("SET GENERATOR {} TO {value}",name(&row)?));
    }
    for table in &schema.tables {
        for column in &table.columns {
            if let Some(generator) = source(column, "generator") {
                let value = client.query(
                    &format!(
                        "SELECT GEN_ID({},0) FROM RDB$DATABASE",
                        identifier(&generator)?
                    ),
                    0,
                    None,
                )?["rows"][0][0]
                    .clone();
                let current = value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
                    .ok_or_else(|| error("Invalid identity counter"))?;
                let step = if has_column(client, "RDB$GENERATORS", "RDB$GENERATOR_INCREMENT")? {
                    client.query_objects("SELECT RDB$GENERATOR_INCREMENT AS N FROM RDB$GENERATORS WHERE RDB$GENERATOR_NAME=?",vec![SqlType::Text(generator)])?[0]["N"].as_i64().unwrap_or(1)
                } else {
                    1
                };
                let restart = if schema.header["engine_major"] == 3 {
                    current
                } else {
                    current
                        .checked_add(step)
                        .ok_or_else(|| error("Identity counter overflow"))?
                };
                statements.push(format!(
                    "ALTER TABLE {} ALTER {} RESTART WITH {restart}",
                    identifier(&table.name)?,
                    name(column)?
                ));
            }
        }
    }
    Ok(statements)
}

struct Pending(PathBuf);
impl Drop for Pending {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn statement(writer: &mut impl Write, sql: &str) -> Result<(), PluginError> {
    crate::control::current().check()?;
    let sql = sql.trim().trim_end_matches(';');
    if sql.is_empty() || sql.len() > MAX_STATEMENT {
        return Err(error("SQL dump statement exceeds the 8 MiB limit"));
    }
    writeln!(writer, "-- TABULARIS-STATEMENT {}\n{sql}\n^", sql.len()).map_err(io)
}

fn export(params: &Value) -> Result<Value, PluginError> {
    let include_data = match params.get("options").and_then(|o| o.get("data")) {
        None | Some(Value::Bool(true)) => true,
        Some(Value::Bool(false)) => false,
        _ => return Err(error("options.data must be boolean")),
    };
    match params.get("options").and_then(|o| o.get("structure")) {
        Some(Value::Bool(false)) => return Ok(Value::Null),
        None | Some(Value::Bool(true)) => {}
        _ => return Err(error("options.structure must be boolean")),
    }
    let path = Path::new(text(params, "file_path")?);
    if !path.is_absolute() {
        return Err(error("file_path must be absolute"));
    }
    let connection = ConnectionParams::from_value(inner_params(params))?;
    let control = crate::control::current();
    let _deadline = crate::control::Deadline::query(
        &control,
        connection
            .query_timeout_secs
            .unwrap_or(crate::limits::current().query_timeout_secs),
    );
    let mut client = Client::connect_without_database_triggers(connection)?;
    client.begin(TransactionConfiguration {
        data_access: TrDataAccessMode::ReadOnly,
        isolation: TrIsolationLevel::Concurrency,
        lock_resolution: TrLockResolution::NoWait,
    })?;
    let schema = catalog(&mut client)?;
    if let Some(tables) = params
        .get("options")
        .and_then(|o| o.get("tables"))
        .filter(|t| !t.is_null())
    {
        let selected = tables
            .as_array()
            .ok_or_else(|| error("options.tables must be an array"))?;
        let all = schema
            .tables
            .iter()
            .map(|t| t.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let selected = selected
            .iter()
            .map(|t| {
                t.as_str()
                    .ok_or_else(|| error("Table names must be strings"))
            })
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        if selected != all {
            return Err(error(
                "A full schema export requires all tables; select data-only for a table subset",
            ));
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| error(e.to_string()))?
        .as_nanos();
    let pending = path.with_file_name(format!(
        ".tabularis-firebird-{}-{stamp}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&pending).map_err(io)?;
    let _cleanup = Pending(pending.clone());
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "{HEADER}{}\nSET SQL DIALECT 3;\nSET TERM ^ ;",
        schema.header
    )
    .map_err(io)?;
    for sql in &schema.before {
        statement(&mut writer, sql)?;
    }
    let mut count = 0;
    for table in &schema.tables {
        if !include_data {
            break;
        }
        let columns = table
            .columns
            .iter()
            .filter(|c| c["computed"].is_null())
            .collect::<Vec<_>>();
        let names = columns
            .iter()
            .map(|c| name(c))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        let qualified = identifier(&table.name)?;
        let overriding = columns.iter().any(|c| c["identity"] == 0);
        let projection = if names.is_empty() { "1" } else { &names };
        count += client.stream(&format!("SELECT {projection} FROM {qualified}"), |row| {
            let values = row
                .iter()
                .zip(&columns)
                .map(|(value, column)| {
                    literal(
                        value,
                        (column["field_type"] == 261 && column["subtype"] != 1)
                            || (matches!(column["field_type"].as_i64(), Some(14 | 37))
                                && column["charset_id"] == 1),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            if columns.is_empty() {
                return statement(
                    &mut writer,
                    &format!("INSERT INTO {qualified} DEFAULT VALUES"),
                );
            }
            statement(
                &mut writer,
                &format!(
                    "INSERT INTO {qualified} ({names}){} VALUES ({values})",
                    if overriding {
                        " OVERRIDING SYSTEM VALUE"
                    } else {
                        ""
                    }
                ),
            )
        })?;
    }
    for sql in &schema.after {
        statement(&mut writer, sql)?;
    }
    for sql in generator_states(&mut client, &schema)? {
        statement(&mut writer, &sql)?;
    }
    statement(&mut writer, "COMMIT")?;
    client.finish(TrOp::Rollback)?;
    writeln!(writer, "SET TERM ; ^").map_err(io)?;
    writer.flush().map_err(io)?;
    writer.get_ref().sync_all().map_err(io)?;
    drop(writer);
    crate::control::current().check()?;
    fs::rename(&pending, path).map_err(io)?;
    Ok(json!({"rows":count,"tables":schema.tables.len()}))
}

pub fn export_sql(id: Value, params: &Value) -> Value {
    respond(id, export(params))
}

fn line(reader: &mut impl BufRead) -> Result<String, PluginError> {
    let mut line = String::new();
    (&mut *reader)
        .take(16385)
        .read_line(&mut line)
        .map_err(io)?;
    if line.len() > 16384 {
        return Err(error("SQL dump control line is too long"));
    }
    Ok(line)
}

fn read_header(reader: &mut impl BufRead) -> Result<Value, PluginError> {
    let first = line(reader)?;
    let header: Value = serde_json::from_str(
        first
            .strip_prefix(HEADER)
            .ok_or_else(|| error("Not a Tabularis Firebird SQL dump"))?
            .trim(),
    )
    .map_err(|e| error(e.to_string()))?;
    if !matches!(header["engine_major"].as_u64(), Some(3..=5))
        || header["charset"].as_str().is_none()
        || header["owner"].as_str().is_none()
    {
        return Err(error("Invalid SQL dump header"));
    }
    if line(reader)? != "SET SQL DIALECT 3;\n" || line(reader)? != "SET TERM ^ ;\n" {
        return Err(error("SQL dump prologue is invalid"));
    }
    Ok(header)
}

fn read_statement(reader: &mut impl BufRead) -> Result<Option<String>, PluginError> {
    crate::control::current().check()?;
    let record = line(reader)?;
    if record == "SET TERM ; ^\n" {
        let mut trailing = [0; 1];
        if reader.read(&mut trailing).map_err(io)? != 0 {
            return Err(error("Unexpected data after the SQL dump terminator"));
        }
        return Ok(None);
    }
    let length = record
        .strip_prefix("-- TABULARIS-STATEMENT ")
        .and_then(|n| n.trim().parse::<usize>().ok())
        .filter(|n| *n > 0 && *n <= MAX_STATEMENT)
        .ok_or_else(|| error("Invalid, truncated or oversized SQL dump record"))?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).map_err(io)?;
    let mut end = [0; 3];
    reader.read_exact(&mut end).map_err(io)?;
    if end != *b"\n^\n" {
        return Err(error("SQL dump statement length/terminator mismatch"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| error("SQL dump statement is not UTF-8"))
}

fn restore(params: &Value) -> Result<Value, PluginError> {
    let path = Path::new(text(params, "file_path")?);
    if !path.is_absolute() {
        return Err(error("file_path must be absolute"));
    }
    let mut connection = ConnectionParams::from_value(inner_params(params))?;
    let control = crate::control::current();
    let _deadline = crate::control::Deadline::query(
        &control,
        connection
            .query_timeout_secs
            .unwrap_or(crate::limits::current().query_timeout_secs),
    );
    // Validate and execute the same private snapshot, even if an editor replaces the input.
    let snapshot_path = std::env::temp_dir().join(format!(
        "tabularis-firebird-import-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| error("Invalid system clock"))?
            .as_nanos()
    ));
    let mut open = OpenOptions::new();
    open.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let _input_guard;
    let mut snapshot = open.open(&snapshot_path).map_err(io)?;
    _input_guard = Pending(snapshot_path);
    let mut input = File::open(path).map_err(io)?;
    let mut buffer = [0u8; 65536];
    loop {
        control.check()?;
        let count = input.read(&mut buffer).map_err(io)?;
        if count == 0 {
            break;
        }
        snapshot.write_all(&buffer[..count]).map_err(io)?;
    }
    drop(input);
    snapshot.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut reader = BufReader::new(snapshot);
    let header = read_header(&mut reader)?;
    let mut records = 0u64;
    while read_statement(&mut reader)?.is_some() {
        records = records
            .checked_add(1)
            .ok_or_else(|| error("Too many SQL records"))?;
    }
    reader.seek(SeekFrom::Start(0)).map_err(io)?;
    read_header(&mut reader)?;
    let create = match params.get("create_database") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        _ => return Err(error("create_database must be boolean")),
    };
    if create {
        drop(Client::create_database(connection.clone())?);
    }
    let result = (|| {
        let mut client = Client::connect_without_database_triggers(connection.clone())?;
        if create && !text(&header, "charset")?.eq_ignore_ascii_case(connection.charset) {
            client.execute(
                &format!(
                    "ALTER DATABASE SET DEFAULT CHARACTER SET {}",
                    identifier(text(&header, "charset")?)?
                ),
                vec![],
                0,
                None,
                None,
            )?;
        }
        let target=client.query_objects("SELECT CURRENT_USER AS \"owner\", TRIM(RDB$CHARACTER_SET_NAME) AS \"charset\", RDB$GET_CONTEXT('SYSTEM','ENGINE_VERSION') AS \"version\" FROM RDB$DATABASE",vec![])?;
        if target[0]["owner"] != header["owner"]
            || target[0]["charset"] != header["charset"]
            || text(&target[0], "version")?
                .split('.')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                != header["engine_major"].as_u64()
        {
            return Err(error("Restore requires the same engine major version, database character set and object owner"));
        }
        for table in [
            "RDB$RELATIONS",
            "RDB$PROCEDURES",
            "RDB$FUNCTIONS",
            "RDB$PACKAGES",
            "RDB$GENERATORS",
            "RDB$EXCEPTIONS",
            "RDB$ROLES",
            "RDB$COLLATIONS",
            "RDB$AUTH_MAPPING",
            "RDB$FILTERS",
        ] {
            if client.query_objects(
                &format!("SELECT COUNT(*) AS N FROM {table} WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0"),
                vec![],
            )?[0]["N"]
                != 0
            {
                return Err(error("Full SQL restore requires a new, empty database; existing databases are never overwritten"));
            }
        }
        if client.query_objects("SELECT COUNT(*) AS N FROM RDB$FIELDS WHERE COALESCE(RDB$SYSTEM_FLAG,0)=0 AND RDB$FIELD_NAME NOT STARTING WITH 'RDB$'",vec![])?[0]["N"]!=0 {return Err(error("Target database already contains domains"));}
        let mut executed = 0u64;
        while let Some(sql) = read_statement(&mut reader)? {
            if !sql.eq_ignore_ascii_case("COMMIT") {
                client
                    .execute(&sql, vec![], 0, Some(0), None)
                    .map_err(|e| PluginError {
                        code: e.code,
                        message: format!(
                            "Restore statement {} failed: {}",
                            executed + 1,
                            e.message
                        ),
                    })?;
            }
            executed += 1;
        }
        if executed != records {
            return Err(error("SQL file changed during restore"));
        }
        Ok(json!({"statements":executed}))
    })();
    match result {
        Err(mut failure) if create => {
            connection.connect_timeout_secs = 5;
            connection.query_timeout_secs = Some(5);
            let cleanup = crate::control::run(std::sync::Arc::default(), || {
                let control = crate::control::current();
                let _deadline = crate::control::Deadline::query(&control, 5);
                Client::connect_without_database_triggers(connection)?.drop_created_database()
            });
            if let Err(cleanup) = cleanup {
                failure.message.push_str(&format!(
                    "; cleanup of the newly created target failed: {}",
                    cleanup.message
                ));
            }
            Err(failure)
        }
        result => result,
    }
}

pub fn import_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let mut file = File::open(text(params, "file_path")?).map_err(io)?;
            let mut prefix = vec![0; HEADER.len()];
            match file.read_exact(&mut prefix) {
                Ok(()) if prefix == HEADER.as_bytes() => restore(params),
                Ok(()) => Ok(Value::Null),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(Value::Null),
                Err(e) => Err(io(e)),
            }
        })(),
    )
}

pub fn capabilities(id: Value) -> Value {
    respond(
        id,
        Ok(
            json!({"format":"tabularis-firebird-sql-v1","create_database":true,"requires_empty_target":true,"target_label":"Database path or alias on the Firebird server"}),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statement_records_keep_psql_quotes_and_length_without_escaping() {
        let sql = "CREATE PROCEDURE P AS BEGIN /* ^ */ SUSPEND; END";
        let mut bytes = Vec::new();
        statement(&mut bytes, sql).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            format!("-- TABULARIS-STATEMENT {}\n{sql}\n^\n", sql.len())
        );
        bytes.extend_from_slice(b"SET TERM ; ^\n");
        let mut reader = std::io::Cursor::new(bytes);
        assert_eq!(read_statement(&mut reader).unwrap().as_deref(), Some(sql));
        assert!(read_statement(&mut reader).unwrap().is_none());
        for invalid in [
            b"-- TABULARIS-STATEMENT 99\nx\n^\n".as_slice(),
            b"-- TABULARIS-STATEMENT 0\n",
            b"SET TERM ; ^\ntrailing",
        ] {
            assert!(read_statement(&mut std::io::Cursor::new(invalid)).is_err());
        }
    }

    #[test]
    #[ignore = "creates disposable source/restore databases; run tests/live-firebird.ps1"]
    fn live_full_database_sql_roundtrip() {
        let config_path = std::env::var("FIREBIRD_TEST_CONFIG").unwrap();
        let config: Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        let base = config["database"].as_str().unwrap();
        let mut original = config.clone();
        original["database"] = json!(format!("{base}.full-source-{}.fdb", std::process::id()));
        let mut target = config.clone();
        target["database"] = json!(format!("{base}.full-target-{}.fdb", std::process::id()));
        let file = Path::new(&config_path)
            .parent()
            .unwrap()
            .join("full-database.sql");
        struct Cleanup {
            databases: Vec<Value>,
            file: PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for config in &self.databases {
                    if let Ok(client) = Client::connect_without_database_triggers(
                        ConnectionParams::from_value(config).unwrap(),
                    ) {
                        let _ = client.drop_created_database();
                    }
                }
                let _ = fs::remove_file(&self.file);
            }
        }
        let mut cleanup = Cleanup {
            databases: vec![],
            file: file.clone(),
        };
        let mut client =
            Client::create_database(ConnectionParams::from_value(&original).unwrap()).unwrap();
        cleanup.databases.push(original.clone());
        assert_eq!(
            client
                .query(
                    "SELECT TRIM(RDB$CHARACTER_SET_NAME) FROM RDB$DATABASE",
                    0,
                    None
                )
                .unwrap()["rows"][0][0],
            "UTF8"
        );
        // Attachment encoding and database defaults are independent.
        client
            .execute(
                "ALTER DATABASE SET DEFAULT CHARACTER SET WIN1252",
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        let statements=[
            "CREATE ROLE E_READER",
            "CREATE COLLATION E_COLLATION FOR UTF8 FROM UNICODE CASE INSENSITIVE",
            "ALTER CHARACTER SET UTF8 SET DEFAULT COLLATION E_COLLATION",
            "CREATE MAPPING E_MAPPING USING PLUGIN Srp FROM USER EXPORT_SOURCE TO USER EXPORT_TARGET",
            "CREATE DOMAIN E_AMOUNT AS NUMERIC(18,4) DEFAULT 1 NOT NULL CHECK (VALUE >= 0)",
            "CREATE DOMAIN E_LABEL AS VARCHAR(60) CHARACTER SET UTF8 COLLATE UNICODE_CI",
            "CREATE SEQUENCE E_SEQ START WITH 73 INCREMENT BY 3",
            "SET GENERATOR E_SEQ TO 111",
            "CREATE EXCEPTION E_BAD 'invalid amount'",
            "DECLARE FILTER E_FILTER INPUT_TYPE 123 OUTPUT_TYPE 124 ENTRY_POINT 'e_filter' MODULE_NAME 'tabularis_fixture_missing'",
            "DECLARE EXTERNAL FUNCTION E_LEGACY INTEGER RETURNS INTEGER BY VALUE ENTRY_POINT 'e_legacy' MODULE_NAME 'tabularis_fixture_missing'",
            "DECLARE EXTERNAL FUNCTION E_LEGACY_TEXT CSTRING(40) RETURNS CSTRING(40) FREE_IT ENTRY_POINT 'e_legacy_text' MODULE_NAME 'tabularis_fixture_missing'",
            "CREATE TABLE E_PARENT (TENANT INTEGER, ID INTEGER, LABEL E_LABEL, CONSTRAINT E_PPK PRIMARY KEY(TENANT,ID))",
            "CREATE TABLE E_CHILD (ID BIGINT GENERATED BY DEFAULT AS IDENTITY (START WITH 10), TENANT INTEGER, PARENT_ID INTEGER, AMOUNT E_AMOUNT, BIN BLOB SUB_TYPE BINARY, NOTES BLOB SUB_TYPE TEXT CHARACTER SET UTF8, OCTETS_VALUE VARCHAR(8) CHARACTER SET OCTETS DEFAULT x'00ff80', DERIVED COMPUTED BY (ID+1), CONSTRAINT E_CPK PRIMARY KEY(ID), CONSTRAINT E_FK FOREIGN KEY(TENANT,PARENT_ID) REFERENCES E_PARENT(TENANT,ID) ON UPDATE CASCADE ON DELETE SET NULL USING DESCENDING INDEX E_FK_IDX, CONSTRAINT E_CHECK CHECK (TENANT>=0))",
            "CREATE TABLE E_AUTO (ID BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, V INTEGER NOT NULL)",
            "CREATE DESCENDING INDEX E_DESC ON E_CHILD(TENANT,PARENT_ID)",
            "ALTER INDEX E_DESC INACTIVE",
            "CREATE INDEX E_EXPR ON E_PARENT COMPUTED BY (UPPER(LABEL))",
            "CREATE PROCEDURE E_ADD(N INTEGER) RETURNS(V INTEGER) AS BEGIN /* ^ marker in PSQL */ V=N+1; SUSPEND; END",
            "CREATE FUNCTION E_TAX(N NUMERIC(18,4)) RETURNS NUMERIC(18,4) DETERMINISTIC AS BEGIN RETURN N*2; END",
            "CREATE VIEW E_VIEW(E_ID,TAX_AMOUNT) AS SELECT ID, E_TAX(AMOUNT) AS TAX FROM E_CHILD",
            "CREATE VIEW E_LATE_VIEW AS SELECT ID, E_TAX(AMOUNT) AS TAX FROM E_CHILD",
            "ALTER VIEW E_VIEW(E_ID,TAX_AMOUNT) AS SELECT ID,TAX FROM E_LATE_VIEW",
            "CREATE PACKAGE E_PACKAGE AS BEGIN FUNCTION F(N INTEGER) RETURNS INTEGER; END",
            "CREATE PACKAGE BODY E_PACKAGE AS BEGIN FUNCTION F(N INTEGER) RETURNS INTEGER AS BEGIN RETURN N*3; END END",
            "ALTER TABLE E_CHILD ADD ROUTINE_DERIVED COMPUTED BY (E_TAX(AMOUNT))",
            "ALTER TABLE E_CHILD ADD PACKAGE_DERIVED COMPUTED BY (E_PACKAGE.F(TENANT))",



            "CREATE GLOBAL TEMPORARY TABLE E_GTT_PRESERVE(ID INTEGER) ON COMMIT PRESERVE ROWS",
            "CREATE GLOBAL TEMPORARY TABLE E_GTT_DELETE(ID INTEGER) ON COMMIT DELETE ROWS",
            "COMMENT ON TABLE E_CHILD IS 'whole database 日本語'",
            "COMMENT ON COLUMN E_CHILD.NOTES IS 'long text'",
            "COMMENT ON DOMAIN E_AMOUNT IS 'domain contract'",
            "COMMENT ON PROCEDURE E_ADD IS 'procedure contract'",
            "COMMENT ON PARAMETER E_ADD.N IS 'input contract'",
            "COMMENT ON PARAMETER E_TAX.N IS 'function input contract'",
            "COMMENT ON INDEX E_FK_IDX IS 'foreign key backing index'",
            "GRANT SELECT ON E_VIEW TO ROLE E_READER",
            "GRANT UPDATE(AMOUNT) ON E_CHILD TO ROLE E_READER WITH GRANT OPTION",
            "GRANT EXECUTE ON PROCEDURE E_ADD TO ROLE E_READER",
            "GRANT USAGE ON SEQUENCE E_SEQ TO ROLE E_READER",
            "GRANT E_READER TO USER EXPORT_ACCOUNT WITH ADMIN OPTION",
            "INSERT INTO E_PARENT VALUES(1,10,'parent 日本語')",
        ];
        for sql in statements {
            let sql = if sql.starts_with("CREATE PACKAGE E_PACKAGE AS")
                && has_column(&mut client, "RDB$PACKAGES", "RDB$SQL_SECURITY").unwrap()
            {
                sql.replace(" AS BEGIN", " SQL SECURITY DEFINER AS BEGIN")
            } else {
                sql.to_string()
            };
            client
                .execute(&sql, vec![], 0, None, None)
                .unwrap_or_else(|e| panic!("Fixture {sql}: {e}"));
        }
        if has_column(&mut client, "RDB$ROLES", "RDB$SYSTEM_PRIVILEGES").unwrap() {
            client.execute("ALTER ROLE E_READER SET SYSTEM PRIVILEGES TO MONITOR_ANY_ATTACHMENT, USE_GSTAT_UTILITY",vec![],0,None,None).unwrap();
            client
                .execute(
                    "GRANT DEFAULT E_READER TO USER EXPORT_DEFAULT",
                    vec![],
                    0,
                    None,
                    None,
                )
                .unwrap();
        }
        let bytes = (0..70000).map(|i| (i % 256) as u8).collect::<Vec<_>>();
        let identity=client.query_objects("SELECT TRIM(RDB$GENERATOR_NAME) AS G FROM RDB$RELATION_FIELDS WHERE RDB$RELATION_NAME='E_CHILD' AND RDB$FIELD_NAME='ID'",vec![]).unwrap();
        let identity = identifier(identity[0]["G"].as_str().unwrap()).unwrap();
        for delta in [1, -1] {
            client
                .execute(
                    &format!("SELECT GEN_ID({identity},{delta}) FROM RDB$DATABASE"),
                    vec![],
                    0,
                    None,
                    None,
                )
                .unwrap();
        }
        let note = format!(
            "literal ^\n-- TABULARIS-STATEMENT 0\n{}",
            "日本語🎉".repeat(7000)
        );
        client
            .execute(
                "INSERT INTO E_CHILD(TENANT,PARENT_ID,AMOUNT,BIN,NOTES) VALUES(1,10,12.3400,?,?)",
                vec![SqlType::Binary(bytes.clone()), SqlType::Text(note.clone())],
                0,
                None,
                None,
            )
            .unwrap();
        client
            .execute(
                "INSERT INTO E_CHILD(TENANT,PARENT_ID) VALUES(1,10)",
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        client.execute("CREATE TRIGGER E_TRIGGER FOR E_CHILD ACTIVE BEFORE INSERT POSITION 7 AS BEGIN IF (NEW.AMOUNT IS NULL) THEN NEW.AMOUNT=5; END",vec![],0,None,None).unwrap();
        client
            .execute(
                "CREATE TRIGGER E_CONNECT INACTIVE ON CONNECT POSITION 3 AS BEGIN END",
                vec![],
                0,
                None,
                None,
            )
            .unwrap();
        client.execute("CREATE TRIGGER E_DDL ACTIVE BEFORE ANY DDL STATEMENT POSITION 5 AS BEGIN EXCEPTION E_BAD; END",vec![],0,None,None).unwrap();
        drop(client);
        let dump = export(&json!({"params":original,"file_path":file})).unwrap();
        assert_eq!(dump["tables"], 5);
        assert_eq!(dump["rows"], 3);
        let restored =
            restore(&json!({"params":target,"file_path":file,"create_database":true})).unwrap();
        assert!(restored["statements"].as_u64().unwrap() > 25);
        cleanup.databases.push(target.clone());
        let mut source = Client::connect_without_database_triggers(
            ConnectionParams::from_value(&original).unwrap(),
        )
        .unwrap();
        let mut copy = Client::connect_without_database_triggers(
            ConnectionParams::from_value(&target).unwrap(),
        )
        .unwrap();
        for sql in [
            "SELECT * FROM E_PARENT ORDER BY TENANT,ID",
            "SELECT * FROM E_CHILD ORDER BY ID",

            "SELECT RDB$RELATION_TYPE FROM RDB$RELATIONS WHERE RDB$RELATION_NAME IN ('E_GTT_PRESERVE','E_GTT_DELETE') ORDER BY RDB$RELATION_NAME",
            "SELECT RDB$RELATION_NAME,RDB$CONSTRAINT_NAME,RDB$CONSTRAINT_TYPE FROM RDB$RELATION_CONSTRAINTS WHERE RDB$RELATION_NAME STARTING WITH 'E_' ORDER BY RDB$RELATION_NAME,RDB$CONSTRAINT_NAME",
            "SELECT RDB$DESCRIPTION FROM RDB$PROCEDURE_PARAMETERS WHERE RDB$PROCEDURE_NAME='E_ADD' AND RDB$PARAMETER_NAME='N'",
            "SELECT * FROM E_VIEW ORDER BY E_ID",
            "SELECT * FROM E_ADD(41)",
            "SELECT RDB$FUNCTION_NAME,RDB$INPUT_SUB_TYPE,RDB$OUTPUT_SUB_TYPE,RDB$ENTRYPOINT,RDB$MODULE_NAME FROM RDB$FILTERS WHERE RDB$FUNCTION_NAME='E_FILTER'",
            "SELECT RDB$FUNCTION_NAME,RDB$ENTRYPOINT,RDB$MODULE_NAME,RDB$RETURN_ARGUMENT,RDB$LEGACY_FLAG FROM RDB$FUNCTIONS WHERE RDB$FUNCTION_NAME STARTING WITH 'E_LEGACY' ORDER BY RDB$FUNCTION_NAME",
            "SELECT RDB$FUNCTION_NAME,RDB$ARGUMENT_POSITION,RDB$MECHANISM,RDB$FIELD_TYPE,RDB$FIELD_LENGTH,RDB$CHARACTER_SET_ID FROM RDB$FUNCTION_ARGUMENTS WHERE RDB$FUNCTION_NAME STARTING WITH 'E_LEGACY' ORDER BY RDB$FUNCTION_NAME,RDB$ARGUMENT_POSITION",
            "SELECT E_PACKAGE.F(7) FROM RDB$DATABASE",
            "SELECT NEXT VALUE FOR E_SEQ FROM RDB$DATABASE",
            "SELECT RDB$INITIAL_VALUE,RDB$GENERATOR_INCREMENT FROM RDB$GENERATORS WHERE RDB$GENERATOR_NAME='E_SEQ'",
            "SELECT RDB$COLLATION_ATTRIBUTES,RDB$SPECIFIC_ATTRIBUTES FROM RDB$COLLATIONS WHERE RDB$COLLATION_NAME='E_COLLATION'",
            "SELECT RDB$MAP_USING,RDB$MAP_PLUGIN,RDB$MAP_DB,RDB$MAP_FROM_TYPE,RDB$MAP_FROM,RDB$MAP_TO_TYPE,RDB$MAP_TO,RDB$DESCRIPTION FROM RDB$AUTH_MAPPING WHERE RDB$MAP_NAME='E_MAPPING'",
            "SELECT RDB$DEFAULT_COLLATE_NAME FROM RDB$CHARACTER_SETS WHERE RDB$CHARACTER_SET_NAME='UTF8'",
            "SELECT g.RDB$INITIAL_VALUE,g.RDB$GENERATOR_INCREMENT FROM RDB$GENERATORS g JOIN RDB$RELATION_FIELDS f ON f.RDB$GENERATOR_NAME=g.RDB$GENERATOR_NAME WHERE f.RDB$RELATION_NAME='E_CHILD' AND f.RDB$FIELD_NAME='ID'",
            "SELECT TRIM(RDB$RELATION_NAME), TRIM(RDB$FIELD_NAME), RDB$FIELD_POSITION, RDB$NULL_FLAG, RDB$IDENTITY_TYPE, RDB$DESCRIPTION FROM RDB$RELATION_FIELDS WHERE RDB$RELATION_NAME IN ('E_PARENT','E_CHILD') ORDER BY RDB$RELATION_NAME,RDB$FIELD_POSITION",
            "SELECT TRIM(RDB$USER),TRIM(RDB$GRANTOR),TRIM(RDB$PRIVILEGE),RDB$GRANT_OPTION,TRIM(RDB$RELATION_NAME),TRIM(RDB$FIELD_NAME),RDB$USER_TYPE,RDB$OBJECT_TYPE FROM RDB$USER_PRIVILEGES WHERE RDB$RELATION_NAME IN ('E_VIEW','E_CHILD','E_ADD','E_SEQ','E_READER') ORDER BY RDB$RELATION_NAME,RDB$USER,RDB$PRIVILEGE,RDB$FIELD_NAME",
            "SELECT TRIM(RDB$INDEX_NAME),RDB$UNIQUE_FLAG,RDB$INDEX_TYPE,RDB$INDEX_INACTIVE FROM RDB$INDICES WHERE RDB$INDEX_NAME IN ('E_DESC','E_EXPR') ORDER BY RDB$INDEX_NAME",
            "SELECT TRIM(RDB$TRIGGER_NAME),RDB$TRIGGER_TYPE,RDB$TRIGGER_SEQUENCE,RDB$TRIGGER_INACTIVE FROM RDB$TRIGGERS WHERE RDB$TRIGGER_NAME IN ('E_TRIGGER','E_CONNECT') ORDER BY RDB$TRIGGER_NAME",
        ] {assert_eq!(source.query(sql,0,None).unwrap()["rows"],copy.query(sql,0,None).unwrap()["rows"],"{sql}");}
        if has_column(&mut source, "RDB$ROLES", "RDB$SYSTEM_PRIVILEGES").unwrap() {
            for sql in [
                "SELECT RDB$SYSTEM_PRIVILEGES FROM RDB$ROLES WHERE RDB$ROLE_NAME='E_READER'",
                "SELECT RDB$SQL_SECURITY FROM RDB$PACKAGES WHERE RDB$PACKAGE_NAME='E_PACKAGE'",
            ] {
                assert_eq!(
                    source.query(sql, 0, None).unwrap()["rows"],
                    copy.query(sql, 0, None).unwrap()["rows"],
                    "{sql}"
                );
            }
        }
        let expected = source
            .query("SELECT MAX(ID)+2 FROM E_CHILD", 0, None)
            .unwrap()["rows"][0][0]
            .clone();
        for client in [&mut source, &mut copy] {
            client
                .execute(
                    "CREATE TABLE E_FUTURE(ID INTEGER PRIMARY KEY, V INTEGER NOT NULL)",
                    vec![],
                    0,
                    None,
                    None,
                )
                .unwrap();
            assert!(client
                .execute(
                    "INSERT INTO E_CHILD(TENANT,PARENT_ID,AMOUNT) VALUES(1,10,-1)",
                    vec![],
                    0,
                    None,
                    None
                )
                .is_err());
            let inserted = client
                .execute(
                    "INSERT INTO E_CHILD(TENANT,PARENT_ID) VALUES(1,10) RETURNING ID",
                    vec![],
                    0,
                    None,
                    None,
                )
                .unwrap();
            // The rejected INSERT also consumes an identity value, on both databases.
            assert_eq!(inserted["rows"][0][0], expected);
        }
        assert!(restore(&json!({"params":target,"file_path":file})).is_err());
        assert!(
            restore(&json!({"params":target,"file_path":file,"create_database":true})).is_err()
        );
        assert_eq!(
            copy.query("SELECT COUNT(*) FROM E_CHILD", 0, None).unwrap()["rows"][0][0],
            3
        );
        let bad_file = file.with_file_name("full-database-invalid.sql");
        let mut bad = BufWriter::new(File::create(&bad_file).unwrap());
        writeln!(bad,"{HEADER}{}\nSET SQL DIALECT 3;\nSET TERM ^ ;",json!({"engine_major":source.query("SELECT RDB$GET_CONTEXT('SYSTEM','ENGINE_VERSION') FROM RDB$DATABASE",0,None).unwrap()["rows"][0][0].as_str().unwrap().split('.').next().unwrap().parse::<u64>().unwrap(),"charset":"UTF8","owner":"SYSDBA"})).unwrap();
        statement(&mut bad, "CREATE TABLE OWNED_PARTIAL(ID INTEGER)").unwrap();
        statement(&mut bad, "INSERT INTO DOES_NOT_EXIST VALUES(1)").unwrap();
        writeln!(bad, "SET TERM ; ^").unwrap();
        bad.flush().unwrap();
        drop(bad);
        let mut failed = config.clone();
        failed["database"] = json!(format!("{base}.full-failed-{}.fdb", std::process::id()));
        let failure =
            restore(&json!({"params":failed,"file_path":bad_file,"create_database":true}))
                .unwrap_err();
        assert!(failure.message.contains("statement 2"), "{failure}");
        assert!(
            Client::connect(ConnectionParams::from_value(&failed).unwrap()).is_err(),
            "Failed restore left its newly created database behind"
        );
        fs::remove_file(bad_file).unwrap();
        drop(source);
        drop(copy);
    }
}
