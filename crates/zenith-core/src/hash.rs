//! A fast hasher for small keys, and the checksums that protect saves and pin state.
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

const SEED: u64 = 0x517C_C1B7_2722_0A95;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
    }
    #[inline]
    fn write_u8(&mut self, v: u8) {
        self.add(v as u64);
    }
    #[inline]
    fn write_u32(&mut self, v: u32) {
        self.add(v as u64);
    }
    #[inline]
    fn write_i32(&mut self, v: i32) {
        self.add(v as u32 as u64);
    }
    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.add(v);
    }
    #[inline]
    fn write_usize(&mut self, v: usize) {
        self.add(v as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        // The low bits pick the bucket, and the multiply leaves them the weakest.
        self.0 ^ (self.0 >> 29)
    }
}

pub type FxBuild = BuildHasherDefault<FxHasher>;
pub type FxMap<K, V> = HashMap<K, V, FxBuild>;
pub type FxSet<K> = HashSet<K, FxBuild>;

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}
static CRC: [u32; 256] = crc_table();

/// CRC-32 (IEEE), the same as zip and PNG.
pub fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |c, &b| CRC[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

/// FNV-1a: a stable 64-bit digest for state hashes (std's hashers promise no stability).
pub fn fnv64(bytes: &[u8]) -> u64 {
    fnv64_more(0xCBF2_9CE4_8422_2325, bytes)
}

/// FNV-1a continued from an earlier digest.
pub fn fnv64_more(start: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(start, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01B3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn fnv64_matches_the_published_vector() {
        assert_eq!(fnv64(b"a"), 0xAF63_DC4C_8601_EC8C);
    }

    #[test]
    fn fnv64_can_continue_a_digest() {
        assert_eq!(fnv64_more(fnv64(b"foo"), b"bar"), fnv64(b"foobar"));
    }
}
