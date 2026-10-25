//! Checksum algorithms supported by RollVault, matching the ASC MHL reference
//! implementation (hex digests are canonical big-endian, lowercase).

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha2::Sha512;
use std::fs::File;
use std::io;
use std::path::Path;
use xxhash_rust::xxh3::Xxh3;
use xxhash_rust::xxh64::Xxh64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgo {
    Xxh64,
    Xxh3,
    Xxh128,
    Md5,
}

impl HashAlgo {
    pub const ALL: [HashAlgo; 4] = [HashAlgo::Xxh64, HashAlgo::Xxh3, HashAlgo::Xxh128, HashAlgo::Md5];

    /// Element name used in ASC MHL v2.0 manifests.
    pub fn ascmhl_name(self) -> &'static str {
        match self {
            HashAlgo::Xxh64 => "xxh64",
            HashAlgo::Xxh3 => "xxh3",
            HashAlgo::Xxh128 => "xxh128",
            HashAlgo::Md5 => "md5",
        }
    }

    /// Element name in legacy MHL v1.1, which only knows md5/sha1/xxhash*.
    pub fn legacy_mhl_tag(self) -> Option<&'static str> {
        match self {
            HashAlgo::Xxh64 => Some("xxhash64be"),
            HashAlgo::Md5 => Some("md5"),
            HashAlgo::Xxh3 | HashAlgo::Xxh128 => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HashAlgo::Xxh64 => "xxHash64",
            HashAlgo::Xxh3 => "XXH3-64",
            HashAlgo::Xxh128 => "XXH128",
            HashAlgo::Md5 => "MD5",
        }
    }

    pub fn from_name(name: &str) -> Option<HashAlgo> {
        HashAlgo::ALL.into_iter().find(|a| a.ascmhl_name() == name)
    }
}

pub enum Hasher {
    Xxh64(Box<Xxh64>),
    Xxh3(Box<Xxh3>),
    Xxh128(Box<Xxh3>),
    Md5(Box<Md5>),
}

impl Hasher {
    pub fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Xxh64 => Hasher::Xxh64(Box::new(Xxh64::new(0))),
            HashAlgo::Xxh3 => Hasher::Xxh3(Box::new(Xxh3::new())),
            HashAlgo::Xxh128 => Hasher::Xxh128(Box::new(Xxh3::new())),
            HashAlgo::Md5 => Hasher::Md5(Box::new(Md5::new())),
        }
    }

    #[inline]
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Xxh64(h) => h.update(data),
            Hasher::Xxh3(h) | Hasher::Xxh128(h) => h.update(data),
            Hasher::Md5(h) => h.update(data),
        }
    }

    pub fn finalize_hex(self) -> String {
        match self {
            Hasher::Xxh64(h) => format!("{:016x}", h.digest()),
            Hasher::Xxh3(h) => format!("{:016x}", h.digest()),
            Hasher::Xxh128(h) => format!("{:032x}", h.digest128()),
            Hasher::Md5(h) => hex::encode(h.finalize().as_slice()),
        }
    }
}

pub fn hash_bytes(algo: HashAlgo, data: &[u8]) -> String {
    let mut h = Hasher::new(algo);
    h.update(data);
    h.finalize_hex()
}

/// Hash of a list of hex hashes, as defined by ASC MHL: sort the hex strings,
/// concatenate their raw bytes, hash the result.
pub fn hash_of_hash_list(algo: HashAlgo, hashes: &mut [String]) -> String {
    hashes.sort();
    let mut h = Hasher::new(algo);
    for s in hashes.iter() {
        h.update(&hex::decode(s).expect("internal hex hash"));
    }
    h.finalize_hex()
}

const C4_CHARSET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// C4 ID (SMPTE ST 2114): SHA-512 encoded in base58, padded to 90 chars.
pub fn c4_of_bytes(data: &[u8]) -> String {
    let digest = Sha512::digest(data);
    c4_from_digest(digest.as_slice())
}

pub fn c4_of_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha512::new();
    io::copy(&mut f, &mut ShaWriter(&mut h))?;
    Ok(c4_from_digest(h.finalize().as_slice()))
}

struct ShaWriter<'a>(&'a mut Sha512);
impl io::Write for ShaWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn c4_from_digest(digest: &[u8]) -> String {
    // Big-endian base conversion by repeated division.
    let mut num: Vec<u8> = digest.to_vec();
    let mut out: Vec<u8> = Vec::with_capacity(90);
    while num.iter().any(|b| *b != 0) {
        let mut rem: u32 = 0;
        for byte in num.iter_mut() {
            let acc = (rem << 8) | (*byte as u32);
            *byte = (acc / 58) as u8;
            rem = acc % 58;
        }
        out.push(C4_CHARSET[rem as usize]);
    }
    while out.len() < 88 {
        out.push(b'1');
    }
    out.reverse();
    let mut s = String::from("c4");
    s.push_str(std::str::from_utf8(&out).expect("ascii"));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors_empty_input() {
        assert_eq!(hash_bytes(HashAlgo::Xxh64, b""), "ef46db3751d8e999");
        assert_eq!(hash_bytes(HashAlgo::Xxh3, b""), "2d06800538d394c2");
        assert_eq!(hash_bytes(HashAlgo::Xxh128, b""), "99aa06d3014798d86001c324468d497f");
        assert_eq!(hash_bytes(HashAlgo::Md5, b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn matches_reference_ascmhl_values() {
        // From `ascmhl create -h xxh64` on a file containing "hello".
        assert_eq!(hash_bytes(HashAlgo::Xxh64, b"hello"), "26c7827d889f6da3");
        // From ascmhl.hasher (Python reference) for b"hello".
        assert_eq!(hash_bytes(HashAlgo::Xxh3, b"hello"), "9555e8555c62dcfd");
        assert_eq!(hash_bytes(HashAlgo::Xxh128, b"hello"), "b5e9c1ad071b3e7fc779cfaa5e523818");
        assert_eq!(
            c4_of_bytes(b"hello"),
            "c447Fm3BJZQ62765jMZJH4m28hrDM7Szbj9CUmj4F4gnvyDYXYz4WfnK2nYRhFvRgYEectEXYBYWLDpLo6XGNAfKdt"
        );
    }

    #[test]
    fn streaming_equals_one_shot() {
        let data: Vec<u8> = (0..1_000_003u32).map(|i| (i * 31 % 251) as u8).collect();
        for algo in HashAlgo::ALL {
            let mut h = Hasher::new(algo);
            for c in data.chunks(65_537) {
                h.update(c);
            }
            assert_eq!(h.finalize_hex(), hash_bytes(algo, &data), "{algo:?}");
        }
    }

    #[test]
    fn c4_has_fixed_length_and_prefix() {
        let id = c4_of_bytes(b"");
        assert!(id.starts_with("c4"));
        assert_eq!(id.len(), 90);
        // Known C4 ID of the empty string (SMPTE ST 2114 test vector).
        assert_eq!(
            id,
            "c459dsjfscH38cYeXXYogktxf4Cd9ibshE3BHUo6a58hBXmRQdZrAkZzsWcbWtDg5oQstpDuni4Hirj75GEmTc1sFT"
        );
    }
}
