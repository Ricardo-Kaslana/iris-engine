// Iris 标准材质着色器：Cook-Torrance PBR（金属度/粗糙度工作流）。
// 照明 = 环境光（Lambert 近似）+ 1 个方向光 + 最多 8 个点光（物理平方衰减）。
// 方向光带级联阴影 CSM v2（R29）：3 层 2D 数组深度贴图（R 通道存光空间
// 深度），片元按 view 深度选级联后做 3×3 手工比较 PCF；首盏点光带
// cubemap 阴影贴图（R 通道存归一化线性距离，单采样无 PCF）。
// 渲染在线性 HDR 空间，色调映射由 blit pass 完成。
// 实例化绘制：每个实例携带模型矩阵（4 个 vec4 列）与法线矩阵（3 个 vec4 列）。

struct PointLight {
    // xyz: 世界位置
    position: vec4<f32>,
    // rgb: 颜色 × 强度, w: 有效半径
    color: vec4<f32>,
};

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: 相机世界坐标
    eye_pos: vec4<f32>,
    // xyz: 指向光源的单位方向（表面 -> 光源）
    light_dir: vec4<f32>,
    // rgb * intensity
    light_color: vec4<f32>,
    // rgb（线性，乘在 albedo 上的环境项）
    ambient: vec4<f32>,
    // x: 实际点光数量
    point_count: vec4<f32>,
    point_lights: array<PointLight, 8>,
    // x: 阴影贴图 texel 尺寸(1/2048), y: 深度偏移 bias, z: 阴影强度, w: 是否启用
    shadow_params: vec4<f32>,
    // x: 预滤波环境最大 mip 级, y: IBL 强度
    env_params: vec4<f32>,
    // 天空盒射线重建用（clip -> world）
    inv_view_proj: mat4x4<f32>,
    // x: 是否启用点光阴影, y: bias（米）, z: 投影灯 range, w: 保留
    point_shadow_params: vec4<f32>,
};

// 级联阴影参数（224 字节，POD；独立 uniform 缓冲，不占 Globals）。
// 与 Rust 侧 CsmParamsRaw 逐字段对齐（array<mat4,3> = 192B + 2×vec4）。
struct CsmParams {
    // 每级联的光空间视图投影矩阵（世界 -> 第 i 级光源裁剪空间）
    view_proj: array<mat4x4<f32>, 3>,
    // xyz: 相机前向单位向量（片元级联选择用），w: 保留
    cam_forward: vec4<f32>,
    // x/y/z: 级联分割点（沿相机前向的距离，递增；与 CPU cascade_splits
    // 同源），w: 保留。级联 i 覆盖 [splits[i-1], splits[i]]。
    splits: vec4<f32>,
};

struct MaterialParams {
    base_color: vec4<f32>,
    emissive: vec4<f32>,
    // x: metallic, y: roughness, z: 绝缘体反射率基准 F0，w: 保留
    surface: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
// 级联阴影贴图：R32Float 2D 数组，层序 = 级联序（R 通道存光空间深度，
// 清空值 1.0 = 无遮挡）；采样复用 binding 2 的 NonFiltering 采样器
// （R32Float 双线性过滤需要 float32-filterable 扩展，不依赖）。
@group(0) @binding(1) var shadow_map: texture_2d_array<f32>;
@group(0) @binding(2) var shadow_sampler: sampler;
// IBL 分割求和：辐照度立方体贴图 / 预滤波环境立方体贴图 / BRDF LUT
@group(0) @binding(3) var irradiance_map: texture_cube<f32>;
@group(0) @binding(4) var env_map: texture_cube<f32>;
@group(0) @binding(5) var brdf_lut: texture_2d<f32>;
@group(0) @binding(6) var env_sampler: sampler;
// 点光 cubemap 阴影贴图（R 存归一化线性距离 length(world-light)/range，
// 清空值 1.0 = 无遮挡）。
@group(0) @binding(7) var point_shadow_cube: texture_cube<f32>;
// 级联阴影参数（排在现有最大 binding 7 之后，不扩 Globals）
@group(0) @binding(8) var<uniform> csm: CsmParams;
@group(1) @binding(0) var<uniform> material: MaterialParams;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
// 法线 / ORM 数据贴图（R42，均线性格式）。两槽位由渲染器保证恒有绑定：
// 材质缺省时绑中性常驻纹理（法线 = (0.5,0.5,1) 的 Rgba16Float——8-bit
// unorm 表示不了 0.5（128/255≈0.502，*2−1 残留 0.0039 扰动），16-bit
// float 恰好精确；ORM = (1,1,1)）。采样共用 base_sampler（Filtering）。
// 零分支保证：无这两张贴图的材质与引入本槽位前（R41）逐位一致，
// 论证链见 material.rs 模块文档。
@group(1) @binding(3) var normal_texture: texture_2d<f32>;
@group(1) @binding(4) var orm_texture: texture_2d<f32>;
// 蒙皮：关节矩阵（vs_skinned 专用；vs_main 静态不使用，
// 因此常规管线的 layout 无需 group 2）
@group(2) @binding(0) var<storage, read> joint_mats: array<mat4x4<f32>>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(7) normal0: vec4<f32>,
    @location(8) normal1: vec4<f32>,
    @location(9) normal2: vec4<f32>,
    // 顶点切线（loc12，独立顶点缓冲；网格无切线时渲染器绑定零填充垫，
    // 片元据此走屏幕空间 TBN 路径——xyz 全零即「无顶点切线」标记）
    @location(12) tangent: vec4<f32>,
};

// 阴影投影改在片元做（按 view 深度选级联后乘该级联矩阵），
// 顶点不再输出 shadow_pos（R29 CSM 前为 light_view_proj × world_pos）。
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // 世界空间切线：xyz = 法线矩阵变换后的切线方向（未归一化，片元
    // 正交化），w = 手性符号（±1，插值保号——凸组合不越过 0）
    @location(3) tangent: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * world_pos;
    out.world_pos = world_pos.xyz;
    out.world_normal = normal_mat * in.normal;
    out.uv = in.uv;
    out.tangent = vec4<f32>(normal_mat * in.tangent.xyz, in.tangent.w);
    return out;
}

// 蒙皮顶点输入：在常规属性之外多一路顶点缓冲（loc10 关节索引 / loc11 权重）。
struct SkinnedVertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(7) normal0: vec4<f32>,
    @location(8) normal1: vec4<f32>,
    @location(9) normal2: vec4<f32>,
    @location(12) tangent: vec4<f32>,
    @location(10) joints: vec4<u32>,
    @location(11) weights: vec4<f32>,
};

// 线性混合蒙皮（LBS）：skin = Σ w_i · J_i，再叠加实例模型矩阵。
// 关节矩阵已在 CPU 侧乘过蒙皮实体全局逆（见 skin.rs），
// 因此 skin × position 处于蒙皮实体局部空间，随父级移动整体跟随。
// 法线按刚性变换处理（关节矩阵的 3x3 部分）；蒙皮实体自身的
// 非均匀缩放仍由实例法线矩阵承担。切线同法线一路变换（方向向量）。
@vertex
fn vs_skinned(in: SkinnedVertexInput) -> VertexOutput {
    let j = in.joints;
    let w = in.weights;
    let skin_mat = joint_mats[j.x] * w.x + joint_mats[j.y] * w.y
                 + joint_mats[j.z] * w.z + joint_mats[j.w] * w.w;
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let skinned_pos = model * (skin_mat * vec4<f32>(in.position, 1.0));
    let skin_nrm = mat3x3<f32>(skin_mat[0].xyz, skin_mat[1].xyz, skin_mat[2].xyz);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * skinned_pos;
    out.world_pos = skinned_pos.xyz;
    out.world_normal = normal_mat * (skin_nrm * in.normal);
    out.uv = in.uv;
    out.tangent = vec4<f32>(normal_mat * (skin_nrm * in.tangent.xyz), in.tangent.w);
    return out;
}

// ---- Cook-Torrance 项 ----

// 法线分布：GGX / Trowbridge-Reitz
fn distribution_ggx(noh: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let d = noh * noh * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * d * d + 1e-7);
}

// 几何遮蔽：Schlick-GGX（k = roughness²/2，直接光照近似）
fn geometry_schlick_ggx(nov: f32, roughness: f32) -> f32 {
    let k = (roughness * roughness) * 0.5;
    return nov / (nov * (1.0 - k) + k);
}

fn geometry_smith(nov: f32, nol: f32, roughness: f32) -> f32 {
    return geometry_schlick_ggx(nov, roughness) * geometry_schlick_ggx(nol, roughness);
}

// 菲涅尔：Schlick 近似
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// 环境光专用菲涅尔：边缘反射率随粗糙度抬高（避免粗糙金属掠射角过暗）
fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let fmax = max(vec3<f32>(1.0 - roughness), f0);
    return f0 + (fmax - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// 单灯的 Cook-Torrance 贡献（radiance 已含强度与衰减）
fn pbr_light(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, radiance: vec3<f32>,
             albedo: vec3<f32>, f0: vec3<f32>, metallic: f32, roughness: f32) -> vec3<f32> {
    let h = normalize(v + l);
    let nol = max(dot(n, l), 0.0);
    let nov = max(dot(n, v), 0.0);
    let noh = max(dot(n, h), 0.0);
    let voh = max(dot(v, h), 0.0);

    let d = distribution_ggx(noh, roughness);
    let g = geometry_smith(nov, nol, roughness);
    let f = fresnel_schlick(voh, f0);
    let specular = (d * g * f) / max(4.0 * nov * nol, 1e-4);
    // 能量守恒：漫反射扣除菲涅尔反射占比，金属无漫反射
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    return (kd * albedo / 3.14159265 + specular) * radiance * nol;
}

/// 级联阴影采样（CSM v2）：按片元的 view 深度选级联，用该级联的光空间
/// 矩阵投影后做 3×3 PCF 百分比渐近软阴影（R 通道存光源空间深度，清空值
/// 1.0 代表无遮挡）。返回阴影后保留的光照比例。
///
/// **级联选择用 view 深度**（世界位置到相机的前向投影距离
/// `dot(world_pos − eye, cam_forward)`），而非欧氏距离——欧氏距离在视锥
/// 边缘会把本属近级联的片元误分到远级联；也不直接用 clip z（透视下
/// 非线性，分割点得先在 CPU 做一次非线性换算，view 深度更直白）。
/// 选择条件与 CPU 分割同源：`view_z < splits[i]` 即落入第 i 级。
///
/// **越最远级联（view_z ≥ splits.z，即超过 SHADOW_DISTANCE 覆盖带）的
/// 片元不投影、保持受光**（lit = 1）——级联只铺到覆盖距离。
///
/// textureSample 要求 uniform 控制流，因此级联越界/阴影关闭都不做提前
/// 返回：级联序号钳制在 [0, 2] 后照常采样，最后用掩码把结果混向「受光」。
/// 阴影相机覆盖范围外（NDC/深度越界，含光空间背后）的片段同样混向受光。
fn shadow_factor(world_pos: vec3<f32>) -> f32 {
    let view_z = dot(world_pos - globals.eye_pos.xyz, csm.cam_forward.xyz);
    var cascade = 2u;
    if (view_z < csm.splits.x) {
        cascade = 0u;
    } else if (view_z < csm.splits.y) {
        cascade = 1u;
    }
    let beyond = view_z >= csm.splits.z || globals.shadow_params.w < 0.5;
    let sp = csm.view_proj[cascade] * vec4<f32>(world_pos, 1.0);
    let w = max(sp.w, 1e-6);
    let ndc = sp.xy / w;
    let depth = sp.z / w;
    // NDC -> UV：NDC y 正方向 = 贴图上端（与 blit 的 V 翻转约定一致）；
    // 数组层 = 级联序。PCF texel 尺寸各级相同（各级都铺满同一张
    // 2048² 贴图，UV 空间 1/2048），循环共用。
    let uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    let texel = globals.shadow_params.x;
    var lit = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let d = textureSample(shadow_map, shadow_sampler,
                                  uv + vec2<f32>(f32(dx), f32(dy)) * texel,
                                  i32(cascade)).r;
            // 遮挡物深度 >= 片段参考深度 -> 未被遮挡
            lit = lit + select(0.0, 1.0, d >= depth - globals.shadow_params.y);
        }
    }
    lit = lit / 9.0;
    let shaded = mix(1.0 - globals.shadow_params.z, 1.0, lit);
    let outside = beyond
        || sp.w <= 0.0
        || ndc.x < -1.0 || ndc.x > 1.0
        || ndc.y < -1.0 || ndc.y > 1.0
        || depth < 0.0 || depth > 1.0;
    return mix(shaded, 1.0, select(0.0, 1.0, outside));
}

// ---- 切线空间（R42）----

// 片元 TBN（切线 → 世界）两路选择：
//
// 路 1「顶点切线」：网格带 TANGENT 属性（渲染器上传独立切线缓冲）。
// 世界空间切线经 Gram-Schmidt 对插值法线正交化后归一化；副切线 =
// cross(N, T) × 手性 w（glTF 约定）。精度最高（逐顶点插值、无导数
// 噪声），首选。
//
// 路 2「屏幕空间推导」：网格无切线（顶点切线 xyz 恒零，渲染器垫零
// 缓冲）。用 world_pos / uv 的屏幕空间偏导解 2×2 线性方程组重建
// T / B（「normal mapping without precomputed tangents」标准做法），
// 手性由解的几何关系自然给出。像素级导数在高频几何上有噪声，精度
// 略逊路 1，但免 CPU 预计算与逐网格 16 B/顶点的切线缓冲。
//
// 选择依据 = 顶点切线是否非零（逐片元 select，无管线变体）。两路的
// TBN 第三列恒取**未归一化**的插值世界法线 in.world_normal：中性法线
// 贴图采样 (0,0,1) 时 `T·0 + B·0 + N·1` 逐位等于该列（x·0 = ±0，
// ±0 + x = x），normalize 的输入与 R41 完全同串位——这是「无纹理
// 逐位一致」的关键，见 material.rs 模块文档。
fn perturbed_normal(in: VertexOutput, n_geo: vec3<f32>) -> vec3<f32> {
    // 路 1：顶点切线
    let t1_raw = in.tangent.xyz - n_geo * dot(n_geo, in.tangent.xyz);
    let t1 = t1_raw / max(length(t1_raw), 1e-8);
    let b1 = cross(n_geo, t1) * sign(in.tangent.w);
    // 路 2：屏幕空间导数
    let dp1 = dpdx(in.world_pos);
    let dp2 = dpdy(in.world_pos);
    let duv1 = dpdx(in.uv);
    let duv2 = dpdy(in.uv);
    let p2perp = cross(dp2, n_geo);
    let p1perp = cross(n_geo, dp1);
    let t2 = p2perp * duv1.x + p1perp * duv2.x;
    let b2 = p2perp * duv1.y + p1perp * duv2.y;
    // 尺度不变归一化；uv 退化（det ≈ 0）时 t2/b2 = 0，1e-12 下限防
    // 除零产生 Inf·0 = NaN
    let invmax = 1.0 / sqrt(max(max(dot(t2, t2), dot(b2, b2)), 1e-12));
    let t2n = t2 * invmax;
    let b2n = b2 * invmax;

    let has_vertex_tangent = dot(in.tangent.xyz, in.tangent.xyz) > 0.0;
    let t = select(t2n, t1, has_vertex_tangent);
    let b = select(b2n, b1, has_vertex_tangent);
    // 切线空间法线解码（*2−1）+ TBN 世界变换
    let ts = textureSample(normal_texture, base_sampler, in.uv).xyz * 2.0 - 1.0;
    return t * ts.x + b * ts.y + n_geo * ts.z;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = material.base_color * textureSample(base_texture, base_sampler, in.uv);
    let albedo = base.rgb;
    // ORM 打包（线性采样）：R = AO、G = 粗糙度、B = 金属度；标量因子
    // 作倍率相乘（glTF factor × texel 语义）。中性 ORM = (1,1,1) 时
    // x·1.0 = x 逐位精确，退化为 R41 的标量路径。
    let orm = textureSample(orm_texture, base_sampler, in.uv);
    let metallic = clamp(material.surface.x * orm.b, 0.0, 1.0);
    let roughness = clamp(material.surface.y * orm.g, 0.045, 1.0);
    let ao = orm.r;
    let f0 = mix(vec3<f32>(material.surface.z), albedo, metallic);

    // 光照法线：几何法线 + 法线贴图扰动。n_geo 未归一化（见
    // perturbed_normal 的逐位一致说明），归一化只在此处做一次，
    // 与 R41 的 normalize(in.world_normal) 同构。
    let n = normalize(perturbed_normal(in, in.world_normal));
    let v = normalize(globals.eye_pos.xyz - in.world_pos);

    // 方向光（受级联阴影遮挡）
    let l = normalize(globals.light_dir.xyz);
    let sun = shadow_factor(in.world_pos);
    let radiance = globals.light_color.rgb * sun;
    var color = pbr_light(n, v, l, radiance, albedo, f0, metallic, roughness);

    // 点光：物理平方衰减 + range 平滑窗口；首灯（i==0）受 cubemap 阴影调制
    let count = u32(globals.point_count.x);
    for (var i = 0u; i < count; i = i + 1u) {
        let light = globals.point_lights[i];
        let to_light = light.position.xyz - in.world_pos;
        let dist = length(to_light);

        // 点光阴影系数：只有查询顺序中的第 0 盏点光投射 cubemap 阴影（v1 限定）。
        // 采样在 `dist < range` 分支之外无条件执行（textureSample 系要求均匀
        // 控制流的掩码模式，与 shadow_factor 一致）；i==0 与开关掩码都由
        // uniform 派生，分支本身均匀。textureSampleLevel 带显式 mip 不依赖导数。
        // 采样方向 = 从灯指向片段的归一化方向——Cube 按此方向查 texel，
        // 与捕获面矩阵的方向约定（POINT_SHADOW_FACES + NDC y 翻转）配对。
        // 超出 range 的片段不会进入受光分支（掩码自然混向受光），贴图清空值
        // 1.0（= range/range）代表无遮挡。
        var shadow = 1.0;
        if (i == 0u && globals.point_shadow_params.x > 0.5) {
            let dir = (in.world_pos - light.position.xyz) / max(dist, 1e-4);
            let stored = textureSampleLevel(point_shadow_cube, shadow_sampler, dir, 0.0).r;
            let range = max(globals.point_shadow_params.z, 1e-4);
            // bias 以米存储，除以 range 归一化到距离值域后再比较
            let biased = dist / range - globals.point_shadow_params.y / range;
            shadow = select(0.0, 1.0, stored >= biased);
        }

        if (dist < light.color.w) {
            let lp = to_light / max(dist, 1e-4);
            let t = dist / light.color.w;
            let window = 1.0 - t * t;
            let atten = (window * window) / max(dist * dist, 0.01);
            let pl = light.color.rgb * atten * shadow;
            color = color + pbr_light(n, v, lp, pl, albedo, f0, metallic, roughness);
        }
    }

    // 环境光（IBL 分割求和）：漫反射采样辐照度贴图；镜面按粗糙度取预滤波
    // 环境 mip，乘 BRDF LUT 的 (缩放, 偏移)。intensity 可整帧关闭 IBL。
    // AO（orm.r）乘环境项：IBL 与美术补光都被遮蔽削弱，直接光不乘
    // （物理上 AO 描述小尺度互遮蔽的环境光损失）。中性 ORM 时 ao = 1.0，
    // x·1.0 = x 逐位精确 = R41 行为。
    let irradiance = textureSampleLevel(irradiance_map, env_sampler, n, 0.0).rgb;
    let nov = max(dot(n, v), 0.0);
    let f_amb = fresnel_schlick_roughness(nov, f0, roughness);
    let kd_amb = (vec3<f32>(1.0) - f_amb) * (1.0 - metallic);
    let env_spec = textureSampleLevel(env_map, env_sampler, reflect(-v, n),
                                      roughness * globals.env_params.x).rgb;
    let brdf = textureSample(brdf_lut, env_sampler, vec2<f32>(nov, roughness)).rg;
    let ibl = (kd_amb * albedo * irradiance + env_spec * (f_amb * brdf.x + brdf.y))
            * globals.env_params.y * ao;
    // 美术补光：方向光资源上的常量环境项（与 IBL 叠加）
    let f_env = mix(vec3<f32>(material.surface.z), albedo, 0.5);
    color = color + ibl
          + mix(albedo, f_env, metallic) * globals.ambient.rgb * ao
          + material.emissive.rgb;

    return vec4<f32>(color, base.a);
}
