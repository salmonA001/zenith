//! Region files: where modified chunks live on disk, safely.
//!
//! A world is its seed plus what players changed, so only modified chunks are stored at all.
//!
//! The format is built so a crash at any moment loses at most the latest save and never corrupts an
//! older one. New data only ever goes into free space, never over live data, and a table of
//! contents makes it visible. There are two table slots, written in turn: the one a crash could
//! tear is the older one's replacement, and the survivor still describes a complete file.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use zenith_core::hash::crc32;
use zenith_core::{ChunkPos, FxMap};

const SECTOR: u64 = 4096;
const CHUNKS: usize = 1024;
const MAGIC: &[u8; 8] = b"ZENITHR1";
/// Magic, generation, checksum, then a row per chunk: offset in sectors, length in bytes, checksum.
const SLOT_HEADER: usize = 8 + 8 + 4;
const ROW: usize = 12;
const SLOT_BYTES: u64 = (SLOT_HEADER + CHUNKS * ROW) as u64;
/// Sectors a slot takes: enough for its bytes.
const SLOT_SECTORS: u64 = SLOT_BYTES.div_ceil(SECTOR);
/// Data starts after both slots.
const FIRST_DATA_SECTOR: u64 = SLOT_SECTORS * 2;
/// Nothing a chunk holds comes close to this; a larger length is corruption, not data.
const MAX_RECORD: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Row {
    offset: u32,
    length: u32,
    crc: u32,
}

impl Row {
    fn sectors(self) -> u64 {
        (self.length as u64).div_ceil(SECTOR)
    }
}

/// One region file: 32 x 32 chunks.
pub struct RegionFile {
    file: File,
    table: Vec<Row>,
    /// The table as of the last commit. What it points at stays untouched until the next commit.
    committed: Vec<Row>,
    generation: u64,
    /// Which slot the table was last read from or written to.
    active_slot: u64,
    dirty: bool,
}

fn index(x: i32, z: i32) -> usize {
    ((z & 31) as usize) * 32 + (x & 31) as usize
}

impl RegionFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let empty = vec![Row::default(); CHUNKS];
        let mut region =
            Self { file, table: empty.clone(), committed: empty, generation: 0, active_slot: 1, dirty: false };
        region.read_table()?;
        region.committed = region.table.clone();
        Ok(region)
    }

    fn read_table(&mut self) -> io::Result<()> {
        let len = self.file.metadata()?.len();
        let mut best: Option<(u64, u64, Vec<Row>)> = None;
        for slot in 0..2u64 {
            let start = slot * SLOT_SECTORS * SECTOR;
            if len < start + SLOT_BYTES {
                continue;
            }
            let mut bytes = vec![0u8; SLOT_BYTES as usize];
            self.file.seek(SeekFrom::Start(start))?;
            self.file.read_exact(&mut bytes)?;
            if &bytes[..8] != MAGIC {
                continue;
            }
            let generation = u64::from_le_bytes(array(&bytes[8..16]));
            let crc = u32::from_le_bytes(array(&bytes[16..20]));
            if crc32(&bytes[SLOT_HEADER..]) != crc {
                continue;
            }
            if best.as_ref().is_none_or(|b| generation > b.0) {
                let rows = bytes[SLOT_HEADER..]
                    .chunks_exact(ROW)
                    .map(|r| Row {
                        offset: u32::from_le_bytes(array(&r[0..4])),
                        length: u32::from_le_bytes(array(&r[4..8])),
                        crc: u32::from_le_bytes(array(&r[8..12])),
                    })
                    .collect();
                best = Some((generation, slot, rows));
            }
        }
        if let Some((generation, slot, table)) = best {
            self.generation = generation;
            self.active_slot = slot;
            self.table = table;
        }
        Ok(())
    }

    /// Reads a chunk's record. `None` if it was never saved, or if its bytes are damaged.
    pub fn read(&mut self, x: i32, z: i32) -> io::Result<Option<Vec<u8>>> {
        let row = self.table[index(x, z)];
        if row.length == 0 {
            return Ok(None);
        }
        if row.length as usize > MAX_RECORD {
            return Ok(None);
        }
        let mut bytes = vec![0u8; row.length as usize];
        self.file.seek(SeekFrom::Start(row.offset as u64 * SECTOR))?;
        if self.file.read_exact(&mut bytes).is_err() || crc32(&bytes) != row.crc {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    /// Writes a chunk's record into free space. Nothing shows until `commit`; until then, reading
    /// the file (or crashing) still gets the old record.
    pub fn write(&mut self, x: i32, z: i32, bytes: &[u8]) -> io::Result<()> {
        let sectors = (bytes.len() as u64).div_ceil(SECTOR).max(1);
        let offset = self.find_free(sectors);
        self.file.seek(SeekFrom::Start(offset * SECTOR))?;
        self.file.write_all(bytes)?;
        self.table[index(x, z)] = Row { offset: offset as u32, length: bytes.len() as u32, crc: crc32(bytes) };
        self.dirty = true;
        Ok(())
    }

    /// The first run of `sectors` free sectors. Whatever the last commit points at counts as live,
    /// so a crash before the next commit always leaves it readable.
    fn find_free(&self, sectors: u64) -> u64 {
        let mut used: Vec<(u64, u64)> = self
            .table
            .iter()
            .chain(self.committed.iter())
            .filter(|r| r.length > 0)
            .map(|r| (r.offset as u64, r.offset as u64 + r.sectors().max(1)))
            .collect();
        used.sort_unstable();
        let mut at = FIRST_DATA_SECTOR;
        for (start, end) in used {
            if start >= at + sectors {
                break;
            }
            at = at.max(end);
        }
        at
    }

    /// Makes everything written so far durable and visible, atomically.
    pub fn commit(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        // The data first: a table must never point at bytes that might not be on disk.
        self.file.sync_data()?;
        let mut body = Vec::with_capacity(CHUNKS * ROW);
        for r in &self.table {
            body.extend_from_slice(&r.offset.to_le_bytes());
            body.extend_from_slice(&r.length.to_le_bytes());
            body.extend_from_slice(&r.crc.to_le_bytes());
        }
        self.generation += 1;
        let slot = 1 - self.active_slot;
        let mut bytes = Vec::with_capacity(SLOT_BYTES as usize);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes.extend_from_slice(&crc32(&body).to_le_bytes());
        bytes.extend_from_slice(&body);
        self.file.seek(SeekFrom::Start(slot * SLOT_SECTORS * SECTOR))?;
        self.file.write_all(&bytes)?;
        self.file.sync_data()?;
        self.active_slot = slot;
        self.committed.clone_from(&self.table);
        self.dirty = false;
        Ok(())
    }

    /// The chunks saved here, as offsets inside this region.
    pub fn saved(&self) -> Vec<(i32, i32)> {
        (0..CHUNKS)
            .filter(|&i| self.table[i].length > 0)
            .map(|i| ((i % 32) as i32, (i / 32) as i32))
            .collect()
    }
}

fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    out
}

/// All of a world's region files, opened on demand. Records are compressed here.
pub struct RegionStore {
    dir: PathBuf,
    open: FxMap<(i32, i32), RegionFile>,
}

impl RegionStore {
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir, open: FxMap::default() })
    }

    fn region(&mut self, rx: i32, rz: i32) -> io::Result<&mut RegionFile> {
        if !self.open.contains_key(&(rx, rz)) {
            // Few regions are in use at a time; closing the lot is simplest when many are open.
            if self.open.len() >= 64 {
                self.commit()?;
                self.open.clear();
            }
            let path = self.dir.join(format!("r.{rx}.{rz}.zrg"));
            self.open.insert((rx, rz), RegionFile::open(&path)?);
        }
        self.open.get_mut(&(rx, rz)).ok_or_else(|| io::Error::other("region vanished"))
    }

    pub fn read(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        let (rx, rz) = (pos.x >> 5, pos.z >> 5);
        let Some(compressed) = self.region(rx, rz)?.read(pos.x, pos.z)? else { return Ok(None) };
        Ok(decompress(&compressed))
    }

    pub fn write(&mut self, pos: ChunkPos, bytes: &[u8]) -> io::Result<()> {
        let (rx, rz) = (pos.x >> 5, pos.z >> 5);
        let compressed = lz4_flex::compress_prepend_size(bytes);
        self.region(rx, rz)?.write(pos.x, pos.z, &compressed)
    }

    pub fn commit(&mut self) -> io::Result<()> {
        for region in self.open.values_mut() {
            region.commit()?;
        }
        Ok(())
    }

    /// Every chunk this world has saved. Reads each region's table.
    pub fn index(&mut self) -> io::Result<Vec<ChunkPos>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            let mut parts = name.split('.');
            let (Some("r"), Some(rx), Some(rz), Some("zrg")) = (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Ok(rx), Ok(rz)) = (rx.parse::<i32>(), rz.parse::<i32>()) else { continue };
            for (x, z) in self.region(rx, rz)?.saved() {
                out.push(ChunkPos::new((rx << 5) + x, (rz << 5) + z));
            }
        }
        Ok(out)
    }
}

fn decompress(bytes: &[u8]) -> Option<Vec<u8>> {
    // The first four bytes claim a size: check it before believing it enough to allocate.
    if bytes.len() < 4 || u32::from_le_bytes(array(&bytes[..4])) as usize > MAX_RECORD {
        return None;
    }
    lz4_flex::decompress_size_prepended(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("zenith-store-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).expect("temp dir");
            Self(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("r.0.0.zrg")
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn records_survive_a_reopen() {
        let dir = TempDir::new("reopen");
        {
            let mut r = RegionFile::open(&dir.file()).expect("open");
            r.write(3, 4, b"hello").expect("write");
            r.write(31, 31, &vec![7u8; 10_000]).expect("write");
            r.commit().expect("commit");
        }
        let mut r = RegionFile::open(&dir.file()).expect("open");
        assert_eq!(r.read(3, 4).expect("read").as_deref(), Some(&b"hello"[..]));
        assert_eq!(r.read(31, 31).expect("read"), Some(vec![7u8; 10_000]));
        assert_eq!(r.read(0, 0).expect("read"), None);
    }

    #[test]
    fn nothing_written_shows_after_a_crash_before_commit() {
        let dir = TempDir::new("nocommit");
        {
            let mut r = RegionFile::open(&dir.file()).expect("open");
            r.write(1, 1, b"old").expect("write");
            r.commit().expect("commit");
            r.write(1, 1, b"new but never committed").expect("write");
            // Dropped here without a commit: the same thing a crash leaves behind.
        }
        let mut r = RegionFile::open(&dir.file()).expect("open");
        assert_eq!(r.read(1, 1).expect("read").as_deref(), Some(&b"old"[..]));
    }

    #[test]
    fn a_torn_table_write_falls_back_to_the_previous_save() {
        let dir = TempDir::new("torn");
        let active_slot;
        {
            let mut r = RegionFile::open(&dir.file()).expect("open");
            r.write(1, 1, b"first").expect("write");
            r.commit().expect("commit");
            r.write(1, 1, b"second").expect("write");
            r.commit().expect("commit");
            active_slot = r.active_slot;
        }
        // Tear the newest table, as a crash in the middle of writing it would.
        let mut raw = fs::read(dir.file()).expect("read");
        let start = (active_slot * SLOT_SECTORS * SECTOR) as usize;
        raw[start + 100] ^= 0xFF;
        fs::write(dir.file(), &raw).expect("write");
        let mut r = RegionFile::open(&dir.file()).expect("open");
        assert_eq!(r.read(1, 1).expect("read").as_deref(), Some(&b"first"[..]));
    }

    #[test]
    fn a_save_never_writes_over_what_the_last_save_points_at() {
        let dir = TempDir::new("noclobber");
        let mut r = RegionFile::open(&dir.file()).expect("open");
        r.write(1, 1, &vec![1u8; 9000]).expect("write");
        r.commit().expect("commit");
        let old = r.table[index(1, 1)];
        r.write(1, 1, &vec![2u8; 9000]).expect("write");
        let new = r.table[index(1, 1)];
        let (old_end, new_end) = (old.offset as u64 + old.sectors(), new.offset as u64 + new.sectors());
        assert!(new.offset as u64 >= old_end || old.offset as u64 >= new_end, "{old:?} overlaps {new:?}");
    }

    #[test]
    fn flipped_bits_in_a_record_read_as_missing_rather_than_as_garbage() {
        let dir = TempDir::new("bitrot");
        {
            let mut r = RegionFile::open(&dir.file()).expect("open");
            r.write(2, 2, b"precious").expect("write");
            r.commit().expect("commit");
        }
        let mut raw = fs::read(dir.file()).expect("read");
        let data_start = (FIRST_DATA_SECTOR * SECTOR) as usize;
        raw[data_start] ^= 0x01;
        fs::write(dir.file(), &raw).expect("write");
        let mut r = RegionFile::open(&dir.file()).expect("open");
        assert_eq!(r.read(2, 2).expect("read"), None);
    }

    #[test]
    fn a_file_of_garbage_opens_as_an_empty_region() {
        let dir = TempDir::new("garbage");
        fs::write(dir.file(), vec![0xABu8; 100_000]).expect("write");
        let mut r = RegionFile::open(&dir.file()).expect("open");
        assert_eq!(r.read(0, 0).expect("read"), None);
        r.write(0, 0, b"fresh").expect("write");
        r.commit().expect("commit");
        assert_eq!(r.read(0, 0).expect("read").as_deref(), Some(&b"fresh"[..]));
    }

    #[test]
    fn space_is_reused_once_a_newer_save_has_replaced_it() {
        let dir = TempDir::new("reuse");
        let mut r = RegionFile::open(&dir.file()).expect("open");
        for i in 0..50u8 {
            r.write(5, 5, &vec![i; 20_000]).expect("write");
            r.commit().expect("commit");
        }
        let size = fs::metadata(dir.file()).expect("metadata").len();
        // One live copy, one the previous table can still see, plus headers: nowhere near 50 copies.
        assert!(size < 200_000, "the file grew to {size} bytes");
        assert_eq!(r.read(5, 5).expect("read"), Some(vec![49u8; 20_000]));
    }

    #[test]
    fn the_store_compresses_and_lists_what_it_holds() {
        let dir = TempDir::new("store");
        let mut s = RegionStore::new(dir.0.join("regions")).expect("store");
        let data = vec![42u8; 50_000];
        let positions = [ChunkPos::new(0, 0), ChunkPos::new(-1, -1), ChunkPos::new(40, -70)];
        for p in positions {
            s.write(p, &data).expect("write");
        }
        s.commit().expect("commit");
        let mut s = RegionStore::new(dir.0.join("regions")).expect("store");
        for p in positions {
            assert_eq!(s.read(p).expect("read"), Some(data.clone()));
        }
        assert_eq!(s.read(ChunkPos::new(1, 1)).expect("read"), None);
        let mut listed = s.index().expect("index");
        listed.sort_unstable();
        let mut expected = positions.to_vec();
        expected.sort_unstable();
        assert_eq!(listed, expected);
        // 3 x 50 KB of one value: compression has to have done its job.
        let on_disk: u64 = fs::read_dir(dir.0.join("regions"))
            .expect("dir")
            .map(|e| e.expect("entry").metadata().expect("metadata").len())
            .sum();
        assert!(on_disk < 3 * 50_000, "{on_disk} bytes on disk");
    }

    #[test]
    fn a_lying_size_prefix_does_not_allocate_gigabytes() {
        assert!(decompress(&[0xFF, 0xFF, 0xFF, 0xFF, 1, 2, 3]).is_none());
        assert!(decompress(&[1, 2]).is_none());
    }
}
