//! Reversible package identities; dots inside quoted identifiers remain literal.
use super::{
    crud::identifier,
    metadata::{self, connect, object_name, respond},
};
use crate::{error::PluginError, utils::identifiers::quote_identifier};
use serde_json::{json, Value};

fn argument(arg: &Value) -> Result<String, PluginError> {
    let raw = match arg.get("is_raw") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(raw)) => *raw,
        _ => return Err(PluginError::invalid_params("is_raw must be boolean")),
    };
    match arg.get("value") {
        None | Some(Value::Null) => Ok("NULL".into()),
        Some(Value::String(value)) if raw => super::ddl::fragment(value),
        Some(Value::String(value)) => Ok(format!("'{}'", value.replace('\'', "''"))),
        _ => Err(PluginError::invalid_params(
            "Routine argument value must be a string or null",
        )),
    }
}

pub fn build_routine_call_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let name = Name::from_request(params)?;
            let kind = kind(params)?
                .ok_or_else(|| PluginError::invalid_params("routine_type is required"))?;
            let args = params
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| PluginError::invalid_params("args must be an array"))?;
            let mut values = std::collections::HashMap::new();
            for arg in args {
                match arg.get("mode").and_then(Value::as_str) {
                    Some("OUT") => continue,
                    Some("IN") => {}
                    _ => {
                        return Err(PluginError::invalid_params(
                            "Firebird routine arguments must have mode IN or OUT",
                        ))
                    }
                }
                let name = arg
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| PluginError::invalid_params("Every IN argument needs a name"))?;
                if values.insert(name, argument(arg)?).is_some() {
                    return Err(PluginError::invalid_params("Duplicate routine argument"));
                }
            }
            let mut client = connect(params)?;
            let routine = metadata::routine(&mut client, &name, Some(kind))?;
            let parameters = metadata::routine_parameters(&mut client, &name, &routine)?;
            let mut inputs = vec![];
            for param in parameters.iter().filter(|p| p["mode"] == "IN") {
                let name = param["name"].as_str().unwrap();
                inputs.push(values.remove(name).ok_or_else(|| {
                    PluginError::invalid_params(format!("Missing routine argument: {name}"))
                })?);
            }
            if !values.is_empty() {
                return Err(PluginError::invalid_params("Unknown routine arguments"));
            }
            let arguments = inputs.join(", ");
            let name = name.sql();
            let sql = if kind == "FUNCTION" {
                format!("SELECT {name}({arguments}) AS \"RESULT_VALUE\" FROM RDB$DATABASE;")
            } else {
                let suffix = if inputs.is_empty() {
                    String::new()
                } else {
                    format!("({arguments})")
                };
                if routine["procedure_type"] == 1 {
                    format!("SELECT * FROM {name}{suffix};")
                } else {
                    format!("EXECUTE PROCEDURE {name}{suffix};")
                }
            };
            Ok(json!(sql))
        })(),
    )
}

pub(super) struct Name {
    pub package: Option<String>,
    pub name: String,
}
impl Name {
    pub fn from_request(params: &Value) -> Result<Self, PluginError> {
        Self::parse(object_name(params, "routine")?)
    }
    fn parse(value: &str) -> Result<Self, PluginError> {
        let mut parts = vec![];
        let mut rest = value;
        loop {
            let part;
            if let Some(quoted) = rest.strip_prefix('"') {
                let mut name = String::new();
                let mut chars = quoted.char_indices().peekable();
                let end = loop {
                    match chars.next() {
                        Some((_, '"')) if chars.peek().is_some_and(|(_, c)| *c == '"') => {
                            chars.next();
                            name.push('"');
                        }
                        Some((i, '"')) => break i + 2,
                        Some((_, c)) => name.push(c),
                        None => {
                            return Err(PluginError::invalid_params(
                                "Unclosed quoted routine identifier",
                            ))
                        }
                    }
                };
                rest = &rest[end..];
                part = name;
            } else {
                let end = rest.find('.').unwrap_or(rest.len());
                part = rest[..end].into();
                rest = &rest[end..];
                if part.contains('"') {
                    return Err(PluginError::invalid_params(
                        "Quote the complete routine identifier",
                    ));
                }
            }
            identifier(&part)?;
            parts.push(part);
            if rest.is_empty() {
                break;
            }
            if parts.len() == 2 {
                return Err(PluginError::invalid_params(
                    "Routine identities contain at most package and routine",
                ));
            }
            rest = rest.strip_prefix('.').ok_or_else(|| {
                PluginError::invalid_params("Unexpected characters after quoted routine name")
            })?;
        }
        let name = parts.pop().unwrap();
        Ok(Self {
            package: parts.pop(),
            name,
        })
    }
    pub fn sql(&self) -> String {
        match &self.package {
            Some(package) => format!(
                "{}.{}",
                quote_identifier(package, '"'),
                quote_identifier(&self.name, '"')
            ),
            None => quote_identifier(&self.name, '"'),
        }
    }
    pub fn display(&self) -> String {
        if self.package.is_some() || self.name.contains(['.', '"']) {
            self.sql()
        } else {
            self.name.clone()
        }
    }
}

pub(super) fn kind(params: &Value) -> Result<Option<&str>, PluginError> {
    let read = |key| match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(kind)) if matches!(kind.as_str(), "FUNCTION" | "PROCEDURE") => {
            Ok(Some(kind.as_str()))
        }
        _ => Err(PluginError::invalid_params(format!(
            "{key} must be FUNCTION or PROCEDURE"
        ))),
    };
    let kind = read("routine_type")?;
    let identity = read("identity_args")?;
    if kind.is_some() && identity.is_some() && kind != identity {
        return Err(PluginError::invalid_params(
            "routine_type and identity_args disagree",
        ));
    }
    Ok(kind.or(identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn preserves_package_boundaries_and_literal_dots_or_quotes() {
        for (package, name) in [
            (None, "plain"),
            (None, "a.b"),
            (None, "odd\"name"),
            (Some("odd.package"), "f\"name"),
            (Some("P"), "F"),
        ] {
            let original = Name {
                package: package.map(str::to_owned),
                name: name.into(),
            };
            let decoded = Name::parse(&original.display()).unwrap();
            assert_eq!(decoded.name, original.name);
            assert_eq!(decoded.package, original.package);
        }
        for invalid in [
            "",
            "P.",
            ".F",
            "P.F.G",
            "\"unclosed",
            "\"P\"extra.F",
            "\"\"",
            "F\0",
        ] {
            assert!(Name::parse(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            kind(&json!({"identity_args":"FUNCTION"})).unwrap(),
            Some("FUNCTION")
        );
        assert!(kind(&json!({"identity_args":"FUNCTION","routine_type":"PROCEDURE"})).is_err());
    }

    #[test]
    fn quotes_literals_and_checks_raw_expressions() {
        assert_eq!(argument(&json!({"value":"O'Brien"})).unwrap(), "'O''Brien'");
        assert_eq!(argument(&json!({"value":null})).unwrap(), "NULL");
        assert_eq!(
            argument(&json!({"value":"CAST(1 AS INTEGER)","is_raw":true})).unwrap(),
            "CAST(1 AS INTEGER)"
        );
        for arg in [
            json!({"value":1}),
            json!({"is_raw":"true"}),
            json!({"is_raw":true,"value":"1); DROP TABLE T;--"}),
        ] {
            assert!(argument(&arg).is_err());
        }
    }

    #[test]
    #[ignore = "requires isolated fixture; run tests/live-firebird.ps1"]
    fn live_builds_executable_firebird_routine_calls() {
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("FIREBIRD_TEST_CONFIG").unwrap()).unwrap(),
        )
        .unwrap();
        let mut client = connect(&json!({"params":config})).unwrap();
        let cases = [
            (
                "FB_ADD",
                "PROCEDURE",
                json!([{"name":"B","mode":"IN","is_raw":true,"value":"3"},{"name":"A","mode":"IN","is_raw":true,"value":"2"}]),
                json!([[5]]),
            ),
            (
                "FB_TAX",
                "FUNCTION",
                json!([{"name":"AMOUNT","mode":"IN","is_raw":true,"value":"12.50"},{"name":"RATE","mode":"IN","is_raw":true,"value":"0.20"}]),
                json!([["2.50"]]),
            ),
            ("FB_NO_ARGS", "PROCEDURE", json!([]), json!([])),
            (
                "\"FB_PACKAGE\".\"FB_ECHO\"",
                "PROCEDURE",
                json!([{"name":"V","mode":"IN","is_raw":true,"value":"42"}]),
                json!([[42]]),
            ),
            (
                "\"FB_PACKAGE\".\"FB_DOUBLE\"",
                "FUNCTION",
                json!([{"name":"V","mode":"IN","is_raw":true,"value":"21"}]),
                json!([[42]]),
            ),
            (
                "\"odd.package\".\"odd\"\"member\"",
                "FUNCTION",
                json!([]),
                json!([[7]]),
            ),
            (
                "\"FB_PACKAGE.FB_DOUBLE\"",
                "FUNCTION",
                json!([]),
                json!([[99]]),
            ),
            ("FB_SHARED", "FUNCTION", json!([]), json!([[1]])),
            ("FB_SHARED", "PROCEDURE", json!([]), json!([])),
            (
                "odd' routine",
                "FUNCTION",
                json!([{"name":"mixedCase","mode":"IN","value":"O'Brien"}]),
                json!([["O'Brien"]]),
            ),
        ];
        for (name, kind, args, rows) in cases {
            let response = build_routine_call_sql(
                json!(1),
                &json!({"params":config,"routine_name":name,"routine_type":kind,"args":args}),
            );
            assert!(response.get("error").is_none(), "{response}");
            let sql = response["result"].as_str().unwrap();
            assert_eq!(
                client.execute(sql, vec![], 0, None, None).unwrap()["rows"],
                rows,
                "{sql}"
            );
        }
        for args in [
            json!([]),
            json!([{"name":"A","mode":"IN","value":"1"},{"name":"A","mode":"IN","value":"2"}]),
            json!([{"name":"WRONG","mode":"IN","value":"1"}]),
        ] {
            let response = build_routine_call_sql(
                json!(1),
                &json!({"params":config,"routine_name":"FB_ADD","routine_type":"PROCEDURE","args":args}),
            );
            assert_eq!(response["error"]["code"], -32602);
        }
    }
}
