//! The host's canonical, complete BLOB wire representation (never a file reference).
use base64::{engine::general_purpose::STANDARD, Engine};

use crate::error::PluginError;

pub fn encode(bytes: &[u8]) -> String {
    let mime = infer::get(bytes)
        .map(|kind| kind.mime_type())
        .unwrap_or("application/octet-stream");
    format!("BLOB:{}:{}:{}", bytes.len(), mime, STANDARD.encode(bytes))
}

pub fn decode(value: &str) -> Result<Vec<u8>, PluginError> {
    let invalid = || PluginError::invalid_params("Invalid, truncated or oversized BLOB wire value");
    let mut fields = value.splitn(4, ':');
    if fields.next() != Some("BLOB") {
        return Err(invalid());
    }
    let size = fields
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(invalid)?;
    let mime = fields.next().ok_or_else(invalid)?;
    let encoded = fields.next().ok_or_else(invalid)?;
    let limit = crate::limits::current().blob_bytes;
    if size > limit
        || mime.len() > 128
        || !mime.contains('/')
        || mime.bytes().any(|b| b.is_ascii_control())
        || encoded.len() > limit.div_ceil(3).saturating_mul(4)
    {
        return Err(invalid());
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| invalid())?;
    if bytes.len() != size || bytes.len() > limit {
        return Err(invalid());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_wire_values_preserve_complete_binary_and_reject_truncation() {
        assert_eq!(encode(&[0, 255]), "BLOB:2:application/octet-stream:AP8=");
        for bytes in [vec![], vec![0], vec![0, 255, 128], vec![42; 20_000]] {
            assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
        }
        for invalid in [
            "BLOB:3:application/octet-stream:AP8=",
            "BLOB:2:x:AP8=",
            "BLOB:2:application/octet-stream:????",
            "BLOB_FILE_REF:2:x:/etc/passwd",
            "BLOB:999999999999:application/octet-stream:",
        ] {
            assert!(decode(invalid).is_err(), "{invalid}");
        }
    }
}
