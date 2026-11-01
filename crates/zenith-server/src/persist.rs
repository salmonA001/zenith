//! Worlds on disk: the level file, player files, and chunk records on a thread of their own.
use crate::inventory::{Furnace, Inventory, Slot};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use zenith_content::{Registry, Stable};
use zenith_core::glam::DVec3;
use zenith_core::{BlockPos, ChunkPos};
use zenith_protocol::GameMode;
use zenith_world::{Chunk, RegionStore};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LevelMeta {
    pub name: String,
    pub seed: u64,
    /// Everything unmodified is regenerated, so this must match to open the world.
    pub generator_version: u32,
    pub tick: u64,
    pub time: u32,
    pub rain: u8,
    pub spawn: [i32; 3],
    pub default_mode: GameMode,
    /// The numbers given to pack content.
    pub stable: Stable,
    /// Item names by number, so stacks in saves survive the numbering changing.
    pub items: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerSave {
    pub pos: DVec3,
    pub health: u8,
    pub air: u8,
    pub mode: GameMode,
    pub flying: bool,
    pub op: bool,
    pub inventory: Inventory,
}

/// A block that holds more than its state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockEntity {
    Chest(Vec<Slot>),
    Furnace(Furnace),
}

/// What a chunk saves: sections the generator would not make, and its block entities.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub sections: Vec<(u8, Vec<u8>)>,
    pub block_entities: Vec<(BlockPos, BlockEntity)>,
}

impl ChunkRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("records always serialise")
    }
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

/// Writes through a temporary file, so a crash mid-write leaves the old file whole.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

pub fn read_level(dir: &Path) -> io::Result<Option<LevelMeta>> {
    let path = dir.join("level.json");
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn write_level(dir: &Path, level: &LevelMeta) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(level).map_err(io::Error::other)?;
    write_atomic(&dir.join("level.json"), &bytes)
}

/// Player names go into file names: anything but these characters is refused at the door.
pub fn valid_name(name: &str) -> bool {
    (1..=24).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn player_path(dir: &Path, name: &str) -> PathBuf {
    dir.join("players").join(format!("{name}.json"))
}

pub fn read_player(dir: &Path, name: &str) -> Option<PlayerSave> {
    serde_json::from_slice(&fs::read(player_path(dir, name)).ok()?).ok()
}

pub fn write_player(dir: &Path, name: &str, save: &PlayerSave) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(save).map_err(io::Error::other)?;
    write_atomic(&player_path(dir, name), &bytes)
}

/// Maps item numbers in a save to this run's numbers, by name.
pub struct ItemRemap(Vec<u16>);

impl ItemRemap {
    pub fn new(saved_names: &[String], reg: &Registry) -> Self {
        Self(saved_names.iter().map(|n| reg.item_by_name(n).unwrap_or(u16::MAX)).collect())
    }
    fn slot(&self, slot: &mut Slot) {
        if let Some(s) = slot {
            match self.0.get(s.item as usize) {
                Some(&new) if new != u16::MAX => s.item = new,
                // An item whose pack is gone: it cannot be held any more.
                _ => *slot = None,
            }
        }
    }
    pub fn inventory(&self, inv: &mut Inventory) {
        inv.slots.iter_mut().for_each(|s| self.slot(s));
        self.slot(&mut inv.cursor);
    }
    pub fn block_entity(&self, be: &mut BlockEntity) {
        match be {
            BlockEntity::Chest(cells) => cells.iter_mut().for_each(|s| self.slot(s)),
            BlockEntity::Furnace(f) => {
                self.slot(&mut f.input);
                self.slot(&mut f.fuel);
                self.slot(&mut f.output);
            }
        }
    }
}

enum Job {
    Save(Box<Chunk>, Vec<(BlockPos, BlockEntity)>),
    Read(ChunkPos, Box<dyn FnOnce(Option<Vec<u8>>) + Send>),
    Commit(Option<Sender<io::Result<()>>>),
}

/// All disk access goes through one thread, in order, so a read after a save sees the save and the
/// tick never waits for a disk. A snapshot of a chunk costs 24 `Arc`s to hand over.
pub struct Disk {
    tx: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl Disk {
    pub fn open(dir: &Path) -> io::Result<(Self, Vec<ChunkPos>)> {
        let mut store = RegionStore::new(dir.join("regions"))?;
        let saved = store.index()?;
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new().name("disk".into()).spawn(move || disk_thread(store, rx))?;
        Ok((Self { tx, thread: Some(thread) }, saved))
    }

    /// Queues a chunk to be saved. Encoding happens on the disk thread.
    pub fn save(&self, chunk: Chunk, block_entities: Vec<(BlockPos, BlockEntity)>) {
        let _ = self.tx.send(Job::Save(Box::new(chunk), block_entities));
    }

    /// Calls `then` on the disk thread with the chunk's saved bytes.
    pub fn read(&self, pos: ChunkPos, then: impl FnOnce(Option<Vec<u8>>) + Send + 'static) {
        let _ = self.tx.send(Job::Read(pos, Box::new(then)));
    }

    /// Makes everything queued durable, without waiting for it.
    pub fn commit(&self) {
        let _ = self.tx.send(Job::Commit(None));
    }

    /// Makes everything queued durable, and waits.
    pub fn commit_wait(&self) -> io::Result<()> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Job::Commit(Some(tx))).map_err(|_| io::Error::other("the disk thread is gone"))?;
        rx.recv().map_err(|_| io::Error::other("the disk thread is gone"))?
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        let _ = self.commit_wait();
        // Closes the channel, which ends the thread.
        let (dead, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.tx, dead));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn disk_thread(mut store: RegionStore, rx: Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Save(chunk, block_entities) => {
                let record = ChunkRecord { sections: chunk.encode_modified(), block_entities };
                if let Err(e) = store.write(chunk.pos, &record.to_bytes()) {
                    eprintln!("zenith: could not save chunk {:?}: {e}", chunk.pos);
                }
            }
            Job::Read(pos, then) => {
                let bytes = store.read(pos).unwrap_or_else(|e| {
                    eprintln!("zenith: could not read chunk {pos:?}: {e}");
                    None
                });
                then(bytes);
            }
            Job::Commit(reply) => {
                let result = store.commit();
                if let Err(e) = &result {
                    eprintln!("zenith: could not commit the world: {e}");
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
        }
    }
    let _ = store.commit();
}

/// Copies a world folder into a time-stamped folder beside it. It works on a running server too:
/// region files only ever gain data in free space until a commit, and the commit is what is copied.
pub fn backup(dir: &Path, stamp: &str) -> io::Result<PathBuf> {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("world");
    let target = dir.with_file_name(format!("{name}-backup-{stamp}"));
    copy_dir(dir, &target)?;
    Ok(target)
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_content::{BlockState, ItemStack, id};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("zenith-persist-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).expect("temp dir");
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn names_that_could_escape_the_players_folder_are_refused() {
        for bad in ["", "../evil", "a/b", "a\\b", "..", "C:", "name with space", &"x".repeat(25)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        for good in ["ann", "Bot_12", "a-b"] {
            assert!(valid_name(good), "{good:?}");
        }
    }

    #[test]
    fn a_chunk_saved_through_the_disk_thread_reads_back() {
        let dir = TempDir::new("disk");
        let pos = ChunkPos::new(3, -4);
        let mut chunk = Chunk::empty(pos);
        chunk.set(1, 70, 1, BlockState::of(id::STONE));
        let be = (BlockPos::new(49, 70, -63), BlockEntity::Chest(vec![None; 3]));
        {
            let (disk, saved) = Disk::open(&dir.0).expect("open");
            assert!(saved.is_empty());
            disk.save(chunk.clone(), vec![be.clone()]);
            disk.commit_wait().expect("commit");
        }
        let (disk, saved) = Disk::open(&dir.0).expect("reopen");
        assert_eq!(saved, vec![pos]);
        let (tx, rx) = mpsc::channel();
        disk.read(pos, move |bytes| tx.send(bytes).expect("send"));
        let bytes = rx.recv().expect("read").expect("saved");
        let record = ChunkRecord::from_bytes(&bytes).expect("decodes");
        assert_eq!(record.block_entities, vec![be]);
        let mut loaded = Chunk::empty(pos);
        loaded.apply_encoded(&record.sections).expect("applies");
        assert_eq!(loaded.get(1, 70, 1), BlockState::of(id::STONE));
    }

    #[test]
    fn a_read_after_a_save_sees_that_save() {
        let dir = TempDir::new("order");
        let pos = ChunkPos::new(0, 0);
        let (disk, _) = Disk::open(&dir.0).expect("open");
        for i in 0..50u16 {
            let mut chunk = Chunk::empty(pos);
            chunk.set(0, 0, 0, BlockState::new(id::STONE, (i % 16) as u8));
            disk.save(chunk, Vec::new());
            let (tx, rx) = mpsc::channel();
            disk.read(pos, move |bytes| tx.send(bytes).expect("send"));
            let record = ChunkRecord::from_bytes(&rx.recv().expect("read").expect("saved")).expect("decodes");
            let mut loaded = Chunk::empty(pos);
            loaded.apply_encoded(&record.sections).expect("applies");
            assert_eq!(loaded.get(0, 0, 0).meta(), (i % 16) as u8);
        }
    }

    #[test]
    fn stacks_follow_their_names_when_item_numbers_change() {
        let reg = Registry::builtin();
        let dirt = reg.item_by_name("dirt").expect("dirt");
        // A save from a world where dirt was number 5, and an item that no longer exists was 6.
        let mut saved_names = vec!["?".to_string(); 7];
        saved_names[5] = "dirt".into();
        saved_names[6] = "removed_item".into();
        let remap = ItemRemap::new(&saved_names, &reg);
        let mut inv = Inventory::default();
        inv.slots[0] = Some(ItemStack::new(5, 3));
        inv.slots[1] = Some(ItemStack::new(6, 1));
        inv.cursor = Some(ItemStack::new(999, 1));
        remap.inventory(&mut inv);
        assert_eq!(inv.slots[0], Some(ItemStack::new(dirt, 3)));
        assert_eq!(inv.slots[1], None);
        assert_eq!(inv.cursor, None);
    }

    #[test]
    fn writing_atomically_never_leaves_the_temporary_behind() {
        let dir = TempDir::new("atomic");
        let path = dir.0.join("sub").join("f.json");
        write_atomic(&path, b"one").expect("write");
        write_atomic(&path, b"two").expect("write");
        assert_eq!(fs::read(&path).expect("read"), b"two");
        assert!(!path.with_extension("tmp").exists());
    }
}
