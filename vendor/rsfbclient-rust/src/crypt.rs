//! Firebird wire ciphers: SHA-256 session-key derivation and server-provided IVs.
use crate::arc4::Arc4;
use chacha20::{
    cipher::{KeyIvInit, StreamCipher, StreamCipherSeek},
    ChaCha20, ChaCha20Legacy,
};
use rsfbclient_core::FbError;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    str::FromStr,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCrypt {
    Disabled,
    Enabled,
    Required,
}
impl FromStr for WireCrypt {
    type Err = FbError;
    fn from_str(value: &str) -> Result<Self, FbError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "enabled" => Ok(Self::Enabled),
            "required" => Ok(Self::Required),
            _ => Err("wire_crypt must be disabled, enabled or required".into()),
        }
    }
}
impl WireCrypt {
    pub(crate) fn code(self) -> u32 {
        match self {
            Self::Disabled => 0,
            Self::Enabled => 1,
            Self::Required => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireOptions {
    pub mode: WireCrypt,
    pub allow_arc4: bool,
}
impl Default for WireOptions {
    fn default() -> Self {
        Self {
            mode: WireCrypt::Enabled,
            allow_arc4: true,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub name: &'static str,
    pub iv: Vec<u8>,
}

/// Untagged clumplets: tag:u8, length:u8, payload. Each key type owns its following plugin/IV records.
pub(crate) fn select(data: &[u8], allow_arc4: bool) -> Result<Option<Selection>, FbError> {
    let mut position = 0;
    let mut key_type = None;
    let mut current_offer = None;
    let mut offers: Vec<(Vec<String>, Vec<(String, Vec<u8>)>)> = vec![];
    while position < data.len() {
        let header = data
            .get(position..position + 2)
            .ok_or_else(|| FbError::from("Truncated wire encryption offer"))?;
        let (tag, length) = (header[0], header[1] as usize);
        position += 2;
        let value = data
            .get(position..position + length)
            .ok_or_else(|| FbError::from("Truncated wire encryption offer"))?;
        position += length;
        match tag {
            0 => {
                key_type = Some(value == b"Symmetric");
                current_offer = None;
            }
            1 => {
                if key_type == Some(true) {
                    let text = std::str::from_utf8(value)
                        .map_err(|_| FbError::from("Invalid encryption plugin list"))?;
                    offers.push((
                        text.split(|c: char| c.is_ascii_whitespace() || matches!(c, ',' | ';'))
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect(),
                        vec![],
                    ));
                    current_offer = Some(offers.len() - 1);
                } else {
                    key_type = Some(false);
                }
            }
            3 if key_type == Some(true) => {
                let split = value
                    .iter()
                    .position(|b| *b == 0)
                    .ok_or_else(|| FbError::from("Invalid encryption plugin data"))?;
                let plugin = std::str::from_utf8(&value[..split])
                    .map_err(|_| FbError::from("Invalid encryption plugin name"))?;
                let offer = offers
                    .get_mut(
                        current_offer
                            .ok_or_else(|| FbError::from("Encryption IV without plugin offer"))?,
                    )
                    .unwrap();
                if offer.1.iter().any(|(name, _)| name == plugin) {
                    return Err("Duplicate encryption plugin data".into());
                }
                offer.1.push((plugin.into(), value[split + 1..].to_vec()));
            }
            _ => {}
        }
    }
    for name in ["ChaCha64", "ChaCha", "Arc4"] {
        if name == "Arc4" && !allow_arc4 {
            continue;
        }
        let mut advertised = false;
        for (plugins, ivs) in offers.iter().rev() {
            if plugins.iter().any(|p| p == name) {
                advertised = true;
                let iv = ivs
                    .iter()
                    .find(|(p, _)| p == name)
                    .map(|(_, iv)| iv.clone())
                    .unwrap_or_default();
                if (name == "ChaCha64" && iv.len() != 8)
                    || (name == "ChaCha" && !matches!(iv.len(), 12 | 16))
                {
                    continue;
                }
                return Ok(Some(Selection { name, iv }));
            }
        }
        if advertised {
            return Err(format!("Invalid or missing {name} nonce in server offers").into());
        }
    }
    Ok(None)
}

enum Cipher {
    Arc4(Box<Arc4>),
    ChaCha(Box<ChaCha20>),
    ChaCha64(Box<ChaCha20Legacy>),
}
impl Cipher {
    fn new(selected: &Selection, session_key: &[u8]) -> Result<Self, FbError> {
        if session_key.len() < 16 {
            return Err("Wire encryption session key is too short".into());
        }
        let key: [u8; 32] = Sha256::digest(session_key).into();
        Ok(match selected.name {
            "Arc4" if session_key.len() <= 256 => Self::Arc4(Box::new(Arc4::new(session_key))),
            "ChaCha64" => Self::ChaCha64(Box::new(
                ChaCha20Legacy::new_from_slices(&key, &selected.iv)
                    .map_err(|_| FbError::from("Invalid ChaCha64 nonce"))?,
            )),
            "ChaCha" => {
                let nonce = selected
                    .iv
                    .get(..12)
                    .ok_or_else(|| FbError::from("Invalid ChaCha nonce"))?;
                let mut cipher = ChaCha20::new_from_slices(&key, nonce)
                    .map_err(|_| FbError::from("Invalid ChaCha nonce"))?;
                if selected.iv.len() == 16 {
                    let counter = u32::from_be_bytes(selected.iv[12..16].try_into().unwrap());
                    cipher
                        .try_seek((counter as u64) * 64)
                        .map_err(|_| FbError::from("Invalid ChaCha counter"))?;
                }
                Self::ChaCha(Box::new(cipher))
            }
            _ => return Err("Unsupported wire encryption plugin".into()),
        })
    }
    fn apply(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Arc4(cipher) => {
                for byte in bytes {
                    *byte ^= cipher.next();
                }
                Ok(())
            }
            Self::ChaCha(cipher) => cipher
                .try_apply_keystream(bytes)
                .map_err(|_| io::Error::other("ChaCha counter exhausted; reconnect required")),
            Self::ChaCha64(cipher) => cipher
                .try_apply_keystream(bytes)
                .map_err(|_| io::Error::other("ChaCha64 counter exhausted; reconnect required")),
        }
    }
}

pub(crate) struct CryptStream<S> {
    stream: S,
    read_cipher: Cipher,
    write_cipher: Cipher,
    buffer: Box<[u8]>,
    poisoned: bool,
    name: &'static str,
}
impl<S> CryptStream<S> {
    pub fn new(
        stream: S,
        selected: &Selection,
        key: &[u8],
        buffer: usize,
    ) -> Result<Self, FbError> {
        Ok(Self {
            stream,
            read_cipher: Cipher::new(selected, key)?,
            write_cipher: Cipher::new(selected, key)?,
            buffer: vec![0; buffer.max(1)].into_boxed_slice(),
            poisoned: false,
            name: selected.name,
        })
    }
    pub fn name(&self) -> &'static str {
        self.name
    }
    fn check(&self) -> io::Result<()> {
        if self.poisoned {
            Err(io::Error::other(
                "Encrypted stream is unusable after an I/O failure",
            ))
        } else {
            Ok(())
        }
    }
}
impl CryptStream<TcpStream> {
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.stream.peer_addr()
    }
}
impl<S: Read> Read for CryptStream<S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let length = self.stream.read(bytes)?;
        if let Err(error) = self.read_cipher.apply(&mut bytes[..length]) {
            self.poisoned = true;
            return Err(error);
        }
        Ok(length)
    }
}
impl<S: Write> Write for CryptStream<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        let length = bytes.len().min(self.buffer.len());
        self.buffer[..length].copy_from_slice(&bytes[..length]);
        let result = self
            .write_cipher
            .apply(&mut self.buffer[..length])
            .and_then(|_| self.stream.write_all(&self.buffer[..length]));
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error);
        }
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        let result = self.stream.flush();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer(plugins: &str, ivs: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = vec![];
        let mut add = |tag, value: &[u8]| {
            bytes.extend_from_slice(&[tag, value.len() as u8]);
            bytes.extend_from_slice(value);
        };
        add(0, b"Symmetric");
        add(1, plugins.as_bytes());
        for (name, iv) in ivs {
            let mut data = name.as_bytes().to_vec();
            data.push(0);
            data.extend_from_slice(iv);
            add(3, &data);
        }
        bytes
    }
    #[test]
    fn selects_modern_ciphers_and_rejects_malformed_offers_without_downgrading() {
        let initial = offer("ChaCha64,ChaCha,Arc4", &[]);
        let final_offer = offer(
            "Arc4 ChaCha ChaCha64",
            &[("ChaCha64", &[7; 8]), ("ChaCha", &[8; 16])],
        );
        let bytes = [initial, final_offer].concat();
        let selected = select(&bytes, true).unwrap().unwrap();
        assert_eq!(selected.name, "ChaCha64");
        assert_eq!(selected.iv, vec![7; 8]);
        assert!(select(&offer("Arc4", &[]), false).unwrap().is_none());
        assert_eq!(
            select(&offer("Arc4", &[]), true).unwrap().unwrap().name,
            "Arc4"
        );
        for bytes in [
            vec![0],
            vec![0, 9, 1],
            offer("ChaCha64,Arc4", &[("ChaCha64", &[0; 7])]),
            offer("ChaCha", &[]),
            offer("ChaCha64", &[("ChaCha64", &[0; 8]), ("ChaCha64", &[1; 8])]),
            vec![
                0, 9, b'S', b'y', b'm', b'm', b'e', b't', b'r', b'i', b'c', 3, 0,
            ],
        ] {
            assert!(select(&bytes, true).is_err(), "{bytes:?}");
        }
        assert!(select(&[], true).unwrap().is_none());
    }
    struct ShortWrite {
        bytes: Vec<u8>,
        interrupted: bool,
        fail: Option<usize>,
    }
    impl Write for ShortWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.fail.is_some_and(|limit| self.bytes.len() >= limit) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let length = bytes.len().min(3);
            self.bytes.extend_from_slice(&bytes[..length]);
            Ok(length)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn short_and_interrupted_writes_preserve_all_ciphers_and_errors_poison_the_stream() {
        for (name, iv) in [
            ("Arc4", vec![]),
            ("ChaCha", vec![5; 12]),
            ("ChaCha64", vec![7; 8]),
        ] {
            let selection = Selection { name, iv };
            let key = b"twenty byte test key!";
            let transport = ShortWrite {
                bytes: vec![],
                interrupted: false,
                fail: None,
            };
            let mut stream = CryptStream::new(transport, &selection, key, 11).unwrap();
            let plain=b"A payload which crosses cipher blocks and several small writes. Another block starts here!";
            stream.write_all(plain).unwrap();
            stream.flush().unwrap();
            let mut peer =
                CryptStream::new(io::Cursor::new(stream.stream.bytes), &selection, key, 17)
                    .unwrap();
            let mut decoded = vec![];
            peer.read_to_end(&mut decoded).unwrap();
            assert_eq!(decoded, plain, "{name}");
            let transport = ShortWrite {
                bytes: vec![],
                interrupted: false,
                fail: Some(3),
            };
            let mut stream = CryptStream::new(transport, &selection, key, 11).unwrap();
            assert!(stream.write_all(plain).is_err());
            assert!(stream.write(b"retry").is_err());
            assert!(stream.flush().is_err());
            assert_eq!(stream.stream.bytes.len(), 3);
        }
    }
    #[test]
    fn chacha64_carries_into_the_high_counter_and_chacha_uses_big_endian_initial_counter() {
        let key = [3; 32];
        let nonce = [7; 8];
        let mut legacy = ChaCha20Legacy::new_from_slices(&key, &nonce).unwrap();
        legacy.try_seek((1u64 << 38) - 64).unwrap();
        let mut across = [0; 128];
        legacy.try_apply_keystream(&mut across).unwrap();
        let mut ietf_nonce = [0; 12];
        ietf_nonce[..4].copy_from_slice(&1u32.to_le_bytes());
        ietf_nonce[4..].copy_from_slice(&nonce);
        let mut ietf = ChaCha20::new_from_slices(&key, &ietf_nonce).unwrap();
        let mut expected = [0; 64];
        ietf.try_apply_keystream(&mut expected).unwrap();
        assert_eq!(&across[64..], expected);
        let mut iv = vec![0; 16];
        iv[15] = 1;
        let mut cipher = Cipher::new(&Selection { name: "ChaCha", iv }, &key).unwrap();
        let derived = Sha256::digest(key);
        let mut expected_cipher = ChaCha20::new_from_slices(&derived, &[0; 12]).unwrap();
        expected_cipher.try_seek(64u64).unwrap();
        let mut actual = [0; 80];
        let mut expected = [0; 80];
        cipher.apply(&mut actual).unwrap();
        expected_cipher.try_apply_keystream(&mut expected).unwrap();
        assert_eq!(actual, expected);
    }
}
