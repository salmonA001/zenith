//! The block storage of one 16^3 section: a value palette over bit-packed cells.
//!
//! Sections are the unit of sharing and of saving, so a section holding one value costs nothing but
//! the value, and `compact` gives memory back after edits. Most sections stay small: real terrain
//! has only a handful of distinct blocks per section.

pub const CELLS: usize = 4096;
const DIRECT_LIMIT: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Repr {
    Single(u16),
    /// `log2` of the cell width is 0..=3 (1, 2, 4 or 8 bits). Cells never span words.
    Packed {
        log_bits: u8,
        palette: Vec<u16>,
        words: Box<[u64]>,
    },
    Direct(Box<[u16]>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paletted {
    repr: Repr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeError(pub &'static str);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad palette data: {}", self.0)
    }
}
impl std::error::Error for DecodeError {}

fn word_count(log_bits: u8) -> usize {
    let per_word = 64usize >> log_bits;
    CELLS.div_ceil(per_word)
}

#[inline]
fn read_cell(words: &[u64], log_bits: u8, i: usize) -> usize {
    let word = i >> (6 - log_bits);
    let shift = (i << log_bits) & 63;
    let mask = (1u64 << (1u32 << log_bits)) - 1;
    ((words[word] >> shift) & mask) as usize
}

#[inline]
fn write_cell(words: &mut [u64], log_bits: u8, i: usize, value: usize) {
    let word = i >> (6 - log_bits);
    let shift = (i << log_bits) & 63;
    let mask = (1u64 << (1u32 << log_bits)) - 1;
    words[word] = (words[word] & !(mask << shift)) | ((value as u64) << shift);
}

impl Paletted {
    pub const fn filled(value: u16) -> Self {
        Self { repr: Repr::Single(value) }
    }

    #[inline]
    pub fn get(&self, i: usize) -> u16 {
        match &self.repr {
            Repr::Single(v) => *v,
            Repr::Packed { log_bits, palette, words } => palette[read_cell(words, *log_bits, i)],
            Repr::Direct(cells) => cells[i],
        }
    }

    /// The value of a section that holds only one.
    pub fn single(&self) -> Option<u16> {
        match &self.repr {
            Repr::Single(v) => Some(*v),
            _ => None,
        }
    }

    /// Every value that may occur; values no cell uses any more can stay until `compact`.
    pub fn values(&self) -> &[u16] {
        match &self.repr {
            Repr::Single(v) => std::slice::from_ref(v),
            Repr::Packed { palette, .. } => palette,
            Repr::Direct(cells) => cells,
        }
    }

    /// Writes a cell and returns what it held.
    pub fn set(&mut self, i: usize, value: u16) -> u16 {
        match &mut self.repr {
            Repr::Single(v) => {
                let old = *v;
                if old != value {
                    let mut words = vec![0u64; word_count(0)].into_boxed_slice();
                    write_cell(&mut words, 0, i, 1);
                    self.repr = Repr::Packed { log_bits: 0, palette: vec![old, value], words };
                }
                old
            }
            Repr::Packed { log_bits, palette, words } => {
                let index = read_cell(words, *log_bits, i);
                let old = palette[index];
                if old == value {
                    return old;
                }
                if let Some(found) = palette.iter().position(|&p| p == value) {
                    write_cell(words, *log_bits, i, found);
                    return old;
                }
                if palette.len() < 1usize << (1u32 << *log_bits) {
                    palette.push(value);
                    write_cell(words, *log_bits, i, palette.len() - 1);
                    return old;
                }
                self.grow();
                self.set(i, value);
                old
            }
            Repr::Direct(cells) => std::mem::replace(&mut cells[i], value),
        }
    }

    fn grow(&mut self) {
        let Repr::Packed { log_bits, palette, words } = &self.repr else { return };
        if *log_bits < 3 {
            let wider = *log_bits + 1;
            let mut new_words = vec![0u64; word_count(wider)].into_boxed_slice();
            for i in 0..CELLS {
                write_cell(&mut new_words, wider, i, read_cell(words, *log_bits, i));
            }
            let palette = palette.clone();
            self.repr = Repr::Packed { log_bits: wider, palette, words: new_words };
        } else {
            let cells: Box<[u16]> = (0..CELLS).map(|i| palette[read_cell(words, *log_bits, i)]).collect();
            self.repr = Repr::Direct(cells);
        }
    }

    /// Builds the smallest form holding `cells`.
    pub fn from_raw(cells: &[u16]) -> Self {
        assert_eq!(cells.len(), CELLS);
        let mut palette: Vec<u16> = Vec::with_capacity(16);
        let mut last = cells[0];
        palette.push(last);
        for &c in cells {
            if c != last {
                last = c;
                if !palette.contains(&c) {
                    palette.push(c);
                    if palette.len() > DIRECT_LIMIT {
                        return Self { repr: Repr::Direct(cells.into()) };
                    }
                }
            }
        }
        if palette.len() == 1 {
            return Self::filled(palette[0]);
        }
        let log_bits = match palette.len() {
            2 => 0u8,
            3..=4 => 1,
            5..=16 => 2,
            _ => 3,
        };
        // Remembering the last hit makes runs of the same value cost nothing.
        let mut words = vec![0u64; word_count(log_bits)].into_boxed_slice();
        let mut last_value = palette[0];
        let mut last_index = 0usize;
        for (i, &c) in cells.iter().enumerate() {
            if c != last_value {
                last_value = c;
                last_index = palette.iter().position(|&p| p == c).unwrap_or(0);
            }
            write_cell(&mut words, log_bits, i, last_index);
        }
        Self { repr: Repr::Packed { log_bits, palette, words } }
    }

    pub fn to_raw(&self, out: &mut [u16]) {
        assert_eq!(out.len(), CELLS);
        match &self.repr {
            Repr::Single(v) => out.fill(*v),
            Repr::Packed { log_bits, palette, words } => {
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = palette[read_cell(words, *log_bits, i)];
                }
            }
            Repr::Direct(cells) => out.copy_from_slice(cells),
        }
    }

    /// Rebuilds in the smallest form, dropping values no cell uses any more.
    pub fn compact(&mut self) {
        if matches!(self.repr, Repr::Single(_)) {
            return;
        }
        let mut raw = vec![0u16; CELLS];
        self.to_raw(&mut raw);
        *self = Self::from_raw(&raw);
    }

    /// Heap memory this section holds on to.
    pub fn heap_bytes(&self) -> usize {
        match &self.repr {
            Repr::Single(_) => 0,
            Repr::Packed { palette, words, .. } => palette.capacity() * 2 + words.len() * 8,
            Repr::Direct(cells) => cells.len() * 2,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        match &self.repr {
            Repr::Single(v) => {
                out.push(0);
                out.extend_from_slice(&v.to_le_bytes());
            }
            Repr::Packed { log_bits, palette, words } => {
                out.push(1u8 << *log_bits);
                out.extend_from_slice(&(palette.len() as u16).to_le_bytes());
                for p in palette {
                    out.extend_from_slice(&p.to_le_bytes());
                }
                for w in words.iter() {
                    out.extend_from_slice(&w.to_le_bytes());
                }
            }
            Repr::Direct(cells) => {
                out.push(16);
                for c in cells.iter() {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
        }
    }

    /// Reads from untrusted bytes: anything `get` could trip over is rejected here.
    pub fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let tag = take(input, 1)?[0];
        match tag {
            0 => Ok(Self::filled(read_u16(input)?)),
            1 | 2 | 4 | 8 => {
                let log_bits = tag.trailing_zeros() as u8;
                let len = read_u16(input)? as usize;
                if len == 0 || len > 1usize << tag {
                    return Err(DecodeError("palette length"));
                }
                let palette = (0..len).map(|_| read_u16(input)).collect::<Result<Vec<u16>, _>>()?;
                let count = word_count(log_bits);
                let bytes = take(input, count * 8)?;
                let words: Box<[u64]> = bytes
                    .chunks_exact(8)
                    .map(|b| {
                        let mut word = [0u8; 8];
                        word.copy_from_slice(b);
                        u64::from_le_bytes(word)
                    })
                    .collect();
                if (0..CELLS).any(|i| read_cell(&words, log_bits, i) >= len) {
                    return Err(DecodeError("cell outside the palette"));
                }
                Ok(Self { repr: Repr::Packed { log_bits, palette, words } })
            }
            16 => {
                let cells = (0..CELLS).map(|_| read_u16(input)).collect::<Result<Box<[u16]>, _>>()?;
                Ok(Self { repr: Repr::Direct(cells) })
            }
            _ => Err(DecodeError("cell width")),
        }
    }
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], DecodeError> {
    if input.len() < n {
        return Err(DecodeError("truncated"));
    }
    let (head, tail) = input.split_at(n);
    *input = tail;
    Ok(head)
}

fn read_u16(input: &mut &[u8]) -> Result<u16, DecodeError> {
    let b = take(input, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// A plain array behaves as storage must: every operation is checked against it.
    #[test]
    fn behaves_like_a_plain_array_under_random_edits() {
        for distinct in [1u32, 2, 3, 5, 17, 200, 300, 2000] {
            let mut rng = Rng::new(distinct as u64);
            let mut model = vec![0u16; CELLS];
            let mut p = Paletted::filled(0);
            for _ in 0..20_000 {
                let (i, v) = (rng.below(CELLS as u32) as usize, rng.below(distinct) as u16);
                assert_eq!(p.set(i, v), model[i]);
                model[i] = v;
            }
            let mut raw = vec![0u16; CELLS];
            p.to_raw(&mut raw);
            assert_eq!(raw, model, "{distinct} distinct values");
        }
    }

    #[test]
    fn compact_returns_memory_after_a_section_is_cleared() {
        let mut rng = Rng::new(1);
        let mut p = Paletted::filled(0);
        for i in 0..CELLS {
            p.set(i, rng.below(1000) as u16);
        }
        assert!(p.heap_bytes() >= CELLS * 2);
        for i in 0..CELLS {
            p.set(i, 7);
        }
        p.compact();
        assert_eq!(p.single(), Some(7));
        assert_eq!(p.heap_bytes(), 0);
    }

    #[test]
    fn from_raw_picks_the_narrowest_width() {
        let mut cells = vec![1u16; CELLS];
        assert_eq!(Paletted::from_raw(&cells).single(), Some(1));
        cells[9] = 2;
        // 1 bit per cell: 64 words, plus the capacity of the palette.
        assert_eq!(Paletted::from_raw(&cells).heap_bytes(), word_count(0) * 8 + 16 * 2);
        for (i, c) in cells.iter_mut().enumerate() {
            *c = (i % 300) as u16;
        }
        assert_eq!(Paletted::from_raw(&cells).heap_bytes(), CELLS * 2);
    }

    #[test]
    fn encoding_round_trips_every_form() {
        let mut rng = Rng::new(5);
        for distinct in [1u32, 2, 4, 16, 256, 1024] {
            let cells: Vec<u16> = (0..CELLS).map(|_| rng.below(distinct) as u16).collect();
            let p = Paletted::from_raw(&cells);
            let mut bytes = Vec::new();
            p.encode(&mut bytes);
            let mut input = bytes.as_slice();
            assert_eq!(Paletted::decode(&mut input).expect("decodes"), p);
            assert!(input.is_empty());
        }
    }

    #[test]
    fn decode_rejects_hostile_input_instead_of_panicking_later() {
        // A 1-bit palette with one entry whose cells point at index 1.
        let mut bytes = vec![1u8];
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(&vec![0xFFu8; word_count(0) * 8]);
        assert!(Paletted::decode(&mut bytes.as_slice()).is_err());
        assert!(Paletted::decode(&mut [3u8].as_slice()).is_err(), "unknown width");
        assert!(Paletted::decode(&mut [16u8, 1, 2].as_slice()).is_err(), "truncated");
        assert!(Paletted::decode(&mut [].as_slice()).is_err(), "empty");
    }
}
