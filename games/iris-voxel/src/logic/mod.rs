//! 纯玩法核心：方块调色板、世界数据、光照、确定性地形、玩家数学与 DDA。
//!
//! 本模块只依赖 glam 与标准库，不引用任何引擎类型，便于脱离窗口与
//! GPU 做确定性单测。

pub mod block;
pub mod light;
pub mod player;
pub mod raycast;
pub mod terrain;
pub mod world;

pub use block::{Block, Face, Tile};
pub use light::{LightGrid, MAX_LIGHT, vertex_rgb};
pub use player::{
    EYE_HEIGHT, EYE_OFFSET, JUMP_SPEED, LOOK_SENSITIVITY, PITCH_LIMIT, PLAYER_HALF_HEIGHT,
    PLAYER_RADIUS, PlayerState, WALK_SPEED, apply_look, flat_forward, flat_right, forward,
    wish_direction,
};
pub use raycast::{BlockHit, pick_block};
pub use world::{
    CHUNK_COUNT, CHUNK_HEIGHT, CHUNK_SIZE, CHUNK_VOLUME, Chunk, EditOutcome, VoxelWorld,
    WORLD_CHUNKS, WORLD_SIZE,
};
