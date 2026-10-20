//! 程序化发带 groom 生成器（R90）。
//!
//! glTF 无毛发标准 ⇒ 毛发几何走程序化生成（assault 程序化资产管线
//! 范式：纯函数 + 无状态哈希流 + 逐项单测）。本模块生成**发带
//! （camera-facing ribbon）控制网格**：每束发丝 = 一条折线，逐控制点
//! 存「位置 + 中心差分切线 + 宽度 + 沿丝参数」，渲染侧由
//! `hair_vs.wgsl` 做相机面向屏幕空间展宽（相邻段共享控制点顶点 ⇒
//! 关节处展宽结果逐位一致、无缝）。
//!
//! # 确定性契约
//!
//! 随机性全部来自 [`mix64`] 无状态哈希流（`particles.rs` 同款，无 rand
//! 依赖、无全局状态）；**逐束随机字的消费顺序就是契约**——同参数同
//! 种子重跑，输出缓冲逐字节一致（单测锁定）。改消费顺序 = 改所有发
//! 型，与改公式同级，必须声明。
//!
//! # 发型模型（v1 诚实边界）
//!
//! 头皮 = XZ 平面圆盘（半径 [`HairGroomParams::scalp_radius`]，面积
//! 均匀采样），发丝沿 −Y 下垂，弯曲 = 每束一个随机方位角 × 正弦相位
//! 的水平偏移（Scheuermann 风格简化）。不做：头皮网格种子（沿法线
//! 分布）、重力/碰撞整形、层级 LOD（R91）、丛簇（clumps）。
//!
//! 壳层（shell fur）不需要 groom——直接挂 [`crate::fur::ShellFur`]
//! 到任意网格实体。

/// splitmix64 终混合函数（`particles.rs` 同款，确定性伪随机原语）。
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 64 位随机字 → [0, 1) 均匀小数（`particles.rs::fraction` 同式）。
fn fraction(u: u64) -> f32 {
    (u >> 11) as f32 * (1.0 / (1u64 << 53) as f32)
}

/// groom 参数。全部字段参与确定性输出（同参数同种子 ⇒ 同网格）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HairGroomParams {
    /// 发束数（每束一条折线）。
    pub strands: u32,
    /// 每束控制点数（≥ 2；越多发丝越平滑）。
    pub points: u32,
    /// 发丝长度（世界单位，直线长度近似——弯曲不改变逐段累计跨度）。
    pub length: f32,
    /// 根部发丝宽度（世界单位；发带全宽）。
    pub width_root: f32,
    /// 梢部发丝宽度（线性收窄；0 = 尖收）。
    pub width_tip: f32,
    /// 弯曲幅度系数 [0, 1]：0 = 纯垂直下垂，1 = 水平偏移与长度同量级。
    pub curl: f32,
    /// 头皮圆盘半径（世界单位）。
    pub scalp_radius: f32,
    /// 随机种子（区分发型）。
    pub seed: u64,
}

impl Default for HairGroomParams {
    fn default() -> Self {
        Self {
            strands: 128,
            points: 8,
            length: 0.35,
            width_root: 0.004,
            width_tip: 0.001,
            curl: 0.35,
            scalp_radius: 0.09,
            seed: 0x1E90_0001,
        }
    }
}

/// 发带控制网格顶点（GPU 顶点缓冲布局，[`HAIR_VERTEX_LAYOUT`]）。
///
/// `position` = 控制点位置（实体局部空间）；`dir_width.xyz` = 控制点
/// 切线（中心差分，归一），`dir_width.w` = 该点发丝全宽；`t_side.x` =
/// 沿丝参数 t ∈ [0,1]（根 0 梢 1，Kajiya-Kay 根梢高光移位的输入），
/// `t_side.y` = 横向侧别 ±1。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HairStrandVertex {
    pub position: [f32; 3],
    pub dir_width: [f32; 4],
    pub t_side: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<HairStrandVertex>() == 44);

/// 发带顶点缓冲布局（hair_vs.wgsl 的 loc 0–2；实例布局复用
/// [`crate::instance::InstanceRaw`] 的 loc 3–9 / 14）。
pub const HAIR_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<HairStrandVertex>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            offset: 12,
            shader_location: 1,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 28,
            shader_location: 2,
            format: wgpu::VertexFormat::Float32x4,
        },
    ],
};

/// groom 产物：发带控制网格（顶点 + 索引）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HairStrandMesh {
    pub vertices: Vec<HairStrandVertex>,
    pub indices: Vec<u32>,
    /// 每束控制点数（R91 动态模拟的控制点布局——`strand_step` 按束 ×
    /// 段 × 4 的顶点布局寻址，自定义网格必须如实填写）。0 = 未声明
    /// （HairDynamics 拒绝模拟并 warn——顶点数分解有歧义，不猜）。
    pub points_per_strand: usize,
}

/// 生成发带控制网格。逐束随机字消费序（契约，勿改）：
/// `mix64(seed + strand × 5 + k)`，k = 0 根径向 / 1 根方位 / 2 弯曲
/// 相位 / 3 弯曲波数 / 4 弯曲方位。
pub fn generate_hair_strands(params: &HairGroomParams) -> HairStrandMesh {
    assert!(params.points >= 2, "每束控制点数至少 2（points >= 2）");
    assert!(params.strands >= 1, "发束数至少 1");
    let segments = (params.points - 1) as usize;
    let mut vertices = Vec::with_capacity(params.strands as usize * segments * 4);
    let mut indices = Vec::with_capacity(params.strands as usize * segments * 6);

    for strand in 0..params.strands as u64 {
        let base = params.seed.wrapping_add(strand.wrapping_mul(5));
        let u_radius = fraction(mix64(base));
        let u_theta = fraction(mix64(base.wrapping_add(1)));
        let u_phase = fraction(mix64(base.wrapping_add(2)));
        let u_waves = fraction(mix64(base.wrapping_add(3)));
        let u_curl_dir = fraction(mix64(base.wrapping_add(4)));

        // 面积均匀圆盘采样：r = R·√u，θ = 2πv
        let root_r = params.scalp_radius * u_radius.sqrt();
        let root_theta = u_theta * std::f32::consts::TAU;
        let root = [root_r * root_theta.cos(), 0.0, root_r * root_theta.sin()];
        // 弯曲：水平方位 ψ 的正弦偏移，波数 ∈ [1, 3]，幅度随 s 增长
        //（根部贴头皮不飘）
        let curl_theta = u_curl_dir * std::f32::consts::TAU;
        let curl_dir = [curl_theta.cos(), 0.0, curl_theta.sin()];
        let phase = u_phase * std::f32::consts::TAU;
        let waves = 1.0 + u_waves * 2.0;

        // 折线控制点 + 中心差分切线 + 线性宽度
        let mut points = Vec::with_capacity(params.points as usize);
        for j in 0..params.points as usize {
            let s = j as f32 / (params.points - 1) as f32;
            let swing =
                params.curl * params.length * (phase + s * waves * std::f32::consts::TAU).sin() * s;
            points.push([
                root[0] + curl_dir[0] * swing,
                root[1] - params.length * s,
                root[2] + curl_dir[2] * swing,
            ]);
        }
        let mut points_tangent = Vec::with_capacity(params.points as usize);
        for j in 0..params.points as usize {
            let prev = points[j.saturating_sub(1)];
            let next = points[(j + 1).min(params.points as usize - 1)];
            let d = [next[0] - prev[0], next[1] - prev[1], next[2] - prev[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let s = j as f32 / (params.points - 1) as f32;
            let width = params.width_root + (params.width_tip - params.width_root) * s;
            // 存切线与宽度：hair_vs 展宽与 Kajiya-Kay 的主方向同源
            points_tangent.push((d[0] / len, d[1] / len, d[2] / len, width));
        }
        // 段四边形：相邻段共享控制点顶点（位置 / 方向 / 侧别全同）
        // ⇒ 展宽后关节无缝（hair_vs 逐顶点独立展宽的衔接根据）
        for j in 0..segments {
            let s0 = j as f32 / (params.points - 1) as f32;
            let s1 = (j + 1) as f32 / (params.points - 1) as f32;
            let quad = [
                (j, s0, -1.0f32),
                (j, s0, 1.0),
                (j + 1, s1, 1.0),
                (j + 1, s1, -1.0),
            ];
            let idx_base = vertices.len() as u32;
            for (pj, t, side) in quad {
                let p = points[pj];
                let (tx, ty, tz, w) = points_tangent[pj];
                vertices.push(HairStrandVertex {
                    position: p,
                    dir_width: [tx, ty, tz, w],
                    t_side: [t, side, 0.0, 0.0],
                });
            }
            indices.extend_from_slice(&[
                idx_base,
                idx_base + 1,
                idx_base + 2,
                idx_base,
                idx_base + 2,
                idx_base + 3,
            ]);
        }
    }

    HairStrandMesh {
        vertices,
        indices,
        points_per_strand: params.points as usize,
    }
}

/// 生成**交叉卡片**网格（R91 远景 LOD）：每束发一条折线 × 两向交叉
/// （相隔 60° 的两个朝向，资产惯例的 X 型卡片堆）——标准顶点格式
/// （position / normal / uv + 切线 = 发丝方向），走标准不透明管线
/// （Kajiya-Kay 材质照常生效——切线通道接续发带语义）。
///
/// 随机字消费序与 [`generate_hair_strands`] **同一契约**（同参数同
/// 种子 ⇒ 发带与卡片的发束根位置 / 弯曲相位一致——LOD 切换时发束
/// 位置不跳变）。卡片宽度 = 该点发丝宽 × `card_scale`（LOD 卡片通常
/// 比单丝宽以补偿密度损失，典型 3–6×）。
///
/// 诚实边界：卡片法线取「垂直于发丝且朝世界 +Y 分量最大的方向」的
/// 简化近似（LOD 远档的画面主导项是剪影与颜色，法线精度不敏感）。
pub fn generate_hair_cards(params: &HairGroomParams, card_scale: f32) -> crate::mesh::MeshData {
    assert!(params.points >= 2, "每束控制点数至少 2（points >= 2）");
    // 先按发带同款契约生成折线（消费序一致），再展开成卡片
    let mut vertices: Vec<crate::mesh::Vertex> = Vec::new();
    let mut tangents: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let segments = (params.points - 1) as usize;

    for strand in 0..params.strands as u64 {
        let base = params.seed.wrapping_add(strand.wrapping_mul(5));
        let u_radius = fraction(mix64(base));
        let u_theta = fraction(mix64(base.wrapping_add(1)));
        let u_phase = fraction(mix64(base.wrapping_add(2)));
        let u_waves = fraction(mix64(base.wrapping_add(3)));
        let u_curl_dir = fraction(mix64(base.wrapping_add(4)));

        let root_r = params.scalp_radius * u_radius.sqrt();
        let root_theta = u_theta * std::f32::consts::TAU;
        let root = [root_r * root_theta.cos(), 0.0, root_r * root_theta.sin()];
        let curl_theta = u_curl_dir * std::f32::consts::TAU;
        let curl_dir = [curl_theta.cos(), 0.0, curl_theta.sin()];
        let phase = u_phase * std::f32::consts::TAU;
        let waves = 1.0 + u_waves * 2.0;

        let mut points = Vec::with_capacity(params.points as usize);
        for j in 0..params.points as usize {
            let s = j as f32 / (params.points - 1) as f32;
            let swing =
                params.curl * params.length * (phase + s * waves * std::f32::consts::TAU).sin() * s;
            points.push([
                root[0] + curl_dir[0] * swing,
                root[1] - params.length * s,
                root[2] + curl_dir[2] * swing,
            ]);
        }
        // 逐控制点切线（中心差分，与发带同式）
        let mut dirs = Vec::with_capacity(points.len());
        for j in 0..points.len() {
            let prev = points[j.saturating_sub(1)];
            let next = points[(j + 1).min(points.len() - 1)];
            let d = [next[0] - prev[0], next[1] - prev[1], next[2] - prev[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            dirs.push([d[0] / len, d[1] / len, d[2] / len]);
        }
        // 两向交叉卡片：朝向 0° 与 60°（世界 Y 轴为参考）
        for orientation in [0.0f32, std::f32::consts::FRAC_PI_3] {
            for j in 0..segments {
                let s0 = j as f32 / segments as f32;
                let s1 = (j + 1) as f32 / segments as f32;
                let w0 = (params.width_root + (params.width_tip - params.width_root) * s0)
                    * card_scale
                    * 0.5;
                let w1 = (params.width_root + (params.width_tip - params.width_root) * s1)
                    * card_scale
                    * 0.5;
                // 卡片横向方向 = 垂直于发丝切线、绕发丝旋转 orientation
                //（参考向量 +Y 与切线的差积正交化）
                let t0 = dirs[j];
                let t1 = dirs[j + 1];
                let side_of = |t: [f32; 3], w: f32| -> [f32; 3] {
                    // n1 = cross(t, Y)（退化时用 X）
                    let mut n = [t[2], 0.0, -t[0]];
                    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                    if len < 1e-6 {
                        n = [1.0, 0.0, 0.0];
                    } else {
                        n = [n[0] / len, n[1] / len, n[2] / len];
                    }
                    // 绕发丝方向旋转 orientation：Rodrigues（n 为单位、
                    // t 为单位 ⇒ n' = n·cos + cross(t,n)·sin）
                    let cos = orientation.cos();
                    let sin = orientation.sin();
                    let ctn = [
                        t[1] * n[2] - t[2] * n[1],
                        t[2] * n[0] - t[0] * n[2],
                        t[0] * n[1] - t[1] * n[0],
                    ];
                    [
                        (n[0] * cos + ctn[0] * sin) * w,
                        (n[1] * cos + ctn[1] * sin) * w,
                        (n[2] * cos + ctn[2] * sin) * w,
                    ]
                };
                let a0 = side_of(t0, w0);
                let a1 = side_of(t1, w1);
                // 简化法线（朝 +Y 分量最大）：卡片面法线 = cross(side, t)
                let normal_of = |side: [f32; 3], t: [f32; 3]| -> [f32; 3] {
                    let mut n = [
                        side[1] * t[2] - side[2] * t[1],
                        side[2] * t[0] - side[0] * t[2],
                        side[0] * t[1] - side[1] * t[0],
                    ];
                    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                    if len < 1e-8 {
                        return [0.0, 1.0, 0.0];
                    }
                    n = [n[0] / len, n[1] / len, n[2] / len];
                    if n[1] < 0.0 { [-n[0], -n[1], -n[2]] } else { n }
                };
                let n0 = normal_of(a0, t0);
                let idx_base = vertices.len() as u32;
                let quad = [
                    (points[j], a0, n0, [s0, 0.0], t0),
                    (points[j], [-a0[0], -a0[1], -a0[2]], n0, [s0, 1.0], t0),
                    (points[j + 1], [-a1[0], -a1[1], -a1[2]], n0, [s1, 1.0], t1),
                    (points[j + 1], a1, n0, [s1, 0.0], t1),
                ];
                for (p, off, normal, uv, tan) in quad {
                    vertices.push(crate::mesh::Vertex {
                        position: [p[0] + off[0], p[1] + off[1], p[2] + off[2]],
                        normal,
                        uv,
                    });
                    tangents.push([tan[0], tan[1], tan[2], 1.0]);
                }
                indices.extend_from_slice(&[
                    idx_base,
                    idx_base + 1,
                    idx_base + 2,
                    idx_base,
                    idx_base + 2,
                    idx_base + 3,
                ]);
            }
        }
    }

    crate::mesh::MeshData {
        vertices,
        indices,
        skin: None,
        tangents: Some(tangents),
        uv2: None,
        colors: None,
        morph: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params() -> HairGroomParams {
        HairGroomParams {
            strands: 16,
            points: 5,
            length: 0.3,
            width_root: 0.005,
            width_tip: 0.001,
            curl: 0.4,
            scalp_radius: 0.08,
            seed: 42,
        }
    }

    /// 确定性铁律：同参数同种子双跑，顶点与索引逐字节一致；
    /// 种子不同 ⇒ 输出不同（哈希流可判别）。
    #[test]
    fn deterministic_and_seed_distinguishable() {
        let a = generate_hair_strands(&sample_params());
        let b = generate_hair_strands(&sample_params());
        assert_eq!(a.vertices, b.vertices, "同参数双跑顶点必须逐位一致");
        assert_eq!(a.indices, b.indices);
        let mut c_params = sample_params();
        c_params.seed = 43;
        let c = generate_hair_strands(&c_params);
        assert_ne!(a.vertices, c.vertices, "不同种子必须可判别");
    }

    /// 数量公式：顶点 = 束 × 段 × 4、索引 = 束 × 段 × 6。
    #[test]
    fn counts_formula() {
        let p = sample_params();
        let m = generate_hair_strands(&p);
        let segments = (p.points - 1) as usize;
        assert_eq!(
            m.vertices.len(),
            p.strands as usize * segments * 4,
            "顶点数 = 束 × 段 × 4"
        );
        assert_eq!(
            m.indices.len(),
            p.strands as usize * segments * 6,
            "索引数 = 束 × 段 × 6"
        );
        assert_eq!(m.indices.len() % 3, 0, "三角形整除");
    }

    /// 几何解析锚：控制点 y 从 0 单调降到 −length（下垂主导，弯曲只在
    /// 水平向）；首点 y = 0、末点 y = −length（精确值——逐段等跨度）。
    #[test]
    fn strand_geometry_anchors() {
        let p = sample_params();
        let m = generate_hair_strands(&p);
        let segments = (p.points - 1) as usize;
        // 段 0 的四个顶点 = 控制点 0（左/右） + 控制点 1（右/左）
        let v_root = &m.vertices[0];
        assert_eq!(v_root.position[1], 0.0, "根部 y = 0（贴头皮）");
        let v_next = &m.vertices[2];
        let step = -p.length / (p.points - 1) as f32;
        assert!(
            (v_next.position[1] - step).abs() < 1e-6,
            "第二控制点 y = −length/(points−1)（精确 {}，得 {}）",
            step,
            v_next.position[1]
        );
        let last_seg = (p.strands as usize - 1) * segments * 4 + (segments - 1) * 4;
        let v_tip = &m.vertices[last_seg + 2];
        assert!(
            (v_tip.position[1] + p.length).abs() < 1e-6,
            "梢部 y = −length（精确），得 {}",
            v_tip.position[1]
        );
        // 宽度线性：根部顶点 w = width_root，梢部 w = width_tip
        assert_eq!(v_root.dir_width[3], p.width_root);
        assert!((v_tip.dir_width[3] - p.width_tip).abs() < 1e-7);
    }

    /// 切线归一 + 沿丝参数域 + 侧别 ±1。
    #[test]
    fn tangent_normalized_and_params_in_domain() {
        let m = generate_hair_strands(&sample_params());
        for v in &m.vertices {
            let t = v.dir_width;
            let len = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "切线必须归一，得 {len}");
            assert!(
                (0.0..=1.0).contains(&v.t_side[0]),
                "沿丝参数 t ∈ [0,1]，得 {}",
                v.t_side[0]
            );
            assert_eq!(v.t_side[1].abs(), 1.0, "侧别 ±1");
        }
    }

    /// 关节共享：段 j 的末端顶点与段 j+1 的首端顶点逐位相等
    ///（位置 / 方向 / 宽度 / t / side 全同）——hair_vs 无缝衔接的根据。
    #[test]
    fn adjacent_segments_share_joint_vertices() {
        let p = sample_params();
        let m = generate_hair_strands(&p);
        let segments = (p.points - 1) as usize;
        for s in 0..p.strands as usize {
            for j in 0..segments - 1 {
                let seg_base = s * segments * 4 + j * 4;
                let next_base = s * segments * 4 + (j + 1) * 4;
                // 段 j 顶点 2/3（梢端 ±）与段 j+1 顶点 1/0（根端 ±）共享
                assert_eq!(m.vertices[seg_base + 2], m.vertices[next_base + 1]);
                assert_eq!(m.vertices[seg_base + 3], m.vertices[next_base]);
            }
        }
    }

    /// 头皮圆盘域：所有根部顶点的水平半径 ≤ scalp_radius（弯曲只加在
    /// 后续控制点——根部位置严格在圆盘内）。
    #[test]
    fn roots_within_scalp_disc() {
        let p = sample_params();
        let m = generate_hair_strands(&p);
        let segments = (p.points - 1) as usize;
        for s in 0..p.strands as usize {
            let v = &m.vertices[s * segments * 4];
            let r = (v.position[0] * v.position[0] + v.position[2] * v.position[2]).sqrt();
            assert!(
                r <= p.scalp_radius + 1e-6,
                "根部半径 {r} 超出头皮 {R}",
                R = p.scalp_radius
            );
        }
    }
}
