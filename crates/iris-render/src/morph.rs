//! 形变目标（morph targets / blend shapes）v1：CPU 混合 + 逐实体动态顶点缓冲。
//!
//! 数据模型（与 glTF 2.0 morphTargets 对应）：
//! - [`MeshData::morph`](crate::mesh::MeshData::morph) 是网格侧表：每个
//!   [`MorphTarget`] 持有「每顶点位置增量 + 可选法线增量」（绑定姿势空间）；
//!   `vertices` 本身仍是绑定姿势；
//! - [`MorphWeights`] 是实体侧组件：逐实体的混合系数（顺序与目标表一对一）。
//!
//! 混合公式（glTF 标准口径，纯函数 [`blend_morph_vertices`]）：
//! `position = p + Σ wᵢ · Δpᵢ`；有法线增量参与时
//! `normal = normalize(n + Σ wᵢ · Δnᵢ)`。求和按目标表固定序累加，
//! `w == 0` 的目标整体跳过（见「逐位锚」）。
//!
//! 执行路径：混合在 CPU 完成，结果逐实体上传为专用顶点缓冲，替换蒙皮
//! 绘制分支（不透明 / 透明 / 阴影 / 场景深度 / RT / 探针捕获 / 水面）
//! 的 slot 0 顶点绑定——**零着色器改动、零管线布局改动**。
//!
//! ## 组件缺席语义
//! [`MorphWeights`] 缺席 = 绑定姿势 = 现有渲染路径**逐位不变**；
//! 网格无 [`MorphData`] 时权重组件在场也无效（静默忽略）。
//!
//! ## 逐位锚
//! - 权重全零（或表空）⇒ 输出顶点与基座**逐位一致**：`w == 0` 的目标
//!   整体跳过，杜绝 `p + 0.0·Δ` 把 `-0.0` 归并为 `+0.0` 的 IEEE 加法
//!   翻转（与 [`skin_vertex`](crate::skin::skin_vertex) 的零权重跳过同法）；
//! - 权重位图案与上帧一致 ⇒ **不重混合、不重上传**（跨运行逐位一致的
//!   静态表达式与零改动路径等价）。
//!
//! ## v1 诚实限制
//! 1. **只对蒙皮网格生效**：非蒙皮网格的 `targets` 照常解析进
//!    [`MorphData`]（CPU 侧可读、可离线混合），但渲染期权重不生效——
//!    静态路径按 (网格, 材质) 共享合批，逐实体形变需先分裂批次；
//!    表情动机场景（蒙皮角色脸）已覆盖；
//! 2. **CPU 混合 O(顶点数 × 目标数) 每帧 + 整份 VBO 重上传**：适合
//!    面部级小网格（数千顶点）；大网格全量形变（如肌肉 / 软体）应走
//!    GPU 存储缓冲形态，未交付；
//! 3. 增量通道只取 POSITION / NORMAL：glTF 的 TEXCOORD 增量与切线增量
//!    加载期丢弃（切线按基座值蒙皮，形变脸的切线误差在法线贴图尺度
//!    以下，可接受——如需要再排）；
//! 4. 权重不做归一化 / 钳制：glTF 语义即「作者声明的直接线性系数」，
//!    超界或 NaN 直通（NaN 使顶点出屏，属资产错误，加载与渲染都不兜底）；
//! 5. 稀疏 accessor 的增量不支持（解析器全池口径，见
//!    `ARCHITECTURE` §8.24），报 `Unsupported`。

use iris_core::Component;

use crate::mesh::Vertex;

/// 单个形变目标：绑定姿势空间的每顶点增量。
#[derive(Clone, Debug)]
pub struct MorphTarget {
    /// 目标名（glTF `mesh.extras.targetNames` 按序对应；无 extras 为 None）。
    pub name: Option<String>,
    /// 位置增量（长度 = 顶点数）。
    pub position_deltas: Vec<[f32; 3]>,
    /// 法线增量（长度 = 顶点数）；None = 该目标不带动法线（混合后
    /// 也不重归一化，基座法线逐位透传）。
    pub normal_deltas: Option<Vec<[f32; 3]>>,
}

impl MorphTarget {
    /// 构造。两个表（若存在）长度必须一致——增量按顶点索引对齐。
    pub fn new(
        name: Option<String>,
        position_deltas: Vec<[f32; 3]>,
        normal_deltas: Option<Vec<[f32; 3]>>,
    ) -> Self {
        if let Some(normals) = &normal_deltas {
            assert_eq!(
                normals.len(),
                position_deltas.len(),
                "法线增量与位置增量的顶点数必须一致"
            );
        }
        Self {
            name,
            position_deltas,
            normal_deltas,
        }
    }

    /// 顶点数（= 位置增量长度；混合期与基座网格数对账）。
    pub fn vertex_count(&self) -> usize {
        self.position_deltas.len()
    }
}

/// 网格的形变目标表（[`MeshData::morph`](crate::mesh::MeshData::morph)
/// 侧表；构造期校验各目标顶点数一致）。
#[derive(Clone, Debug)]
pub struct MorphData {
    pub targets: Vec<MorphTarget>,
}

impl MorphData {
    /// 构造：所有目标的位置增量长度必须一致且非空表（空表 = 无信息，
    /// 调用方应保持 `morph = None`）。
    pub fn new(targets: Vec<MorphTarget>) -> Self {
        assert!(!targets.is_empty(), "形变表至少需要一个目标");
        let count = targets[0].vertex_count();
        for target in &targets {
            assert_eq!(target.vertex_count(), count, "各形变目标的顶点数必须一致");
        }
        Self { targets }
    }

    pub fn target_count(&self) -> usize {
        self.targets.len()
    }

    /// CPU 侧字节数（内存对账口径，与
    /// [`MeshStore::memory_bytes`](crate::mesh::MeshStore::memory_bytes)
    /// 的侧表统计一致）。
    pub fn memory_bytes(&self) -> u64 {
        self.targets
            .iter()
            .map(|target| {
                (target.position_deltas.len() * 12) as u64
                    + target
                        .normal_deltas
                        .as_ref()
                        .map_or(0, |n| (n.len() * 12) as u64)
            })
            .sum()
    }
}

/// 形变权重组件：逐实体的目标混合系数，索引与
/// [`MorphData::targets`] 一对一。
///
/// 配对口径：按 `min(weights.len(), targets.len())` 逐对生效——权重短
/// 则余目标取 0，长则多余权重忽略（表情驱动的运行时表长度可能随
/// 资产变化，不做硬校验）。
///
/// 缺席 = 绑定姿势（渲染路径逐位不变）；仅对**蒙皮**实体生效
/// （模块文档「v1 诚实限制」①）。
#[derive(Clone, Debug, PartialEq, Component)]
pub struct MorphWeights {
    pub weights: Vec<f32>,
}

impl MorphWeights {
    pub fn new(weights: Vec<f32>) -> Self {
        Self { weights }
    }
}

/// 混合基座顶点为一帧的形变顶点（纯函数，脱离 World / GPU 可单测）。
///
/// 语义：
/// - `pairs = min(base 顶点数, 目标数, 权重数)`，按目标表固定序累加：
///   `out[i] = base[i] + Σ wₖ · Δₖ[i]`（浮点加法次序恒定 ⇒ 确定性）；
/// - **`w == 0.0` 的目标整体跳过**：零权重 ⇒ 输出与基座逐位一致
///   （IEEE 中 `-0.0 + 0.0 = +0.0` 会翻转符号位，跳过才是逐位透传）；
/// - 法线：有至少一个「带法线增量且权重非零」的目标参与时，对**每个
///   顶点**做 `n + Σ wₖ·Δnₖ` 后重归一化（`len > 0` 才归一，退化零法线
///   保留原值——glTF 标准「normalize after blending」口径）；否则基座
///   法线逐位透传；
/// - uv 直通。
pub fn blend_morph_vertices(base: &[Vertex], morph: &MorphData, weights: &[f32]) -> Vec<Vertex> {
    let pairs = base.len().min(morph.targets.len()).min(weights.len());
    let mut out = base.to_vec();
    let mut normal_active = false;
    for (target, &w) in morph.targets[..pairs].iter().zip(&weights[..pairs]) {
        if w == 0.0 {
            continue;
        }
        for (i, vertex) in out.iter_mut().enumerate() {
            let dp = target.position_deltas[i];
            vertex.position = [
                vertex.position[0] + w * dp[0],
                vertex.position[1] + w * dp[1],
                vertex.position[2] + w * dp[2],
            ];
        }
        if let Some(deltas) = &target.normal_deltas {
            normal_active = true;
            for (i, vertex) in out.iter_mut().enumerate() {
                let dn = deltas[i];
                vertex.normal = [
                    vertex.normal[0] + w * dn[0],
                    vertex.normal[1] + w * dn[1],
                    vertex.normal[2] + w * dn[2],
                ];
            }
        }
    }
    if normal_active {
        for vertex in out.iter_mut() {
            let n = vertex.normal;
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len > 0.0 {
                vertex.normal = [n[0] / len, n[1] / len, n[2] / len];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(x: f32, y: f32, z: f32) -> Vertex {
        Vertex {
            position: [x, y, z],
            normal: [0.0, 1.0, 0.0],
            uv: [0.25, 0.75],
        }
    }

    fn base() -> Vec<Vertex> {
        vec![vertex(1.0, 0.0, 0.0), vertex(-2.0, 3.0, -0.0)]
    }

    /// 目标 0：位置 (+1,0,0) / 法线 (0,1,1)；目标 1：位置 (0,-2,0)，无增量。
    fn two_targets() -> MorphData {
        MorphData::new(vec![
            MorphTarget::new(
                Some("smile".into()),
                vec![[1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
                Some(vec![[0.0, 1.0, 1.0], [0.0, 1.0, 1.0]]),
            ),
            MorphTarget::new(
                Some("blink".into()),
                vec![[0.0, -2.0, 0.0], [0.0, -2.0, 0.0]],
                None,
            ),
        ])
    }

    fn bits(v: [f32; 3]) -> [u32; 3] {
        [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()]
    }

    /// 全零权重 ⇒ 逐位透传（含 `-0.0` 符号位——跳过口径的锚）。
    #[test]
    fn zero_weights_pass_base_through_bitwise() {
        let blended = blend_morph_vertices(&base(), &two_targets(), &[0.0, 0.0]);
        for (a, b) in base().iter().zip(&blended) {
            assert_eq!(bits(a.position), bits(b.position), "位置逐位");
            assert_eq!(bits(a.normal), bits(b.normal), "法线逐位");
            assert_eq!(a.uv, b.uv);
        }
        // 第二顶点位置的 -0.0 必须保持符号（未跳过口径会归并成 +0.0）
        assert_eq!(
            blended[1].position[2].to_bits(),
            (-0.0f32).to_bits(),
            "w==0 跳过的 -0.0 保号锚"
        );
    }

    /// 单目标解析值：位置线性叠加；法线相加后重归一化
    /// （(0,1,0)+(0,1,1) = (0,2,1)，除以 √5）。
    #[test]
    fn single_target_analytic_blend() {
        let blended = blend_morph_vertices(&base(), &two_targets(), &[1.0, 0.0]);
        assert_eq!(blended[0].position, [2.0, 0.0, 0.0]);
        assert_eq!(blended[1].position, [-1.0, 3.0, -0.0]);
        let len = 5.0f32.sqrt();
        let expected = [0.0f32, 2.0 / len, 1.0 / len];
        assert_eq!(bits(blended[0].normal), bits(expected), "法线归一化");
    }

    /// 双目标累加次序 = 表序（w=0.5 / w=-1 解析对照）。
    #[test]
    fn two_targets_accumulate_in_table_order() {
        let blended = blend_morph_vertices(&base(), &two_targets(), &[0.5, -1.0]);
        // 顶点 0：(1,0,0) + 0.5·(1,0,0) + (-1)·(0,-2,0) = (1.5, 2, 0)
        assert_eq!(blended[0].position, [1.5, 2.0, 0.0]);
    }

    /// 目标无法线增量 ⇒ 基座法线逐位透传（即便权重非零）。
    #[test]
    fn position_only_target_keeps_normals_bitwise() {
        let morph = MorphData::new(vec![MorphTarget::new(
            None,
            vec![[1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            None,
        )]);
        let blended = blend_morph_vertices(&base(), &morph, &[2.0]);
        assert_eq!(blended[0].position, [3.0, 0.0, 0.0]);
        assert_eq!(bits(blended[0].normal), bits([0.0, 1.0, 0.0]));
    }

    /// 配对截断：权重多退、目标多余取 0（min 配对口径）。
    #[test]
    fn weight_pairing_truncates_to_shorter_side() {
        let with_extra_weights = blend_morph_vertices(&base(), &two_targets(), &[1.0, 0.0, 7.0]);
        let exact = blend_morph_vertices(&base(), &two_targets(), &[1.0, 0.0]);
        for (a, b) in exact.iter().zip(&with_extra_weights) {
            assert_eq!(bits(a.position), bits(b.position));
        }
        let short_weights = blend_morph_vertices(&base(), &two_targets(), &[0.0]);
        assert_eq!(bits(short_weights[0].position), bits([1.0, 0.0, 0.0]));
    }

    /// 确定性：同输入两次跑逐位一致。
    #[test]
    fn blend_is_deterministic_bitwise() {
        let morph = two_targets();
        let w = [0.3, 0.7];
        let a = blend_morph_vertices(&base(), &morph, &w);
        let b = blend_morph_vertices(&base(), &morph, &w);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(bits(x.position), bits(y.position));
            assert_eq!(bits(x.normal), bits(y.normal));
        }
    }

    /// 退化零法线：混合后长度恰为 0 ⇒ 归一化除零守卫保留零值
    /// （同表中非退化顶点照常归一化，守卫是逐顶点判定）。
    #[test]
    fn degenerate_zero_normal_survives_normalize_guard() {
        let morph = MorphData::new(vec![MorphTarget::new(
            None,
            vec![[1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            Some(vec![[0.0, -1.0, 0.0], [0.0, 1.0, 0.0]]),
        )]);
        let mut flat = base();
        flat[0].normal = [0.0, 1.0, 0.0]; // + (0,-1,0) ⇒ 零法线
        let blended = blend_morph_vertices(&flat, &morph, &[1.0]);
        assert_eq!(bits(blended[0].normal), bits([0.0, 0.0, 0.0]), "零法线守卫");
        assert_eq!(
            bits(blended[1].normal),
            bits([0.0, 1.0, 0.0]),
            "非退化顶点 (0,1,0)+(0,1,0) 归一化回 (0,1,0)"
        );
        assert_eq!(blended[1].position, [-1.0, 3.0, -0.0]);
    }

    /// 构造校验：表内顶点数不齐 ⇒ panic（无条件 assert，release 同防）。
    #[test]
    #[should_panic(expected = "各形变目标的顶点数必须一致")]
    fn mismatched_target_lengths_panic() {
        MorphData::new(vec![
            MorphTarget::new(None, vec![[0.0; 3]; 2], None),
            MorphTarget::new(None, vec![[0.0; 3]; 3], None),
        ]);
    }
}
