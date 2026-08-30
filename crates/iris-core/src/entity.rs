//! 实体标识与分配器。
//!
//! 实体由 `索引 + 代数` 组成：索引回收复用时代数递增，
//! 因此持有已销毁实体的旧 `Entity` 值无法匹配新实体，
//! 一切访问都会安全地返回 `None`（而非未定义行为）。

use std::fmt;

/// 实体句柄：轻量、可复制，可安全地长期持有或序列化（`to_bits`）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entity {
    index: u32,
    generation: u32,
}

impl Entity {
    pub(crate) fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// 实体在分配器内的槽位索引。
    pub fn index(&self) -> u32 {
        self.index
    }

    /// 实体的代数：每次索引复用后递增，用于检测悬垂引用。
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// 序列化为 64 位整数（索引 + 代数），便于网络同步或存档。
    pub fn to_bits(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }

    /// 从 `to_bits` 的结果还原实体。
    pub fn from_bits(bits: u64) -> Self {
        Self { index: bits as u32, generation: (bits >> 32) as u32 }
    }
}

impl fmt::Debug for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}v{}", self.index, self.generation)
    }
}

/// 实体分配器：空闲列表 + 代数组，全部为连续内存结构。
/// 引擎内部实现细节，通过 [`World`](crate::world::World) 间接使用。
pub struct Entities {
    generations: Vec<u32>,
    alive: Vec<bool>,
    free: Vec<u32>,
    alive_count: usize,
}

impl Entities {
    pub(crate) fn new() -> Self {
        Self {
            generations: Vec::new(),
            alive: Vec::new(),
            free: Vec::new(),
            alive_count: 0,
        }
    }

    pub(crate) fn alloc(&mut self) -> Entity {
        self.alive_count += 1;
        if let Some(index) = self.free.pop() {
            self.generations[index as usize] += 1;
            self.alive[index as usize] = true;
            Entity::new(index, self.generations[index as usize])
        } else {
            let index = self.generations.len() as u32;
            self.generations.push(0);
            self.alive.push(true);
            Entity::new(index, 0)
        }
    }

    /// 释放实体。若实体已失效（不存在 / 代数不匹配）返回 `false`。
    pub(crate) fn free(&mut self, entity: Entity) -> bool {
        let index = entity.index() as usize;
        if !self.contains(entity) {
            return false;
        }
        self.alive[index] = false;
        self.alive_count -= 1;
        self.free.push(entity.index());
        true
    }

    /// 实体是否仍然存活（索引有效且代数匹配）。
    pub fn contains(&self, entity: Entity) -> bool {
        let index = entity.index() as usize;
        index < self.generations.len()
            && self.alive[index]
            && self.generations[index] == entity.generation()
    }

    /// 槽位总数（含已回收槽位）。供「以实体本身为查询项」的迭代基准使用。
    pub(crate) fn slot_count(&self) -> usize {
        self.generations.len()
    }

    /// 第 `index` 个槽位当前代数的实体（无论是否存活）。
    pub(crate) fn entity_at_slot(&self, index: usize) -> Entity {
        Entity::new(index as u32, self.generations[index])
    }

    pub(crate) fn len(&self) -> usize {
        self.alive_count
    }

    /// 按索引升序遍历所有存活实体。
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.alive
            .iter()
            .enumerate()
            .filter(|(_, alive)| **alive)
            .map(|(index, _)| Entity::new(index as u32, self.generations[index]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_yields_distinct_entities() {
        let mut entities = Entities::new();
        let a = entities.alloc();
        let b = entities.alloc();
        assert_ne!(a, b);
        assert!(entities.contains(a));
        assert!(entities.contains(b));
        assert_eq!(entities.len(), 2);
    }

    #[test]
    fn freed_entity_becomes_invalid() {
        let mut entities = Entities::new();
        let a = entities.alloc();
        assert!(entities.free(a));
        assert!(!entities.contains(a));
        // 重复释放无效
        assert!(!entities.free(a));
        assert_eq!(entities.len(), 0);
    }

    #[test]
    fn reused_index_gets_new_generation() {
        let mut entities = Entities::new();
        let a = entities.alloc();
        assert!(entities.free(a));
        let b = entities.alloc();
        assert_eq!(a.index(), b.index(), "索引应被复用");
        assert_ne!(a, b, "代数应递增，旧引用立即失效");
        assert!(!entities.contains(a));
        assert!(entities.contains(b));
    }

    #[test]
    fn bits_roundtrip() {
        let mut entities = Entities::new();
        for _ in 0..5 {
            entities.alloc();
        }
        let e = entities.alloc();
        assert_eq!(Entity::from_bits(e.to_bits()), e);
    }

    #[test]
    fn iter_returns_only_alive() {
        let mut entities = Entities::new();
        let a = entities.alloc();
        entities.alloc();
        entities.free(a);
        let alive: Vec<_> = entities.iter().collect();
        assert_eq!(alive.len(), 1);
        assert_ne!(alive[0], a);
    }
}
