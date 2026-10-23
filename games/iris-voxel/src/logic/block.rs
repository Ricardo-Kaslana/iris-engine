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
    CoalOre,
    IronOre,
    GoldOre,
    Leaves,
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
            Tile::CoalOre => (2, 2),
            Tile::IronOre => (3, 2),
            Tile::GoldOre => (0, 3),
            Tile::Leaves => (1, 3),
        }
    }
}

/// 方块种类。
///
/// 编号是存档口径：只能追加，不能重排。9 号起是本轮新增的矿石与树叶。
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
    CoalOre = 9,
    IronOre = 10,
    GoldOre = 11,
    Leaves = 12,
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
            9 => Block::CoalOre,
            10 => Block::IronOre,
            11 => Block::GoldOre,
            12 => Block::Leaves,
            _ => Block::Air,
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// 是否阻挡移动与视线。当前只有空气不实心。树叶仍然实心：玩家可以
    /// 站在树冠上，射线也会被树叶挡住。
    pub const fn is_solid(self) -> bool {
        !matches!(self, Block::Air)
    }

    /// 是否完全遮蔽相邻面。空气与树叶不遮蔽——树叶是裁切方块，邻面
    /// 必须照常发射，否则透过叶隙会看到背面剔除后的空洞。
    pub const fn is_opaque(self) -> bool {
        !matches!(self, Block::Air | Block::Leaves)
    }

    /// 是否走裁切网格（alpha 测试）。叶片在贴图里挖出孔洞，必须与
    /// 不透明方块分成两套网格与两个材质。
    pub const fn is_cutout(self) -> bool {
        matches!(self, Block::Leaves)
    }

    /// 光线穿过该方块额外损失多少等级；`None` 表示完全阻挡。
    ///
    /// 空气 0（只按距离衰减），树叶 1：穿过一层叶片的代价是「距离 1 +
    /// 遮挡 1」，树冠下的天空光因此是 13 而不是全黑，同时也不会像
    /// 不透明方块那样把整根柱截断。
    pub const fn light_cost(self) -> Option<u8> {
        match self {
            Block::Air => Some(0),
            Block::Leaves => Some(1),
            _ => None,
        }
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

    /// 破坏后的掉落物。草方块掉泥土，基岩与空气不掉落，其余方块掉自身。
    pub const fn drop_item(self) -> Option<Block> {
        match self {
            Block::Air | Block::Bedrock => None,
            Block::Grass => Some(Block::Dirt),
            other => Some(other),
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
            Block::CoalOre => Tile::CoalOre,
            Block::IronOre => Tile::IronOre,
            Block::GoldOre => Tile::GoldOre,
            Block::Leaves => Tile::Leaves,
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
            Block::CoalOre => "COAL ORE",
            Block::IronOre => "IRON ORE",
            Block::GoldOre => "GOLD ORE",
            Block::Leaves => "LEAVES",
        }
    }

    /// 快捷栏色块的代表色（sRGB，仅 UI 使用）。
    /// 快捷栏色块的代表色。UI 管线按线性 RGBA 消费，这里直接给目标
    /// sRGB 颜色的线性等价，色块在屏幕上才会呈现预期的饱和色。
    pub const fn ui_color(self) -> [f32; 4] {
        match self {
            Block::Air => [0.033, 0.033, 0.033, 1.0],
            Block::Bedrock => [0.056, 0.066, 0.076, 1.0],
            Block::Stone => [0.262, 0.296, 0.323, 1.0],
            Block::Dirt => [0.287, 0.144, 0.069, 1.0],
            Block::Grass => [0.306, 0.508, 0.171, 1.0],
            Block::Sand => [0.792, 0.674, 0.380, 1.0],
            Block::Wood => [0.228, 0.122, 0.048, 1.0],
            Block::Planks => [0.435, 0.244, 0.098, 1.0],
            Block::Glowstone => [0.933, 0.639, 0.155, 1.0],
            Block::CoalOre => [0.064, 0.068, 0.073, 1.0],
            Block::IronOre => [0.393, 0.253, 0.171, 1.0],
            Block::GoldOre => [0.692, 0.448, 0.051, 1.0],
            Block::Leaves => [0.084, 0.238, 0.037, 1.0],
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
            Block::CoalOre,
            Block::IronOre,
            Block::GoldOre,
            Block::Leaves,
        ];
        for block in blocks {
            assert_eq!(Block::from_u8(block.as_u8()), block);
        }
        assert_eq!(Block::from_u8(200), Block::Air);
        assert_eq!(Block::CoalOre.as_u8(), 9, "新增方块只能追加编号");
        assert_eq!(Block::Leaves.as_u8(), 12);
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
            Block::CoalOre,
            Block::IronOre,
            Block::GoldOre,
            Block::Leaves,
        ] {
            assert!(block.is_solid());
        }
        assert!(!Block::Bedrock.is_breakable());
        assert!(!Block::Air.is_breakable());
        assert!(Block::Grass.is_breakable());
        assert!(Block::Leaves.is_breakable());
        assert!(Block::GoldOre.is_breakable());
    }

    /// 遮蔽、裁切与透光是三件不同的事：树叶实心、不遮蔽、走裁切网格，
    /// 并且让天空光额外衰减一级。
    #[test]
    fn leaves_are_solid_but_not_opaque() {
        assert!(Block::Leaves.is_solid(), "树叶仍然挡人与挡视线");
        assert!(!Block::Leaves.is_opaque(), "树叶不能遮蔽邻面");
        assert!(Block::Leaves.is_cutout());
        assert_eq!(Block::Leaves.light_cost(), Some(1));

        assert!(!Block::Air.is_opaque());
        assert_eq!(Block::Air.light_cost(), Some(0));
        assert!(!Block::Air.is_cutout());

        for block in [
            Block::Bedrock,
            Block::Stone,
            Block::Dirt,
            Block::Grass,
            Block::Sand,
            Block::Wood,
            Block::Planks,
            Block::Glowstone,
            Block::CoalOre,
            Block::IronOre,
            Block::GoldOre,
        ] {
            assert!(block.is_opaque(), "{block:?} 应完全遮蔽邻面");
            assert!(!block.is_cutout(), "{block:?} 不应走裁切网格");
            assert_eq!(block.light_cost(), None, "{block:?} 不应透光");
        }
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
            Block::CoalOre,
            Block::IronOre,
            Block::GoldOre,
            Block::Leaves,
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
        assert_eq!(Block::CoalOre.atlas_tile(Face::Top), Tile::CoalOre);
        assert_eq!(Block::IronOre.atlas_tile(Face::West), Tile::IronOre);
        assert_eq!(Block::GoldOre.atlas_tile(Face::Bottom), Tile::GoldOre);
        assert_eq!(Block::Leaves.atlas_tile(Face::South), Tile::Leaves);
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
            Tile::CoalOre,
            Tile::IronOre,
            Tile::GoldOre,
            Tile::Leaves,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for tile in tiles {
            let (col, row) = tile.slot();
            assert!(col < 4 && row < 4);
            assert!(seen.insert((col, row)), "图集槽位重复：{tile:?}");
        }
        assert_eq!(seen.len(), 14, "14 个槽位应全部唯一");
    }

    /// 快捷栏名称是位图字体可渲染的 ASCII，且两两不同。
    #[test]
    fn names_are_ascii_and_unique() {
        let blocks = [
            Block::Bedrock,
            Block::Stone,
            Block::Dirt,
            Block::Grass,
            Block::Sand,
            Block::Wood,
            Block::Planks,
            Block::Glowstone,
            Block::CoalOre,
            Block::IronOre,
            Block::GoldOre,
            Block::Leaves,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for block in blocks {
            let name = block.name();
            assert!(name.is_ascii(), "名称必须能被位图字体渲染：{name}");
            assert!(name.chars().all(|ch| (32u8..127).contains(&(ch as u8))));
            assert!(seen.insert(name), "名称重复：{name}");
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
