//! SHA-256 content hashes for the audit log: the area set's and the track picture's content
//! hashes and the event hash chain. Hashes are lowercase hex.

use sha2::{Digest, Sha256};

/// `prev` of the first event in a log: there is no previous event.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

/// The SHA-256 (hex) of `value`'s compact `serde_json` serialization, the same bytes
/// `serde_json::to_string` gives, hashed as they are written (a large value, the track picture,
/// is never held as one string).
pub fn sha256_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    struct Hashing(Sha256);
    impl std::io::Write for Hashing {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.update(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = Hashing(Sha256::new());
    serde_json::to_writer(&mut w, value)?;
    Ok(hex(w.0.finalize().as_slice()))
}

fn hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &b in digest {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(GENESIS.len(), 64);
    }
}
