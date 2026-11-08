//! The fingerprint of a file: the same content hash that Dropbox reports.
//!
//! The bytes are split into blocks of 4 MiB. Each block gets a SHA-256. The
//! fingerprint is the SHA-256 of those hashes put together, in lower-case hex.

use sha2::{Digest, Sha256};

const BLOCK: usize = 4 * 1024 * 1024;

pub fn fingerprint(bytes: &[u8]) -> String {
    let mut outer = Sha256::new();
    for block in bytes.chunks(BLOCK) {
        outer.update(Sha256::digest(block));
    }
    outer
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The same fingerprint, for bytes that come in parts, as in a stream.
#[derive(Default)]
pub struct Fingerprinter {
    outer: Sha256,
    block: Vec<u8>,
    size: u64,
}

impl Fingerprinter {
    pub fn update(&mut self, mut bytes: &[u8]) {
        self.size += bytes.len() as u64;
        while !bytes.is_empty() {
            let room = BLOCK - self.block.len();
            let take = room.min(bytes.len());
            self.block.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.block.len() == BLOCK {
                self.outer.update(Sha256::digest(&self.block));
                self.block.clear();
            }
        }
    }

    /// How many bytes came in so far.
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn finish(mut self) -> String {
        if !self.block.is_empty() {
            self.outer.update(Sha256::digest(&self.block));
        }
        self.outer
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_give_the_same_fingerprint_as_the_whole() {
        let bytes: Vec<u8> = (0..BLOCK * 2 + 123).map(|i| (i % 251) as u8).collect();
        for step in [3, 1000, BLOCK - 1, BLOCK, BLOCK + 7] {
            let mut f = Fingerprinter::default();
            for part in bytes.chunks(step) {
                f.update(part);
            }
            assert_eq!(f.size(), bytes.len() as u64);
            assert_eq!(f.finish(), fingerprint(&bytes), "step {step}");
        }
        assert_eq!(Fingerprinter::default().finish(), fingerprint(b""));
    }

    // Expected values computed separately with Python's hashlib.
    #[test]
    fn short_text_is_the_hash_of_its_hash() {
        assert_eq!(
            fingerprint(b"hello\n"),
            "ecb65bb98f9d905b70458986c39fcbad7715e5f2fcc3b1f07767d7c83e2438cc"
        );
    }

    #[test]
    fn empty_input_is_the_hash_of_nothing() {
        assert_eq!(
            fingerprint(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_file_over_one_block_uses_two_block_hashes() {
        let bytes = vec![b'x'; BLOCK + 1];
        assert_eq!(
            fingerprint(&bytes),
            "656e7e27f7d0459e319264cdccb61f6667e8e964f8e763c332f35d36ee1cb924"
        );
    }
}
