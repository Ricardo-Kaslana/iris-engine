//! [`ExpPlugin`]：实验项目的全部内容——场景装配（地面 / 方向光 /
//! 轮廓与补光 / 相机）+ 外部 GLB 角色实例化 + 材质升级覆写 + 动作切换
//! 接线 + 轨道相机与角色移动控制。
//!
//! 与完整游戏（iris-assault 等）刻意保持最小：无逻辑状态机、无物理、
//! 无音频内容、无 UI——只保留回答「渲染 / 动画 / 切换 / 控制」四问所需
//! 的最低装配。`build_schedule` 独立暴露供 headless 测试与离屏快照复用
//! （动画推进由 `AnimationPlugin` 负责，headless 路径需自行注册——与
//! 窗口路径 `DefaultPlugins` 同一编排）。
//!
//! # 控制（frictions ⑥：自由视角 / 缩放 / 角色移动）
//!
//! 纯游戏层装配（引擎无改动）：[`OrbitCamera`] 资源记录轨道状态
//! （yaw / pitch / dist / focus_h），右键拖拽环绕 + 滚轮缩放 + Q/E 调
//! 注视点高度；WASD 相对相机移动 + 朝向平滑转身。两条硬语义：
//!
//! - **无输入帧恒等**：鼠标与 WASD 全静默的帧不写相机与角色
//!   `Transform`——headless / 离屏路径（无输入注入）与窗口静态帧的
//!   逐位行为与 R66b 之前一致，外部机位覆盖（如快照 example 的
//!   `EXP_CAM_HEAD` 诊断特写）不会被运行期系统改写。
//! - **常态 Idle + 动作随移动切换（crossfade 过渡 + 速度平滑）**：三状
//!   态机驱动（Idle 与 Walk 是循环常态，Jump 是空格单次），全部切换带
//!   过渡混合而非硬切。`Mixamo_Idle` 是常态——停止且无进行中的单次
//!   动作时循环播放；移动即以 0.15 s crossfade 淡入步行循环；`空格`
//!   跳跃单次（0.08 s 淡入），播完由引擎条件转移（`ClipFinished` 守卫
//!   读 `moving` 参数）自动回落——静止回 Idle、移动中直落 Walk（落地
//!   即走），回落带 0.10 s 淡出。
//! - **停步与起步的运动感**：停步不立即切换——等步行剪辑走到「姿势距
//!   Idle 最近」的过步窗口（实测相位区段表，至多约半步 0.45 s）才以
//!   0.10 s 淡出，等待期速度保持全速走完最后一步；起步速度按 6/s 指数
//!   加速（约 0.5 s 到 95% 全速），停步放行后按 20/s 指数减速，残差低
//!   于 0.05 m/s 精确归零。速度非零的帧照常积分位置、相机照常跟随；
//!   **完全静止后的无输入帧不写相机与角色 `Transform`**（恒等语义的
//!   精确口径）。动画推进、过渡计时与速度趋近全部是确定性的 dt-based
//!   计算，双跑逐位一致不受影响。
//!
//! # 角色材质升级与布光（渲染质量）
//!
//! GLB 导入路径按契约把皮肤次表面散射留关（glTF 规范没有这种参数，
//! 需要方在导入后覆写），本插件装配后按**节点名**覆写：皮肤节点（含
//! 身体与脸部两个 prim）开预积分次表面散射，头发与睫毛节点抬到哑光
//! 粗糙度，眼球节点材质的粗糙度从全镜面 0 抬到角膜量级（路由与参数
//! 见 [`material_upgrade_for_node`] 与 [`apply_material_upgrade`]）。
//! 布光在单方向光之外加一盏冷色轮廓锥光与一盏暖色补光（三点布光的
//! 背光与填充位，均不投影），用于近景角色的轮廓分离与暗部层次。
//!
//! **头发与睫毛不挂 Kajiya-Kay（实测证据链）**：R90 的固定高光色
//! `mix(白, albedo, 0.5)` 按中等深度发色的假设设计，本资产发色的
//! albedo 约 0.06，高光色被钳在约 0.53 的灰白，直射高光在黑发上读作
//! 漂白。引擎 R96 补 `highlight_tint` 染色乘子（缺省与固定式逐位一致）
//! 之后，染色 0.12 档在 720p 下仍余可见的蓝灰渐变；再叠加天空 IBL 镜面
//! （间接项各向同性且不受染色控制），窗口实机中的头发读作未修复。头发
//! 贴图本身接近纯黑：任何可见的定向高光都读作灰白，压到不可见则等于
//! 没有高光。因此本资产的终点是哑光黑（roughness 0.85、不挂 KK），染色
//! 旋钮留给浅色毛发或彩色毛发的资产使用。

use iris_core::prelude::{Entity, Stage};
use iris_engine::prelude::{
    Access, ActiveCamera, AnimGuard, AnimTransition, AnimationClip, AnimationEdge, AnimationPlayer,
    AnimationState, AoSettings, App, Camera, DirectionalLight, GltfInstance, Input, KeyCode,
    MaterialDef, MaterialHandle, MaterialStore, MeshData, MeshStore, MipSettings,
    MotionBlurSettings, MouseButton, Name, OffscreenRenderer, Plugin, PointLight, Quat, Resource,
    Schedule, SpotLight, TaaSettings, TextureStore, Time, Transform, Vec3, VignetteSettings,
    Visibility, World, asset_path, spawn_gltf_instance,
};

/// 角色 GLB 路径（`asset_path` 解析：CWD 相对优先，workspace 根相对回退）。
/// 本体 32MB 版权模型不入库（`assets/exp_weiqing.glb` 已在 .gitignore），
/// 缺席时插件降级为无角色模式（场景照常，诚实告警）。
pub const MODEL_PATH: &str = "assets/exp_weiqing.glb";
/// 步行循环剪辑名（GLB 剪辑名即契约）。
pub const WALK_CLIP: &str = "Mixamo_Walk";
/// 跳跃单次剪辑名（播完自动回落 Idle）。
pub const JUMP_CLIP: &str = "Mixamo_Jump";
/// 待机循环剪辑名——**常态动作**：无移动、无进行中单次动作时播放。
pub const IDLE_CLIP: &str = "Mixamo_Idle";

/// 轨道相机注视点**初始**高度（世界坐标 y 偏移；角色约 4 m 高，取胸腹
/// 位）。运行期由 Q/E 连续调节（[`OrbitCamera::focus_h`]，区间
/// [FOCUS_H_MIN, FOCUS_H_MAX]）。
pub const TARGET_HEIGHT: f32 = 1.5;
/// 注视点高度下限（m）：贴地（脚部特写）。
pub const FOCUS_H_MIN: f32 = 0.0;
/// 注视点高度上限（m）：角色约 4 m 高，头顶之上留余量。
pub const FOCUS_H_MAX: f32 = 4.5;
/// 注视点高度调节速率（m/s，Q/E 按住线性升降）。
pub const FOCUS_H_RATE: f32 = 2.5;
/// 拖拽环绕灵敏度（rad / 像素；≈0.2°/px，800 px 拖满 ≈160°）。
pub const ORBIT_SENS: f32 = 0.0035;
/// 滚轮缩放系数（`dist ×= exp(−wheel·k)`；窗口路径行模式一格 = ±40，
/// 每格缩放 ≈12.7%）。
pub const WHEEL_SENS: f32 = 0.003;
/// 俯角下限（rad）：略低于水平，可仰拍；再低会钻地。
pub const PITCH_MIN: f32 = -5.0_f32.to_radians();
/// 俯角上限（rad）：接近顶视但不过顶。
pub const PITCH_MAX: f32 = 85.0_f32.to_radians();
/// 轨道距离下限（m）：贴近面部特写。下限取相机近裁剪面（`z_near=0.1`）
/// 之上留裕量——再近角色表面会被近裁剪面切穿。
pub const DIST_MIN: f32 = 0.3;
/// 轨道距离上限（m）：拉远看全景（视觉地面 240 m 尺度之上再留余量）。
pub const DIST_MAX: f32 = 60.0;
/// 角色移动速度（m/s）。**步幅匹配值**：Walk 剪辑 58 键 @60fps =
/// 0.967 s/循环，常速步行步幅 ≈1.8 m → 1.8 m/s 起步脚不打滑。
/// （「走路过快」排查结论：**非帧率问题**——引擎动画推进 dt-based
/// [`AnimationPlayer::advanced`] 读 `Time::delta_secs()`，帧率无关；
/// GLB 剪辑时长口径正确。旧值 3.0 m/s 是慢跑速度，与步行动画的
/// 步频不匹配 → 位移快于步幅、视觉打滑。）
pub const MOVE_SPEED: f32 = 1.8;
/// 朝向转身速率（1/s；帧率无关指数趋近，见 [`exp_approach`]）。
pub const TURN_RATE: f32 = 12.0;
/// 角色活动范围半宽（m）：**玩法边界**（与视觉地面尺寸解耦），在边界
/// 内任意位置身后都有 ≥100 m 的真实地面。
pub const GROUND_HALF: f32 = 11.0;
/// 视觉地面半宽（m）：24→240 的原因——24 m 地面的远缘只离角色 12 m，
/// 天空盒「地平线以下淡出」的白蓝渐变（程序化天空在地平线处最亮、
/// 下方缓变）从远缘一直铺到真实地平线，把 12–19 m 的**中距离**染成
/// 一条 20% 画高的白纱（逐像素实测：远缘以上 (207,211,217)，是读作
/// 雾的天空渐变，不是地面上的雾——地面远缘本身是硬边）。扩到 ±120 m
/// 后真实地面填满地平线以下区域，默认机位下地-天边缘升到距眼角
/// ≈1.3°（拉远到 DIST_MAX 也在画外），白纱压缩成一条细线。只改视觉
/// 网格，玩法钳制（[`GROUND_HALF`]）不变。
pub const GROUND_VISUAL_HALF: f32 = 120.0;

// ---------------------------------------------------------------------------
// 动作状态机（crossfade 过渡）
// ---------------------------------------------------------------------------

/// 动作状态下标（`AnimationPlayer::states` 表）：Idle 常态、Walk 移动、
/// Jump 空格单次。iris-ascent 同款 usize 常量口径。
pub const STATE_IDLE: usize = 0;
/// 见 [`STATE_IDLE`]。
pub const STATE_WALK: usize = 1;
/// 见 [`STATE_IDLE`]。
pub const STATE_JUMP: usize = 2;
/// Idle 与 Walk 双向切换的 crossfade 时长（秒）。0.15 s 是 iris-ascent 的
/// 实测观感值：明显短于 0.1 s 读作姿势跳变，长于 0.3 s 起走拖沓。
pub const STATE_BLEND: f32 = 0.15;
/// 进入跳跃的 crossfade 时长（秒）。起势要快：更长的过渡会把蓄力帧糊成
/// 发软的下蹲，0.08 s 只抹掉姿势突变本身。
pub const JUMP_IN_BLEND: f32 = 0.08;
/// 跳跃播完回落（Idle 或 Walk）的 crossfade 时长（秒）。比步态切换略长
/// 以吸收落地缓冲的姿势差，但不长到吞掉落地反馈。
pub const JUMP_OUT_BLEND: f32 = 0.10;
/// 跳跃落地分流参数名：`update_exp_control` 每帧写布尔值，Jump 状态的
/// `ClipFinished` 条件转移读它，决定落地直落 Walk 还是回 Idle。
pub const MOVING_PARAM: &str = "moving";
/// Walk 切回 Idle 的 crossfade 时长（秒）。停步方向比起步短：同样的
/// 姿势差用更短的窗口覆盖，读作「收住脚步」而非「形体渐变」。
pub const STOP_BLEND: f32 = 0.10;
/// 起步速度趋近速率（1/s）：0 到 95% 全速约 0.5 s——站立帧全速位移会
/// 读作脚底打滑，真人起步也要踩出半步才到全速。
pub const ACCEL_RATE: f32 = 6.0;
/// 停步速度趋近速率（1/s）：与 [`STOP_BLEND`] 同量级——相位门放行后
/// 位移随姿势一起收住，过渡末段速度残留约 0.24 m/s，其后总滑行量
/// 约十厘米内不可感知。
pub const DECEL_RATE: f32 = 20.0;
/// 速度归零阈值（m/s）：停步减速残差低于此值精确置零——其后无输入帧
/// 恢复「不写角色与相机 Transform」的逐位恒等语义。
pub const STOP_EPS: f32 = 0.05;
/// 停步相位窗口（归一化相位区段，实测依据见 [`stop_phase_ready`]）。
pub const STOP_PHASE_WINDOWS: &[(f32, f32)] = &[(0.93, 1.0), (0.0, 0.10), (0.57, 0.70)];

// ---------------------------------------------------------------------------
// 角色材质升级（GLB 导入后按节点名覆写；引擎 SSS 通道按导入契约恒关
// ——glTF 规范无此参数，由游戏层显式打开）
// ---------------------------------------------------------------------------

/// 皮肤节点名（GLB 节点名即契约，与 `Mixamo_Walk` 剪辑名同款口径）：
/// 该节点下挂身体与脸部两个 prim 的材质，均开预积分 SSS。
pub const SKIN_NODE: &str = "body.001";
/// 头发节点名：材质抬到哑光粗糙度（见 [`HAIR_ROUGHNESS`]），并且不挂
/// Kajiya-Kay。近黑发色上任何可见的定向高光都读作灰白，完整证据链见
/// 模块文档与 frictions ⑳。
pub const HAIR_NODE: &str = "mesh_56";
/// 睫毛节点名：同 [`HAIR_ROUGHNESS`] 哑光档。
pub const LASH_NODE: &str = "mesh_57";
/// 眼球节点名：材质无 ORM 图、粗糙度因子 0（全镜面），覆写为角膜量级。
pub const EYE_NODE: &str = "mesh_30";
/// 皮肤 SSS 强度（0..1）：0.55 取「终结线软坡明显、不到蜡像」的中强档
/// （引擎调参表的默认观感档是 strength 由调用方定、scale 定包裹角）。
pub const SKIN_SSS_STRENGTH: f32 = 0.55;
/// 皮肤 SSS 曲率尺度半径（米，越小包裹越强）：引擎调参表按真人尺度给
/// 头部 0.1 m；本 GLB 角色约 4 m 高（约 2.2 倍），头/前臂等比放大取
/// 0.22 m 量级（包裹参数 t ≈ 0.23，介于调参表「躯干」与「头部」档
/// 之间——身体与脸部两 prim 共用一档的折中，脸部略欠包裹是有意取舍）。
pub const SKIN_SSS_SCALE: f32 = 0.22;
/// 眼球材质覆写后的粗糙度：GLB 原值 0（无 ORM 图、因子 0）只采样预
/// 滤理环境图 mip0，高光锐得失真；0.12 是「角膜湿润反光」量级。
pub const EYE_ROUGHNESS: f32 = 0.12;
/// 头发与睫毛覆写后的粗糙度（GLB 原值 0.5）：哑光档同时压制两路高光，
/// 一路是天空 IBL 镜面（间接项；引擎诚实边界为各向同性且不受染色控制），
/// 另一路是直射 GGX 镜面。头发贴图本身接近纯黑，哑光黑是本资产的终点。
pub const HAIR_ROUGHNESS: f32 = 0.85;

/// 轨道相机状态（游戏层资源）：yaw = 环绕方位角（0 = 相机在 +Z 侧）、
/// pitch = 俯角（0 = 水平，正值 = 相机升高俯视）、dist = 轨道距离、
/// focus_h = 注视点在角色上方的高度（Q/E 调节）。位姿几何见
/// [`orbit_pose`]。资源由插件 Startup 兜底插入；缺席时轨道系统空转
/// （相机静止——与引擎「资源缺席 = 关」语义同款）。
#[derive(Clone, Copy, Debug, Resource)]
pub struct OrbitCamera {
    /// 环绕方位角（rad）。
    pub yaw: f32,
    /// 俯角（rad）。
    pub pitch: f32,
    /// 轨道距离（m）。
    pub dist: f32,
    /// 注视点高度（m，角色本地 y 偏移；Q/E 调节，区间
    /// [FOCUS_H_MIN, FOCUS_H_MAX]）。
    pub focus_h: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        // 初始机位：正面平视稍俯（相机在 +Z 侧，GLB 角色面向 +Z——
        // 默认看到正脸），与 R66b 之前的观感同构（同俯角、同侧）。
        Self {
            yaw: 0.0,
            pitch: 9.0_f32.to_radians(),
            dist: 7.5,
            focus_h: TARGET_HEIGHT,
        }
    }
}

/// 角色控制跨帧状态：当前朝向角（rad，[`facing_yaw`] 口径）与水平速度。
/// 单独成资源而非写回 `ExpEntities`（那是实体表不是状态袋）；旋转
/// 始终写作 `from_rotation_y(yaw)`，无其他旋转分量，避免从四元数
/// 反提取的万向锁与浮点往返误差。
#[derive(Clone, Copy, Debug, Resource)]
pub struct ExpCharacter {
    /// 当前朝向角（rad）。
    pub yaw: f32,
    /// 水平速度（m/s，世界系）。起步按 [`ACCEL_RATE`] 指数加速，停步在
    /// 相位门放行后按 [`DECEL_RATE`] 指数减速，残差低于 [`STOP_EPS`]
    /// 精确归零——其后无输入帧恢复逐位恒等；相位门等待期保持当前值
    /// （走完最后一步，脚步与位移匹配）。
    pub velocity: Vec3,
}

impl Default for ExpCharacter {
    fn default() -> Self {
        // GLB 角色面向本地 +Z（EXP_CAM_HEAD 诊断注释与旧快照正脸为证；
        // build_scene 旧注释「面向 -Z」系笔误陈留），rotation 恒等时正脸
        // 朝 +Z（初始相机侧）。
        Self {
            yaw: 0.0,
            velocity: Vec3::ZERO,
        }
    }
}

/// 场景实体表：相机 + 角色模型根（加载失败为 None）+ 三个剪辑的索引。
#[derive(Clone, Copy, Resource)]
pub struct ExpEntities {
    /// 轨道相机实体（`update_orbit_camera` 的写目标）。
    pub camera: Entity,
    /// 角色模型根实体（GLB 收编后的顶层；`None` = 资产缺席降级）。
    pub model: Option<Entity>,
    /// 步行循环剪辑下标。
    pub walk: usize,
    /// 跳跃单次剪辑下标。
    pub jump: usize,
    /// 待机循环剪辑下标（常态；GLB 缺 Idle 时回退指 walk——
    /// 行为退化为「停止 = 步行剪辑定格」的旧语义，不 panic）。
    pub idle: usize,
}

/// 角色材质升级种类：按 GLB 节点名路由的四种覆写（纯数据，供单测与
/// headless 断言直接消费）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialUpgrade {
    /// 皮肤（含脸部）：开预积分次表面散射。
    Skin,
    /// 头发：抬到哑光粗糙度（不挂 Kajiya-Kay，理由见模块文档）。
    Hair,
    /// 睫毛：同哑光档。
    Lash,
    /// 眼球：粗糙度抬到角膜量级。
    Eye,
}

/// 节点名 → 材质升级种类（纯函数）。GLB 节点名即契约：换版资产改动了
/// 这些节点名时路由落空、材质停在导入默认（渲染照常，只是升级缺席），
/// headless 契约测试会先一步失败——与剪辑名（`Mixamo_Walk` 等）同款
/// 失效模式。
pub fn material_upgrade_for_node(node_name: &str) -> Option<MaterialUpgrade> {
    match node_name {
        SKIN_NODE => Some(MaterialUpgrade::Skin),
        HAIR_NODE => Some(MaterialUpgrade::Hair),
        LASH_NODE => Some(MaterialUpgrade::Lash),
        EYE_NODE => Some(MaterialUpgrade::Eye),
        _ => None,
    }
}

/// 应用材质升级（纯函数：直接改传入的 [`MaterialDef`]，可脱离 World 单
/// 测）。只写目标字段，其余保持导入原值——布料的 ORM、皮肤的金属度、
/// 各材质的透明模式与双面声明都不受影响。
pub fn apply_material_upgrade(def: &mut MaterialDef, upgrade: MaterialUpgrade) {
    match upgrade {
        MaterialUpgrade::Skin => {
            def.subsurface_strength = SKIN_SSS_STRENGTH;
            def.subsurface_scale = SKIN_SSS_SCALE;
        }
        MaterialUpgrade::Hair => {
            // 纯哑光：粗糙度从 GLB 原值 0.5 抬到 0.85。近黑发色（albedo
            // 约 0.06）上任何可见的定向高光（KK 瓣染色已试到 0.12，以及
            // 天空 IBL 镜面）在窗口分辨率下都读作灰白高光；本资产头发
            // 贴图本身接近纯黑，哑光是唯一自洽档。KK 不挂，完整证据链
            // 见模块文档与 frictions ⑳；引擎 highlight_tint 旋钮为浅色
            // 毛发资产保留。
            def.roughness = HAIR_ROUGHNESS;
        }
        MaterialUpgrade::Lash => {
            def.roughness = HAIR_ROUGHNESS;
        }
        MaterialUpgrade::Eye => {
            def.roughness = EYE_ROUGHNESS;
        }
    }
}

/// 走访 GLB 实例的全部节点，对命中路由表的节点覆写其 prim 材质。材质
/// 句柄挂在节点实体的 **part 子实体**上（加载器给每个 primitive 一个
/// 子实体），故路径是：节点名路由 → 节点实体 → `children` 取 part →
/// `MaterialHandle` 组件 → `MaterialStore::get_mut` 覆写。返回覆写数
/// （预期 5：皮肤双 prim + 头发 + 睫毛 + 眼球各一），装配日志据此对账。
fn upgrade_character_materials(world: &mut World, instance: &GltfInstance) -> usize {
    let mut upgraded = 0usize;
    for (name, entity) in instance
        .node_names
        .iter()
        .zip(instance.node_entities.iter())
    {
        let Some(upgrade) = name.as_deref().and_then(material_upgrade_for_node) else {
            continue;
        };
        let Some(&entity) = entity.as_ref() else {
            continue;
        };
        for part in world.children(entity) {
            let Some(handle) = world.get::<MaterialHandle>(part) else {
                continue;
            };
            let handle = *handle;
            let mut store = world.resource_mut::<MaterialStore>();
            let Some(def) = store.get_mut(handle) else {
                continue;
            };
            apply_material_upgrade(def, upgrade);
            upgraded += 1;
        }
    }
    upgraded
}

/// 实验项目插件：Startup 装配场景与角色，Update 接线动作切换、角色
/// 移动与轨道相机。
pub struct ExpPlugin;

impl Default for ExpPlugin {
    fn default() -> Self {
        Self
    }
}

impl ExpPlugin {
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露供 headless 测试与离屏快照复用。**动画与层级不在此注册**：
    /// 调用方按窗口路径的编排补上 `AnimationPlugin`（Update）与
    /// `HierarchyPlugin`（PostUpdate）。调度器不去重——任何一方重复注册
    /// 都会让 `update_animations` 每帧执行两遍、动画双倍速。
    ///
    /// Update 系统注册序即执行序（同组件 `write` 冲突由调度器串行）：
    /// `update_exp_control`（角色移动 + 动画）在前、`update_orbit_camera`
    /// 在后——相机同帧读到角色新位置，移动时注视点零滞后跟随。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        schedule
            .add_system(Stage::Startup, setup_resources)
            .add_system(Stage::Startup, build_scene)
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<ExpEntities>()
                    .read::<OrbitCamera>()
                    .write::<ExpCharacter>()
                    .write::<Transform>()
                    .write::<AnimationPlayer>(),
                update_exp_control,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<ExpEntities>()
                    .write::<OrbitCamera>()
                    .write::<Transform>(),
                update_orbit_camera,
            );
    }
}

impl Plugin for ExpPlugin {
    fn build(&self, app: &mut App) {
        // 只注册自身系统：动画推进与层级传播由调用方编排——窗口路径的
        // `DefaultPlugins` 已含 `AnimationPlugin` 与 `HierarchyPlugin`，
        // headless / 离屏路径自行注册（iris-arena 同款约定）。调度器
        // 不去重，此处再 build 一次会让 `update_animations` 每帧执行
        // 两遍、动画以双倍速播放——本插件曾因此在窗口路径双倍速，已修，
        // 回归锚见 tests/headless.rs 的 exp_plugin_registers_animation_advance_once。
        self.build_schedule(&mut app.schedule);
    }
}

/// 按名查剪辑下标（GLB 剪辑名即契约；缺席告警）。
fn clip_index(clips: &[AnimationClip], name: &str) -> Option<usize> {
    let found = clips.iter().position(|clip| clip.name == name);
    if found.is_none() {
        log::warn!(
            "ExpPlugin：GLB 缺少剪辑 {name}（现有 {:?}）",
            clips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
    }
    found
}

/// Startup 1：资源兜底插入（ResourcePlugin 语义——窗口程序里
/// DefaultPlugins 已就位，不得覆盖；headless / 离屏路径由这里补齐）。
fn setup_resources(world: &mut World) {
    if !world.contains_resource::<MeshStore>() {
        world.insert_resource(MeshStore::new());
    }
    if !world.contains_resource::<MaterialStore>() {
        world.insert_resource(MaterialStore::new());
    }
    if !world.contains_resource::<TextureStore>() {
        world.insert_resource(TextureStore::new());
    }
    // mip 链 + 各向异性过滤（R66）：外部 GLB 的发片（黑底透明 + 彩色
    // 发丝）远距缩小采样必须走 mip，否则高频 alpha 逐像素翻噪点
    //（frictions ①）。headless 路径无渲染器，资源在场无害。
    if !world.contains_resource::<MipSettings>() {
        world.insert_resource(MipSettings { anisotropy: 8 });
    }
    // ---- 引擎渲染能力装配（后处理链 opt-in 资源，全部「缺席 = 关」，
    // 游戏层显式开启；headless 路径无渲染器，资源在场无害）----
    // GTAO（R77）：角色-地面接触、衣褶锁骨缝隙的环境光遮蔽。radius 0.5
    // 对 4 m 高角色的手指级缝隙合适（世界半径口径）。
    if !world.contains_resource::<AoSettings>() {
        world.insert_resource(AoSettings {
            enabled: true,
            intensity: 1.0,
            radius: 0.5,
        });
    }
    // TAA：发片/睫毛高频 alpha 边缘与地面网格线的时域抗锯齿。
    if !world.contains_resource::<TaaSettings>() {
        world.insert_resource(TaaSettings { enabled: true });
    }
    // 暗角：轻微电影感（0.35 = 不抢戏的边角压暗）。
    if !world.contains_resource::<VignetteSettings>() {
        world.insert_resource(VignetteSettings { strength: 0.35 });
    }
    // 运动模糊：装配就位但暂关（enabled = false，缺席语义逐位一致）。
    // 快照实测：跳跃腾空段肢体帧速度大，strength 32/16 档拖影都长到读作
    // 残影糊块，反损验收图判读；窗口游玩想开拖影把 enabled 翻真值即可。
    if !world.contains_resource::<MotionBlurSettings>() {
        world.insert_resource(MotionBlurSettings {
            enabled: false,
            strength: 16.0,
            ..Default::default()
        });
    }
    // 泛光 BloomSettings 未注册即默认启用（0.15），不动；SSR 不开
    //（场景无反射面，地面哑光）、遮挡剔除不开（24 m 小场景无收益）——
    // 诚实理由见模块文档。
    if !world.contains_resource::<Camera>() {
        world.insert_resource(Camera::default());
    }
    if !world.contains_resource::<DirectionalLight>() {
        world.insert_resource(DirectionalLight::default());
    }
    if !world.contains_resource::<OrbitCamera>() {
        world.insert_resource(OrbitCamera::default());
    }
    if !world.contains_resource::<ExpCharacter>() {
        world.insert_resource(ExpCharacter::default());
    }
}

/// Startup 2：场景（地面 / 方向光 / 相机）+ 角色 GLB 装配。
///
/// 资产缺席不 panic：降级为无角色模式（场景照常 + 诚实告警）——本项目
/// 的资产是 32MB 版权模型，不在开源仓库内。相机位姿由 [`orbit_pose`]
/// 构造（初始参数 [`OrbitCamera::default`]），保证窗口首帧与轨道系统的
/// 状态口径一致（拖拽不跳变）。
fn build_scene(world: &mut World) {
    // ---- 地面：大扁立方体（哑光灰，衬托角色）----
    // 视觉半宽 GROUND_VISUAL_HALF（中距离白纱修复，见常量文档）；玩法
    // 活动范围仍由 GROUND_HALF 钳制，两者解耦。
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.32, 0.34, 0.36, 1.0));
    world.spawn((
        Name("exp/ground".to_string()),
        Transform {
            position: Vec3::new(0.0, -0.05, 0.0),
            scale: Vec3::new(GROUND_VISUAL_HALF * 2.0, 0.1, GROUND_VISUAL_HALF * 2.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        ground_mat,
        Visibility::VISIBLE,
    ));

    // ---- 方向光：右上前方斜射，开阴影 ----
    // R66 调参（frictions ②）：intensity 2.6 + ambient 0.22 观感整体偏白
    //（白色服装大面积过曝、贴图细节被冲掉），按离屏快照逐轮目检调低至
    // 白色布料恢复层次。离屏与窗口共用 ACES 链，观感可直接对照。
    if let Some(mut light) = world.try_resource_mut::<DirectionalLight>() {
        light.direction = Vec3::new(-0.45, -1.0, -0.35).normalize_or_zero();
        light.intensity = 1.4;
        light.ambient = Vec3::new(0.11, 0.12, 0.14);
        light.shadows = true;
    }

    // ---- 轮廓光（三点布光的背光位）：冷色锥光从角色背后上方斜照 ----
    // 角色面向 +Z、初始相机在 +Z 侧，背光在 -Z 高处把肩、发、衣缘从灰
    // 背景里剥出（立体分离）。方向取指向角色胸腹 (0, 2.2, 0) 的向量；
    // inner 24° 在 4.4 m 距离全覆盖 4 m 身高、outer 46° 软化边缘溢出。
    // 强度 8：锥光到头部约 4.4 m，平方衰减 (1−d/range)² ≈ 0.6 后有效
    // 强度约 4.8——与主光 1.4 拉开约两档才是可读的轮廓分离（初版 4.0
    // 在快照中不可辨，肩臂边缘没有冷亮边）。SpotLight v1 诚实限制：
    // direction 是世界空间固定指向、不投影、不进探针捕获——本场景固定
    // 机位量级，无碍。
    world.spawn((
        Name("exp/rim-light".to_string()),
        Transform {
            position: Vec3::new(0.0, 5.2, -4.2),
            ..Transform::IDENTITY
        },
        SpotLight {
            color: Vec3::new(0.55, 0.68, 1.0),
            intensity: 8.0,
            range: 18.0,
            direction: Vec3::new(0.0, -0.6, 0.8),
            inner_cutoff: 24.0_f32.to_radians(),
            outer_cutoff: 46.0_f32.to_radians(),
        },
    ));
    // ---- 补光（三点布光的填充位）：暖色点光从相机左前上方填暗部 ----
    // 不投影、不给第二阴影——把面部与服装的背光半区抬一档并补回肤色
    // 暖调（天空 IBL 与冷色环境底会把皮肤读灰）。距离面部约 4.2 m 处
    // 衰减后有效强度约 0.9，仍弱于主光一个量级（填充位不造新阴影）。
    world.spawn((
        Name("exp/fill-light".to_string()),
        Transform {
            position: Vec3::new(-3.2, 2.6, 4.6),
            ..Transform::IDENTITY
        },
        PointLight {
            color: Vec3::new(1.0, 0.82, 0.62),
            intensity: 2.6,
            range: 11.0,
            shadows: false,
        },
    ));

    // ---- 相机：轨道位姿构造（注视原点上方 orbit.focus_h）----
    let orbit = *world
        .try_resource::<OrbitCamera>()
        .expect("OrbitCamera 已由 setup_resources 插入");
    let (eye, rot) = orbit_pose(
        orbit.yaw,
        orbit.pitch,
        orbit.dist,
        Vec3::new(0.0, orbit.focus_h, 0.0),
    );
    let camera = world.spawn((
        Name("exp/camera".to_string()),
        Transform {
            position: eye,
            rotation: rot,
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
    ));
    // ---- 角色 GLB：装配 + 三状态机（常态 Idle 开播）----
    let path = asset_path(MODEL_PATH);
    let entities = match spawn_gltf_instance(world, &path) {
        Ok(instance) => {
            let clips = instance.clips.clone();
            let walk = clip_index(&clips, WALK_CLIP);
            let jump = clip_index(&clips, JUMP_CLIP);
            let idle = clip_index(&clips, IDLE_CLIP);
            // 剪辑缺席的退化索引（与 ExpEntities 字段落账同口径）：
            // walk 缺席指 0（下标语义退化为「首个剪辑」）；idle 缺席回退
            // 步行剪辑；jump 缺席指 0 且与 walk 或 idle 重合时视为无跳跃。
            let walk_idx = walk.unwrap_or(0);
            let jump_idx = jump.unwrap_or(0);
            let idle_idx = idle.or(walk).unwrap_or(0);
            let model = instance.roots[0];
            world.insert(model, Name("exp/character".to_string()));
            world.insert(model, Visibility::VISIBLE);
            let mut player = instance.player();
            // 常态 Idle 循环开播（缺 Idle 时回退步行剪辑——退化语义）。
            // 首帧 Update 按移动意图经状态机切换，见 update_exp_control。
            player.active = idle_idx;
            player.looping = true;
            player.playing = true;
            configure_state_machine(&mut player, idle_idx, walk_idx, jump_idx);
            player.capture_rest(world);
            world.insert(model, player);
            // 材质升级（渲染质量轮）：按节点名覆写 SSS / Kajiya-Kay（染
            // 色档）/ 眼球粗糙度——GLB 导入契约恒关，游戏层显式打开。
            // 预期 5 项（皮肤双 prim + 头发 + 睫毛 + 眼球），数字进日志
            // 供装配对账。
            let upgraded = upgrade_character_materials(world, &instance);
            log::info!(
                "ExpPlugin：角色已装配（{} 节点 / {} 剪辑 / 材质升级 {upgraded} 项），\
                 控制：WASD 移动 / 右键拖拽视角 / 滚轮缩放 / Q·E 注视点高度 / \
                 空格 跳跃（停止即回落 Idle）",
                instance.node_entities.len(),
                clips.len()
            );
            ExpEntities {
                camera,
                model: Some(model),
                walk: walk_idx,
                jump: jump_idx,
                idle: idle_idx,
            }
        }
        Err(err) => {
            log::warn!(
                "ExpPlugin：角色 GLB {} 加载失败（{err}）——无角色模式\
                 （把 GLB 放到该路径后重跑即可）",
                path.display()
            );
            ExpEntities {
                camera,
                model: None,
                walk: 0,
                jump: 0,
                idle: 0,
            }
        }
    };
    world.insert_resource(entities);
}

/// 给播放器装配三状态机（Idle 与 Walk 循环、Jump 单次）与过渡表：
///
/// - 手动边表（v1 路径）：Idle 进 Walk 是 [`STATE_BLEND`] 秒过渡，Walk
///   切回 Idle 是 [`STOP_BLEND`] 秒（停步方向更短，理由见常量文档——
///   且切换时机先过停步相位门）；Idle 或 Walk 进 Jump 是
///   [`JUMP_IN_BLEND`] 秒。`request_state` 查边启动 crossfade，游戏侧
///   每次状态切换只调这一个方法。
/// - 自动转移表（v2 条件路径）：Jump 播完（`ClipFinished` 守卫）按
///   `moving` 参数分流——静止回 Idle、移动中直落 Walk（落地即走），
///   均带 [`JUMP_OUT_BLEND`] 秒过渡。由引擎 `update_animations` 每帧
///   步检，游戏侧不手写播完轮询。
///
/// 退化 GLB 不 panic：零剪辑（理论上 GLB 无动画轨）直接不装配状态机
/// （`update_exp_control` 见空表跳过动画段）；剪辑索引重合（缺 idle 或
/// 缺 jump）时同剪辑 `transition` 是 no-op、Jump 状态不可达即自动转移
/// 永不触发。
fn configure_state_machine(player: &mut AnimationPlayer, idle: usize, walk: usize, jump: usize) {
    if player.clips.is_empty() {
        return;
    }
    player.states = vec![
        AnimationState {
            clip: idle,
            looping: true,
            speed: 1.0,
        },
        AnimationState {
            clip: walk,
            looping: true,
            speed: 1.0,
        },
        AnimationState {
            clip: jump,
            looping: false,
            speed: 1.0,
        },
    ];
    player.edges = vec![
        AnimationEdge {
            from: STATE_IDLE,
            to: STATE_WALK,
            duration: STATE_BLEND,
        },
        AnimationEdge {
            from: STATE_WALK,
            to: STATE_IDLE,
            duration: STOP_BLEND,
        },
        AnimationEdge {
            from: STATE_IDLE,
            to: STATE_JUMP,
            duration: JUMP_IN_BLEND,
        },
        AnimationEdge {
            from: STATE_WALK,
            to: STATE_JUMP,
            duration: JUMP_IN_BLEND,
        },
    ];
    player.transitions = vec![
        AnimTransition {
            from: STATE_JUMP,
            to: STATE_IDLE,
            duration: JUMP_OUT_BLEND,
            guard: AnimGuard::All(vec![
                AnimGuard::ClipFinished,
                AnimGuard::Not(Box::new(AnimGuard::BoolIs(MOVING_PARAM.to_string(), true))),
            ]),
        },
        AnimTransition {
            from: STATE_JUMP,
            to: STATE_WALK,
            duration: JUMP_OUT_BLEND,
            guard: AnimGuard::All(vec![
                AnimGuard::ClipFinished,
                AnimGuard::BoolIs(MOVING_PARAM.to_string(), true),
            ]),
        },
    ];
    player.current_state = STATE_IDLE;
}

/// Update：三状态动作机（crossfade 过渡 + 停步相位门 + 速度平滑）+
/// WASD 角色移动 + 朝向转身。状态语义：
///
/// - `空格` 跳跃单次（进 Jump 状态，非循环）；播完由引擎条件转移自动
///   回落——静止回 Idle、移动中直落 Walk（落地即走），回落带
///   [`JUMP_OUT_BLEND`] 秒 crossfade，游戏侧不手写播完轮询；
/// - 移动即从 Idle 过渡到 Walk（[`STATE_BLEND`] 秒，起步速度按
///   [`ACCEL_RATE`] 指数加速）；停止不立即切换——步行中先等停步相位
///   门（[`stop_phase_ready`]，至多约半步）走完当前步，再以
///   [`STOP_BLEND`] 秒淡出，速度随后按 [`DECEL_RATE`] 指数减速，残差
///   低于 [`STOP_EPS`] 精确归零；
/// - 跳跃进行中不被移动打断（Jump 状态期间只写 `moving` 参数，不请求
///   切换）；空格在跳跃中同样无效；
/// - 速度非零的帧写角色 `Transform` 位置（起步加速与停步减速窗口内
///   位移照常）；完全静止后的无输入帧不写任何 `Transform`（逐位恒等，
///   精确口径见模块文档）；角色缺席（资产降级）时整帧空转。
///
/// GLB 缺 jump 或 idle 的退化：jump 索引与 walk 或 idle 重合时视为「无
/// 跳跃」（不请求 Jump 状态，自动转移因状态不可达永不触发）；idle 缺席
/// 时 Idle 状态与 Walk 状态共用步行剪辑（同剪辑 transition 为 no-op，
/// 停止等于步行循环续播），均不 panic。
fn update_exp_control(world: &World) {
    let Some(ents) = world.try_resource::<ExpEntities>() else {
        return;
    };
    let Some(model) = ents.model else {
        return;
    };
    let Some(input) = world.try_resource::<Input>() else {
        return;
    };
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);
    // 移动方向以相机方位为参照（读 OrbitCamera 状态而非相机 Transform：
    // 本帧拖拽后的最新值，且与渲染无关、headless 可注入断言）
    let cam_yaw = world
        .try_resource::<OrbitCamera>()
        .map(|o| o.yaw)
        .unwrap_or(0.0);

    // ---- WASD 意图方向（相机系：W = 屏幕里侧，D = 屏幕右侧）----
    let forward = Vec3::new(-cam_yaw.sin(), 0.0, -cam_yaw.cos());
    let right = Vec3::new(cam_yaw.cos(), 0.0, -cam_yaw.sin());
    let mut dir = Vec3::ZERO;
    if input.key(KeyCode::KeyW) {
        dir += forward;
    }
    if input.key(KeyCode::KeyS) {
        dir -= forward;
    }
    if input.key(KeyCode::KeyD) {
        dir += right;
    }
    if input.key(KeyCode::KeyA) {
        dir -= right;
    }
    let moving = dir != Vec3::ZERO;
    let dir = if moving { dir.normalize_or_zero() } else { dir };

    let space_pressed = input.key_pressed(KeyCode::Space);

    let Some(mut player) = world.get_mut::<AnimationPlayer>(model) else {
        return;
    };
    // 巡航条件：速度目标保持全速——移动中，或停步相位门等待中（走完
    // 最后一步，脚步与位移保持匹配）；其余（停步切换后、静止腾空减速）
    // 目标归零进入指数减速。
    let mut cruise = moving;
    // 零剪辑 GLB 退化（states 空表，见 configure_state_machine）：无状态
    // 机可驱动，动画段跳过、移动照常。正常装配下表恒非空。
    if !player.states.is_empty() {
        // moving 参数每帧先写：跳跃落地的条件转移在 update_animations
        // 步检时读取，而本系统注册在其后，故步检读到的是上一帧写入的
        // 值——至多一帧陈旧，按住移动键跨落地帧的场景无感。
        player.set_bool(MOVING_PARAM, moving);
        let jump_is_real = ents.jump != ents.walk && ents.jump != ents.idle;
        let in_jump = jump_is_real && player.current_state == STATE_JUMP;
        let jump_requested = space_pressed && jump_is_real && !in_jump;
        // 停步相位门：无输入、正在步行、且未落在过步窗口——本帧不请求
        // 切换，保持 Walk 走完当前步（速度照常全速，脚步不打滑）。
        let hold_walk = !in_jump
            && !moving
            && !jump_requested
            && player.current_state == STATE_WALK
            && !stop_phase_ready(player.time, player.clips[ents.walk].duration);
        if jump_requested {
            player.request_state(STATE_JUMP);
        } else if !in_jump {
            if moving {
                player.request_state(STATE_WALK);
            } else if !hold_walk {
                player.request_state(STATE_IDLE);
            }
            // hold_walk 帧：不请求——等过步窗口
        }
        cruise = moving || hold_walk;
    }

    // ---- 速度平滑 + 位置积分 + 朝向转身 ----
    // 目标速度：移动中是意图方向乘全速（起步按 ACCEL_RATE 加速）；相位
    // 门等待期保持当前值（走完最后一步）；其余归零（停步按 DECEL_RATE
    // 减速）。速度非零的帧写位置（停步减速窗口内位移照常推进，完全
    // 静止后恢复无输入恒等，语义见模块文档）；朝向只在有输入帧更新。
    let (velocity, yaw) = {
        let Some(mut ch) = world.try_resource_mut::<ExpCharacter>() else {
            return;
        };
        let target = if moving {
            dir * MOVE_SPEED
        } else if cruise {
            ch.velocity
        } else {
            Vec3::ZERO
        };
        let rate = if moving { ACCEL_RATE } else { DECEL_RATE };
        let mut vel = exp_approach_vec3(ch.velocity, target, rate, dt);
        if !cruise && vel.length() < STOP_EPS {
            vel = Vec3::ZERO;
        }
        ch.velocity = vel;
        if moving {
            ch.yaw = wrap_angle(exp_approach(ch.yaw, facing_yaw(dir), TURN_RATE, dt));
        }
        (ch.velocity, ch.yaw)
    };
    if velocity != Vec3::ZERO {
        let Some(mut t) = world.get_mut::<Transform>(model) else {
            return;
        };
        let mut pos = t.position + velocity * dt;
        pos.x = pos.x.clamp(-GROUND_HALF, GROUND_HALF);
        pos.z = pos.z.clamp(-GROUND_HALF, GROUND_HALF);
        t.position = pos;
    }
    if moving {
        if let Some(mut t) = world.get_mut::<Transform>(model) {
            t.rotation = Quat::from_rotation_y(yaw);
        }
    }
}

/// WASD 任一处于按住状态（移动意图与相机「角色移动帧跟随」判定共用
/// 的键位表——两处必须同步增删）。
fn wasd_held(input: &Input) -> bool {
    input.key(KeyCode::KeyW)
        || input.key(KeyCode::KeyS)
        || input.key(KeyCode::KeyA)
        || input.key(KeyCode::KeyD)
}

/// Update：右键拖拽轨道环绕 + 滚轮缩放 + Q/E 调注视点高度 + 注视点跟随
/// 角色。鼠标、滚轮、Q/E 与 WASD 全静默且角色已完全静止的帧直接返回
/// （不写相机 `Transform`、不写资源——恒等帧保护外部机位覆盖与
/// headless 逐位稳定）；WASD 按住帧照写（注视点跟随移动中的角色，钳制
/// 边界上写的是逐位恒等值，无副作用）；停步减速滑行窗口（无输入但速度
/// 未归零）照常跟随最后几厘米位移。相机实体缺席（理论上不发生，实体表
/// 自有即建）时仅状态更新不写位姿。
fn update_orbit_camera(world: &World) {
    let Some(ents) = world.try_resource::<ExpEntities>() else {
        return;
    };
    let Some(input) = world.try_resource::<Input>() else {
        return;
    };
    let dragging = input.mouse_button(MouseButton::Right);
    let (dx, dy) = input.mouse_delta();
    let wheel = input.wheel_delta();
    let focus_key_held = input.key(KeyCode::KeyQ) || input.key(KeyCode::KeyE);
    // 停步减速滑行（无输入但速度未归零）：注视点跟着最后几厘米位移走，
    // 完全静止后恢复无输入恒等。
    let gliding = world
        .try_resource::<ExpCharacter>()
        .map(|c| c.velocity != Vec3::ZERO)
        .unwrap_or(false);
    if !dragging
        && dx == 0.0
        && dy == 0.0
        && wheel == 0.0
        && !focus_key_held
        && !wasd_held(&input)
        && !gliding
    {
        return;
    }
    // 轨道状态读取圈进作用域（try_resource 守卫活到块尾——在守卫持有
    // 期间 try_resource_mut 同一资源会触发 already borrowed panic）
    let (mut yaw, mut pitch, mut dist, mut focus_h) = {
        let Some(orbit) = world.try_resource::<OrbitCamera>() else {
            return;
        };
        (orbit.yaw, orbit.pitch, orbit.dist, orbit.focus_h)
    };
    if dragging {
        // 右移 = 相机向右环绕（yaw 增）；上移 = 相机升高俯视（pitch 增）
        yaw += dx as f32 * ORBIT_SENS;
        pitch = (pitch - dy as f32 * ORBIT_SENS).clamp(PITCH_MIN, PITCH_MAX);
    }
    if wheel != 0.0 {
        // 上滚（wheel > 0）= 拉近
        dist *= (-wheel * WHEEL_SENS).exp();
    }
    dist = dist.clamp(DIST_MIN, DIST_MAX);
    if let Some(dt) = world.try_resource::<Time>().map(|t| t.delta_secs()) {
        // Q 降 / E 升（注视点朝脚部 / 头顶移动；相机同向跟随）
        if input.key(KeyCode::KeyQ) {
            focus_h -= FOCUS_H_RATE * dt;
        }
        if input.key(KeyCode::KeyE) {
            focus_h += FOCUS_H_RATE * dt;
        }
    }
    focus_h = focus_h.clamp(FOCUS_H_MIN, FOCUS_H_MAX);

    // ---- 注视点：角色当前位置 + focus_h 高度（同帧新值——注册序在
    // 移动系统之后）----
    let focus = match ents.model {
        Some(m) => world
            .get::<Transform>(m)
            .map(|t| t.position)
            .unwrap_or(Vec3::ZERO),
        None => Vec3::ZERO,
    } + Vec3::new(0.0, focus_h, 0.0);
    let (eye, rot) = orbit_pose(yaw, pitch, dist, focus);

    if let Some(mut t) = world.get_mut::<Transform>(ents.camera) {
        t.position = eye;
        t.rotation = rot;
    }
    if let Some(mut o) = world.try_resource_mut::<OrbitCamera>() {
        o.yaw = yaw;
        o.pitch = pitch;
        o.dist = dist;
        o.focus_h = focus_h;
    }
}

/// 轨道位姿几何（纯函数）：绕 `target` 的球坐标 → 相机（世界位）。
///
/// `offset = dist·(cos(pitch)·sin(yaw), sin(pitch), cos(pitch)·cos(yaw))`，
/// 旋转 = `from_rotation_y(yaw) × from_rotation_x(−pitch)`（先俯仰后绕
/// 世界 Y 偏航）——相机前向 `R × (0,0,−1)` 与「从 eye 指向 target」方向
/// 精确一致（解析可证，单测守护）。pitch 为俯角口径（正值 = 相机高于
/// 注视点）。
pub fn orbit_pose(yaw: f32, pitch: f32, dist: f32, target: Vec3) -> (Vec3, Quat) {
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let offset = Vec3::new(dist * cp * sy, dist * sp, dist * cp * cy);
    (
        target + offset,
        Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-pitch),
    )
}

/// 移动方向 → 角色朝向角（纯函数）：模型前向为本地 **+Z**（GLB 导出口径，
/// 离屏快照正脸与 `EXP_CAM_HEAD` 诊断机位双侧证实——initial rotation
/// 恒等时 +Z 侧相机看到正脸），旋转 `from_rotation_y(yaw)` 须把 `(0,0,1)`
/// 转到 `dir`，解析解 `yaw = atan2(dir.x, dir.z)`。值域 [−π, π]，面向
/// +Z 恰为 0（与初始朝向一致，S 向移动不触发转身）。
pub fn facing_yaw(dir: Vec3) -> f32 {
    dir.x.atan2(dir.z)
}

/// 最短角差（纯函数）：`to − from` 归一到 (−π, π]，供朝向环绕转身
/// （跨 ±π 不绕远路）。
pub fn shortest_angle(from: f32, to: f32) -> f32 {
    let d = (to - from + core::f32::consts::PI) % core::f32::consts::TAU;
    let d = if d < 0.0 {
        d + core::f32::consts::TAU
    } else {
        d
    };
    d - core::f32::consts::PI
}

/// 角度规范到 [−π, π)（纯函数）：转身状态的规范表示——连续同向转身
/// 会让 `exp_approach` 的状态角沿等价旋转漂出 ±π（如 −π → −2π），旋转
/// 语义不变但状态值漂移；每步规范一次即锚定。边界 ±π 的去向由
/// `.round()` 的远离半取整决定（±π 互转，旋转等价无碍）。
pub fn wrap_angle(a: f32) -> f32 {
    a - core::f32::consts::TAU * (a / core::f32::consts::TAU).round()
}

/// 帧率无关指数趋近（纯函数）：每帧朝 `target` 走「当前角差 ×
/// `1 − exp(−rate·dt)`」。残差比例逐帧连乘 = `exp(−rate·Σdt)`（指数
/// 衰减解析解），同一段真实时间任意切分收敛轨迹一致（浮点舍入内），
/// `rate ≤ 0` 视为直接贴合。
pub fn exp_approach(cur: f32, target: f32, rate: f32, dt: f32) -> f32 {
    if rate <= 0.0 {
        return target;
    }
    let k = 1.0 - (-rate * dt).exp();
    cur + shortest_angle(cur, target) * k
}

/// 帧率无关向量指数趋近（纯函数）：与 [`exp_approach`] 同式但无角环绕
/// 语义，逐分量独立趋近。残差向量按 `exp(−rate·Σdt)` 收缩（解析解），
/// 同一段真实时间任意切分收敛轨迹一致（浮点舍入内），`rate ≤ 0` 直接
/// 贴合。起步加速与停步减速的速度平滑都用它。
pub fn exp_approach_vec3(cur: Vec3, target: Vec3, rate: f32, dt: f32) -> Vec3 {
    if rate <= 0.0 {
        return target;
    }
    let k = 1.0 - (-rate * dt).exp();
    cur + (target - cur) * k
}

/// 停步相位门（纯函数）：步行剪辑的归一化时间是否落在「姿势距 Idle
/// 最近」的窗口内——此刻切回 Idle 要跨越的姿势距离最小，过渡中间姿势
/// 最不像两姿势的算术平均。窗口表 [`STOP_PHASE_WINDOWS`] 的实测依据：
/// 对 Mixamo_Walk 全部 261 通道按逐分量 L1 计算对 Idle 首帧的距离曲线
/// （96 相位采样），循环末尾最深（相位 0.93~1.0，距离 10.1~10.6，两脚
/// 并拢的触地换步点），0~0.10 与 0.57~0.70 两段次窗口距离 12.9~13.7
/// （与循环首帧 12.9 同量级），三者把最大等待压到约半步（0.45 s）；
/// 曲线峰值在 0.26 与 0.52（满幅分腿，17~18），窗口避开。
/// `walk_duration ≤ 0` 视为无门（退化资产直切，不等待）。
pub fn stop_phase_ready(walk_time: f32, walk_duration: f32) -> bool {
    if walk_duration <= 0.0 {
        return true;
    }
    let phase = (walk_time / walk_duration).rem_euclid(1.0);
    STOP_PHASE_WINDOWS
        .iter()
        .any(|&(lo, hi)| phase >= lo && phase <= hi)
}

/// 离屏渲染器便捷构造（快照 example / 手动验收用；无 GPU 返回 None）。
pub fn offscreen(width: u32, height: u32) -> Option<OffscreenRenderer> {
    OffscreenRenderer::new(width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn assert_close(a: Vec3, b: Vec3, what: &str) {
        assert!(
            a.abs_diff_eq(b, EPS),
            "{what}：期望 {b:?}，实际 {a:?}（差 {:?}）",
            a - b
        );
    }

    #[test]
    fn orbit_pose_analytic() {
        // 水平正对（yaw=0, pitch=0）：相机在 target +Z 方向 dist 处，恒等旋转
        let (eye, rot) = orbit_pose(0.0, 0.0, 5.0, Vec3::new(1.0, 2.0, 3.0));
        assert_close(eye, Vec3::new(1.0, 2.0, 8.0), "水平正对 eye");
        assert!(
            rot.abs_diff_eq(Quat::IDENTITY, EPS),
            "水平正对旋转应恒等，实际 {rot:?}"
        );
        // 正上方俯拍（pitch=90°）：eye 在 target 正上方，前向朝下
        let (eye, rot) = orbit_pose(0.0, core::f32::consts::FRAC_PI_2, 4.0, Vec3::ZERO);
        assert_close(eye, Vec3::new(0.0, 4.0, 0.0), "正上方 eye");
        let fwd = rot * Vec3::NEG_Z;
        assert_close(fwd, Vec3::NEG_Y, "正上方相机前向应朝下");
        // 侧位 90°（yaw=90°）：相机绕到 +X 侧
        let (eye, _) = orbit_pose(core::f32::consts::FRAC_PI_2, 0.0, 5.0, Vec3::ZERO);
        assert_close(eye, Vec3::new(5.0, 0.0, 0.0), "侧位 90° eye");
        // 不变量：|eye − target| = dist（任意角度）
        for &(yaw, pitch) in &[(0.3_f32, 0.2_f32), (-2.1, 1.2), (5.0, -0.05)] {
            let (eye, _) = orbit_pose(yaw, pitch, 7.25, Vec3::new(0.5, 1.0, -0.5));
            assert!(
                (eye.distance(Vec3::new(0.5, 1.0, -0.5)) - 7.25).abs() < EPS,
                "轨道距离不变量被破坏（yaw={yaw} pitch={pitch}）"
            );
        }
        // 初始机位（OrbitCamera::default）：与 R66b 旧机位同构观感——
        // 相机在 +Z 侧俯视 9°，注视点在 target 上方
        let o = OrbitCamera::default();
        let (eye, rot) = orbit_pose(o.yaw, o.pitch, o.dist, Vec3::new(0.0, TARGET_HEIGHT, 0.0));
        assert_close(
            eye,
            Vec3::new(
                0.0,
                1.5 + 7.5 * 9f32.to_radians().sin(),
                7.5 * 9f32.to_radians().cos(),
            ),
            "初始机位 eye",
        );
        assert_close(
            rot * Vec3::NEG_Z,
            Vec3::new(0.0, -9f32.to_radians().sin(), -9f32.to_radians().cos()),
            "初始机位前向（俯 9°）",
        );
    }

    #[test]
    fn facing_yaw_cardinal() {
        let pi = core::f32::consts::PI;
        // 模型原生面向 +Z：dir = +Z 无需转身（S 向移动不触发转身）
        assert!(
            facing_yaw(Vec3::Z).abs() < EPS,
            "面向 +Z 应为 yaw=0，实际 {}",
            facing_yaw(Vec3::Z)
        );
        // 面向 -Z：±π 皆合法（同一旋转；atan2 对 (−0.0, 负数) 返回 −π，
        // IEEE 负零语义决定符号，跨平台确定）
        assert!(
            (facing_yaw(Vec3::NEG_Z).abs() - pi).abs() < EPS,
            "面向 -Z 应为 |yaw|=π，实际 {}",
            facing_yaw(Vec3::NEG_Z)
        );
        assert!(
            (facing_yaw(Vec3::X) - core::f32::consts::FRAC_PI_2).abs() < EPS,
            "面向 +X 应为 yaw=+π/2（from_rotation_y(π/2) 把 +Z 转到 +X）"
        );
        assert!(
            (facing_yaw(Vec3::NEG_X) + core::f32::consts::FRAC_PI_2).abs() < EPS,
            "面向 -X 应为 yaw=-π/2"
        );
    }

    #[test]
    fn wrap_angle_canonical() {
        let pi = core::f32::consts::PI;
        // 主值区内恒等
        assert!((wrap_angle(1.234) - 1.234).abs() < EPS);
        // 漂出 ±π 的等价角拉回主值区（连续同向转身的漂移场景）
        assert!((wrap_angle(-2.0 * pi).abs()) < EPS, "-2π 应规范到 0");
        assert!(
            (wrap_angle(3.0 * pi).abs() - pi).abs() < EPS,
            "3π 应规范到 ±π（round 远离半取整实取 −π，等价）"
        );
        assert!(wrap_angle(-5.0).abs() < pi, "-5 rad 应规范进 [−π, π)");
        // 边界 ±π 互转（旋转等价）
        assert!((wrap_angle(-pi).abs() - pi).abs() < EPS);
    }

    #[test]
    fn shortest_angle_wrap() {
        let pi = core::f32::consts::PI;
        // 跨 ±π 取短弧：0 → 350° 应为 -10° 而非 +350°
        assert!(
            (shortest_angle(0.0, 350.0_f32.to_radians()) + 10.0_f32.to_radians()).abs() < EPS,
            "350° 应取 -10° 短弧"
        );
        assert!(
            (shortest_angle(350.0_f32.to_radians(), 0.0) - 10.0_f32.to_radians()).abs() < EPS,
            "反向同样取短弧"
        );
        // 同角差为 0
        assert!(shortest_angle(1.234, 1.234).abs() < EPS);
        // 半圈边界（±π）取值有定义（幅度恒 π）
        assert!((shortest_angle(0.0, -pi).abs() - pi).abs() < EPS);
    }

    #[test]
    fn exp_approach_frame_rate_independent() {
        let pi = core::f32::consts::PI;
        // 同一段真实时间（1/20 s）不同切分（3×1/60 s vs 1×1/20 s vs
        // 10×1/200 s）收敛一致（指数衰减解析解，容差 = 浮点舍入）
        let run = |steps: usize, dt: f32| -> f32 {
            let mut cur = 0.0_f32;
            for _ in 0..steps {
                cur = exp_approach(cur, pi, TURN_RATE, dt);
            }
            cur
        };
        let a = run(3, 1.0 / 60.0);
        let b = run(1, 1.0 / 20.0);
        let c = run(10, 1.0 / 200.0);
        assert!(
            (a - b).abs() < 1e-6 && (a - c).abs() < 1e-6,
            "切分无关性被破坏：{a} vs {b} vs {c}"
        );
        // 单步解析解：残差 = 角差 × exp(−rate·dt)（取非边界目标角 2.0 rad
        // ——边界 ±π 处短弧方向本就双向等长，无符号约定）
        let t0 = 2.0_f32;
        let cur = exp_approach(0.0, t0, TURN_RATE, 1.0 / 60.0);
        let expect = t0 * (-TURN_RATE * 1.0 / 60.0).exp();
        assert!(
            ((t0 - cur) - expect).abs() < 1e-6,
            "单步残差应等于解析解：期望 {expect}，实际残差 {}",
            t0 - cur
        );
        // rate ≤ 0 直接贴合；朝向环绕（0 → -π+ε）走短弧
        assert_eq!(exp_approach(0.3, 2.0, 0.0, 1.0 / 60.0), 2.0);
        let wrapped = exp_approach(0.0, -pi + 0.01, TURN_RATE, 1.0 / 60.0);
        assert!(wrapped < 0.0, "目标在 -π 附近应走负向短弧，实际 {wrapped}");
    }

    #[test]
    fn exp_approach_vec3_analytic() {
        // 单步解析解：残差向量长度 = 初差 × exp(−rate·dt)（与 exp_approach
        // 同式，无角环绕）
        let cur = Vec3::new(1.0, -2.0, 0.5);
        let target = Vec3::new(-3.0, 2.0, 0.5);
        let v = exp_approach_vec3(cur, target, ACCEL_RATE, 1.0 / 60.0);
        let expect = target - (target - cur) * (-ACCEL_RATE / 60.0).exp();
        assert!(
            v.abs_diff_eq(expect, 1e-6),
            "单步应等于解析解：期望 {expect:?}，实际 {v:?}"
        );
        // 同一段真实时间（0.5 s）不同切分（30×1/60 vs 1×0.5 vs 100×0.005）
        // 收敛一致（指数衰减解析解）
        let run = |steps: usize, dt: f32| -> Vec3 {
            let mut v = Vec3::ZERO;
            let t = Vec3::new(0.0, 0.0, -MOVE_SPEED);
            for _ in 0..steps {
                v = exp_approach_vec3(v, t, ACCEL_RATE, dt);
            }
            v
        };
        let a = run(30, 1.0 / 60.0);
        let b = run(1, 0.5);
        let c = run(100, 0.005);
        assert!(
            a.abs_diff_eq(b, 1e-5) && a.abs_diff_eq(c, 1e-5),
            "切分无关性被破坏：{a:?} vs {b:?} vs {c:?}"
        );
        // 与目标重合时恒等（零差值）；rate ≤ 0 直接贴合
        assert_eq!(exp_approach_vec3(a, a, ACCEL_RATE, 1.0 / 60.0), a);
        assert_eq!(exp_approach_vec3(a, Vec3::ONE, 0.0, 1.0 / 60.0), Vec3::ONE);
    }

    #[test]
    fn stop_phase_windows_membership() {
        // 主窗口（循环末尾两脚并拢）与两段次窗口内放行
        for phase in [0.0_f32, 0.05, 0.10, 0.57, 0.635, 0.70, 0.93, 0.97, 0.999] {
            assert!(
                stop_phase_ready(phase * 0.966_666_64, 0.966_666_64),
                "相位 {phase} 应在停步窗口内"
            );
        }
        // 窗外保持步行（满幅分腿区与过渡肩部）
        for phase in [0.11_f32, 0.26, 0.45, 0.56, 0.71, 0.85, 0.92] {
            assert!(
                !stop_phase_ready(phase * 0.966_666_64, 0.966_666_64),
                "相位 {phase} 应在停步窗口外"
            );
        }
        // 时间跨过循环长度按相位回绕：1.5 圈 = 相位 0.5 → 窗外
        assert!(!stop_phase_ready(1.5 * 0.966_666_64, 0.966_666_64));
        assert!(stop_phase_ready(2.0 * 0.966_666_64, 0.966_666_64));
        // 退化：非正时长视为无门（直切不等待）
        assert!(stop_phase_ready(0.25, 0.0));
    }

    #[test]
    fn material_upgrade_routing() {
        // 四个契约节点名各命中其升级
        assert_eq!(
            material_upgrade_for_node(SKIN_NODE),
            Some(MaterialUpgrade::Skin)
        );
        assert_eq!(
            material_upgrade_for_node(HAIR_NODE),
            Some(MaterialUpgrade::Hair)
        );
        assert_eq!(
            material_upgrade_for_node(LASH_NODE),
            Some(MaterialUpgrade::Lash)
        );
        assert_eq!(
            material_upgrade_for_node(EYE_NODE),
            Some(MaterialUpgrade::Eye)
        );
        // 骨骼 / 布料 / 空名不路由（覆写不外溢）
        assert_eq!(material_upgrade_for_node("gMan Head"), None);
        assert_eq!(material_upgrade_for_node("cloth01"), None);
        assert_eq!(material_upgrade_for_node(""), None);
    }

    #[test]
    fn apply_material_upgrade_each_kind() {
        // 皮肤：SSS 双字段精确落位，KK 不动，其余保持默认
        let mut skin = MaterialDef::default();
        apply_material_upgrade(&mut skin, MaterialUpgrade::Skin);
        assert_eq!(skin.subsurface_strength, SKIN_SSS_STRENGTH);
        assert_eq!(skin.subsurface_scale, SKIN_SSS_SCALE);
        assert!(skin.kajiya_kay.is_none(), "皮肤不应挂 Kajiya-Kay");
        assert_eq!(skin.roughness, MaterialDef::default().roughness);
        // 头发：纯哑光（粗糙度抬升 + **不挂 KK**——近黑发上任何可见
        // 定向高光都读灰白，回归断言防 KK 复挂）
        let mut hair = MaterialDef::default();
        apply_material_upgrade(&mut hair, MaterialUpgrade::Hair);
        assert_eq!(hair.roughness, HAIR_ROUGHNESS, "头发粗糙度应抬到哑光档");
        assert!(
            hair.kajiya_kay.is_none(),
            "头发不得挂 Kajiya-Kay（漂白证据链）"
        );
        assert_eq!(hair.subsurface_strength, 0.0, "头发不应开 SSS");
        // 睫毛：同哑光档
        let mut lash = MaterialDef::default();
        apply_material_upgrade(&mut lash, MaterialUpgrade::Lash);
        assert_eq!(lash.roughness, HAIR_ROUGHNESS);
        assert!(lash.kajiya_kay.is_none(), "睫毛不得挂 Kajiya-Kay");
        // 眼球：粗糙度从 GLB 导入原值 0 抬到目标，金属度等其他字段不动
        let mut eye = MaterialDef {
            roughness: 0.0,
            ..Default::default()
        };
        apply_material_upgrade(&mut eye, MaterialUpgrade::Eye);
        assert_eq!(eye.roughness, EYE_ROUGHNESS);
        assert!(eye.roughness > 0.0, "目标粗糙度应是抬升而非维持全镜面");
        assert_eq!(eye.metallic, 0.0);
        assert!(eye.kajiya_kay.is_none());
    }
}
