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

use glam::{Mat4, Quat, Vec2, Vec3};
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
        // 部件自检：闭合壳的有向体积必须为正（法线朝外的又一重保险——
        // 放样 / 旋转体 / 超椭球已在生成器里统一纠向，这里挡的是新原语与新调用）。
        debug_assert!(
            signed_volume(&piece.mesh) > 0.0,
            "部件法线朝内（有向体积 {}）；闭合壳须外向",
            signed_volume(&piece.mesh)
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
            morph: None,
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
    // 超椭球以原点为中心；极点环的退化边跳过判向后仍有侧面可用
    orient_outward(&mut mesh, |_, _| Vec3::ZERO);
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
        self.point_at(theta, 0.0)
    }

    /// 环上一点并向截面中心内缩 `inset`（弧形壳的内表面用：两半轴各减
    /// `inset`，非严格等距偏移但视觉等价，且不会有自交风险）。
    pub fn point_at(&self, theta: f32, inset: f32) -> Vec3 {
        let power = 2.0 / self.exponent;
        let (sin_t, cos_t) = theta.sin_cos();
        let half_right = (self.half_right - inset).max(1e-4);
        let half_up = (self.half_up - inset).max(1e-4);
        let x = half_right * cos_t.signum() * cos_t.abs().powf(power);
        let y = half_up * sin_t.signum() * sin_t.abs().powf(power);
        self.center + self.right * x + self.up * y
    }
}

/// 沿截面序列放样成闭合管；`caps` = 两端封盖（截面中心 + 环的三角扇）。
/// 闭合壳的环绕方向自检：首个有效面的法线若不背离参考中心，则**整体翻转**索引。
///
/// 放样 / 旋转体的环绕方向取决于「截面基 (right, up) 与行方向是否成右手系」
/// （right × up = 行方向），而这是**调用方**给的——写反的壳在光照下整片发灰、
/// 背面剔除后直接看不见。实测整身有 20 个部件踩了这个坑（鞋楦三件、双肩三角肌、
/// 双掌鱼际、头盔耳罩、脸侧滤罐、腰侧挂具、腕环……），且**左右两侧未必同病**
/// （镜像部件的行方向相反）。与其在几十个调用点各写一遍「截面顺序要反着排」，
/// 不如在这里统一兜住——与 arc_shell 同一策略，调用方不必操心。
///
/// 用三角形重心判向（极点扇面的首顶点在轴上，用顶点会退化成零向量而漏判）；
/// 整壳环绕一致，故翻转全部三角形即可。
fn orient_outward(mesh: &mut MeshData, center_of: impl Fn(usize, Vec3) -> Vec3) {
    let mut inverted = false;
    for triangle in mesh.indices.chunks_exact(3) {
        let i0 = triangle[0] as usize;
        let p0 = Vec3::from(mesh.vertices[i0].position);
        let p1 = Vec3::from(mesh.vertices[triangle[1] as usize].position);
        let p2 = Vec3::from(mesh.vertices[triangle[2] as usize].position);
        let face = (p1 - p0).cross(p2 - p0);
        if face.length_squared() <= 1e-18 {
            continue;
        }
        let centroid = (p0 + p1 + p2) / 3.0;
        let radial = centroid - center_of(i0, centroid);
        if radial.length_squared() <= 1e-12 {
            continue;
        }
        // 端面（法线与径向近乎垂直，如旋转体的封口圆盘）判不出朝向，跳过：
        // 拿它下结论会把整个壳翻反（侧面螺栓的圆盘端面正是如此）。
        let alignment = radial.dot(face) / (radial.length() * face.length());
        if alignment.abs() < 0.25 {
            continue;
        }
        inverted = alignment < 0.0;
        break;
    }
    if inverted {
        for triangle in mesh.indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
}

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
    // 封盖之后再定环绕方向：封盖的索引序是跟着**原始**环序写的，
    // 若在封盖前翻转，盖面会与侧面反向（漏盖内翻）。
    orient_outward(&mut mesh, |index, _| {
        sections[(index / row).min(sections.len() - 1)].center
    });
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
    // 旋转体的参考中心 = 同高度处的轴点（轮廓序反了照样兜住）
    orient_outward(&mut mesh, |_, point| Vec3::new(0.0, point.y, 0.0));
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

// ---------------------------------------------------------------------------
// 硬表面基本体（视觉上限轮）：倒角板 / 凸棱柱 / 弧形壳 / 折线扫掠
//
// 超椭球是**平滑法线**的圆角盒——做护甲板读作「软胶疙瘩」。硬表面装备需要
// 三条：①面与面之间**顶点分裂**（法线硬边，棱上有高光）；②45° 倒角（棱上一条
// 连续高光带，这是「金属板」而非「塑料块」的视觉签名）；③可包覆身体的**弧形壳**。
// 本节的四个生成器专供这三条，与既有生成器共用同一套 UV / 权重 / 装配管线。
// ---------------------------------------------------------------------------

/// 闭合网格的有符号体积（∑ (p₀ × p₁) · p₂ / 6，原点在体内时正 = 外向环绕）。
///
/// 用于自检环绕方向：硬表面基本体若整体反绕，法线会全部朝内——表现为
/// 「装备发黑、像被掏空」，且在暗场景里极难定位。
fn signed_volume(mesh: &MeshData) -> f32 {
    let mut sum = 0.0f32;
    for triangle in mesh.indices.chunks_exact(3) {
        let p0 = Vec3::from(mesh.vertices[triangle[0] as usize].position);
        let p1 = Vec3::from(mesh.vertices[triangle[1] as usize].position);
        let p2 = Vec3::from(mesh.vertices[triangle[2] as usize].position);
        sum += p0.cross(p1).dot(p2);
    }
    sum / 6.0
}

/// 推入一个**顶点分裂**的四边形（`hint` 指体外，环绕方向不合就翻）。
/// 分裂顶点 = 平滑法线无法跨面平均 ⇒ 硬边；这是硬表面与超椭球的根本区别。
fn push_hard_quad(mesh: &mut MeshData, corners: [Vec3; 4], hint: Vec3) {
    let normal = (corners[1] - corners[0]).cross(corners[3] - corners[0]);
    let order: [usize; 4] = if normal.dot(hint) < 0.0 {
        [0, 3, 2, 1]
    } else {
        [0, 1, 2, 3]
    };
    let base = mesh.vertices.len() as u32;
    for &index in &order {
        mesh.vertices.push(Vertex {
            position: corners[index].into(),
            normal: [0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 推入一个顶点分裂的三角形（同样按 `hint` 定环绕）。
fn push_hard_triangle(mesh: &mut MeshData, corners: [Vec3; 3], hint: Vec3) {
    let normal = (corners[1] - corners[0]).cross(corners[2] - corners[0]);
    let order: [usize; 3] = if normal.dot(hint) < 0.0 {
        [0, 2, 1]
    } else {
        [0, 1, 2]
    };
    let base = mesh.vertices.len() as u32;
    for &index in &order {
        mesh.vertices.push(Vertex {
            position: corners[index].into(),
            normal: [0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
        });
    }
    mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// 倒角板：轴对齐盒 + 45° 倒角（6 面 / 12 棱条 / 8 角片），面与棱**顶点全部分裂**。
///
/// `bevel` 为倒角宽度（0 < bevel < min(half) / 2）。96 顶点 / 44 三角形——
/// 与一颗 12×9 的超椭球同量级，但读作机加工件而非橡皮块。
pub fn plate(half: Vec3, bevel: f32) -> MeshData {
    let min = half.x.min(half.y).min(half.z);
    assert!(min > 0.0, "半轴必须为正");
    // 约束是「倒角后内盒不退化为零」：bevel < min(half) 即可（= min 时某个面缩成一点、
    // 三角形退化）。小件（扣具 / 导轨）常给到 min 的一半以上，故不设更紧的上限。
    assert!(
        bevel > 0.0 && bevel < min,
        "倒角宽度须在 (0, min(half)) 内（bevel={bevel}, min={min}）"
    );
    let inner = half - Vec3::splat(bevel);
    let mut mesh = MeshData::default();
    let axis = |i: usize, v: f32| {
        let mut out = Vec3::ZERO;
        out[i] = v;
        out
    };
    // 6 个面（每面 4 顶点，顶点分裂 ⇒ 面内法线常量）
    for a in 0..3 {
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        for sign in [-1.0f32, 1.0] {
            let center = axis(a, sign * half[a]);
            let u = axis(b, inner[b]);
            let v = axis(c, inner[c]);
            push_hard_quad(
                &mut mesh,
                [
                    center - u - v,
                    center + u - v,
                    center + u + v,
                    center - u + v,
                ],
                axis(a, sign),
            );
        }
    }
    // 12 条棱（沿第三条轴拉伸的 45° 倒角条）
    for a in 0..3 {
        for b in a + 1..3 {
            let c = 3 - a - b;
            for sa in [-1.0f32, 1.0] {
                for sb in [-1.0f32, 1.0] {
                    let p1 = axis(a, sa * half[a]) + axis(b, sb * inner[b]);
                    let p2 = axis(a, sa * inner[a]) + axis(b, sb * half[b]);
                    let dz = axis(c, inner[c]);
                    let hint = (axis(a, sa) + axis(b, sb)).normalize();
                    push_hard_quad(&mut mesh, [p1 - dz, p2 - dz, p2 + dz, p1 + dz], hint);
                }
            }
        }
    }
    // 8 个角片
    for sx in [-1.0f32, 1.0] {
        for sy in [-1.0f32, 1.0] {
            for sz in [-1.0f32, 1.0] {
                let hint = Vec3::new(sx, sy, sz);
                let p = Vec3::new(sx * half.x, sy * inner.y, sz * inner.z);
                let q = Vec3::new(sx * inner.x, sy * half.y, sz * inner.z);
                let r = Vec3::new(sx * inner.x, sy * inner.y, sz * half.z);
                push_hard_triangle(&mut mesh, [p, q, r], hint);
            }
        }
    }
    smooth_normals(&mut mesh);
    debug_assert!(signed_volume(&mesh) > 0.0, "倒角板环绕方向反了");
    mesh
}

/// 凸棱柱：XY 平面凸多边形沿 ±Z 拉伸，侧面与端盖顶点分裂（硬边）。
///
/// 轮廓须**凸**（端盖用质心扇三角化；凹轮廓会自交，故断言拒绝）。环绕方向
/// 自动归一（面积符号为负则反转顶点序），故调用方给顺时针或逆时针都行。
pub fn prism(outline: &[Vec2], half_depth: f32) -> MeshData {
    assert!(outline.len() >= 3, "棱柱轮廓至少三点");
    assert!(half_depth > 0.0, "棱柱半厚必须为正");
    let mut points = outline.to_vec();
    let area2: f32 = (0..points.len())
        .map(|i| {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            a.x * b.y - b.x * a.y
        })
        .sum();
    assert!(area2.abs() > 1e-9, "棱柱轮廓退化（面积为零）");
    if area2 < 0.0 {
        points.reverse();
    }
    let count = points.len();
    for i in 0..count {
        let a = points[i];
        let b = points[(i + 1) % count];
        let c = points[(i + 2) % count];
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        assert!(cross >= -1e-6, "棱柱轮廓必须为凸（第 {i} 点处凹陷）");
    }
    let centroid = points.iter().copied().sum::<Vec2>() / count as f32;
    let mut mesh = MeshData::default();
    for (z, hint) in [(half_depth, Vec3::Z), (-half_depth, -Vec3::Z)] {
        let center = Vec3::new(centroid.x, centroid.y, z);
        for i in 0..count {
            let a = points[i];
            let b = points[(i + 1) % count];
            push_hard_triangle(
                &mut mesh,
                [center, Vec3::new(a.x, a.y, z), Vec3::new(b.x, b.y, z)],
                hint,
            );
        }
    }
    for i in 0..count {
        let a = points[i];
        let b = points[(i + 1) % count];
        let edge = b - a;
        let hint = Vec3::new(edge.y, -edge.x, 0.0).normalize_or_zero();
        let hint = if hint.length_squared() < 0.5 {
            Vec3::new(a.x - centroid.x, a.y - centroid.y, 0.0).normalize()
        } else {
            hint
        };
        push_hard_quad(
            &mut mesh,
            [
                Vec3::new(a.x, a.y, -half_depth),
                Vec3::new(b.x, b.y, -half_depth),
                Vec3::new(b.x, b.y, half_depth),
                Vec3::new(a.x, a.y, half_depth),
            ],
            hint,
        );
    }
    smooth_normals(&mut mesh);
    debug_assert!(signed_volume(&mesh) > 0.0, "棱柱环绕方向反了");
    mesh
}

/// 弧形壳的一行：截面 + 该行的角度区间（弧度，绕截面的 `right` → `up` 方向）。
#[derive(Clone, Copy, Debug)]
pub struct ArcRow {
    /// 该行截面。
    pub section: Section,
    /// 起始角。
    pub start: f32,
    /// 结束角（大于 `start`；差值 ≈ 2π 视为闭合环）。
    pub end: f32,
}

impl ArcRow {
    /// 构造。
    pub fn new(section: Section, start: f32, end: f32) -> Self {
        assert!(end > start, "弧形行的结束角必须大于起始角");
        Self {
            section,
            start,
            end,
        }
    }
}

/// 弧形壳：一行行截面沿角度区间张开，再**向内偏移 `thickness`** 生成内表面，
/// 四条边界封边 ⇒ 闭合实体。
///
/// 这是护甲板 / 头盔 / 护胫 / 腰带 / 靴筒的通用形态：外内表面顶点**共享**
/// （沿弧与沿轮廓平滑），封边顶点**分裂**（边缘硬边），于是「贴合身体的曲面
/// + 一圈锐利包边」——正是现代单兵装具的读法。
///
/// 各行可给不同角度区间（做上窄下宽的甲片），但闭合性必须一致（要么都是
/// 闭合环、要么都是开弧）。环绕方向由首个四边形的几何自动判定，调用方不必操心。
pub fn arc_shell(rows: &[ArcRow], segments: u32, thickness: f32) -> MeshData {
    assert!(rows.len() >= 2, "弧形壳至少两行");
    assert!(segments >= 2, "分段数太小");
    assert!(thickness > 0.0, "壳厚必须为正");
    let is_closed = |row: &ArcRow| (row.end - row.start) >= std::f32::consts::TAU - 1e-3;
    let closed = is_closed(&rows[0]);
    assert!(
        rows.iter().all(|row| is_closed(row) == closed),
        "各行的闭合性必须一致（要么都是闭合环、要么都是开弧）"
    );
    let rows_count = rows.len();
    let columns = if closed {
        segments as usize
    } else {
        segments as usize + 1
    };
    let theta =
        |row: &ArcRow, j: usize| row.start + (row.end - row.start) * j as f32 / segments as f32;
    // 两张网格的位置先全算出来：封边既要读两侧位置、又要同时写 mesh
    let mut positions: Vec<Vec3> = Vec::with_capacity(rows_count * columns * 2);
    for inset in [0.0f32, thickness] {
        for row in rows {
            for j in 0..columns {
                positions.push(row.section.point_at(theta(row, j), inset));
            }
        }
    }
    let outer = |i: usize, j: usize| i * columns + j;
    let inner = |i: usize, j: usize| rows_count * columns + i * columns + j;
    let next = |j: usize| {
        if closed { (j + 1) % columns } else { j + 1 }
    };
    let mut mesh = MeshData::default();
    for position in &positions {
        mesh.vertices.push(Vertex {
            position: (*position).into(),
            normal: [0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
        });
    }
    let face = |a: usize, b: usize, c: usize| {
        (positions[b] - positions[a]).cross(positions[c] - positions[a])
    };
    let flip = face(outer(0, 0), outer(1, 0), outer(1, next(0)))
        .dot(positions[outer(0, 0)] - rows[0].section.center)
        < 0.0;
    let mut push_quad = |a: usize, b: usize, c: usize, d: usize, reversed: bool| {
        let (a, b, c, d) = (a as u32, b as u32, c as u32, d as u32);
        if reversed {
            mesh.indices.extend_from_slice(&[a, d, c, a, c, b]);
        } else {
            mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    };
    for i in 0..rows_count - 1 {
        for j in 0..segments as usize {
            let j1 = next(j);
            push_quad(
                outer(i, j),
                outer(i + 1, j),
                outer(i + 1, j1),
                outer(i, j1),
                flip,
            );
            // 内表面：与外表面对应的角点序相同、环绕相反 ⇒ 法线朝轴心
            push_quad(
                inner(i, j),
                inner(i + 1, j),
                inner(i + 1, j1),
                inner(i, j1),
                !flip,
            );
        }
    }
    // 封边（顶点分裂 ⇒ 与内外表面之间是硬边）
    let tangent = |row: &ArcRow, at: f32| {
        let eps = 1e-3f32;
        row.section.point_at(at + eps, 0.0) - row.section.point_at(at - eps, 0.0)
    };
    if !closed {
        let last = columns - 1;
        for i in 0..rows_count - 1 {
            let start_hint = -tangent(&rows[i], rows[i].start);
            push_hard_quad(
                &mut mesh,
                [
                    positions[outer(i, 0)],
                    positions[outer(i + 1, 0)],
                    positions[inner(i + 1, 0)],
                    positions[inner(i, 0)],
                ],
                start_hint,
            );
            let end_hint = tangent(&rows[i], rows[i].end);
            push_hard_quad(
                &mut mesh,
                [
                    positions[outer(i, last)],
                    positions[outer(i + 1, last)],
                    positions[inner(i + 1, last)],
                    positions[inner(i, last)],
                ],
                end_hint,
            );
        }
    }
    let row_hint = |i: usize| rows[i + 1].section.center - rows[i].section.center;
    for (i, hint) in [
        (0usize, -row_hint(0)),
        (rows_count - 1, row_hint(rows_count - 2)),
    ] {
        for j in 0..segments as usize {
            let j1 = next(j);
            push_hard_quad(
                &mut mesh,
                [
                    positions[outer(i, j)],
                    positions[outer(i, j1)],
                    positions[inner(i, j1)],
                    positions[inner(i, j)],
                ],
                hint,
            );
        }
    }
    smooth_normals(&mut mesh);
    let volume = signed_volume(&mesh);
    debug_assert!(volume > 0.0, "弧形壳环绕方向反了（体积 {volume}）");
    mesh
}

/// 折线扫掠：剖面沿三维折线扫过，**平行传输框架**避免扭转（背带 / 下巴带 /
/// 枪背带 / 鞋带 / 线缆）。
///
/// 剖面 `(t, 半宽, 半高, 指数)`，`t ∈ [0,1]` 是**弧长比例**（不是控制点比例，
/// 故控制点疏密不影响剖面分布）。两端封盖。折线不得有零长段（断言）。
pub fn sweep(
    path: &[Vec3],
    profile: &[(f32, f32, f32, f32)],
    segments: u32,
    up_hint: Vec3,
) -> MeshData {
    assert!(path.len() >= 2, "扫掠路径至少两点");
    assert!(profile.len() >= 2, "剖面至少两点");
    let mut arc = vec![0.0f32];
    for window in path.windows(2) {
        let distance = (window[1] - window[0]).length();
        assert!(distance > 1e-6, "扫掠路径存在零长段");
        arc.push(arc[arc.len() - 1] + distance);
    }
    let total = arc[arc.len() - 1];
    let mut frames: Vec<(Vec3, Vec3, Vec3)> = Vec::with_capacity(path.len());
    for index in 0..path.len() {
        let tangent = if index == 0 {
            (path[1] - path[0]).normalize()
        } else if index == path.len() - 1 {
            (path[index] - path[index - 1]).normalize()
        } else {
            (path[index + 1] - path[index - 1]).normalize()
        };
        let (right, up) = match frames.last() {
            None => {
                let hint = if up_hint.cross(tangent).length_squared() < 1e-8 {
                    Vec3::X
                } else {
                    up_hint
                };
                let right = hint.cross(tangent).normalize();
                (right, tangent.cross(right).normalize())
            }
            Some(&(right, _, previous)) => {
                let rotation = Quat::from_rotation_arc(previous, tangent);
                let right = (rotation * right).normalize();
                (right, tangent.cross(right).normalize())
            }
        };
        frames.push((right, up, tangent));
    }
    let sections: Vec<Section> = profile
        .iter()
        .map(|&(t, half_right, half_up, exponent)| {
            let distance = t.clamp(0.0, 1.0) * total;
            let mut index = path.len() - 2;
            for k in 0..path.len() - 1 {
                if distance <= arc[k + 1] {
                    index = k;
                    break;
                }
            }
            let span = arc[index + 1] - arc[index];
            let f = ((distance - arc[index]) / span).clamp(0.0, 1.0);
            let (right, up, _) = frames[index];
            Section::new(
                path[index].lerp(path[index + 1], f),
                right,
                up,
                half_right,
                half_up,
                exponent,
            )
        })
        .collect();
    loft(&sections, segments, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环绕方向自检：截面序反着排（镜像部件的行方向相反）也产出外向壳。
    ///
    /// 这条不变量此前靠「调用方记得反着排截面」维持，整身实测漏了 20 个部件
    /// （鞋楦三件、双肩三角肌、双掌鱼际、头盔耳罩、脸侧滤罐……）。现在由
    /// loft / lathe / 超椭球在生成期按几何纠向，本测试把该行为钉死：同一几何
    /// 的两个截面序都应给出**正**有向体积，且体积相等（只差索引序）。
    #[test]
    fn primitive_orientation_is_independent_of_input_order() {
        let section =
            |y: f32| Section::new(Vec3::new(0.0, y, 0.0), Vec3::X, Vec3::Z, 0.08, 0.06, 2.4);
        let up = loft(&[section(0.0), section(0.1), section(0.2)], 12, true);
        let down = loft(&[section(0.2), section(0.1), section(0.0)], 12, true);
        let (a, b) = (signed_volume(&up), signed_volume(&down));
        assert!(a > 0.0 && b > 0.0, "两个截面序都应外向：{a} / {b}");
        assert!((a - b).abs() < 1e-6, "同一几何的体积应相等：{a} vs {b}");
        let profile: [(f32, f32); 4] = [(0.0, -0.05), (0.04, -0.03), (0.04, 0.03), (0.0, 0.05)];
        let reversed: Vec<(f32, f32)> = profile.iter().rev().copied().collect();
        let (c, d) = (
            signed_volume(&lathe(&profile, 12)),
            signed_volume(&lathe(&reversed, 12)),
        );
        assert!(c > 0.0 && d > 0.0, "旋转体两个轮廓序都应外向：{c} / {d}");
        assert!(
            (c - d).abs() < 1e-6,
            "旋转体同一几何的体积应相等：{c} vs {d}"
        );
    }

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
    #[test]
    fn plate_faces_are_flat_and_solid_is_positive() {
        let half = Vec3::new(0.06, 0.09, 0.03);
        let mesh = plate(half, 0.006);
        assert_eq!(mesh.vertices.len(), 96, "6 面 + 12 棱 + 8 角，顶点全分裂");
        assert_eq!(mesh.indices.len() / 3, 44, "12 + 24 + 8 三角形");
        assert!(signed_volume(&mesh) > 0.0, "环绕方向应为外向");
        for v in &mesh.vertices {
            let p = Vec3::from(v.position);
            assert!(p.x.abs() <= half.x + 1e-6);
            assert!(p.y.abs() <= half.y + 1e-6);
            assert!(p.z.abs() <= half.z + 1e-6);
        }
        for triangle in mesh.indices.chunks_exact(3) {
            let point = |i: u32| Vec3::from(mesh.vertices[i as usize].position);
            let face = (point(triangle[1]) - point(triangle[0]))
                .cross(point(triangle[2]) - point(triangle[0]));
            assert!(face.length() > 1e-9, "退化三角形");
            let face = face.normalize();
            for &index in triangle {
                let n = Vec3::from(mesh.vertices[index as usize].normal);
                assert!(
                    n.dot(face) > 0.9999,
                    "硬表面三角形的顶点法线应等于面法线（点积 {}）",
                    n.dot(face)
                );
            }
        }
    }

    #[test]
    fn prism_volume_matches_analytic_and_normalizes_winding() {
        // 等腰梯形（凸）：下底 0.10 / 上底 0.06 / 高 0.08
        let outline = [
            Vec2::new(-0.05, -0.04),
            Vec2::new(0.05, -0.04),
            Vec2::new(0.03, 0.04),
            Vec2::new(-0.03, 0.04),
        ];
        let half_depth = 0.02f32;
        let mesh = prism(&outline, half_depth);
        let area = 0.5 * (0.10 + 0.06) * 0.08;
        let expected = area * 2.0 * half_depth;
        let volume = signed_volume(&mesh);
        assert!(
            (volume - expected).abs() < expected * 1e-5,
            "棱柱体积 {volume} 应等于解析值 {expected}"
        );
        let mut reversed = outline;
        reversed.reverse();
        let flipped = prism(&reversed, half_depth);
        assert!(
            signed_volume(&flipped) > 0.0,
            "顺时针输入应自动归一为外向环绕"
        );
        assert_eq!(bits(&mesh), bits(&flipped), "顶点序归一后应逐位一致");
    }

    #[test]
    fn arc_shell_is_closed_and_matches_analytic_volume() {
        // 90° 圆弧壳：半径 0.30、厚 0.02、高 0.20（中径 × 弧长 × 厚 × 高）
        let radius = 0.3f32;
        let thickness = 0.02f32;
        let rows: Vec<ArcRow> = [0.0f32, 0.10, 0.20]
            .iter()
            .map(|&y| {
                ArcRow::new(
                    Section::new(
                        Vec3::new(0.0, y, 0.0),
                        Vec3::X,
                        Vec3::Z,
                        radius,
                        radius,
                        2.0,
                    ),
                    0.0,
                    std::f32::consts::FRAC_PI_2,
                )
            })
            .collect();
        let mesh = arc_shell(&rows, 48, thickness);
        let inner_radius = radius - thickness;
        let expected = 0.5
            * (radius * radius - inner_radius * inner_radius)
            * std::f32::consts::FRAC_PI_2
            * 0.20;
        let volume = signed_volume(&mesh);
        assert!(
            (volume - expected).abs() < expected * 0.01,
            "弧形壳体积 {volume} 应接近解析值 {expected}"
        );
        // 外表面（共享顶点）平滑 / 封边（分裂顶点）硬边——两者必须同时存在
        let mut flat = 0usize;
        let mut smooth = 0usize;
        for triangle in mesh.indices.chunks_exact(3) {
            let point = |i: u32| Vec3::from(mesh.vertices[i as usize].position);
            let face = (point(triangle[1]) - point(triangle[0]))
                .cross(point(triangle[2]) - point(triangle[0]))
                .normalize();
            let hard = triangle
                .iter()
                .all(|&index| Vec3::from(mesh.vertices[index as usize].normal).dot(face) > 0.9999);
            if hard {
                flat += 1;
            } else {
                smooth += 1;
            }
        }
        assert!(
            flat > 0 && smooth > 0,
            "应为「曲面 + 硬封边」（flat={flat}, smooth={smooth}）"
        );
        // 闭合环：整圈 2π，体积 = π(R² − r²) × 高
        let ring: Vec<ArcRow> = [0.0f32, 0.20]
            .iter()
            .map(|&y| {
                ArcRow::new(
                    Section::new(
                        Vec3::new(0.0, y, 0.0),
                        Vec3::X,
                        Vec3::Z,
                        radius,
                        radius,
                        2.0,
                    ),
                    0.0,
                    std::f32::consts::TAU,
                )
            })
            .collect();
        let ring_mesh = arc_shell(&ring, 64, thickness);
        let inner = radius - thickness;
        let ring_expected = std::f32::consts::PI * (radius * radius - inner * inner) * 0.20;
        let ring_volume = signed_volume(&ring_mesh);
        assert!(
            (ring_volume - ring_expected).abs() < ring_expected * 0.01,
            "闭合环体积 {ring_volume} 应接近解析值 {ring_expected}"
        );
    }

    #[test]
    fn sweep_maps_profile_to_arc_length() {
        let path = [
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -0.5),
            Vec3::new(0.4, 0.0, -0.5),
        ];
        let profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.0, 0.05, 0.05, 2.0),
            (0.5, 0.012, 0.012, 2.0),
            (1.0, 0.05, 0.05, 2.0),
        ];
        let mesh = sweep(&path, &profile, 12, Vec3::Y);
        assert_eq!(mesh.vertices.len(), 3 * 12 + 2, "三截面 × 12 段 + 两端盖心");
        // 顶点到折线的距离不超过剖面最大半径
        let distance = |p: Vec3| {
            path.windows(2)
                .map(|w| {
                    let d = w[1] - w[0];
                    let t = (p - w[0]).dot(d) / d.length_squared();
                    (p - (w[0] + d * t.clamp(0.0, 1.0))).length()
                })
                .fold(f32::MAX, f32::min)
        };
        for v in &mesh.vertices {
            let d = distance(Vec3::from(v.position));
            assert!(d <= 0.05 + 1e-5, "顶点离路径 {d} 超出剖面半径");
        }
        // 剖面 t = 0.5 落在**弧长中点**（z ≈ −0.45），而不是控制点中点（z = −0.25）
        let middle = Vec3::new(0.0, 0.0, -0.45);
        let nearest = mesh
            .vertices
            .iter()
            .map(|v| (Vec3::from(v.position) - middle).length())
            .fold(f32::MAX, f32::min);
        assert!(
            nearest < 0.02,
            "t=0.5 截面应在弧长中点，最近顶点距离 {nearest}"
        );
    }

    #[test]
    fn hard_surface_generators_are_deterministic_bitwise() {
        let half = Vec3::new(0.05, 0.08, 0.02);
        assert_eq!(bits(&plate(half, 0.005)), bits(&plate(half, 0.005)));
        let outline = [
            Vec2::new(-0.05, -0.04),
            Vec2::new(0.05, -0.04),
            Vec2::new(0.03, 0.04),
        ];
        assert_eq!(bits(&prism(&outline, 0.02)), bits(&prism(&outline, 0.02)));
        let rows = [
            ArcRow::new(
                Section::new(Vec3::new(0.0, 0.0, 0.0), Vec3::X, Vec3::Z, 0.2, 0.15, 2.4),
                0.4,
                2.2,
            ),
            ArcRow::new(
                Section::new(Vec3::new(0.0, 0.1, 0.0), Vec3::X, Vec3::Z, 0.18, 0.13, 2.4),
                0.4,
                2.2,
            ),
        ];
        assert_eq!(
            bits(&arc_shell(&rows, 16, 0.01)),
            bits(&arc_shell(&rows, 16, 0.01))
        );
        let path = [
            Vec3::ZERO,
            Vec3::new(0.0, 0.2, 0.1),
            Vec3::new(0.2, 0.2, 0.1),
        ];
        let profile: Vec<(f32, f32, f32, f32)> =
            vec![(0.0, 0.02, 0.02, 2.0), (1.0, 0.02, 0.02, 2.0)];
        assert_eq!(
            bits(&sweep(&path, &profile, 10, Vec3::Y)),
            bits(&sweep(&path, &profile, 10, Vec3::Y))
        );
    }
}
