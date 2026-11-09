//! Translate the terminal LIMIT clause emitted by Tabularis's generic data-grid filters.
use crate::error::PluginError;

pub fn firebird_query(sql: &str) -> Result<String, PluginError> {
    // The data grid wraps an explicit row cap before applying its page filters.
    let trimmed = sql.trim_start();
    if let Some(inner) = trimmed.strip_prefix("SELECT * FROM (") {
        if let Some(end) = inner.rfind(") AS limited_subset") {
            return Ok(format!(
                "SELECT * FROM ({}{}",
                terminal_limit(&inner[..end])?,
                &inner[end..]
            ));
        }
    }
    terminal_limit(sql)
}

fn terminal_limit(sql: &str) -> Result<String, PluginError> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    let mut limit = None;
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut comments = 1;
                while i < bytes.len() && comments > 0 {
                    if bytes.get(i..i + 2) == Some(b"/*") {
                        comments += 1;
                        i += 2;
                    } else if bytes.get(i..i + 2) == Some(b"*/") {
                        comments -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'q' | b'Q' if bytes.get(i + 1) == Some(&b'\'') && bytes.get(i + 2).is_some() => {
                let end = match bytes[i + 2] {
                    b'{' => b'}',
                    b'[' => b']',
                    b'(' => b')',
                    b'<' => b'>',
                    other => other,
                };
                i += 3;
                while i < bytes.len() && !(bytes[i] == end && bytes.get(i + 1) == Some(&b'\'')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            quote @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        i += 1;
                        if bytes.get(i) == Some(&quote) {
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' || b >= 128 => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || matches!(bytes[i], b'_' | b'$')
                        || bytes[i] >= 128)
                {
                    i += 1;
                }
                if depth == 0 && sql[start..i].eq_ignore_ascii_case("LIMIT") {
                    limit = Some(start);
                }
            }
            _ => i += 1,
        }
    }
    let Some(start) = limit else {
        return Ok(sql.into());
    };
    let tail = sql[start + 5..].trim();
    let tail = tail.strip_suffix(';').unwrap_or(tail).trim();
    let parts: Vec<_> = tail.split_whitespace().collect();
    let (count, offset) = match parts.as_slice() {
        [count] => (count.parse::<u64>().ok(), Some(0u64)),
        [count, keyword, offset] if keyword.eq_ignore_ascii_case("OFFSET") => {
            (count.parse().ok(), offset.parse().ok())
        }
        _ => return Ok(sql.into()),
    };
    let (Some(count), Some(offset)) = (count, offset) else {
        return Ok(sql.into());
    };
    let rows = if count == 0 {
        "0".into()
    } else {
        let first = offset.checked_add(1).ok_or_else(|| {
            PluginError::invalid_params("LIMIT/OFFSET exceeds Firebird row range")
        })?;
        let last = offset
            .checked_add(count)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| {
                PluginError::invalid_params("LIMIT/OFFSET exceeds Firebird row range")
            })?;
        format!("{first} TO {last}")
    };
    Ok(format!("{} ROWS {rows}", sql[..start].trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translates_terminal_grid_limits() {
        assert_eq!(
            firebird_query("SELECT * FROM (SELECT * FROM \"FB_CHILD\" LIMIT 1) AS limited_subset")
                .unwrap(),
            "SELECT * FROM (SELECT * FROM \"FB_CHILD\" ROWS 1 TO 1) AS limited_subset"
        );
        assert_eq!(
            firebird_query("SELECT * FROM T LIMIT 1;").unwrap(),
            "SELECT * FROM T ROWS 1 TO 1"
        );
        assert_eq!(
            firebird_query("SELECT * FROM T ORDER BY ID limit 10 offset 20").unwrap(),
            "SELECT * FROM T ORDER BY ID ROWS 21 TO 30"
        );
        assert_eq!(
            firebird_query("SELECT * FROM T LIMIT 0").unwrap(),
            "SELECT * FROM T ROWS 0"
        );
        assert!(firebird_query("SELECT * FROM T LIMIT 2 OFFSET 18446744073709551615").is_err());
    }
    #[test]
    fn leaves_identifiers_literals_comments_and_subqueries_untouched() {
        for sql in [
            "SELECT 'LIMIT 1' FROM T",
            "SELECT 1 AS \"LIMIT 1\" FROM T",
            "SELECT * FROM T -- LIMIT 1",
            "SELECT * FROM T /* LIMIT 1 */",
            "SELECT * FROM (SELECT * FROM T LIMIT 1)",
            "SELECT q'{it's LIMIT 1}' FROM T",
            "SELECT * FROM T WHERE SOMELIMIT = 1",
        ] {
            assert_eq!(firebird_query(sql).unwrap(), sql);
        }
        assert_eq!(
            firebird_query("SELECT 'LIMIT 9' FROM T /* LIMIT 8 */ LIMIT 1").unwrap(),
            "SELECT 'LIMIT 9' FROM T /* LIMIT 8 */ ROWS 1 TO 1"
        );
    }
}
