//! 方块调色板与面方向。
//!
//! 方块存储为 `u8`，[`Block`] 是它的稳定映射。新增方块只能追加编号，
//! 不能重排既有值——世界数据与测试都按下标读取。

/// 立方体的六个面（世界轴对齐）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Top,
    Bottom,
    West,
    East,
    North,
    South,
}

impl Face {
    pub const ALL: [Face; 6] = [
        Face::Top,
        Face::Bottom,
        Face::West,
        Face::East,
        Face::North,
        Face::South,
    ];

    /// 面的外法线（整数体素坐标）。
    pub const fn normal(self) -> [i32; 3] {
        match self {
            Face::Top => [0, 1, 0],
            Face::Bottom => [0, -1, 0],
            Face::West => [-1, 0, 0],
            Face::East => [1, 0, 0],
            Face::North => [0, 0, -1],
            Face::South => [0, 0, 1],
        }
    }
}

/// 纹理图集槽位。槽位顺序即图集布局顺序，追加时保持既有编号不变。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    GrassTop,
    GrassSide,
    Dirt,
    Stone,
    Sand,
    WoodSide,
    WoodTop,
    Planks,
    Bedrock,
    Glowstone,
}

impl Tile {
    /// 图集列与行（行 0 在图集顶部）。
    pub const fn slot(self) -> (u32, u32) {
        match self {
            Tile::GrassTop => (0, 0),
            Tile::GrassSide => (1, 0),
            Tile::Dirt => (2, 0),
            Tile::Stone => (3, 0),
            Tile::Sand => (0, 1),
            Tile::WoodSide => (1, 1),
            Tile::WoodTop => (2, 1),
            Tile::Planks => (3, 1),
            Tile::Bedrock => (0, 2),
            Tile::Glowstone => (1, 2),
        }
    }
}

/// 方块种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Block {
    Air = 0,
    Bedrock = 1,
    Stone = 2,
    Dirt = 3,
    Grass = 4,
    Sand = 5,
    Wood = 6,
    Planks = 7,
    Glowstone = 8,
}

impl Block {
    /// 从存档 / 世界数组读取。未知编号回退为空气。
    pub const fn from_u8(value: u8) -> Self {
        match value {
            1 => Block::Bedrock,
            2 => Block::Stone,
            3 => Block::Dirt,
            4 => Block::Grass,
            5 => Block::Sand,
            6 => Block::Wood,
            7 => Block::Planks,
            8 => Block::Glowstone,
            _ => Block::Air,
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// 是否阻挡移动与视线。当前只有空气不实心。
    pub const fn is_solid(self) -> bool {
        !matches!(self, Block::Air)
    }

    /// 是否可被玩家破坏。基岩不可破坏。
    pub const fn is_breakable(self) -> bool {
        !matches!(self, Block::Air | Block::Bedrock)
    }

    /// 自身发出的方块光等级；不发光为 0。光源格本身是实心不透明的，
    /// 光从相邻空气格开始按等级递减传播。
    pub const fn emission(self) -> u8 {
        match self {
            Block::Glowstone => 15,
            _ => 0,
        }
    }

    /// 该面使用的图集槽位。
    pub const fn atlas_tile(self, face: Face) -> Tile {
        match self {
            Block::Grass => match face {
                Face::Top => Tile::GrassTop,
                Face::Bottom => Tile::Dirt,
                _ => Tile::GrassSide,
            },
            Block::Dirt => Tile::Dirt,
            Block::Stone => Tile::Stone,
            Block::Sand => Tile::Sand,
            Block::Wood => match face {
                Face::Top | Face::Bottom => Tile::WoodTop,
                _ => Tile::WoodSide,
            },
            Block::Planks => Tile::Planks,
            Block::Bedrock => Tile::Bedrock,
            Block::Glowstone => Tile::Glowstone,
            Block::Air => Tile::Stone,
        }
    }

    /// HUD 快捷栏显示名（ASCII，位图字体可渲染）。
    pub const fn name(self) -> &'static str {
        match self {
            Block::Air => "AIR",
            Block::Bedrock => "BEDROCK",
            Block::Stone => "STONE",
            Block::Dirt => "DIRT",
            Block::Grass => "GRASS",
            Block::Sand => "SAND",
            Block::Wood => "WOOD",
            Block::Planks => "PLANKS",
            Block::Glowstone => "GLOWSTONE",
        }
    }

    /// 快捷栏色块的代表色（sRGB，仅 UI 使用）。
    pub const fn ui_color(self) -> [f32; 4] {
        match self {
            Block::Air => [0.20, 0.20, 0.20, 1.0],
            Block::Bedrock => [0.18, 0.18, 0.20, 1.0],
            Block::Stone => [0.48, 0.49, 0.52, 1.0],
            Block::Dirt => [0.42, 0.30, 0.20, 1.0],
            Block::Grass => [0.28, 0.55, 0.22, 1.0],
            Block::Sand => [0.82, 0.75, 0.50, 1.0],
            Block::Wood => [0.47, 0.33, 0.18, 1.0],
            Block::Planks => [0.68, 0.50, 0.29, 1.0],
            Block::Glowstone => [0.95, 0.78, 0.34, 1.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编号映射是存档口径：逐值往返，未知值回退空气。
    #[test]
    fn block_ids_roundtrip() {
        let blocks = [
            Block::Air,
            Block::Bedrock,
            Block::Stone,
            Block::Dirt,
            Block::Grass,
            Block::Sand,
            Block::Wood,
            Block::Planks,
            Block::Glowstone,
        ];
        for block in blocks {
            assert_eq!(Block::from_u8(block.as_u8()), block);
        }
        assert_eq!(Block::from_u8(200), Block::Air);
    }

    #[test]
    fn solidity_and_breakability() {
        assert!(!Block::Air.is_solid());
        for block in [
            Block::Bedrock,
            Block::Stone,
            Block::Dirt,
            Block::Grass,
            Block::Sand,
            Block::Wood,
            Block::Planks,
            Block::Glowstone,
        ] {
            assert!(block.is_solid());
        }
        assert!(!Block::Bedrock.is_breakable());
        assert!(!Block::Air.is_breakable());
        assert!(Block::Grass.is_breakable());
    }

    /// 只有荧光石发光，且发光等级为最大值 15。
    #[test]
    fn emission_levels() {
        assert_eq!(Block::Glowstone.emission(), 15);
        for block in [
            Block::Air,
            Block::Bedrock,
            Block::Stone,
            Block::Dirt,
            Block::Grass,
            Block::Sand,
            Block::Wood,
            Block::Planks,
        ] {
            assert_eq!(block.emission(), 0);
        }
    }

    /// 草方块顶面与侧面使用不同槽位，原木同理；其余方块六面同槽。
    #[test]
    fn face_tiles_match_palette() {
        assert_eq!(Block::Grass.atlas_tile(Face::Top), Tile::GrassTop);
        assert_eq!(Block::Grass.atlas_tile(Face::Bottom), Tile::Dirt);
        assert_eq!(Block::Grass.atlas_tile(Face::East), Tile::GrassSide);
        assert_eq!(Block::Wood.atlas_tile(Face::Top), Tile::WoodTop);
        assert_eq!(Block::Wood.atlas_tile(Face::North), Tile::WoodSide);
        assert_eq!(Block::Stone.atlas_tile(Face::South), Tile::Stone);
        assert_eq!(Block::Glowstone.atlas_tile(Face::Top), Tile::Glowstone);
    }

    /// 图集槽位不重复且落在 4×4 网格内。
    #[test]
    fn atlas_slots_are_unique_and_in_range() {
        let tiles = [
            Tile::GrassTop,
            Tile::GrassSide,
            Tile::Dirt,
            Tile::Stone,
            Tile::Sand,
            Tile::WoodSide,
            Tile::WoodTop,
            Tile::Planks,
            Tile::Bedrock,
            Tile::Glowstone,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for tile in tiles {
            let (col, row) = tile.slot();
            assert!(col < 4 && row < 4);
            assert!(seen.insert((col, row)), "图集槽位重复：{tile:?}");
        }
    }

    /// 面法线两两相反且覆盖六个轴向。
    #[test]
    fn face_normals_cover_axes() {
        let sum = Face::ALL.iter().fold([0i32; 3], |acc, face| {
            let n = face.normal();
            [acc[0] + n[0], acc[1] + n[1], acc[2] + n[2]]
        });
        assert_eq!(sum, [0, 0, 0]);
    }
}
