//! 均匀网格 broadphase：把 O(n²) 全配对的**候选筛选**降为「只测邻近体」。
//!
//! # 为什么是 BTreeMap 而非 HashMap
//!
//! 引擎物理是 **bit 级确定性**的（`physics` 模块文档 + 逐采样一致性单测）：
//! 同一输入两次模拟必须逐比特一致。`HashMap` 的迭代序依赖 SipHash 与
//! 历史插入 / 删除序列——虽在单次运行内自洽，但属实现细节，不做任何
//! 跨版本 / 跨平台的顺序保证。本模块用 [`BTreeMap`] 按
//! `(i32, i32, i32)` cell 坐标**字典序**迭代：键序即几何序，与哈希种子、
//! 插入顺序、平台全部无关——确定性是选型原因，不是性能取舍。
//!
//! # 语义保证（`physics::step_physics` 依赖的两条）
//!
//! - **保守超集**：凡 AABB 真重叠的两体，必互相出现在对方的
//!   [`candidates`](BroadphaseGrid::candidates) 里（重叠点所在 cell 同时
//!   被两者的插入范围覆盖）——粗筛只可能多给候选、不会漏掉真碰撞；
//! - **升序去重**：`candidates` 返回升序去重的索引——配对循环配
//!   `j > i` 过滤后，配对遍历序与朴素 O(n²) 双层循环**完全一致**，
//!   既有 bit 级确定性测试无需改动即守护等价性。
//!
//! cell 坐标 = `(coord / cell_size).floor()` 钳到 `i32`（float → int 的
//! `as` 转换本就饱和，这里显式钳制把语义写进代码）。

use std::collections::BTreeMap;

use glam::Vec3;

/// 均匀网格 broadphase：cell 尺寸 + 「cell 坐标 → 碰撞体索引列表」。
///
/// 一次性构建、一次性消费（每帧按 AABB 快照重建，不做增量维护——
/// 配合 `step_physics` 的两阶段收集模式，迭代期间不持任何组件借用）。
#[derive(Clone, Debug)]
pub struct BroadphaseGrid {
    /// cell 边长（米），必须为正。
    cell_size: f32,
    /// cell 坐标 → 覆盖该 cell 的碰撞体索引（同一索引可出现在多个 cell）。
    /// BTreeMap：迭代序 = 字典序 = 确定性（见模块文档）。
    cells: BTreeMap<(i32, i32, i32), Vec<usize>>,
}

impl BroadphaseGrid {
    /// 以指定 cell 边长建空网格。`cell_size` 必须为正（断言守护——
    /// 零 / 负边长会让坐标换算除零 / 反向）。
    pub fn new(cell_size: f32) -> Self {
        assert!(
            cell_size > 0.0,
            "BroadphaseGrid 的 cell_size 必须为正，实际 {cell_size}"
        );
        Self {
            cell_size,
            cells: BTreeMap::new(),
        }
    }

    /// 插入一个 AABB（覆盖其跨越的**全部** cell）。
    ///
    /// 同一索引可多次插入（调用方对多个 AABB 各插一次时共用同一索引
    /// 语义不变）；单次插入内每 cell 至多记录一次。
    pub fn insert_aabb(&mut self, index: usize, min: Vec3, max: Vec3) {
        let (x0, y0, z0) = self.cell_of(min);
        let (x1, y1, z1) = self.cell_of(max);
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    // 覆盖型插入：cell 内无去重需求（索引每 cell 只插一次）
                    self.cells.entry((x, y, z)).or_default().push(index);
                }
            }
        }
    }

    /// 查询与 AABB `min..max` 覆盖 cell 有交集的全部索引（**升序去重**）。
    ///
    /// 保守超集性质：凡与查询 AABB 真重叠的已插入 AABB，其索引必在
    /// 返回值中（重叠点所在 cell 双方都覆盖，见模块文档）。
    pub fn candidates(&self, min: Vec3, max: Vec3) -> Vec<usize> {
        let (x0, y0, z0) = self.cell_of(min);
        let (x1, y1, z1) = self.cell_of(max);
        let mut found: Vec<usize> = Vec::new();
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if let Some(indices) = self.cells.get(&(x, y, z)) {
                        found.extend_from_slice(indices);
                    }
                }
            }
        }
        // 升序去重：同一索引跨多 cell 会出现多次；排序保证配对循环的
        // 遍历序与 O(n²) 双层循环一致（确定性契约的一部分）
        found.sort_unstable();
        found.dedup();
        found
    }

    /// 世界坐标 → cell 坐标（floor + 钳制到 i32 值域；负坐标正确向负取整）。
    fn cell_of(&self, coord: Vec3) -> (i32, i32, i32) {
        let to_cell = |v: f32| -> i32 {
            let scaled = (v / self.cell_size).floor();
            // f32 → i32 的 as 转换本就饱和（NaN → 0），显式钳制让语义可见
            scaled.clamp(i32::MIN as f32, i32::MAX as f32) as i32
        };
        (to_cell(coord.x), to_cell(coord.y), to_cell(coord.z))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 基础：插入 / 查询往返 ----

    /// 单 AABB 跨多 cell：查询其覆盖范围内的任何位置都能召回索引。
    #[test]
    fn insert_and_query_roundtrip() {
        let mut grid = BroadphaseGrid::new(1.0);
        // AABB 覆盖 (0..2)³ → 8 个 cell
        grid.insert_aabb(7, Vec3::ZERO, Vec3::new(2.0, 2.0, 2.0));
        assert_eq!(
            grid.candidates(Vec3::new(0.5, 0.5, 0.5), Vec3::new(0.6, 0.6, 0.6)),
            vec![7]
        );
        assert_eq!(
            grid.candidates(Vec3::new(1.5, 1.5, 1.5), Vec3::new(1.6, 1.6, 1.6)),
            vec![7]
        );
        // 边界恰好落在相邻 cell 起点上（cell 2 起于坐标 2.0）：仍命中
        assert_eq!(
            grid.candidates(Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.1, 0.1, 0.1)),
            vec![7]
        );
        // 远处查询：空
        assert!(
            grid.candidates(Vec3::new(9.0, 9.0, 9.0), Vec3::new(9.1, 9.1, 9.1))
                .is_empty()
        );

        // 多索引升序去重：3 与 11 都跨 cell (0,0,0)，查询召回 [3, 11]
        let mut grid = BroadphaseGrid::new(1.0);
        grid.insert_aabb(11, Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5));
        grid.insert_aabb(3, Vec3::new(-0.2, -0.2, -0.2), Vec3::new(3.0, 0.2, 0.2));
        let found = grid.candidates(Vec3::new(-0.1, -0.1, -0.1), Vec3::new(0.1, 0.1, 0.1));
        assert_eq!(found, vec![3, 11], "返回应升序去重");
    }

    /// 负坐标与 cell 边界：floor 语义正确（-0.1 → cell -1）。
    #[test]
    fn negative_coordinates_floor_correctly() {
        let mut grid = BroadphaseGrid::new(2.0);
        grid.insert_aabb(0, Vec3::new(-3.0, -3.0, -3.0), Vec3::new(-1.0, -1.0, -1.0));
        // (-3..-1) / 2 → cell -2..-1；查询 (-2.5..-1.5) → cell -2..-1 命中
        assert_eq!(
            grid.candidates(Vec3::new(-2.5, -2.5, -2.5), Vec3::new(-1.5, -1.5, -1.5)),
            vec![0]
        );
        // 正向远处不误召回
        assert!(
            grid.candidates(Vec3::new(4.0, 4.0, 4.0), Vec3::new(5.0, 5.0, 5.0))
                .is_empty()
        );
    }

    /// cell_size 断言：非正边长立即 panic（消息中文）。
    #[test]
    #[should_panic(expected = "cell_size 必须为正")]
    fn zero_cell_size_panics() {
        let _ = BroadphaseGrid::new(0.0);
    }

    /// 确定性：同输入两次构建 + 查询，结果逐元素相等（BTreeMap 字典序，
    /// 见模块文档——HashMap 无此保证）。
    #[test]
    fn same_input_yields_identical_candidates() {
        fn build() -> BroadphaseGrid {
            let mut grid = BroadphaseGrid::new(1.5);
            // 刻意乱序插入：迭代序不应依赖插入序
            grid.insert_aabb(4, Vec3::new(5.0, -2.0, 0.0), Vec3::new(6.0, -1.0, 1.0));
            grid.insert_aabb(0, Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0));
            grid.insert_aabb(2, Vec3::new(-4.0, 3.0, -4.0), Vec3::new(-3.0, 4.0, -3.0));
            grid
        }
        let queries = [
            (Vec3::new(-5.0, -5.0, -5.0), Vec3::new(5.0, 5.0, 5.0)),
            (Vec3::new(0.2, 0.2, 0.2), Vec3::new(0.8, 0.8, 0.8)),
            (Vec3::new(-4.2, 3.1, -3.9), Vec3::new(5.9, 5.5, -3.1)),
        ];
        for (min, max) in queries {
            assert_eq!(
                build().candidates(min, max),
                build().candidates(min, max),
                "同输入两次查询应逐元素相等（{min:?}..{max:?}）"
            );
        }
    }

    /// 确定性 LCG（线性同余，5 行手写——不引入 rand 依赖，固定 seed 可复现）。
    struct Lcg(u64);

    impl Lcg {
        /// 下一状态（Knuth 64 位常数），返回 [0, 1) 均匀 f32。
        fn next_f32(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            // 取高 24 位做 [0,1)：低位周期性更强，乘后右移取高位
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    /// **保守超集性质**（本模块的核心契约）：确定性 LCG 生成 ~200 个固定
    /// AABB，断言「凡 AABB 真重叠的 (i, j) 对，双方都出现在彼此的候选里」
    /// ——粗筛只许多给、不许漏掉。
    #[test]
    fn candidates_are_conservative_superset_of_true_overlaps() {
        const COUNT: usize = 200;
        const SPAN: f32 = 12.0; // 场景尺度：cell_size = 2.0 时平均每 AABB 跨 ~8 cell
        let mut rng = Lcg(0x5EED_2026); // 固定 seed：测试可复现（值本身无含义）

        // 生成固定数据（一次生成、两处使用，避免重生成顺序差异）
        let mut boxes: Vec<(Vec3, Vec3)> = Vec::with_capacity(COUNT);
        for _ in 0..COUNT {
            let min = Vec3::new(
                rng.next_f32() * SPAN - SPAN * 0.5,
                rng.next_f32() * SPAN - SPAN * 0.5,
                rng.next_f32() * SPAN - SPAN * 0.5,
            );
            // 边长 0.3..2.4：有跨多 cell 的大盒也有单 cell 小盒
            let size = Vec3::new(
                0.3 + rng.next_f32() * 2.1,
                0.3 + rng.next_f32() * 2.1,
                0.3 + rng.next_f32() * 2.1,
            );
            boxes.push((min, min + size));
        }

        let mut grid = BroadphaseGrid::new(2.0);
        for (index, (min, max)) in boxes.iter().enumerate() {
            grid.insert_aabb(index, *min, *max);
        }

        // 全配对真重叠检测 + 双向候选断言
        let overlaps = |a: &(Vec3, Vec3), b: &(Vec3, Vec3)| -> bool {
            a.0.x <= b.1.x
                && b.0.x <= a.1.x
                && a.0.y <= b.1.y
                && b.0.y <= a.1.y
                && a.0.z <= b.1.z
                && b.0.z <= a.1.z
        };
        let mut true_pairs = 0;
        for i in 0..COUNT {
            let candidates_i = grid.candidates(boxes[i].0, boxes[i].1);
            for j in (i + 1)..COUNT {
                if overlaps(&boxes[i], &boxes[j]) {
                    true_pairs += 1;
                    assert!(
                        candidates_i.contains(&j),
                        "真重叠对 ({i},{j})：j 应在 i 的候选里"
                    );
                    assert!(
                        grid.candidates(boxes[j].0, boxes[j].1).contains(&i),
                        "真重叠对 ({i},{j})：i 应在 j 的候选里"
                    );
                }
            }
        }
        // 采样非空转的佐证：SPAN=12、COUNT=200 下真重叠对数量可观
        assert!(
            true_pairs > 100,
            "LCG 场景应有可观的真重叠对，实际 {true_pairs}"
        );
    }
}
