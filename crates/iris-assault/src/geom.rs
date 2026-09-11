//! 程序化几何工具（纯函数 · 确定性 · 零 unsafe）——角色与场景建模的地基。
//!
//! 本模块只产 [`MeshData`]，不碰 World / GPU：所有生成器都是「参数 →
//! 顶点数组」的纯函数，同输入逐位同输出（单测用 `to_bits` 锁定），随机性
//! 一律走无状态哈希（见 [`crate::textures`] 的 splitmix64），与全仓确定性
//! 文化一致。
//!
//! # 构件
//! - [`superellipsoid`]：超椭球（指数 2 = 椭球、4~8 = 圆角盒）——护甲板 /
//!   头盔 / 弹匣 / 道具的通用体块；
//! - [`loft`]：任意截面序列放样（截面 = 超椭圆，可逐截面变半径 / 变指数）
//!   ——躯干 / 水盘 / 渐变枪管；
//! - [`tube`]：直线轴放样（起止点 + 逐段半径剖面）——最常用的肢体 / 柱身；
//! - [`lathe`]：轮廓线绕 Y 轴旋转——柱础 / 柱头 / 圆盘 / 穹顶。
//!
//! # 装配（[`Piece`] → [`Assembly`]）
//! 部件在**局部空间**生成 → [`Piece::uv_box`] / [`Piece::uv_cylinder`] 把
//! UV 投影进图集 tile（先 UV 后摆放：UV 与变换解耦）→ [`Piece::transform`]
//! 摆到模型空间 → [`Piece::rigid`] / [`Piece::blend_chain`] 按模型空间位置
//! 分配蒙皮权重 → [`Assembly::add`] 合并、[`Assembly::finish`] 补切线并
//! 合成单张 [`MeshData`]（每部件一个材质 → 单网格单材质 = 单次 draw）。
//!
//! # 诚实限制
//! - 法线是**面积加权顶点法线**（平滑）：硬边靠几何倒角而非拆点，故 90°
//!   硬转角有轻微平滑（圆角盒指数 ≥ 6 时肉眼不可见）；两端封盖与侧面共享
//!   环顶点 ⇒ 端面法线会向侧面过渡（封盖通常埋在别的部件里）；
//! - UV 盒投按主导轴投影，面与面之间有接缝；柱面投在 ±180° 有一条接缝
//!   （故意留在转体背面）；
//! - 切线由 UV 差分累积：UV 退化的三角形不贡献，全退化顶点的切线回退为
//!   法线的任一正交向量（着色器另有屏幕空间 TBN 兜底）；
//! - 蒙皮权重最多 4 骨（与引擎 SkinVertex 一致）；链混合是「相邻两骨
//!   平滑混合」，不做球形衰减 / 双四元数 / 体积保持。

use glam::{Mat4, Vec2, Vec3};
use iris_engine::prelude::{MeshData, SkinVertex, Vertex};

/// 主轴（UV 投影与柱面映射的基准）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// X 轴。
    X,
    /// Y 轴（模型竖直方向：躯干 / 四肢 / 柱身）。
    Y,
    /// Z 轴（模型前后方向：枪管 / 刀身）。
    Z,
}

/// 图集分区（归一化 UV 矩形，`v` 与纹理行序一致：行 0 = v 0）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    /// 左下角 u。
    pub u0: f32,
    /// 左下角 v。
    pub v0: f32,
    /// u 向宽度。
    pub du: f32,
    /// v 向高度。
    pub dv: f32,
}

impl Tile {
    /// 直接给出四边。
    pub const fn new(u0: f32, v0: f32, du: f32, dv: f32) -> Self {
        Self { u0, v0, du, dv }
    }

    /// `cols × rows` 网格中的第 `(col, row)` 格（`row` 0 = v 最小侧）。
    pub fn grid(cols: u32, rows: u32, col: u32, row: u32) -> Self {
        assert!(cols > 0 && rows > 0, "图集网格至少 1×1");
        assert!(
            col < cols && row < rows,
            "图集格 ({col},{row}) 越界 {cols}×{rows}"
        );
        let du = 1.0 / cols as f32;
        let dv = 1.0 / rows as f32;
        Self {
            u0: col as f32 * du,
            v0: row as f32 * dv,
            du,
            dv,
        }
    }

    /// 内缩边距（双线性采样在 tile 边界会取到邻格纹素，留 2~4 纹素边距）。
    pub fn inset(self, margin: f32) -> Self {
        assert!(margin >= 0.0, "内缩边距不能为负");
        assert!(
            margin * 2.0 < self.du && margin * 2.0 < self.dv,
            "内缩边距吃掉整个 tile"
        );
        Self {
            u0: self.u0 + margin,
            v0: self.v0 + margin,
            du: self.du - margin * 2.0,
            dv: self.dv - margin * 2.0,
        }
    }

    /// 把 `[0,1]²` 的局部 UV 映射进本 tile。
    pub fn map(self, uv: Vec2) -> Vec2 {
        Vec2::new(self.u0 + uv.x * self.du, self.v0 + uv.y * self.dv)
    }
}

/// 单顶点蒙皮权重（最多 4 骨，与引擎 SkinVertex 同构）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weights {
    /// 关节下标（指向蒙皮 joints 表）。
    pub joints: [u32; 4],
    /// 混合权重（约定和为 1；全零表示未分配）。
    pub weights: [f32; 4],
}

impl Weights {
    /// 全零（未分配；[`Assembly::add`] 对未分配顶点会 panic）。
    pub const UNASSIGNED: Self = Self {
        joints: [0; 4],
        weights: [0.0; 4],
    };

    /// 刚性绑定到单骨。
    pub const fn rigid(bone: u32) -> Self {
        Self {
            joints: [bone, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        }
    }

    /// 两骨线性混合（`t` 会钳制到 [0,1]，并剔除零权重分量）。
    pub fn blend(a: u32, b: u32, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return Self::rigid(a);
        }
        if t >= 1.0 {
            return Self::rigid(b);
        }
        Self {
            joints: [a, b, 0, 0],
            weights: [1.0 - t, t, 0.0, 0.0],
        }
    }

    /// 权重和（检验用）。
    pub fn sum(&self) -> f32 {
        self.weights.iter().sum()
    }

    /// 归一化（权重和 ≤ 0 时退化为刚性首骨）。
    fn normalized(mut self) -> Self {
        let sum = self.sum();
        if sum <= 1e-6 {
            return Self::rigid(self.joints[0]);
        }
        for w in &mut self.weights {
            *w /= sum;
        }
        self
    }
}

/// 蒙皮链上一节：关节下标 + 该骨在**模型空间**的头尾位置。
#[derive(Clone, Copy, Debug)]
pub struct ChainBone {
    /// 关节下标（蒙皮 joints 表下标）。
    pub bone: u32,
    /// 骨头（靠根侧）。
    pub head: Vec3,
    /// 骨尾（靠叶侧）。
    pub tail: Vec3,
}

impl ChainBone {
    /// 构造。
    pub const fn new(bone: u32, head: Vec3, tail: Vec3) -> Self {
        Self { bone, head, tail }
    }
}

/// 一个部件：局部空间几何 + （变换到模型空间后分配的）蒙皮权重。
#[derive(Clone, Debug)]
pub struct Piece {
    /// 几何（模型空间；UV 已投影进图集 tile）。
    pub mesh: MeshData,
    /// 每顶点权重（与 `mesh.vertices` 等长；空 = 未分配）。
    pub weights: Vec<Weights>,
}

impl Piece {
    /// 包一个裸网格（UV / 法线由生成器或调用方负责）。
    pub fn new(mesh: MeshData) -> Self {
        Self {
            mesh,
            weights: Vec::new(),
        }
    }

    /// UV 盒投：按顶点法线主导轴投影另两轴，乘 `scale`（纹理平铺率）
    /// 后映射进 `tile`。
    pub fn uv_box(mut self, tile: Tile, scale: f32, offset: Vec2) -> Self {
        assert!(scale > 0.0, "UV 平铺率必须为正");
        for vertex in &mut self.mesh.vertices {
            let n = Vec3::from(vertex.normal);
            let p = Vec3::from(vertex.position);
            let (a, b) = dominant_axes(n);
            let uv = Vec2::new(p[a], p[b]) * scale + offset;
            vertex.uv = tile
                .map(Vec2::new(uv.x.rem_euclid(1.0), uv.y.rem_euclid(1.0)))
                .into();
        }
        self
    }

    /// UV 柱面投：绕 `axis` 的角度作 u、轴向坐标作 v（肢体 / 柱身）。
    pub fn uv_cylinder(
        mut self,
        tile: Tile,
        axis: Axis,
        u_repeat: f32,
        v_scale: f32,
        offset: Vec2,
    ) -> Self {
        assert!(u_repeat > 0.0 && v_scale > 0.0, "柱面投影参数必须为正");
        for vertex in &mut self.mesh.vertices {
            let p = Vec3::from(vertex.position);
            let (u, v) = match axis {
                Axis::Y => (p.z.atan2(p.x) / std::f32::consts::TAU + 0.5, p.y),
                Axis::X => (p.z.atan2(p.y) / std::f32::consts::TAU + 0.5, p.x),
                Axis::Z => (p.y.atan2(p.x) / std::f32::consts::TAU + 0.5, p.z),
            };
            let local = Vec2::new(
                (u * u_repeat + offset.x).rem_euclid(1.0),
                (v * v_scale + offset.y).rem_euclid(1.0),
            );
            vertex.uv = tile.map(local).into();
        }
        self
    }

    /// 变换到模型空间（法线走逆转置并归一化；非等比缩放因此正确）。
    pub fn transform(mut self, m: Mat4) -> Self {
        let normal_matrix = m.inverse().transpose();
        for vertex in &mut self.mesh.vertices {
            let p = m.transform_point3(Vec3::from(vertex.position));
            let n = normal_matrix
                .transform_vector3(Vec3::from(vertex.normal))
                .normalize_or_zero();
            vertex.position = p.into();
            vertex.normal = n.into();
        }
        self
    }

    /// 全部顶点刚性绑定到单骨。
    pub fn rigid(mut self, bone: u32) -> Self {
        self.weights = vec![Weights::rigid(bone); self.mesh.vertices.len()];
        self
    }

    /// 沿骨链分配权重：顶点到折线的最短投影确定弧长坐标，关节两侧各
    /// `band`（米）内与相邻骨平滑混合（smoothstep），其余刚性。
    ///
    /// 链按根→叶给出；`band ≤ 0` 时退化为最近骨刚性绑定（硬关节）。
    pub fn blend_chain(mut self, chain: &[ChainBone], band: f32) -> Self {
        assert!(!chain.is_empty(), "链至少一节");
        let segments: Vec<(Vec3, Vec3, f32)> = chain
            .iter()
            .map(|b| {
                let d = b.tail - b.head;
                let len = d.length();
                assert!(len > 1e-6, "骨长必须为正（bone {}）", b.bone);
                (b.head, d / len, len)
            })
            .collect();
        // 累积弧长边界：bounds[k] = 第 k 节头部的弧长坐标
        let mut bounds = Vec::with_capacity(chain.len() + 1);
        let mut acc = 0.0f32;
        for (_, _, len) in &segments {
            bounds.push(acc);
            acc += len;
        }
        bounds.push(acc);
        self.weights = self
            .mesh
            .vertices
            .iter()
            .map(|v| {
                let p = Vec3::from(v.position);
                let mut best = (f32::MAX, 0.0f32);
                for (index, (head, dir, len)) in segments.iter().enumerate() {
                    let t = (p - *head).dot(*dir).clamp(0.0, *len);
                    let closest = *head + *dir * t;
                    let dist = p.distance_squared(closest);
                    if dist < best.0 {
                        best = (dist, bounds[index] + t);
                    }
                }
                let arc = best.1;
                if band <= 0.0 {
                    let mut bone = chain[0].bone;
                    for (index, _) in segments.iter().enumerate() {
                        if arc >= bounds[index] {
                            bone = chain[index].bone;
                        }
                    }
                    return Weights::rigid(bone);
                }
                let mut acc_joints = [0u32; 4];
                let mut acc_weights = [0.0f32; 4];
                let mut count = 0usize;
                for (index, bone) in chain.iter().enumerate() {
                    let start = bounds[index];
                    let end = bounds[index + 1];
                    let rise = smoothstep((arc - (start - band)) / (2.0 * band));
                    let fall = 1.0 - smoothstep((arc - (end - band)) / (2.0 * band));
                    let w = rise * fall;
                    if w > 1e-4 && count < 4 {
                        acc_joints[count] = bone.bone;
                        acc_weights[count] = w;
                        count += 1;
                    }
                }
                if count == 0 {
                    return Weights::rigid(chain[0].bone);
                }
                Weights {
                    joints: acc_joints,
                    weights: acc_weights,
                }
                .normalized()
            })
            .collect();
        self
    }

    /// 顶点数（装配诊断用）。
    pub fn vertex_count(&self) -> usize {
        self.mesh.vertices.len()
    }
}

/// 多部件合并器：所有部件共享一个材质 → 最终合成单张 [`MeshData`]。
#[derive(Default, Clone, Debug)]
pub struct Assembly {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    skin: Vec<SkinVertex>,
    any_weights: bool,
    any_rigid: bool,
}

impl Assembly {
    /// 空装配。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个**刚性**部件（不写蒙皮属性；权重被忽略）——用于挂在骨节点
    /// 上、由节点变换驱动的网格（如武器挂件）。一张网格要么全蒙皮要么全
    /// 刚性：两者混用会让「哪些顶点跟骨走」失去单一来源，故直接 panic。
    pub fn add_rigid(&mut self, piece: Piece) -> &mut Self {
        assert!(
            !self.any_weights,
            "刚性部件与蒙皮部件不能混进同一张网格（网格要么全蒙皮要么全刚性）"
        );
        let base = self.vertices.len() as u32;
        self.vertices.extend(piece.mesh.vertices.iter().copied());
        self.indices
            .extend(piece.mesh.indices.iter().map(|i| i + base));
        self.any_rigid = true;
        self
    }

    /// 追加一个蒙皮部件（UV / 权重须已分配；未分配权重 → panic，早爆胜过静默）。
    pub fn add(&mut self, piece: Piece) -> &mut Self {
        assert!(
            !self.any_rigid,
            "蒙皮部件与刚性部件不能混进同一张网格（网格要么全蒙皮要么全刚性）"
        );
        assert_eq!(
            piece.mesh.vertices.len(),
            piece.weights.len(),
            "部件权重数 {} 与顶点数 {} 不一致（漏调 rigid / blend_chain？）",
            piece.weights.len(),
            piece.mesh.vertices.len()
        );
        assert!(
            piece.mesh.skin.is_none(),
            "部件网格不应自带 skin（权重由 Piece 承载，装配期统一写入）"
        );
        let base = self.vertices.len() as u32;
        for (vertex, weights) in piece.mesh.vertices.iter().zip(&piece.weights) {
            assert!(
                weights.sum() > 0.5,
                "顶点权重和 {:.3} 非法（未分配的 Weights::UNASSIGNED？）",
                weights.sum()
            );
            self.vertices.push(*vertex);
            self.skin.push(SkinVertex {
                joints: weights.joints,
                weights: weights.weights,
            });
        }
        self.indices
            .extend(piece.mesh.indices.iter().map(|i| i + base));
        self.any_weights = true;
        self
    }

    /// 三角面数（预算诊断 / 测试断言）。
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// 顶点数。
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// 合成最终网格：重算平滑法线 + 顶点切线。
    pub fn finish(self) -> MeshData {
        let mut mesh = MeshData {
            vertices: self.vertices,
            indices: self.indices,
            skin: if self.any_weights {
                Some(self.skin)
            } else {
                None
            },
            tangents: None,
            uv2: None,
        };
        smooth_normals(&mut mesh);
        compute_tangents(&mut mesh);
        mesh
    }
}

/// 超椭球（Lamé 曲面）：`half` 半轴、`n` 指数（2 = 椭球、4~8 = 圆角盒，
/// 越大越接近长方体）、经纬分段。极点环重合顶点不产生退化三角形（跳过退化边）。
pub fn superellipsoid(half: Vec3, n: f32, seg_u: u32, seg_v: u32) -> MeshData {
    assert!(half.x > 0.0 && half.y > 0.0 && half.z > 0.0, "半轴必须为正");
    assert!(n >= 1.0, "超椭球指数至少 1（1 = 菱形截面）");
    assert!(seg_u >= 3 && seg_v >= 2, "分段数太小");
    let power = 2.0 / n;
    let sign_pow = |v: f32| v.signum() * v.abs().powf(power);
    let mut mesh = MeshData::default();
    for ring in 0..=seg_v {
        let phi = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * ring as f32 / seg_v as f32;
        let (sin_phi, cos_phi) = phi.sin_cos();
        let y = half.y * sign_pow(sin_phi);
        let r = cos_phi.abs().powf(power);
        for seg in 0..seg_u {
            let theta = std::f32::consts::TAU * seg as f32 / seg_u as f32;
            let (sin_t, cos_t) = theta.sin_cos();
            mesh.vertices.push(Vertex {
                position: [
                    half.x * r * sign_pow(cos_t),
                    y,
                    half.z * r * sign_pow(sin_t),
                ],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 0.0],
            });
        }
    }
    let row = seg_u;
    for ring in 0..seg_v {
        for seg in 0..seg_u {
            let next = (seg + 1) % seg_u;
            let a = ring * row + seg;
            let b = ring * row + next;
            let c = (ring + 1) * row + next;
            let d = (ring + 1) * row + seg;
            // 极点环整圈重合 ⇒ 跳过退化三角形
            let top_degenerate = ring == 0 && a == b;
            let bottom_degenerate = ring == seg_v - 1 && c == d;
            if top_degenerate {
                mesh.indices.extend_from_slice(&[a, c, d]);
            } else if bottom_degenerate {
                mesh.indices.extend_from_slice(&[a, b, c]);
            } else {
                mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
    }
    smooth_normals(&mut mesh);
    mesh
}

/// 放样截面：中心 + 两正交基 + 两个半轴 + 超椭圆指数。
#[derive(Clone, Copy, Debug)]
pub struct Section {
    /// 截面中心。
    pub center: Vec3,
    /// 第一基（半轴 `half_right` 方向）。
    pub right: Vec3,
    /// 第二基（半轴 `half_up` 方向）。
    pub up: Vec3,
    /// `right` 方向半轴。
    pub half_right: f32,
    /// `up` 方向半轴。
    pub half_up: f32,
    /// 超椭圆指数（2 = 椭圆、1 = 菱形、4~8 = 圆角矩形）。
    pub exponent: f32,
}

impl Section {
    /// 构造（`right` / `up` 会被归一化）。
    pub fn new(
        center: Vec3,
        right: Vec3,
        up: Vec3,
        half_right: f32,
        half_up: f32,
        exponent: f32,
    ) -> Self {
        Self {
            center,
            right: right.normalize(),
            up: up.normalize(),
            half_right,
            half_up,
            exponent,
        }
    }

    /// 环上一点（超椭圆参数化）。
    fn point(&self, theta: f32) -> Vec3 {
        let power = 2.0 / self.exponent;
        let (sin_t, cos_t) = theta.sin_cos();
        let x = self.half_right * cos_t.signum() * cos_t.abs().powf(power);
        let y = self.half_up * sin_t.signum() * sin_t.abs().powf(power);
        self.center + self.right * x + self.up * y
    }
}

/// 沿截面序列放样成闭合管；`caps` = 两端封盖（截面中心 + 环的三角扇）。
pub fn loft(sections: &[Section], segments: u32, caps: bool) -> MeshData {
    assert!(sections.len() >= 2, "放样至少两个截面");
    assert!(segments >= 3, "分段数太小");
    let mut mesh = MeshData::default();
    for section in sections {
        for seg in 0..segments {
            let theta = std::f32::consts::TAU * seg as f32 / segments as f32;
            mesh.vertices.push(Vertex {
                position: section.point(theta).into(),
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 0.0],
            });
        }
    }
    let row = segments as usize;
    for ring in 0..sections.len() - 1 {
        for seg in 0..row {
            let next = (seg + 1) % row;
            let a = (ring * row + seg) as u32;
            let b = (ring * row + next) as u32;
            let c = ((ring + 1) * row + next) as u32;
            let d = ((ring + 1) * row + seg) as u32;
            mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    if caps {
        let last = sections.len() - 1;
        for (center_pos, base, flip) in [
            (sections[0].center, 0usize, true),
            (sections[last].center, last * row, false),
        ] {
            let center = mesh.vertices.len() as u32;
            mesh.vertices.push(Vertex {
                position: center_pos.into(),
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 0.0],
            });
            for seg in 0..row {
                let next = (seg + 1) % row;
                let (i0, i1) = (base as u32 + seg as u32, base as u32 + next as u32);
                if flip {
                    mesh.indices.extend_from_slice(&[center, i1, i0]);
                } else {
                    mesh.indices.extend_from_slice(&[center, i0, i1]);
                }
            }
        }
    }
    smooth_normals(&mut mesh);
    mesh
}

/// 直线轴放样（肢体 / 柱身 / 枪管的常用形态）：轴 `a → b`，剖面 =
/// `(t, half_right, half_up, exponent)`（`t ∈ [0,1]` 沿轴参数，须单调）。
/// `up_hint` 决定截面基朝向（与轴平行时退化为任意正交基）。
pub fn tube(
    a: Vec3,
    b: Vec3,
    profile: &[(f32, f32, f32, f32)],
    segments: u32,
    up_hint: Vec3,
) -> MeshData {
    assert!(profile.len() >= 2, "剖面至少两点");
    let axis = (b - a).normalize();
    let hint = if up_hint.cross(axis).length_squared() < 1e-8 {
        Vec3::X
    } else {
        up_hint
    };
    let right = hint.cross(axis).normalize();
    let up = axis.cross(right).normalize();
    let sections: Vec<Section> = profile
        .iter()
        .map(|&(t, hr, hu, e)| Section::new(a + (b - a) * t, right, up, hr, hu, e))
        .collect();
    loft(&sections, segments, true)
}

/// 旋转体：轮廓 `(半径, y)` 序列绕 Y 轴旋转（半径 0 的端点收成极点）。
pub fn lathe(profile: &[(f32, f32)], segments: u32) -> MeshData {
    assert!(profile.len() >= 2, "轮廓至少两点");
    assert!(segments >= 3, "分段数太小");
    let mut mesh = MeshData::default();
    let mut ring_base = Vec::with_capacity(profile.len());
    for &(r, y) in profile {
        assert!(r >= 0.0, "轮廓半径不能为负");
        ring_base.push(mesh.vertices.len() as u32);
        if r <= 1e-6 {
            mesh.vertices.push(Vertex {
                position: [0.0, y, 0.0],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 0.0],
            });
        } else {
            for seg in 0..segments {
                let theta = std::f32::consts::TAU * seg as f32 / segments as f32;
                let (sin_t, cos_t) = theta.sin_cos();
                mesh.vertices.push(Vertex {
                    position: [r * cos_t, y, r * sin_t],
                    normal: [0.0, 1.0, 0.0],
                    uv: [0.0, 0.0],
                });
            }
        }
    }
    for index in 0..profile.len() - 1 {
        let r0 = profile[index].0;
        let r1 = profile[index + 1].0;
        let base0 = ring_base[index];
        let base1 = ring_base[index + 1];
        if r0 <= 1e-6 {
            for seg in 0..segments {
                let next = (seg + 1) % segments;
                mesh.indices
                    .extend_from_slice(&[base0, base1 + next, base1 + seg]);
            }
        } else if r1 <= 1e-6 {
            for seg in 0..segments {
                let next = (seg + 1) % segments;
                mesh.indices
                    .extend_from_slice(&[base1, base0 + seg, base0 + next]);
            }
        } else {
            for seg in 0..segments {
                let next = (seg + 1) % segments;
                let a = base0 + seg;
                let b = base0 + next;
                let c = base1 + next;
                let d = base1 + seg;
                mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
    }
    smooth_normals(&mut mesh);
    mesh
}

/// 面积加权顶点法线（零面积三角形不贡献；全零顶点回退 +Y）。
pub fn smooth_normals(mesh: &mut MeshData) {
    let mut accum = vec![Vec3::ZERO; mesh.vertices.len()];
    for triangle in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        );
        let p0 = Vec3::from(mesh.vertices[i0].position);
        let p1 = Vec3::from(mesh.vertices[i1].position);
        let p2 = Vec3::from(mesh.vertices[i2].position);
        let face = (p1 - p0).cross(p2 - p0);
        if !face.is_finite() || face.length_squared() <= 1e-12 {
            continue;
        }
        accum[i0] += face;
        accum[i1] += face;
        accum[i2] += face;
    }
    for (vertex, normal) in mesh.vertices.iter_mut().zip(accum) {
        let n = if normal.length_squared() > 1e-12 {
            normal.normalize()
        } else {
            Vec3::Y
        };
        vertex.normal = n.into();
    }
}

/// 由 UV 差分累积顶点切线（w = 手性符号，副切线 = cross(N, T)·w）。
///
/// UV 面积退化的三角形跳过；全退化顶点的切线回退为法线的任一正交向量。
pub fn compute_tangents(mesh: &mut MeshData) {
    let mut tangent = vec![Vec3::ZERO; mesh.vertices.len()];
    let mut bitangent = vec![Vec3::ZERO; mesh.vertices.len()];
    for triangle in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        );
        let p0 = Vec3::from(mesh.vertices[i0].position);
        let p1 = Vec3::from(mesh.vertices[i1].position);
        let p2 = Vec3::from(mesh.vertices[i2].position);
        let uv0 = Vec2::from(mesh.vertices[i0].uv);
        let uv1 = Vec2::from(mesh.vertices[i1].uv);
        let uv2 = Vec2::from(mesh.vertices[i2].uv);
        let e1 = p1 - p0;
        let e2 = p2 - p0;
        let d1 = uv1 - uv0;
        let d2 = uv2 - uv0;
        let det = d1.x * d2.y - d2.x * d1.y;
        if det.abs() < 1e-12 {
            continue;
        }
        let r = 1.0 / det;
        let t = (e1 * d2.y - e2 * d1.y) * r;
        let b = (e2 * d1.x - e1 * d2.x) * r;
        for index in [i0, i1, i2] {
            tangent[index] += t;
            bitangent[index] += b;
        }
    }
    let mut out = Vec::with_capacity(mesh.vertices.len());
    for (vertex, (t, b)) in mesh.vertices.iter().zip(tangent.into_iter().zip(bitangent)) {
        let n = Vec3::from(vertex.normal);
        let mut t = t - n * n.dot(t);
        if t.length_squared() <= 1e-12 {
            let fallback = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
            t = (fallback - n * n.dot(fallback)).normalize_or_zero();
            if t.length_squared() <= 1e-12 {
                t = Vec3::Z;
            }
        }
        let t = t.normalize();
        let w = if n.cross(t).dot(b) < 0.0 { -1.0 } else { 1.0 };
        out.push([t.x, t.y, t.z, w]);
    }
    mesh.tangents = Some(out);
}

/// 法线主导轴 → 另两轴下标（盒投用）。
fn dominant_axes(n: Vec3) -> (usize, usize) {
    let a = n.abs();
    if a.x >= a.y && a.x >= a.z {
        (2, 1)
    } else if a.y >= a.z {
        (0, 2)
    } else {
        (0, 1)
    }
}

/// `smoothstep`（`t²(3−2t)`，两端钳制）。
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(mesh: &MeshData) -> Vec<u32> {
        let mut out = Vec::new();
        for v in &mesh.vertices {
            out.extend(v.position.iter().map(|f| f.to_bits()));
            out.extend(v.normal.iter().map(|f| f.to_bits()));
            out.extend(v.uv.iter().map(|f| f.to_bits()));
        }
        out.extend(mesh.indices.iter().copied());
        out
    }

    #[test]
    fn generators_are_deterministic_bitwise() {
        let a = superellipsoid(Vec3::new(0.2, 0.3, 0.4), 6.0, 16, 10);
        let b = superellipsoid(Vec3::new(0.2, 0.3, 0.4), 6.0, 16, 10);
        assert_eq!(bits(&a), bits(&b), "超椭球两次生成应逐位一致");
        let p: Vec<(f32, f32, f32, f32)> = vec![(0.0, 0.1, 0.1, 3.0), (1.0, 0.06, 0.06, 3.0)];
        let c = tube(Vec3::ZERO, Vec3::Y, &p, 12, Vec3::X);
        let d = tube(Vec3::ZERO, Vec3::Y, &p, 12, Vec3::X);
        assert_eq!(bits(&c), bits(&d), "放样两次生成应逐位一致");
        let e = lathe(&[(0.0, 0.0), (0.5, 0.2), (0.5, 0.4), (0.0, 0.6)], 16);
        let f = lathe(&[(0.0, 0.0), (0.5, 0.2), (0.5, 0.4), (0.0, 0.6)], 16);
        assert_eq!(bits(&e), bits(&f), "旋转体两次生成应逐位一致");
    }

    #[test]
    fn superellipsoid_vertices_stay_within_half_extents() {
        let half = Vec3::new(0.3, 0.5, 0.2);
        let mesh = superellipsoid(half, 5.0, 24, 16);
        for v in &mesh.vertices {
            let p = Vec3::from(v.position);
            assert!(
                p.x.abs() <= half.x + 1e-5
                    && p.y.abs() <= half.y + 1e-5
                    && p.z.abs() <= half.z + 1e-5
            );
            assert!(
                (Vec3::from(v.normal).length() - 1.0).abs() < 1e-4,
                "法线应为单位向量"
            );
        }
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len()),
            "索引越界"
        );
    }

    #[test]
    fn superellipsoid_has_no_degenerate_triangles() {
        let mesh = superellipsoid(Vec3::new(0.2, 0.4, 0.3), 4.0, 20, 12);
        for triangle in mesh.indices.chunks_exact(3) {
            let p0 = Vec3::from(mesh.vertices[triangle[0] as usize].position);
            let p1 = Vec3::from(mesh.vertices[triangle[1] as usize].position);
            let p2 = Vec3::from(mesh.vertices[triangle[2] as usize].position);
            let area = (p1 - p0).cross(p2 - p0).length() * 0.5;
            assert!(area > 1e-9, "退化三角形（面积 {area:e}）");
        }
    }

    #[test]
    fn tube_radius_follows_profile() {
        let profile: Vec<(f32, f32, f32, f32)> = vec![(0.0, 0.2, 0.2, 2.0), (1.0, 0.05, 0.05, 2.0)];
        let mesh = tube(Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0), &profile, 24, Vec3::X);
        for v in &mesh.vertices {
            let p = Vec3::from(v.position);
            let radius = (p.x * p.x + p.z * p.z).sqrt();
            assert!(radius <= 0.2 + 1e-5, "半径不应超过剖面最大值");
        }
        let max = mesh
            .vertices
            .iter()
            .map(|v| v.position[0].abs())
            .fold(0.0f32, f32::max);
        assert!((max - 0.2).abs() < 1e-5, "起始截面应达到剖面半径");
    }

    #[test]
    fn lathe_poles_are_single_vertices() {
        let mesh = lathe(&[(0.0, 0.0), (0.5, 0.1), (0.5, 0.3), (0.0, 0.4)], 12);
        assert_eq!(mesh.vertices.len(), 2 + 24, "两极点 + 两环 × 12");
        assert_eq!(mesh.indices.len(), 12 * 3 * 2 + 12 * 6, "两极扇 + 侧壁");
    }

    #[test]
    fn blend_chain_weights_are_normalized_and_local() {
        let chain = [
            ChainBone::new(1, Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)),
            ChainBone::new(2, Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 2.0, 0.0)),
            ChainBone::new(3, Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.0, 2.5, 0.0)),
        ];
        // 剖面须密（关节过渡带要有顶点落进去——2 截面放样只有两端环）
        let profile: Vec<(f32, f32, f32, f32)> =
            (0..=24).map(|i| (i as f32 / 24.0, 0.1, 0.1, 3.0)).collect();
        let mesh = tube(
            Vec3::new(0.0, 0.05, 0.0),
            Vec3::new(0.0, 2.45, 0.0),
            &profile,
            8,
            Vec3::X,
        );
        let piece = Piece::new(mesh).blend_chain(&chain, 0.15);
        for w in &piece.weights {
            assert!(
                (w.sum() - 1.0).abs() < 1e-5,
                "权重和应为 1，实际 {}",
                w.sum()
            );
            let active = w.weights.iter().filter(|x| **x > 1e-4).count();
            assert!(active <= 2, "平滑链混合最多两骨同时生效，实际 {active}");
        }
        let blended = piece
            .weights
            .iter()
            .filter(|w| w.weights.iter().filter(|x| **x > 1e-3).count() == 2)
            .count();
        assert!(blended > 0, "关节过渡带应存在两骨混合顶点");
    }

    #[test]
    fn uv_maps_stay_inside_tile() {
        let tile = Tile::grid(4, 4, 2, 1).inset(0.01);
        let mesh = superellipsoid(Vec3::new(0.2, 0.2, 0.2), 4.0, 12, 8);
        let piece = Piece::new(mesh).uv_box(tile, 1.7, Vec2::ZERO);
        for v in &piece.mesh.vertices {
            let uv = Vec2::from(v.uv);
            assert!(
                uv.x >= tile.u0 - 1e-6 && uv.x <= tile.u0 + tile.du + 1e-6,
                "u 越出 tile"
            );
            assert!(
                uv.y >= tile.v0 - 1e-6 && uv.y <= tile.v0 + tile.dv + 1e-6,
                "v 越出 tile"
            );
        }
        let mesh = superellipsoid(Vec3::new(0.2, 0.2, 0.2), 4.0, 12, 8);
        let piece = Piece::new(mesh).uv_cylinder(tile, Axis::Y, 1.0, 2.0, Vec2::ZERO);
        for v in &piece.mesh.vertices {
            let uv = Vec2::from(v.uv);
            assert!(uv.x >= tile.u0 - 1e-6 && uv.x <= tile.u0 + tile.du + 1e-6);
            assert!(uv.y >= tile.v0 - 1e-6 && uv.y <= tile.v0 + tile.dv + 1e-6);
        }
    }

    #[test]
    fn assembly_merges_and_produces_valid_skin_and_tangents() {
        let mut assembly = Assembly::new();
        assembly.add(Piece::new(superellipsoid(Vec3::splat(0.1), 4.0, 10, 6)).rigid(3));
        assembly.add(
            Piece::new(superellipsoid(Vec3::splat(0.1), 4.0, 10, 6))
                .uv_box(Tile::grid(2, 2, 1, 1), 2.0, Vec2::ZERO)
                .transform(Mat4::from_translation(Vec3::new(0.3, 0.0, 0.0)))
                .rigid(5),
        );
        let triangles = assembly.triangle_count();
        let mesh = assembly.finish();
        assert_eq!(mesh.indices.len() / 3, triangles);
        let skin = mesh.skin.as_ref().expect("应有蒙皮属性");
        assert_eq!(skin.len(), mesh.vertices.len());
        assert_eq!(skin[0].joints[0], 3);
        let tangents = mesh.tangents.as_ref().expect("应有切线");
        assert_eq!(tangents.len(), mesh.vertices.len());
        for (t, v) in tangents.iter().zip(&mesh.vertices) {
            let tvec = Vec3::new(t[0], t[1], t[2]);
            let n = Vec3::from(v.normal);
            assert!((tvec.length() - 1.0).abs() < 1e-4, "切线应为单位向量");
            assert!(
                tvec.dot(n).abs() < 1e-3,
                "切线与法线应正交，实际 {}",
                tvec.dot(n)
            );
        }
    }

    #[test]
    fn transform_keeps_normals_unit_under_nonuniform_scale() {
        let mesh = superellipsoid(Vec3::splat(0.2), 4.0, 12, 8);
        let piece = Piece::new(mesh).transform(Mat4::from_scale(Vec3::new(2.0, 0.5, 1.0)));
        for v in &piece.mesh.vertices {
            assert!((Vec3::from(v.normal).length() - 1.0).abs() < 1e-4);
        }
    }
}
