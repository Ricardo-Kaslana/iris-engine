//! 背包：按方块种类计数的极简物品栏。
//!
//! 本轮没有物品实体与合成，方块本身就是物品。背包只记录每种方块的
//! 数量，破坏方块时按掉落表增加，放置时消耗一格。数量用 `u32` 饱和
//! 累加，避免长时间游玩后溢出回绕。

use super::block::Block;

/// 方块种类数量（含空气占位，下标即方块编号）。
pub const BLOCK_KINDS: usize = 13;

/// 按方块种类计数的背包。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    counts: [u32; BLOCK_KINDS],
}

impl Inventory {
    pub fn new() -> Self {
        Self::default()
    }

    /// 某种方块的数量。
    pub fn count(&self, block: Block) -> u32 {
        self.counts[block.as_u8() as usize]
    }

    /// 增加数量；空气不入背包。
    pub fn add(&mut self, block: Block, amount: u32) {
        if block == Block::Air || amount == 0 {
            return;
        }
        let slot = &mut self.counts[block.as_u8() as usize];
        *slot = slot.saturating_add(amount);
    }

    /// 扣除数量。数量不足时不扣除并返回 `false`。
    pub fn take(&mut self, block: Block, amount: u32) -> bool {
        if block == Block::Air {
            return false;
        }
        let slot = &mut self.counts[block.as_u8() as usize];
        if *slot < amount {
            return false;
        }
        *slot -= amount;
        true
    }

    /// 背包里所有非零项，按方块编号升序，供 HUD 汇总。
    pub fn entries(&self) -> impl Iterator<Item = (Block, u32)> + '_ {
        self.counts
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .map(|(index, count)| (Block::from_u8(index as u8), *count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 增加与扣除的基本语义。
    #[test]
    fn add_and_take_roundtrip() {
        let mut inventory = Inventory::new();
        assert_eq!(inventory.count(Block::Stone), 0);
        inventory.add(Block::Stone, 3);
        assert_eq!(inventory.count(Block::Stone), 3);
        assert!(inventory.take(Block::Stone, 2));
        assert_eq!(inventory.count(Block::Stone), 1);
        assert!(!inventory.take(Block::Stone, 2), "不足时不应扣除");
        assert_eq!(inventory.count(Block::Stone), 1);
        assert!(inventory.take(Block::Stone, 1));
        assert_eq!(inventory.count(Block::Stone), 0);
    }

    /// 空气不入背包，数量饱和累加不回绕。
    #[test]
    fn air_is_rejected_and_counts_saturate() {
        let mut inventory = Inventory::new();
        inventory.add(Block::Air, 5);
        assert_eq!(inventory.count(Block::Air), 0);
        inventory.add(Block::Dirt, u32::MAX);
        inventory.add(Block::Dirt, 10);
        assert_eq!(inventory.count(Block::Dirt), u32::MAX);
        assert!(!inventory.take(Block::Air, 1));
    }

    /// 非零项按编号升序列出。
    #[test]
    fn entries_skip_empty_slots() {
        let mut inventory = Inventory::new();
        inventory.add(Block::GoldOre, 2);
        inventory.add(Block::Stone, 7);
        let entries: Vec<_> = inventory.entries().collect();
        assert_eq!(
            entries,
            vec![(Block::Stone, 7), (Block::GoldOre, 2)],
            "应按方块编号升序"
        );
    }

    /// 掉落表：草方块掉泥土，基岩与空气无掉落，其余掉自身。
    #[test]
    fn drop_table() {
        assert_eq!(Block::Grass.drop_item(), Some(Block::Dirt));
        assert_eq!(Block::Bedrock.drop_item(), None);
        assert_eq!(Block::Air.drop_item(), None);
        for block in [
            Block::Stone,
            Block::Dirt,
            Block::Sand,
            Block::Wood,
            Block::Planks,
            Block::Glowstone,
            Block::CoalOre,
            Block::IronOre,
            Block::GoldOre,
            Block::Leaves,
        ] {
            assert_eq!(block.drop_item(), Some(block));
        }
    }
}
