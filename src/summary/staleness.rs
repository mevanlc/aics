//! Compute a cheap "fingerprint" of a JSONL session file so we can decide
//! whether a stored summary is still fresh.
//!
//! The fingerprint is `(non_empty_line_count, sha256(last_non_empty_line))`.
//! Appended lines change the count; edits to the tail line change the hash.
//! Both together are cheap to compute with a streaming read.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub line_count: usize,
    pub last_line_sha256: String,
}

pub fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    fingerprint_from_reader(BufReader::new(file))
}

pub fn fingerprint_from_reader<R: BufRead>(mut reader: R) -> Result<Fingerprint> {
    let mut line_count = 0usize;
    let mut last_line = String::new();
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = buf.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    continue;
                }
                line_count += 1;
                last_line.clear();
                last_line.push_str(trimmed);
            }
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                // Non-UTF8 tail bytes — skip and keep going; summaries are
                // advisory so we don't want to fail the whole job.
                continue;
            }
            Err(err) => return Err(err.into()),
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(last_line.as_bytes());
    let digest = hasher.finalize();
    Ok(Fingerprint {
        line_count,
        last_line_sha256: hex_encode(&digest),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fingerprint_counts_non_empty_lines() {
        let data = "alpha\n\nbeta\n\n\ngamma\n";
        let fp = fingerprint_from_reader(Cursor::new(data)).unwrap();
        assert_eq!(fp.line_count, 3);
    }

    #[test]
    fn hash_depends_on_last_non_empty_line() {
        let a = fingerprint_from_reader(Cursor::new("x\ny\nz\n")).unwrap();
        let b = fingerprint_from_reader(Cursor::new("x\ny\nz")).unwrap();
        let c = fingerprint_from_reader(Cursor::new("x\ny\nz\n\n")).unwrap();
        assert_eq!(a.last_line_sha256, b.last_line_sha256);
        assert_eq!(a.last_line_sha256, c.last_line_sha256);

        let d = fingerprint_from_reader(Cursor::new("x\ny\nQ\n")).unwrap();
        assert_ne!(a.last_line_sha256, d.last_line_sha256);
    }

    #[test]
    fn empty_input_yields_zero_count_and_hash_of_empty() {
        let fp = fingerprint_from_reader(Cursor::new("")).unwrap();
        assert_eq!(fp.line_count, 0);
        assert_eq!(
            fp.last_line_sha256,
            // sha256 of empty string
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn appending_a_line_changes_fingerprint() {
        let before = fingerprint_from_reader(Cursor::new("{\"a\":1}\n")).unwrap();
        let after = fingerprint_from_reader(Cursor::new("{\"a\":1}\n{\"b\":2}\n")).unwrap();
        assert_ne!(before, after);
        assert_eq!(after.line_count, 2);
    }
}
