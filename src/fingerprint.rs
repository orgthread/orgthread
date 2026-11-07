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

#[cfg(test)]
mod tests {
    use super::*;

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
