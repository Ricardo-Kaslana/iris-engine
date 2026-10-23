//! 纯玩法核心：方块调色板、世界数据、光照、确定性地形、玩家数学与 DDA。
//!
//! 本模块只依赖 glam 与标准库，不引用任何引擎类型，便于脱离窗口与
//! GPU 做确定性单测。

pub mod block;
pub mod inventory;
pub mod light;
pub mod player;
pub mod raycast;
pub mod terrain;
pub mod world;

pub use block::{Block, Face, Tile};
pub use inventory::{BLOCK_KINDS, Inventory};
pub use light::{LightGrid, MAX_LIGHT, vertex_rgb};
pub use player::{
    EYE_HEIGHT, EYE_OFFSET, GRAVITY_SCALE, JUMP_SPEED, LOOK_SENSITIVITY, PITCH_LIMIT,
    PLAYER_HALF_HEIGHT, PLAYER_RADIUS, PlayerState, SPRINT_SPEED, TERMINAL_SPEED, WALK_SPEED,
    apply_look, flat_forward, flat_right, forward, wants_sprint, wish_direction,
};
pub use raycast::{BlockHit, pick_block};
pub use world::{
    CHUNK_COUNT, CHUNK_HEIGHT, CHUNK_SIZE, CHUNK_VOLUME, Chunk, EditOutcome, VoxelWorld,
    WORLD_CHUNKS, WORLD_SIZE,
};

/// splitmix64 终混函数。地形噪声、树木选址、矿脉流与方块色调抖动共用，
/// 无全局状态，同输入逐位一致。
pub(crate) fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
