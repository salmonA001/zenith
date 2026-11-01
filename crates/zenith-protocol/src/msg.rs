//! Every message, in both directions.
use serde::{Deserialize, Serialize};
use zenith_content::{BlockState, ItemStack};
use zenith_core::{BlockPos, ChunkPos};
use zenith_world::physics::{Body, MoveInput};

/// Bump whenever a message changes shape. Clients and servers refuse each other otherwise.
pub const PROTOCOL: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameMode {
    #[default]
    Survival,
    Creative,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum C2S {
    Hello {
        protocol: u32,
        name: String,
        password: Option<String>,
    },
    /// One tick of movement. Sent once per client tick, and the only way a player moves.
    Input {
        seq: u32,
        input: MoveInput,
    },
    /// Tell me what differs from the generator in these chunks, and about changes to them.
    Subscribe(Vec<ChunkPos>),
    Unsubscribe(Vec<ChunkPos>),
    /// Start breaking a block (at once, in creative).
    Dig {
        pos: BlockPos,
        face: u8,
    },
    CancelDig,
    /// Use what is in hand on a face of a block: place it, or open what is there.
    UseOn {
        pos: BlockPos,
        face: u8,
    },
    /// Use what is in hand on nothing: eat it.
    UseItem,
    Attack {
        entity: u32,
    },
    SelectSlot(u8),
    DropItem {
        all: bool,
    },
    /// A click in the open screen. See `slots` for how slots are numbered.
    Click {
        slot: u16,
        button: ClickButton,
    },
    CloseScreen,
    /// Creative only: put a whole stack of an item on the cursor.
    Pick {
        item: u16,
    },
    SetFlying(bool),
    Chat(String),
    Respawn,
    Pong(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClickButton {
    Left,
    Right,
    /// Shift-click: move the whole stack to the other side.
    Quick,
}

/// How slots of a screen are numbered in `C2S::Click`.
pub mod slots {
    /// The player's inventory: 0-8 is the hotbar, 9-35 the rest.
    pub const INVENTORY: u16 = 36;
    /// The open screen's own slots start here. Crafting: the grid, then the result.
    /// Furnace: input, fuel, output. Chest: its cells.
    pub const SCREEN: u16 = 100;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum S2C {
    Welcome(Box<Welcome>),
    Reject(String),
    Deltas(Vec<ChunkDelta>),
    Blocks(Vec<BlockChange>),
    /// This player's own body, after the input numbered `ack`. Sent every tick.
    Me {
        ack: u32,
        body: Body,
    },
    Spawn(Vec<EntityInfo>),
    Moves(Vec<EntityMove>),
    Despawn(Vec<u32>),
    Vitals(Vitals),
    Inventory {
        slots: Vec<Option<ItemStack>>,
        selected: u8,
    },
    Cursor(Option<ItemStack>),
    /// The open screen, or none. Sent when it opens and whenever its contents change.
    Screen(Option<Screen>),
    Chat(ChatLine),
    Clock(Clock),
    Effect(Effect),
    /// How far along breaking a block is, 0-9, or `None` when no longer being broken.
    Cracks {
        pos: BlockPos,
        stage: Option<u8>,
    },
    Players(Vec<PlayerEntry>),
    Ping(u32),
    Bye(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub entity_id: u32,
    /// With these two the client generates every chunk the server has not modified.
    pub seed: u64,
    pub generator_version: u32,
    /// `Registry::to_bytes`, compressed.
    #[serde(with = "serde_bytes")]
    pub registry: Vec<u8>,
    pub body: Body,
    pub mode: GameMode,
    pub clock: Clock,
    pub server_name: String,
    /// The radius the server simulates. Terrain further out is frozen for this client.
    pub sim_distance: u8,
}

/// The sections of a chunk that differ from what the generator makes. Usually there are none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChunkDelta {
    pub pos: ChunkPos,
    /// `Chunk::encode_modified`, compressed. Empty when nothing differs.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

impl ChunkDelta {
    pub fn pack(pos: ChunkPos, sections: &[(u8, Vec<u8>)]) -> Self {
        if sections.is_empty() {
            return Self { pos, data: Vec::new() };
        }
        let raw = postcard::to_stdvec(sections).expect("sections always serialise");
        Self { pos, data: lz4_flex::compress_prepend_size(&raw) }
    }
    /// Reads what `pack` wrote, from a peer that may send anything.
    pub fn unpack(&self) -> Result<Vec<(u8, Vec<u8>)>, String> {
        if self.data.is_empty() {
            return Ok(Vec::new());
        }
        let raw = decompress(&self.data)?;
        postcard::from_bytes(&raw).map_err(|e| format!("bad chunk delta: {e}"))
    }
}

/// Decompresses a size-prefixed block, refusing sizes no honest message reaches.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    const LIMIT: usize = 64 * 1024 * 1024;
    let Some(prefix) = data.get(..4) else { return Err("truncated".into()) };
    let size = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
    if size > LIMIT {
        return Err(format!("{size} bytes is more than any message holds"));
    }
    lz4_flex::decompress_size_prepended(data).map_err(|e| e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockChange {
    pub pos: BlockPos,
    pub state: BlockState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityKind {
    Player,
    Item,
    FallingBlock,
    Tnt,
    Arrow,
    Sheep,
    Pig,
    Chicken,
    Zombie,
    Skeleton,
}

impl EntityKind {
    pub const fn is_mob(self) -> bool {
        matches!(self, Self::Sheep | Self::Pig | Self::Chicken | Self::Zombie | Self::Skeleton)
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Item => "item",
            Self::FallingBlock => "falling_block",
            Self::Tnt => "tnt",
            Self::Arrow => "arrow",
            Self::Sheep => "sheep",
            Self::Pig => "pig",
            Self::Chicken => "chicken",
            Self::Zombie => "zombie",
            Self::Skeleton => "skeleton",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Sheep, Self::Pig, Self::Chicken, Self::Zombie, Self::Skeleton, Self::Tnt]
            .into_iter()
            .find(|k| k.name() == name)
    }
    /// Width and height.
    pub const fn size(self) -> (f64, f64) {
        match self {
            Self::Player | Self::Zombie | Self::Skeleton => (0.6, 1.8),
            Self::Item => (0.25, 0.25),
            Self::FallingBlock | Self::Tnt => (0.98, 0.98),
            Self::Arrow => (0.2, 0.2),
            Self::Sheep | Self::Pig => (0.9, 0.9),
            Self::Chicken => (0.4, 0.7),
        }
    }
}

/// Positions on the wire are in 1/256 of a block.
pub const POS_SCALE: f64 = 256.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityInfo {
    pub id: u32,
    pub kind: EntityKind,
    /// In `POS_SCALE`ths of a block. The moves that follow are relative to this.
    pub pos: [i32; 3],
    pub yaw: u8,
    pub pitch: i8,
    /// A player's name.
    pub name: Option<String>,
    /// What an item entity is.
    pub stack: Option<ItemStack>,
    /// What a falling block is.
    pub block: BlockState,
}

/// How an entity moved since the last tick, in `POS_SCALE`ths of a block. Every viewer gets the
/// same numbers, because they all track the same last-sent position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityMove {
    pub id: u32,
    pub d: [i16; 3],
    pub yaw: u8,
    pub pitch: i8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vitals {
    /// Hit points, 0-20.
    pub health: u8,
    /// Breath, 0-20.
    pub air: u8,
    pub mode: GameMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenKind {
    /// The player's own 2x2 crafting.
    Inventory,
    /// 3x3 crafting.
    Bench,
    Furnace,
    Chest,
}

/// The slots of an open screen. The player's inventory is sent apart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    pub kind: ScreenKind,
    pub slots: Vec<Option<ItemStack>>,
    /// Furnace: cooking progress, and fuel left, as 0-255.
    pub progress: [u8; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatLine {
    /// Who said it; `None` for messages from the game itself.
    pub from: Option<String>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    pub tick: u64,
    /// Time of day, 0-23999. 0 is sunrise, 6000 noon, 18000 midnight.
    pub time: u32,
    /// 0 clear, 255 pouring.
    pub rain: u8,
}

/// Something brief to see or hear at a place.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub kind: EffectKind,
    pub pos: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    BlockBroken(BlockState),
    BlockPlaced(BlockState),
    Step(BlockState),
    Hurt,
    Death,
    Explosion,
    Splash,
    Pickup,
    Eat,
    Lever,
    Fizz,
    Shoot,
    Open,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerEntry {
    pub name: String,
    pub ping_ms: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{decode, encode};
    use zenith_core::glam::DVec3;
    use zenith_world::Chunk;
    use zenith_world::physics::Keys;

    #[test]
    fn messages_round_trip() {
        let msgs = vec![
            C2S::Hello { protocol: PROTOCOL, name: "ann".into(), password: None },
            C2S::Input { seq: 9, input: MoveInput { keys: Keys(Keys::JUMP), yaw: 300, pitch: -20 } },
            C2S::Subscribe(vec![ChunkPos::new(-1, 200)]),
            C2S::Click { slot: slots::SCREEN + 2, button: ClickButton::Quick },
        ];
        for m in msgs {
            let frame = encode(&m);
            assert_eq!(decode::<C2S>(&frame).expect("decodes"), m);
        }
        let body = Body::at(DVec3::new(1.5, 70.25, -3.0));
        let m = S2C::Me { ack: 7, body };
        assert_eq!(decode::<S2C>(&encode(&m)).expect("decodes"), m);
    }

    #[test]
    fn the_hot_messages_stay_small() {
        // Sent once per entity, per viewer, per tick: every byte here costs a lot at scale.
        let moved = S2C::Moves(vec![EntityMove { id: 1200, d: [12, 0, -30], yaw: 3, pitch: 0 }]);
        assert!(encode(&moved).len() <= 10, "{} bytes", encode(&moved).len());
        let input = C2S::Input { seq: 100_000, input: MoveInput { keys: Keys(3), yaw: 40000, pitch: -300 } };
        assert!(encode(&input).len() <= 12, "{} bytes", encode(&input).len());
    }

    #[test]
    fn an_untouched_chunk_costs_almost_nothing_to_describe() {
        let d = ChunkDelta::pack(ChunkPos::new(5, -5), &[]);
        assert!(d.data.is_empty());
        assert!(encode(&S2C::Deltas(vec![d])).len() < 8);
    }

    #[test]
    fn chunk_deltas_round_trip_and_compress() {
        let mut c = Chunk::empty(ChunkPos::new(1, 2));
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, 70, z, BlockState::of(zenith_content::id::STONE));
            }
        }
        let sections = c.encode_modified();
        let d = ChunkDelta::pack(c.pos, &sections);
        assert_eq!(d.unpack().expect("unpacks"), sections);
        let raw: usize = sections.iter().map(|(_, b)| b.len()).sum();
        assert!(d.data.len() < raw);
    }

    #[test]
    fn hostile_deltas_are_errors_not_panics() {
        let lying = ChunkDelta { pos: ChunkPos::new(0, 0), data: vec![0xFF, 0xFF, 0xFF, 0x7F, 1, 2, 3] };
        assert!(lying.unpack().is_err());
        let short = ChunkDelta { pos: ChunkPos::new(0, 0), data: vec![1] };
        assert!(short.unpack().is_err());
        assert!(decode::<S2C>(&[250, 250, 250]).is_err());
        assert!(decode::<C2S>(&[]).is_err());
    }
}
