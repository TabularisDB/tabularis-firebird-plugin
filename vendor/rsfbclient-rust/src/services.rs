//! Firebird's public Services SPB and info encodings (consts_pub.h / protocol.cpp).
use bytes::{BufMut, BytesMut};
use rsfbclient_core::FbError;

pub struct ServiceOutput {
    pub data: Vec<u8>,
    pub stdin: usize,
    pub more: bool,
}

pub(crate) fn start_block(database: &str, restore: bool) -> Result<Vec<u8>, FbError> {
    if database.is_empty() || database.contains('\0') {
        return Err("Invalid service database path".into());
    }
    let mut block = BytesMut::new();
    block.put_u8(if restore { 2 } else { 1 });
    for (tag, value) in [
        (106, database),
        (5, if restore { "stdin" } else { "stdout" }),
    ] {
        let length = u16::try_from(value.len())
            .map_err(|_| FbError::from("Service database path too long"))?;
        block.put_u8(tag);
        block.put_u16_le(length);
        block.extend_from_slice(value.as_bytes());
    }
    block.put_u8(108); // isc_spb_options
                       // Create only; omit source shadow paths. Backup includes external-table data
                       // as ordinary tables and suppresses connect/DDL triggers.
    block.put_u32_le(if restore {
        0x2000 | 0x0200
    } else {
        0x8000 | 0x40
    });
    Ok(block.to_vec())
}

pub(crate) fn parse_output(mut bytes: &[u8]) -> Result<ServiceOutput, FbError> {
    let mut output = ServiceOutput {
        data: vec![],
        stdin: 0,
        more: false,
    };
    while let Some((&tag, rest)) = bytes.split_first() {
        bytes = rest;
        match tag {
            1 => return Ok(output), // isc_info_end
            2 => {
                output.more = true;
                return Ok(output);
            } // isc_info_truncated terminates the buffer
            4 | 64 => output.more = true, // not ready / timeout
            62 | 63 => {
                if bytes.len() < 2 {
                    return Err("Truncated service output length".into());
                }
                let length = u16::from_le_bytes(bytes[..2].try_into().unwrap()) as usize;
                bytes = &bytes[2..];
                if bytes.len() < length {
                    return Err("Truncated service output data".into());
                }
                output.more |= length > 0;
                output.data.extend_from_slice(&bytes[..length]);
                bytes = &bytes[length..];
            }
            78 => {
                if bytes.len() < 4 {
                    return Err("Truncated service stdin request".into());
                }
                output.stdin = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
                output.more |= output.stdin > 0;
                bytes = &bytes[4..];
            }
            _ => return Err(format!("Unexpected service info tag: {tag}").into()),
        }
    }
    Err("Service output has no end marker".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_blocks_never_replace_a_database_or_restore_source_shadows() {
        let block = start_block("target.fdb", true).unwrap();
        assert_eq!(block[0], 2);
        assert!(block.windows(5).any(|p| p == [108, 0, 34, 0, 0]));
        assert!(!block.windows(5).any(|p| p == [108, 0, 16, 0, 0]));
        assert!(start_block("a\0b", true).is_err());
        assert!(start_block(&"x".repeat(65536), false).is_err());
    }
    #[test]
    fn service_output_preserves_binary_and_validates_all_lengths() {
        let bytes = [63, 3, 0, 0, 255, 128, 78, 0, 128, 0, 0, 1];
        let output = parse_output(&bytes).unwrap();
        assert_eq!(output.data, [0, 255, 128]);
        assert_eq!(output.stdin, 32768);
        assert!(output.more);
        for end in 0..bytes.len() {
            assert!(parse_output(&bytes[..end]).is_err());
        }
        assert!(!parse_output(&[63, 0, 0, 1]).unwrap().more);
        assert!(parse_output(&[64, 1]).unwrap().more);
        assert!(parse_output(&[99, 1]).is_err());
    }
}
