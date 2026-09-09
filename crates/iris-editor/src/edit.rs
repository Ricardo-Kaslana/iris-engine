//! 编辑命令层（R58a a 阶段 → R58b b 阶段）：编辑器状态 + 全部编辑操作 +
//! undo / redo 快照栈 + 光源参数编辑 + 烘焙入口 + NavGrid 重建与可视化 +
//! 相机轨道旋转 + 瞬态视觉 + 只读属性面板 + 关卡档 v2 存取。
//!
//! # 分层：命令层与交互壳分离
//!
//! 本模块**不含任何窗口 / 输入 / 渲染代码**——每个操作都是
//! `World + EditorState + 事件参数 → 确定性结果` 的自由函数，可脱离窗口
//! 单测（`tests/edit_headless.rs` 即验收①的载体）。输入翻译（鼠标像素 →
//! 射线、键位 → 命令）与飞行相机在 [`crate::shell`]，那一层刻意做薄：
//! 它只负责把设备事件换算成本模块的参数。
//!
//! # 存档往返的两个机制前提
//!
//! 编辑器的产物必须能被 `save_scene` 完整还原（关卡档 v2 直读），这依赖
//! 两条不变量：
//!
//! 1. **登记表同步登记**：`MeshStore` / `MaterialStore` 只存顶点与数值，
//!    没有「这份数据来自哪里」的概念；[`save_scene`] 靠
//!    [`SceneAssetRegistry`] 还原 mesh / material 段。因此编辑器每创建一个
//!    **关卡内容**网格就登记 [`MeshRef::Primitive`]（段 = 摆放参数），
//!    每创建一个内容材质就登记 [`MaterialRef::default()`（= 无外部纹理
//!    来源）](MaterialRef)。不登记 ⇒ save 认不出来源 ⇒ mesh 段整段省略
//!    + 告警（重载后实体没有网格）。
//! 2. **句柄编号对齐**：编辑器侧对同源资产去重（同 [`PrimitiveSection`]
//!    只入 `MeshStore` 一次、同预设只入 `MaterialStore` 一次），与
//!    `load_scene` 的 `LoadCache` 去重语义**一致**；且[`open_level`] 把
//!    瞬态视觉资产（网格线 / 选中框的网格与材质）创建在**关卡内容之前**，
//!    两侧都如此。于是「摆放 → 保存」与「重载」两侧的 `MeshHandle` /
//!    `MaterialHandle` / `Entity` 编号逐位相同——往返对照测试可以直接
//!    比较句柄数值而不只是比较内容。
//!
//! # undo / redo（b 阶段）：快照栈设计
//!
//! 每个写操作（摆放 / 删除 / 复制 / 拖拽按下 / 微移 / 缩放 / 光源参数
//! 编辑）执行前拍一次 [`WorldSnapshot`] 入 undo 栈（深度 [`UNDO_LIMIT`]，
//! 环形丢弃最旧）；`Ctrl+Z` 后退、`Ctrl+Y`（或 `Ctrl+Shift+Z`）前进。
//!
//! - **快照字段集 = R58a 世界 dump 的字段集 + 光源参数**：可编辑实体全
//!   组件（Transform / Name / 句柄 / Visibility / Collider / RigidBody /
//!   PointLight / ParticleEmitter，存储序）+ 两 Store **整库克隆** + 登记表
//!   全文 + 方向光资源 + `level` 段快照 + 编辑器句柄缓存。undo 后世界
//!   dump 与快照时点**逐字符一致**（测试钉住）。Store 记整库而非长度：
//!   undo 截掉的数据 redo 无法凭空恢复（长度不可逆、数据可逆）；快照
//!   世界里 Store 只含图元 / 灯泡网格（glTF 世界清栈、烘焙清栈），体量
//!   有界。
//! - **恢复 = 全删全建**：组件存储是 swap_remove 语义，删除会打乱存储
//!   序——部分重建无法还原 dump 的清单序，故恢复先 despawn 全部可编辑
//!   实体、再按快照序重建（实体编号因此改变：选中态清空、粒子以
//!   `Entity::to_bits` 为种子的运行期表现不保证复原——配置字段在 dump
//!   里逐位一致，运行期状态本就不入 dump）。
//! - **拖拽**：按下（成功拾取）时拍一次，拖动中途不拍——一次拖拽 =
//!   一步。按下后未移动就松开 = 一步 no-op 历史（可 undo 回原状）。
//! - **不入栈**：烘焙与 NavGrid 重建（幂等可重跑，且烘焙新增的 UV2
//!   副本网格与「快照整库恢复」语义冲突——跨烘焙 undo 会把在用的 UV2
//!   副本从库里换掉，故烘焙**清空** undo 栈）。存档 / 参数切换
//!   （Tab / R / C / G）不改世界，同样不入栈。
//!   存档 / 参数切换（Tab / R / C / G）不改世界，同样不入栈。
//! - **挂载根边界**：存在带子实体的可编辑实体（glTF 挂载根）时不拍
//!   快照、清空双栈（恢复挂载子树需重解析文件，b 阶段不做）——含根
//!   世界里 undo 历史恒为空，删除挂载根不可逆。
//!
//! # 光源参数编辑（b 阶段）
//!
//! 模式由选中态决定（[`light_edit_mode`]）：**选中点光实体** → 点光四键
//! （`[` `]` 强度 ×/÷、`-` `=` 范围 ±、`H` 色相 +30°（HSV 纯函数，饱和
//! 度 / 明度保持）、`K` 阴影开关）；**未选中实体** → 方向光五键（`[`
//! `]` 强度、`;` `'` 环境光 ×/÷、`J`/`L` 偏航、`I`/`K` 俯仰，0.05 rad
//! 步进、归一化回写）。全部入 undo 栈。**键位冲突裁决**：`L` 在 R58a
//! 是摆放点光——b 阶段起 L 双语义：未选中实体时 = 方向光偏航 +（规格
//! 指定），有选中实体时 = 摆放点光（R58a 语义保留，摆放后自动选中 ⇒
//! 「连按连摆」的主流流程不受影响）。
//!
//! # 烘焙入口（b 阶段，`B` 键）
//!
//! [`bake_and_install`]：档内 `level.bake` 参数（缺席 = 引擎默认）→
//! `bake_lightmap` 全链 → 产物 IRISLMAP 落盘（`level.bake.artifact` 引用
//! 名，相对档目录解析；无段 = 档同目录 `*.ilm` 默认名）→
//! `install_lightmap` 挂载 → **登记表回补**（照抄 R57 `attach` 语义：
//! UV2 副本句柄继承原句柄来源 ⇒ 烘焙后 `Ctrl+S` 仍写原始 mesh 段、重载
//! 重挂）。烘焙前 despawn 全部瞬态视觉（网格线会进烘焙几何！）、后重建。
//! 挂载换了 MeshHandle 但来源已回补 ⇒ 存档往返闭环；烘焙后继续摆放的
//! 新实体无图集绑定，走 IBL（引擎既有混合语义）。
//!
//! # NavGrid 重建与可视化（b 阶段，`N` 键）
//!
//! [`toggle_nav_viz`]：开 = 重建 + 叠加可视化，关 = 撤掉可视化。重建
//! 优先用档内 `level.nav` 参数；缺席时 [`nav_bounds_from_statics`] 从
//! 世界静态碰撞体 AABB 并集自动推 bounds（纯函数）+ [`NavGridParams::new`]
//! 默认格子参数。可视化 = blocked 格半透明红薄片（复用瞬态线网格，挂
//! [`EditorTransient`]——不入档、不可拾取）+ 域边框线；面板显示格子
//! 总数 / blocked 数（[`nav_blocked_count`] 用 `is_blocked` 逐格统计，
//! 与 nav.rs 语义同源）/ 域尺寸。**每次开都重建**（关再开 = 手动重建）；
//! 之后的编辑不会自动刷新（可视化是重建时点的快照，与 NavGrid 资源
//! 同步陈旧——诚实限制）。
//!
//! # 相机轨道旋转（b 阶段）
//!
//! [`orbit_eye`] 球坐标解析式纯函数：eye 绕 target 转（偏航 `atan2`、
//! 俯仰 `asin`，俯仰钳制 ±89° 防退化），半径解析上恒保持（f32 三角
//! 函数重建下逐位半径只在轴向特例成立——测试用同式解析对照逐位 +
//! 一般情形 1e-5 容差）。中键拖拽 = 轨道旋转；`F` =
//! [`focus_selected`]（target 直设为选中实体世界 AABB 中心）。相机仍
//! 不入档（§8.33⑥ 语义不变）。
//!
//! # 拾取机制选型
//!
//! 刻意**不复用** `iris_core::physics::raycast`：它只查
//! `(Transform, Collider)`，而编辑器摆放的实体可以没有碰撞体（可选挂载），
//! 那样就拾取不到自己刚摆的东西。改用「网格实际包围盒」路线：
//! [`MeshData::bounds()`](iris_engine::prelude::MeshData) 的局部 AABB 经
//! 实体模型矩阵变换成世界 AABB（[`local_aabb_to_world`]），再交给引擎纯
//! 函数 [`ray_box`](iris_core::physics::ray_box)（slab 法，与物理同一份
//! 求交实现——dogfooding）。代价：旋转按轴对齐语义放大包围盒，与引擎
//! `ColliderShape::Box` 忽略旋转的既有简化一致（诚实清单第 3 条）。
//!
//! 命中裁决：取 `t` 最小者；`t` 位相等时取**存储序在前**者（= spawn 序）
//! ——遍历用严格小于比较即天然满足，无须排序、无须 HashMap。
//!
//! # 瞬态视觉不入档
//!
//! 地面参考网格与选中框是纯视觉：它们挂 [`EditorTransient`] 标记，
//! [`save_level`] 在调 `save_scene` **之前全部 despawn**、之后按参数重建
//! （重建只用缓存句柄，不再新占编号）。选此方案而非引擎侧过滤标记的理由：
//! `save_scene` 的跳过名单是 [`GltfMounted`] / [`LevelGenerated`]——两者的
//! 语义都是「该实体由档内某段整体重建」，编辑器视觉不属于任何档内段，
//! 塞进那两个标记会污染语义；为它加第三个引擎级标记则要改引擎的存档
//! 过滤规则（a 阶段不必）。属性面板 HUD 实体不挂 `Transform`，
//! `save_scene` 的查询天然不含它（`world.query::<(Entity, &Transform)>`）。
//!
//! # 确定性选型
//!
//! - 句柄缓存用 `Vec` 线性扫描而非 `HashMap`：键含 `f32`（图元参数）不可
//!   `Hash`，且缓存需要确定的插入序（= 句柄编号序）；条目上界 = 图元种类
//!   × 预设数（≤ 数十），扫描成本可忽略。
//! - 实体遍历一律走 `world.query`（组件存储序 = spawn 序，稳定）；命名
//!   计数（[`next_name`]）取「已有同前缀名的最大下标 + 1」，不依赖任何
//!   跨调用的可变计数器 ⇒ 加载既有档后接着摆放不会撞名。
//! - 吸附 [`snap_value`] 只用除法 / `round` / 乘法（IEEE 确定运算），无
//!   累积状态。
//!
//! # 诚实清单（b 阶段做不到 / 刻意不做）
//!
//! 1. 单选：一次只有一个选中实体（多选 / 框选不在本轮）。
//! 2. **undo 覆盖扁平可编辑世界**：存在带子实体的可编辑实体（glTF 挂载
//!    根）时快照栈清空（见 undo 章节）——挂载根的删除 / 结构变化不可
//!    undo；undo 恢复重建实体 ⇒ 实体编号改变（选中清空、以实体位为
//!    种子的粒子运行期表现不保证复原；配置字段逐位一致）。
//! 3. **烘焙 / NavGrid 不入 undo 栈**（幂等可重跑；烘焙清栈，理由见
//!    undo 章节）；`level` 段（terrain / nav / bake）**只读**：烘焙 /
//!    导航参数只读使用，无数值编辑面板（参数编辑需要 `LevelRegistry`
//!    快照语义升级为现读 + 回写，属 §8.33② 语义变更，单独一轮评审）；
//!    无 bake 段的档烘焙后产物落盘但档内无引用——重载不自动挂载（状态
//!    行提示产物路径，要持久挂载需手写 bake 段）。
//! 4. **NavGrid 可视化是重建时点的快照**：之后的编辑（摆放 / 删除 /
//!    undo）不会自动刷新 blocked 片与统计（关再开 = 重建）；nav 自动
//!    bounds 推导按轴对齐语义忽略旋转（与引擎 `ColliderShape::Box` 的
//!    既有简化一致）。
//! 5. 拾取与选中框都基于**世界 AABB**：旋转后的实体包围盒被放大，拾取
//!    判定比可见网格宽松（逐三角形精确拾取的引擎原语
//!    `ray_triangle_intersection` / `raycast_bvh` 在位，留待需要时接）。
//! 6. 无网格的实体（纯逻辑 / 纯灯光，如手写的 `{"point_light": ...}` 段
//!    而没有 mesh 段）拾取不到——编辑器自己摆的点光带灯泡网格，故可拾取。
//! 7. terrain 派生实体（[`LevelGenerated`]）不可拾取 / 不可删——它们由
//!    `level.terrain` 段重建，改了也会被下次加载覆盖。
//! 8. glTF 实体：内部实体（[`GltfMounted`]）不可拾取；挂载根可拾取 / 可
//!    拖拽 / 可删（删除会连子树一起 despawn），但**不可复制**（复制
//!    glTF 实例树需要重解析文件与重挂载子树）。
//! 9. 属性面板是**只读**位图字体文本（8×8 ASCII 字体，无 IME）：数值修改
//!    走热键（方向键微移 / PgUp·PgDn 缩放 / 光源热键），非 ASCII 字符在
//!    面板里显示为 `?`（不伪装成能显示中文）。
//! 10. 相机不入档（ARCHITECTURE §8.33）：[`open_level`] 恒以编辑器默认
//!     视角接管 `Camera` 资源；编辑器**不挂** `ActiveCamera` 实体（那会把
//!     相机变成有 `Transform` 的实体、进而被 `save_scene` 写进档里）。
//! 11. 摆放只支持四种图元 + 五种无纹理材质预设 + 一种点光（GLB 摆放与
//!     资产浏览器、材质编辑器、地形雕刻在拒绝清单内）。
//! 12. **键位冲突裁决**：`L` 双语义（未选中 = 方向光偏航 +；有选中 =
//!     摆放点光，见光源章节）；无快捷键自定义 / 主题（拒绝清单）。

use std::path::{Path, PathBuf};

use glam::{Mat4, Quat, Vec2, Vec3};
use iris_core::physics::ray_box;
use iris_engine::prelude::*;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 缺省关卡档路径（CLI 未给定时用）。相对路径的解析（`cargo run` 的 cwd
/// = 工作区根 / `cargo test` 的 cwd = crate 目录）由 [`crate::shell`] 经
/// [`asset_path`](iris_engine::prelude::asset_path) 处理。
pub const DEFAULT_LEVEL_ENTRY: &str = "assets/editor_mvp.level.json";

/// 地面参考网格半幅（米）：线在 `-EXTENT..=EXTENT` 上按 [`GRID_STEP`] 排布。
pub const GRID_EXTENT: f32 = 12.0;
/// 网格线间距（米）。
pub const GRID_STEP: f32 = 1.0;
/// 网格线抬升高度（米）：略高于 y = 0 数学地面，避免与关卡地面深度冲突。
pub const GRID_Y: f32 = 0.002;
/// 网格线粗细（米）。
pub const GRID_THICKNESS: f32 = 0.02;
/// 选中框线粗细（米）。
pub const GIZMO_THICKNESS: f32 = 0.03;
/// 选中框相对实体世界 AABB 的外扩量（米）：避免与实体表面深度冲突。
pub const GIZMO_MARGIN: f32 = 0.015;
/// 摆放 / 拾取用的数学地面高度（米）。
pub const GROUND_Y: f32 = 0.0;
/// 网格吸附步长缺省值（米）。
pub const DEFAULT_SNAP: f32 = 0.5;
/// 方向键微移步长（米）；按住 Shift 为十倍。
pub const NUDGE_STEP: f32 = 0.1;
/// PgUp / PgDn 的缩放倍率（每次 ×1.1 / ÷1.1）。
pub const SCALE_STEP: f32 = 1.1;
/// 缩放下限（逐轴）：防止缩到 0 / 负值后 AABB 退化、射线求交失真。
pub const MIN_SCALE: f32 = 0.01;
/// 静态盒碰撞体的最小半厚（米）：平面网格无厚度，退化盒在物理侧无意义。
pub const MIN_COLLIDER_HALF_THICKNESS: f32 = 0.05;
/// 属性面板 HUD 实体名（无 `Transform`，天然不入档）。
pub const HUD_NAME: &str = "editor/hud";
/// 拾取 / 摆放射线的最大距离（米）。
pub const PICK_MAX_T: f32 = 1000.0;
/// 射线与平面平行判定阈值（方向分量绝对值下限）。
pub const RAY_PARALLEL_EPS: f32 = 1e-6;
/// 点光实体的灯光颜色（线性空间，暖白）——摆放时的初值（编辑走光源
/// 热键，见模块文档「光源参数编辑」）。
pub const LIGHT_COLOR: [f32; 3] = [1.0, 0.82, 0.58];
/// 点光实体的强度（a 阶段固定）。
pub const LIGHT_INTENSITY: f32 = 6.0;
/// 点光实体的有效半径（米，a 阶段固定）。
pub const LIGHT_RANGE: f32 = 10.0;
/// 点光实体的灯泡网格（小球，让点光在视口里可见且可拾取）。
pub const LIGHT_BULB: PrimitiveSection = PrimitiveSection::Sphere {
    radius: 0.12,
    segments: 16,
    rings: 10,
};

// ---- undo / redo（b 阶段）----

/// undo 栈深度（环形丢弃最旧）。
pub const UNDO_LIMIT: usize = 32;

// ---- 光源参数编辑（b 阶段）----

/// 点光 / 方向光强度的乘法步长（`]` ×1.25、`[` ÷1.25——乘法覆盖任意量级，
/// 除法实现而非 `powi`，IEEE 确定运算）。
pub const LIGHT_INTENSITY_STEP: f32 = 1.25;
/// 点光强度下限（连续除法的地板）。
pub const LIGHT_INTENSITY_MIN: f32 = 0.05;
/// 点光范围步长（`=` +0.5 m、`-` −0.5 m）。
pub const LIGHT_RANGE_STEP: f32 = 0.5;
/// 点光范围下限（灯光半径为 0 无意义）。
pub const LIGHT_RANGE_MIN: f32 = 0.5;
/// `H` 色相旋转步长（度，HSV 纯函数，饱和度 / 明度保持）。
pub const HUE_STEP_DEG: f32 = 30.0;
/// 方向光强度乘法步长（`[` `]`）。
pub const SUN_INTENSITY_STEP: f32 = 1.1;
/// 方向光环境光乘法步长（`;` `'`——三通道同乘，保持环境光色比）。
pub const SUN_AMBIENT_STEP: f32 = 1.1;
/// 方向光偏航 / 俯仰步长（弧度，`J`/`L`/`I`/`K`）。
pub const SUN_DIR_STEP: f32 = 0.05;
/// 方向光俯仰限位（±89°，防 atan2 在竖直方向退化）。
pub const SUN_PITCH_LIMIT: f32 = 89.0f32.to_radians();

// ---- 相机轨道（b 阶段）----

/// 中键轨道旋转灵敏度（弧度 / 像素）。
pub const ORBIT_SENSITIVITY: f32 = 0.005;
/// 轨道俯仰限位（±89°，防 gimbal 退化——eye 贴竖直轴时 yaw 无定义）。
pub const ORBIT_PITCH_LIMIT: f32 = 89.0f32.to_radians();

// ---- NavGrid 可视化（b 阶段）----

/// blocked 格红片厚度（米）：薄盒浮在可站立表面上。
pub const NAV_SLAB_THICKNESS: f32 = 0.04;
/// blocked 格红片相对表面的抬升（米，避免与表面深度冲突）。
pub const NAV_SLAB_LIFT: f32 = 0.03;
/// 导航域边框线粗细（米）。
pub const NAV_BORDER_THICKNESS: f32 = 0.06;
/// 导航域边框线高度（米，沿域四周画一圈）。
pub const NAV_BORDER_Y: f32 = 0.05;

// ---------------------------------------------------------------------------
// 摆放类型与材质预设
// ---------------------------------------------------------------------------

/// 可摆放的图元类型（`Tab` 循环切换）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    /// 单位立方体。
    Cube,
    /// 4 m 见方的地面砖（`plane`）。
    Plane,
    /// 半径 0.5 的 UV 球。
    Sphere,
    /// 半径 0.4、高 1.6 的圆柱。
    Cylinder,
}

impl PlaceKind {
    /// 循环顺序（`Tab` 依此推进）。
    pub const ALL: [PlaceKind; 4] = [Self::Cube, Self::Plane, Self::Sphere, Self::Cylinder];

    /// 名字前缀（实体命名 `{label}/{n}`）与面板显示用标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Cube => "cube",
            Self::Plane => "plane",
            Self::Sphere => "sphere",
            Self::Cylinder => "cylinder",
        }
    }

    /// 对应的关卡档图元段（= 存档里写出的 mesh 段，也是登记表登记的来源）。
    pub fn section(self) -> PrimitiveSection {
        match self {
            Self::Cube => PrimitiveSection::Cube,
            Self::Plane => PrimitiveSection::Plane { size: 4.0 },
            Self::Sphere => PrimitiveSection::Sphere {
                radius: 0.5,
                segments: 24,
                rings: 16,
            },
            Self::Cylinder => PrimitiveSection::Cylinder {
                radius: 0.4,
                height: 1.6,
                segments: 24,
            },
        }
    }

    /// 循环切换到下一种图元。
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// 材质预设（摆放用）：全部**无纹理、不透明**——数值材质往返无损
/// （`save_scene` 只写数值字段，不触发「纹理无来源登记」告警）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialPreset {
    /// 面板显示名（ASCII）。
    pub name: &'static str,
    /// 基础色（线性 RGBA）。
    pub base_color: [f32; 4],
    /// 金属度。
    pub metallic: f32,
    /// 粗糙度。
    pub roughness: f32,
    /// 自发光（RGB）。
    pub emissive: [f32; 3],
}

impl MaterialPreset {
    /// 转引擎材质定义（无纹理：基色槽走 0 号白色兜底句柄）。
    pub fn def(&self) -> MaterialDef {
        MaterialDef {
            base_color: self.base_color,
            emissive: [self.emissive[0], self.emissive[1], self.emissive[2], 1.0],
            metallic: self.metallic,
            roughness: self.roughness,
            ..Default::default()
        }
    }
}

/// 材质预设表（`R` 循环切换）。**各项的 [`MaterialDef`] 位模式互不相同**
/// （单测钉住）：`load_scene` 侧按位模式去重材质，若两个预设位相同则重载
/// 后会并成一个句柄，句柄编号对齐不变量随之破裂。
pub const MATERIAL_PRESETS: &[MaterialPreset] = &[
    MaterialPreset {
        name: "stone",
        base_color: [0.62, 0.62, 0.60, 1.0],
        metallic: 0.0,
        roughness: 0.85,
        emissive: [0.0; 3],
    },
    MaterialPreset {
        name: "brick",
        base_color: [0.55, 0.30, 0.24, 1.0],
        metallic: 0.0,
        roughness: 0.80,
        emissive: [0.0; 3],
    },
    MaterialPreset {
        name: "metal",
        base_color: [0.75, 0.77, 0.80, 1.0],
        metallic: 0.90,
        roughness: 0.35,
        emissive: [0.0; 3],
    },
    MaterialPreset {
        name: "wood",
        base_color: [0.45, 0.32, 0.19, 1.0],
        metallic: 0.0,
        roughness: 0.70,
        emissive: [0.0; 3],
    },
    MaterialPreset {
        name: "glow",
        base_color: [0.95, 0.85, 0.45, 1.0],
        metallic: 0.0,
        roughness: 0.40,
        emissive: [1.60, 1.20, 0.40],
    },
];

/// 点光灯泡材质（自发光暖白，与预设表分开——它不参与 `R` 循环）。
fn bulb_def() -> MaterialDef {
    MaterialDef {
        base_color: [1.0, 0.92, 0.72, 1.0],
        emissive: [3.0, 2.2, 1.2, 1.0],
        metallic: 0.0,
        roughness: 0.3,
        ..Default::default()
    }
}

/// 内容材质的缓存键（预设下标 / 灯泡两种来源）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentMaterial {
    /// [`MATERIAL_PRESETS`] 下标。
    Preset(usize),
    /// 点光灯泡。
    Bulb,
}

impl ContentMaterial {
    fn def(self) -> MaterialDef {
        match self {
            Self::Preset(index) => MATERIAL_PRESETS
                .get(index)
                .copied()
                .unwrap_or(MATERIAL_PRESETS[0])
                .def(),
            Self::Bulb => bulb_def(),
        }
    }
}

// ---------------------------------------------------------------------------
// 瞬态视觉标记与编辑器状态
// ---------------------------------------------------------------------------

/// 编辑器瞬态视觉实体标记（地面参考网格 / 选中框）：**不得进存档**。
///
/// [`editable_entities`] 据此排除它们，[`save_level`] 在写文件前把它们
/// 全部 despawn、写完再重建（机制与理由见模块文档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Component)]
pub struct EditorTransient;

/// 瞬态视觉资产句柄（网格 1 个 + 材质 6 个，[`open_level`] 一次创建后
/// 终身复用——重建网格 / 选中框 / Nav 可视化不再新占句柄编号）。
///
/// b 阶段新增 2 个 Nav 可视化材质：**必须在关卡内容之前创建**（「句柄
/// 编号对齐」不变量——`open_level` 两侧——编辑侧与重载侧——都经同一条
/// 路径建这些资产，内容句柄的起点才逐位一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TransientAssets {
    /// 线段网格（单位立方体按 scale 拉成细条；Nav 红片也复用它）。
    line_mesh: MeshHandle,
    /// 普通网格线（灰）。
    grid: MaterialHandle,
    /// X 轴（z = 0 那条线，红）。
    axis_x: MaterialHandle,
    /// Z 轴（x = 0 那条线，蓝）。
    axis_z: MaterialHandle,
    /// 选中框（自发光黄）。
    gizmo: MaterialHandle,
    /// Nav blocked 格（半透明红薄片，b 阶段）。
    nav_block: MaterialHandle,
    /// 导航域边框（橙线，b 阶段）。
    nav_border: MaterialHandle,
}

/// 编辑器状态（World 资源）：选择集 / 摆放类型 / 吸附开关等交互状态 +
/// 句柄缓存 + 瞬态实体清单。由 [`open_level`] 插入。
///
/// 公开字段即 a 阶段的编辑参数（交互壳只读它们做输入翻译、只经命令层
/// 函数改它们）；缓存与瞬态清单是私有的——它们承载「句柄编号对齐」
/// 不变量，绕过命令层直接改会破坏往返一致性。
#[derive(Resource)]
pub struct EditorState {
    /// 当前关卡档路径（`Ctrl+S` 写这里，启动时存在则读这里）。
    pub level_path: PathBuf,
    /// 当前选中实体（a 阶段单选；`None` = 无选中）。
    pub selected: Option<Entity>,
    /// 当前摆放图元（`Tab` 循环）。
    pub place_kind: PlaceKind,
    /// 当前材质预设下标（`R` 循环，索引 [`MATERIAL_PRESETS`]）。
    pub preset: usize,
    /// 摆放时是否挂静态碰撞体（`C` 开关）。
    pub place_collider: bool,
    /// 网格吸附开关（`G` 开关；对摆放与拖拽同时生效）。
    pub snap: bool,
    /// 吸附步长（米）。
    pub snap_size: f32,
    /// 最近一次操作的反馈行（属性面板底部显示）。
    pub status: String,
    /// 是否正在拖拽（左键按住且已拾取到实体）。
    pub dragging: bool,
    /// 拖拽平面的高度（= 拾取命中点的 y，规格「射线 ∩ y = hit 平面」）。
    pub drag_plane_y: f32,
    /// 拖拽偏移（实体枢轴 − 命中点，仅 x / z）：保持抓取点跟着光标，
    /// 避免按下瞬间实体跳到光标下。
    pub drag_offset: Vec3,
    /// undo 栈（快照 = 写操作执行前的世界；深度 [`UNDO_LIMIT`]，环形丢弃
    /// 最旧）。私有：只经 [`undo`] / [`redo`] / [`push_undo`] 消费。
    undo_stack: Vec<WorldSnapshot>,
    /// redo 栈（undo 弹出的「当前状态」；任何新写操作清空）。
    redo_stack: Vec<WorldSnapshot>,
    /// Nav 可视化开关（`N` 切换；开着时每次开都伴随一次重建）。
    nav_viz_on: bool,
    /// Nav 可视化实体（blocked 红片 + 域边框，[`EditorTransient`]）。
    nav_viz: Vec<Entity>,
    /// 关卡内容网格缓存（同源复用，见模块文档「句柄编号对齐」）。
    content_meshes: Vec<(PrimitiveSection, MeshHandle)>,
    /// 关卡内容材质缓存（同上）。
    content_materials: Vec<(ContentMaterial, MaterialHandle)>,
    /// 瞬态视觉资产句柄（[`open_level`] 创建）。
    transient: Option<TransientAssets>,
    /// 当前网格线实体（存储序 = 生成序）。
    grid: Vec<Entity>,
    /// 当前选中框实体（恒 12 条边，或空）。
    gizmo: Vec<Entity>,
}

impl EditorState {
    /// 以关卡档路径新建状态（其余字段取 a 阶段缺省：立方体 / stone 预设 /
    /// 挂碰撞体 / 吸附 0.5 m 开）。
    pub fn new(level_path: impl Into<PathBuf>) -> Self {
        Self {
            level_path: level_path.into(),
            selected: None,
            place_kind: PlaceKind::Cube,
            preset: 0,
            place_collider: true,
            snap: true,
            snap_size: DEFAULT_SNAP,
            status: String::from("READY"),
            dragging: false,
            drag_plane_y: GROUND_Y,
            drag_offset: Vec3::ZERO,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            nav_viz_on: false,
            nav_viz: Vec::new(),
            content_meshes: Vec::new(),
            content_materials: Vec::new(),
            transient: None,
            grid: Vec::new(),
            gizmo: Vec::new(),
        }
    }
}

/// [`open_level`] 的结果报告。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenReport {
    /// 关卡档是否存在并被加载（`false` = 从空世界开始）。
    pub loaded: bool,
    /// 加载出的实体数（空世界为 0）。
    pub entities: usize,
    /// 可编辑实体总数（= `save_scene` 会写出的实体数）。
    pub editable: usize,
}

// ---------------------------------------------------------------------------
// 开关卡
// ---------------------------------------------------------------------------

/// 打开关卡档（编辑器 Startup 的唯一数据入口）。
///
/// 顺序有讲究（= 模块文档的「句柄编号对齐」不变量）：状态 → 兜底资源 →
/// 编辑器基线资源（相机恒接管 / 方向光缺席才插）→ 瞬态视觉资产（网格与
/// HUD，占句柄序与实体序前段）→ 关卡档加载（存在才读；v2 直读，`level`
/// 段经 [`LevelRegistry`] 快照语义原样保留）。
///
/// 同一世界只应调用一次：重复调用会重置句柄缓存（缓存与 `MeshStore`
/// 实际内容脱钩后，同源复用失效 ⇒ 往返句柄编号不再对齐）。
pub fn open_level(world: &mut World, level_path: &Path) -> Result<OpenReport, SceneError> {
    let exists = level_path.exists();
    world.insert_resource(EditorState::new(level_path.to_path_buf()));
    ensure_stores(world);
    ensure_baseline(world);
    // 瞬态视觉的**资产句柄**（Store 条目，非实体）必须先于内容创建
    // ——「句柄编号对齐」不变量，与 load_scene 侧一致。
    build_transient_assets(world);
    // **内容实体先行、瞬态家具（网格线 / HUD）殿后**（R58b）：烘焙产物
    // 按 `Entity::to_bits` 引用实体，编辑器世界的位布局必须与
    // `load_scene` 的规范布局（内容实体 0 起连续）一致，否则产物在
    // 任何一侧重载都会「失配报错」（R57 语义）。家具实体排在内容
    // 之后、只在瞬态槽位间复用，内容位从此跨存 / 载稳定。
    let entities = if exists {
        load_scene(world, level_path)?.len()
    } else {
        // 新建会话也要有 level 段快照（load_scene 恒插入该资源）：
        // save_scene 据此写 level 段（空段省略），往返两侧资源集一致
        world.insert_resource(LevelRegistry {
            section: LevelSection::default(),
        });
        0
    };
    rebuild_grid(world);
    spawn_hud(world);
    let editable = editable_entities(world).len();
    let status = if exists {
        format!("OPENED {} ({} entities)", file_label(level_path), entities)
    } else {
        format!("NEW {} (empty)", file_label(level_path))
    };
    set_status(world, status);
    refresh_hud(world);
    Ok(OpenReport {
        loaded: exists,
        entities,
        editable,
    })
}

/// 保存关卡档（`Ctrl+S`）：瞬态视觉先 despawn → `save_scene` → 重建。
///
/// 返回写出的实体数。失败时同样重建视觉（编辑器不因存档失败而失去网格 /
/// 选中框），错误经 `log::error` 与面板状态行双通道报告。
pub fn save_level(world: &mut World) -> Result<usize, SceneError> {
    let path = {
        world
            .try_resource::<EditorState>()
            .map(|state| state.level_path.clone())
            .unwrap_or_default()
    };
    clear_transient(world);
    let result = save_scene(world, &path);
    rebuild_grid(world);
    rebuild_gizmo(world);
    rebuild_nav_viz(world);
    match &result {
        Ok(count) => set_status(
            world,
            format!("SAVED {} entities -> {}", count, file_label(&path)),
        ),
        Err(err) => {
            log::error!("关卡档保存失败（{}）：{err}", path.display());
            set_status(world, format!("SAVE FAILED: {err}"));
        }
    }
    refresh_hud(world);
    result
}

/// 兜底插入存档读写所需的资源（缺席才插——与 `RenderPlugin` /
/// `load_scene` 同款语义，headless 世界无须先建插件）。
fn ensure_stores(world: &mut World) {
    if !world.contains_resource::<MeshStore>() {
        world.insert_resource(MeshStore::new());
    }
    if !world.contains_resource::<MaterialStore>() {
        world.insert_resource(MaterialStore::new());
    }
    if !world.contains_resource::<TextureStore>() {
        world.insert_resource(TextureStore::new());
    }
    if !world.contains_resource::<SceneAssetRegistry>() {
        world.insert_resource(SceneAssetRegistry::default());
    }
}

/// 编辑器基线资源：相机恒以默认视角接管（相机不入档，模块文档诚实清单
/// 第 8 条），方向光缺席才插（关卡档环境段优先）。
fn ensure_baseline(world: &mut World) {
    world.insert_resource(Camera {
        eye: Vec3::new(9.0, 9.5, 12.0),
        target: Vec3::new(0.0, 0.6, 0.0),
        up: Vec3::Y,
        ..Camera::default()
    });
    if !world.contains_resource::<DirectionalLight>() {
        world.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.35, -0.85, -0.4).normalize_or_zero(),
            color: Vec3::new(1.0, 0.95, 0.88),
            intensity: 1.15,
            ambient: Vec3::new(0.24, 0.25, 0.28),
            ..DirectionalLight::default()
        });
    }
}

/// 路径的面板显示标签（文件名部分，非 ASCII 折成 `?`）。
fn file_label(path: &Path) -> String {
    ascii_only(
        &path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
    )
}

// ---------------------------------------------------------------------------
// 内容资产：同源复用 + 登记表登记
// ---------------------------------------------------------------------------

/// 关卡内容网格句柄（同源复用 + 登记 [`MeshRef::Primitive`]）。
///
/// 生成失败（图元分段数低于生成器下限——预设常量不会触发）记
/// `log::error` 并返回 `None`：摆放降级为「什么都没发生」，编辑器不 panic。
fn content_mesh(world: &mut World, section: &PrimitiveSection) -> Option<MeshHandle> {
    if let Some(handle) = cached_mesh(world, section) {
        return Some(handle);
    }
    let data = match section.to_mesh_data() {
        Ok(data) => data,
        Err(err) => {
            log::error!("图元网格生成失败（{section:?}）：{err}");
            return None;
        }
    };
    let handle = world.resource_mut::<MeshStore>().add(data);
    // 登记表同步登记：save_scene 据此还原 mesh 段（模块文档前提 1）
    world
        .resource_mut::<SceneAssetRegistry>()
        .meshes
        .insert(handle.0, MeshRef::Primitive(section.clone()));
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.content_meshes.push((section.clone(), handle));
    }
    Some(handle)
}

/// 缓存查询：同一图元段只入 `MeshStore` 一次（线性扫描，见模块文档
/// 「确定性选型」）。
fn cached_mesh(world: &World, section: &PrimitiveSection) -> Option<MeshHandle> {
    let state = world.try_resource::<EditorState>()?;
    state
        .content_meshes
        .iter()
        .find(|(cached, _)| cached == section)
        .map(|(_, handle)| *handle)
}

/// 关卡内容材质句柄（同源复用 + 登记 [`MaterialRef::default()`]）。
fn content_material(world: &mut World, key: ContentMaterial) -> MaterialHandle {
    if let Some(state) = world.try_resource::<EditorState>() {
        if let Some((_, handle)) = state.content_materials.iter().find(|(k, _)| *k == key) {
            return *handle;
        }
    }
    let handle = world.resource_mut::<MaterialStore>().add(key.def());
    // 登记「无外部纹理来源」：与加载侧对无纹理材质段的登记逐字段相同，
    // 于是往返两侧的登记表也一致（对照测试直接比较登记表）
    world
        .resource_mut::<SceneAssetRegistry>()
        .materials
        .insert(handle.0, MaterialRef::default());
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.content_materials.push((key, handle));
    }
    handle
}

// ---------------------------------------------------------------------------
// 摆放
// ---------------------------------------------------------------------------

/// 在当前摆放点摆一个图元实体（当前图元类型 + 当前材质预设 + 可选静态
/// 碰撞体），返回新实体；摆放后**自动选中**（便于立刻拖拽 / 微调）。
///
/// 落点规则：`point` 是命中点（[`placement_point`] 的输出），实体沿 +Y
/// 抬升「网格局部包围盒底面到枢轴的距离」⇒ 底面正好落在命中点上
/// （地面上摆放 = 立在地面，实体顶上摆放 = 叠上去）。吸附开启时 x / z
/// 吸附到 [`EditorState::snap_size`] 网格，**y 不吸附**（保持贴合表面）。
pub fn place_at(world: &mut World, point: Vec3) -> Option<Entity> {
    let (kind, preset, with_collider, snap, snap_size) = {
        let state = world.try_resource::<EditorState>()?;
        (
            state.place_kind,
            ContentMaterial::Preset(state.preset),
            state.place_collider,
            state.snap,
            state.snap_size,
        )
    };
    push_undo(world);
    let section = kind.section();
    let mesh = content_mesh(world, &section)?;
    let material = content_material(world, preset);
    let scale = Vec3::ONE;
    let position = rest_position(world, point, mesh, scale, snap, snap_size);
    let name = next_name(world, kind.label());
    let entity = world.spawn((
        Transform {
            position,
            rotation: Quat::IDENTITY,
            scale,
        },
        mesh,
        material,
        Name(name.clone()),
    ));
    if with_collider {
        if let Some(collider) = collider_of(world, &section, mesh, scale) {
            world.insert(entity, collider);
        }
    }
    set_status(
        world,
        format!(
            "PLACED {} at {:.2} {:.2} {:.2}",
            name, position.x, position.y, position.z
        ),
    );
    select(world, Some(entity));
    Some(entity)
}

/// 在当前摆放点摆一个点光实体（灯泡网格 + 自发光材质 + [`PointLight`]），
/// 返回新实体；摆放后自动选中。灯光初值 = [`LIGHT_COLOR`] /
/// [`LIGHT_INTENSITY`] / [`LIGHT_RANGE`]（编辑走光源热键，见模块文档
/// 「光源参数编辑」）。
pub fn place_light_at(world: &mut World, point: Vec3) -> Option<Entity> {
    let (snap, snap_size) = {
        let state = world.try_resource::<EditorState>()?;
        (state.snap, state.snap_size)
    };
    push_undo(world);
    let mesh = content_mesh(world, &LIGHT_BULB)?;
    let material = content_material(world, ContentMaterial::Bulb);
    let position = rest_position(world, point, mesh, Vec3::ONE, snap, snap_size);
    let name = next_name(world, "light");
    let entity = world.spawn((
        Transform {
            position,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        },
        mesh,
        material,
        Name(name.clone()),
        PointLight {
            color: Vec3::from(LIGHT_COLOR),
            intensity: LIGHT_INTENSITY,
            range: LIGHT_RANGE,
            shadows: false,
        },
    ));
    set_status(
        world,
        format!(
            "PLACED {} at {:.2} {:.2} {:.2}",
            name, position.x, position.y, position.z
        ),
    );
    select(world, Some(entity));
    Some(entity)
}

/// 射线版摆放：命中点由 [`placement_point`] 求出（实体表面或 y = 0 数学
/// 地面，取最近者）。无命中返回 `None`（例如射线朝天）。
pub fn place_from_ray(world: &mut World, origin: Vec3, dir: Vec3) -> Option<Entity> {
    let point = placement_point(world, origin, dir)?;
    place_at(world, point)
}

/// 射线版点光摆放（同 [`place_from_ray`] 的命中规则）。
pub fn place_light_from_ray(world: &mut World, origin: Vec3, dir: Vec3) -> Option<Entity> {
    let point = placement_point(world, origin, dir)?;
    place_light_at(world, point)
}

/// 摆放落点：命中点 + 底面抬升（+ 可选 x / z 吸附）。
fn rest_position(
    world: &World,
    point: Vec3,
    mesh: MeshHandle,
    scale: Vec3,
    snap: bool,
    snap_size: f32,
) -> Vec3 {
    let mut position = point + Vec3::new(0.0, rest_lift(world, mesh, scale), 0.0);
    if snap {
        position.x = snap_value(position.x, snap_size);
        position.z = snap_value(position.z, snap_size);
    }
    position
}

/// 底面抬升量：网格局部包围盒底面（min.y）到枢轴的距离 × scale.y。
/// 单位立方体 = 0.5、平面 = 0、半径 0.5 的球 = 0.5。
fn rest_lift(world: &World, mesh: MeshHandle, scale: Vec3) -> f32 {
    let store = world.try_resource::<MeshStore>();
    let Some(data) = store.as_deref().and_then(|store| store.get(mesh)) else {
        return 0.0;
    };
    match data.bounds() {
        Some((min, _)) => -min[1] * scale.y,
        None => 0.0,
    }
}

/// 实体的静态碰撞体（可选挂载）：球体图元 → 静态球（半径 × scale.x），
/// 其余 → 静态 AABB 盒（**网格实际包围盒**半幅 × scale 逐轴）。盒半厚小于
/// [`MIN_COLLIDER_HALF_THICKNESS`] 时抬到该阈值（平面网格无厚度，退化盒
/// 在物理侧无意义）。半幅取网格包围盒而非按图元参数重算——碰撞体与可见
/// 网格同源，参数改动不会两处漂移。
pub fn collider_for(
    section: &PrimitiveSection,
    bounds: ([f32; 3], [f32; 3]),
    scale: Vec3,
) -> Collider {
    let (min, max) = bounds;
    match section {
        PrimitiveSection::Sphere { radius, .. } => Collider::static_sphere(*radius * scale.x),
        _ => {
            let mut half = (Vec3::from(max) - Vec3::from(min)) * 0.5 * scale;
            if half.y < MIN_COLLIDER_HALF_THICKNESS {
                half.y = MIN_COLLIDER_HALF_THICKNESS;
            }
            Collider::static_box(half)
        }
    }
}

/// 从 `MeshStore` 取包围盒再算碰撞体（[`collider_for`] 的 World 版）。
fn collider_of(
    world: &World,
    section: &PrimitiveSection,
    mesh: MeshHandle,
    scale: Vec3,
) -> Option<Collider> {
    let store = world.try_resource::<MeshStore>();
    let bounds = store
        .as_deref()
        .and_then(|store| store.get(mesh))?
        .bounds()?;
    Some(collider_for(section, bounds, scale))
}

/// 下一个可用实体名：`{prefix}/{n}`，n = 已有同前缀名的最大下标 + 1
/// （无则 1）。刻意扫描世界而不维护计数器——加载既有档后接着摆放不会
/// 与档内已有名字相撞。遍历序 = 组件存储序（确定）。
fn next_name(world: &World, prefix: &str) -> String {
    let mut max = 0u32;
    world.query::<&Name>(|name| {
        if let Some(rest) = name
            .0
            .strip_prefix(prefix)
            .and_then(|r| r.strip_prefix('/'))
        {
            if let Ok(index) = rest.parse::<u32>() {
                max = max.max(index);
            }
        }
    });
    format!("{prefix}/{}", max + 1)
}

// ---------------------------------------------------------------------------
// 拾取
// ---------------------------------------------------------------------------

/// 可编辑实体清单（= `save_scene` 会写进文件的实体集）：有 `Transform`，
/// 且无 [`EditorTransient`] / [`GltfMounted`] / [`LevelGenerated`] 标记。
/// 顺序 = 组件存储序（= spawn 序，稳定）。
///
/// 与 `save_scene` 的跳过名单保持同语义（模块文档「瞬态视觉不入档」）：
/// 前两个标记由引擎过滤，`EditorTransient` 由本模块在存前 despawn。
pub fn editable_entities(world: &World) -> Vec<Entity> {
    let mut out = Vec::new();
    world.query::<(Entity, &Transform)>(|(entity, _)| {
        if world.get::<EditorTransient>(entity).is_some()
            || world.get::<GltfMounted>(entity).is_some()
            || world.get::<LevelGenerated>(entity).is_some()
        {
            return;
        }
        out.push(entity);
    });
    out
}

/// 射线拾取：返回最近命中的可编辑实体与命中点。
///
/// 裁决规则见模块文档「拾取机制选型」：`t` 最小者胜，`t` 位相等取存储序
/// 在前者。`dir` 内部归一化（零向量 → `None`）。
pub fn pick_with_hit(world: &World, origin: Vec3, dir: Vec3) -> Option<(Entity, Vec3)> {
    let (_, entity, point) = pick_ray(world, origin, dir)?;
    Some((entity, point))
}

/// 射线拾取（只取实体）。
pub fn pick(world: &World, origin: Vec3, dir: Vec3) -> Option<Entity> {
    pick_ray(world, origin, dir).map(|(_, entity, _)| entity)
}

/// 拾取内核：返回 `(t, 实体, 命中点)`。
fn pick_ray(world: &World, origin: Vec3, dir: Vec3) -> Option<(f32, Entity, Vec3)> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    let mut best: Option<(f32, Entity, Vec3)> = None;
    for entity in editable_entities(world) {
        let Some((min, max)) = world_aabb_of(world, entity) else {
            continue;
        };
        let center = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        if let Some((t, _)) = ray_box(origin, dir, center, &half) {
            // 严格小于：同距时保留存储序在前的实体（确定性裁决）
            if (0.0..=PICK_MAX_T).contains(&t) && best.is_none_or(|(best_t, _, _)| t < best_t) {
                best = Some((t, entity, origin + dir * t));
            }
        }
    }
    best
}

/// 摆放命中点：射线与「可编辑实体世界 AABB」或「y = [`GROUND_Y`] 数学
/// 地面」的交点，取 `t` 最小者。两者都没有（射线朝天 / 与地面平行且无
/// 实体命中）返回 `None`。
pub fn placement_point(world: &World, origin: Vec3, dir: Vec3) -> Option<Vec3> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    let mut best_t = f32::MAX;
    if let Some((t, _, _)) = pick_ray(world, origin, dir) {
        best_t = t;
    }
    if dir.y < -RAY_PARALLEL_EPS {
        let t = (GROUND_Y - origin.y) / dir.y;
        if (0.0..=PICK_MAX_T).contains(&t) {
            best_t = best_t.min(t);
        }
    }
    (best_t < f32::MAX).then(|| origin + dir * best_t)
}

/// 实体的世界 AABB（由网格局部包围盒经模型矩阵变换得到）。无 `Transform`
/// / 无网格 / 网格无顶点 → `None`。
pub fn world_aabb_of(world: &World, entity: Entity) -> Option<(Vec3, Vec3)> {
    let transform = *world.get::<Transform>(entity)?;
    let handle = *world.get::<MeshHandle>(entity)?;
    let store = world.try_resource::<MeshStore>()?;
    let (min, max) = store.get(handle)?.bounds()?;
    Some(local_aabb_to_world(min, max, &transform.matrix()))
}

/// 局部 AABB 的 8 角经模型矩阵变换后重取世界轴对齐包围盒（纯函数）。
///
/// iris-render 的 `occlusion::world_aabb` 是同语义实现，但为 `pub(crate)`
/// ——此处按同一算法重写（旋转后 AABB 会放大，轴对齐语义使然）。
pub fn local_aabb_to_world(min: [f32; 3], max: [f32; 3], model: &Mat4) -> (Vec3, Vec3) {
    let mut out_min = Vec3::splat(f32::MAX);
    let mut out_max = Vec3::splat(f32::MIN);
    for corner in 0..8 {
        let point = Vec3::new(
            if corner & 1 == 0 { min[0] } else { max[0] },
            if corner & 2 == 0 { min[1] } else { max[1] },
            if corner & 4 == 0 { min[2] } else { max[2] },
        );
        let transformed = model.transform_point3(point);
        out_min = out_min.min(transformed);
        out_max = out_max.max(transformed);
    }
    (out_min, out_max)
}

// ---------------------------------------------------------------------------
// 选择 / 拖拽
// ---------------------------------------------------------------------------

/// 设置选中实体（`None` = 清空选择），并重建选中框与属性面板。
pub fn select(world: &mut World, entity: Option<Entity>) {
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.selected = entity;
    }
    rebuild_gizmo(world);
    refresh_hud(world);
}

/// 按下左键：拾取 → 选中 → 进入拖拽。返回被拾取的实体（未命中返回
/// `None`，此时**不清空**选择——点空白处取消选择由交互壳显式调
/// [`select`]`(world, None)` 决定）。
///
/// 拖拽平面 = 命中点高度的水平面（规格「射线 ∩ y = hit 平面」）；
/// 拖拽偏移 = 实体枢轴 − 命中点（仅 x / z），使抓取点跟着光标走。
pub fn begin_drag(world: &mut World, origin: Vec3, dir: Vec3) -> Option<Entity> {
    let (_, entity, point) = pick_ray(world, origin, dir)?;
    let pivot = world.get::<Transform>(entity).map(|tr| tr.position)?;
    let name = name_of(world, entity);
    // 拖拽快照语义：按下时拍一次（拖动中途不拍——一次拖拽 = 一步）
    push_undo(world);
    select(world, Some(entity));
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.dragging = true;
        state.drag_plane_y = point.y;
        state.drag_offset = Vec3::new(pivot.x - point.x, 0.0, pivot.z - point.z);
    }
    set_status(world, format!("DRAG {name}"));
    Some(entity)
}

/// 拖拽中：射线 ∩ 拖拽平面 → 新位置（x / z 写入，**y 保持**——拖拽是
/// 水平移动；吸附开启时 x / z 吸附）。返回新位置，未在拖拽 / 射线与平面
/// 平行 / 交点在射线反向时返回 `None`（实体不动）。
pub fn drag_to(world: &mut World, origin: Vec3, dir: Vec3) -> Option<Vec3> {
    let (entity, plane_y, offset, snap, snap_size) = {
        let state = world.try_resource::<EditorState>()?;
        if !state.dragging {
            return None;
        }
        (
            state.selected?,
            state.drag_plane_y,
            state.drag_offset,
            state.snap,
            state.snap_size,
        )
    };
    let dir = dir.normalize_or_zero();
    if dir.y.abs() <= RAY_PARALLEL_EPS {
        return None;
    }
    let t = (plane_y - origin.y) / dir.y;
    if !(0.0..=PICK_MAX_T).contains(&t) {
        return None;
    }
    let hit = origin + dir * t;
    let mut x = hit.x + offset.x;
    let mut z = hit.z + offset.z;
    if snap {
        x = snap_value(x, snap_size);
        z = snap_value(z, snap_size);
    }
    let mut transform = world.get_mut::<Transform>(entity)?;
    let mut next = *transform;
    // y 保持：拖拽是水平移动（高度由摆放 / 微移决定）
    next.position.x = x;
    next.position.z = z;
    *transform = next;
    let position = next.position;
    drop(transform);
    update_gizmo(world);
    Some(position)
}

/// 松开左键：结束拖拽。
pub fn end_drag(world: &mut World) {
    let was_dragging = {
        let Some(mut state) = world.try_resource_mut::<EditorState>() else {
            return;
        };
        let was = state.dragging;
        state.dragging = false;
        state.drag_offset = Vec3::ZERO;
        was
    };
    if was_dragging {
        refresh_hud(world);
    }
}

/// 删除选中实体（连同其子树——glTF 挂载根的内部实体是子实体，留着会变
/// 孤儿）。返回被删实体；无选中 / 实体已不存在返回 `None`。
pub fn delete_selected(world: &mut World) -> Option<Entity> {
    let entity = { world.try_resource::<EditorState>()?.selected? };
    if !world.contains(entity) {
        select(world, None);
        return None;
    }
    push_undo(world);
    let name = name_of(world, entity);
    // 级联销毁（引擎既有语义）：glTF 挂载根的内部实体是子实体，一并销毁
    // 并从父的 Children 摘除，不留孤儿 / 悬垂子记录
    world.despawn_recursive(entity);
    select(world, None);
    set_status(world, format!("DELETED {name}"));
    Some(entity)
}

/// 复制选中实体（`Ctrl+D`）：原地复制（新实体 id 递增、名字加 `_copy{n}`
/// 后缀），复制 `Transform` / `Name` / `Visibility` / `MeshHandle` /
/// `MaterialHandle` / `Collider` / `RigidBody` / `PointLight` /
/// `ParticleEmitter`。复制后选中新实体。
///
/// **不复制子树**（glTF 挂载根不可复制，模块文档诚实清单第 6 条）：
/// 子树重挂载需要重解析文件，a 阶段只做扁平组件复制；带子实体者返回
/// `None` 并在状态行说明。
pub fn duplicate_selected(world: &mut World) -> Option<Entity> {
    let source = { world.try_resource::<EditorState>()?.selected? };
    if !world.contains(source) {
        select(world, None);
        return None;
    }
    if !world.children(source).is_empty() {
        set_status(
            world,
            "COPY SKIPPED: entity has children (gltf)".to_string(),
        );
        refresh_hud(world);
        return None;
    }
    // 只读快照阶段（收集），再写回（两阶段模式，避免借用冲突）
    let transform = *world.get::<Transform>(source)?;
    let name = world.get::<Name>(source).map(|n| n.0.clone());
    let visibility = world.get::<Visibility>(source).map(|v| *v);
    let mesh = world.get::<MeshHandle>(source).map(|m| *m);
    let material = world.get::<MaterialHandle>(source).map(|m| *m);
    let collider = world.get::<Collider>(source).map(|c| *c);
    let body = world.get::<RigidBody>(source).map(|b| *b);
    let light = world.get::<PointLight>(source).map(|l| *l);
    let emitter = world.get::<ParticleEmitter>(source).map(|e| e.clone());

    push_undo(world);
    let copy = world.spawn((transform,));
    let new_name = name.map(|base| unique_copy_name(world, &base));
    if let Some(new_name) = &new_name {
        world.insert(copy, Name(new_name.clone()));
    }
    if let Some(visibility) = visibility {
        world.insert(copy, visibility);
    }
    if let Some(mesh) = mesh {
        world.insert(copy, mesh);
    }
    if let Some(material) = material {
        world.insert(copy, material);
    }
    if let Some(collider) = collider {
        world.insert(copy, collider);
    }
    if let Some(body) = body {
        world.insert(copy, body);
    }
    if let Some(light) = light {
        world.insert(copy, light);
    }
    if let Some(emitter) = emitter {
        world.insert(copy, emitter);
    }
    select(world, Some(copy));
    set_status(
        world,
        format!(
            "COPIED {}",
            new_name.unwrap_or_else(|| "(unnamed)".to_string())
        ),
    );
    Some(copy)
}

/// 复制名：`{base}_copy{n}`，n = 已有同后缀名的最大下标 + 1。
fn unique_copy_name(world: &World, base: &str) -> String {
    let prefix = format!("{base}_copy");
    let mut max = 0u32;
    world.query::<&Name>(|name| {
        if let Some(rest) = name.0.strip_prefix(&prefix) {
            let index = rest.parse::<u32>().unwrap_or(1);
            max = max.max(index);
        }
    });
    format!("{prefix}{}", max + 1)
}

/// 方向键微移选中实体：`position += axis * amount`（**不吸附**——微移就是
/// 精确编辑）。返回新位置；无选中返回 `None`。
pub fn nudge_selected(world: &mut World, axis: Vec3, amount: f32) -> Option<Vec3> {
    let entity = { world.try_resource::<EditorState>()?.selected? };
    push_undo(world);
    let position = {
        let mut transform = world.get_mut::<Transform>(entity)?;
        transform.position += axis * amount;
        transform.position
    };
    update_gizmo(world);
    set_status(
        world,
        format!(
            "NUDGE {:.2} {:.2} {:.2}",
            position.x, position.y, position.z
        ),
    );
    refresh_hud(world);
    Some(position)
}

/// PgUp / PgDn 缩放选中实体：`scale *= factor`，逐轴下限 [`MIN_SCALE`]。
/// 返回新缩放；无选中返回 `None`。
pub fn scale_selected(world: &mut World, factor: f32) -> Option<Vec3> {
    let entity = { world.try_resource::<EditorState>()?.selected? };
    push_undo(world);
    let scale = {
        let mut transform = world.get_mut::<Transform>(entity)?;
        transform.scale = (transform.scale * factor).max(Vec3::splat(MIN_SCALE));
        transform.scale
    };
    update_gizmo(world);
    set_status(
        world,
        format!("SCALE {:.2} {:.2} {:.2}", scale.x, scale.y, scale.z),
    );
    refresh_hud(world);
    Some(scale)
}

// ---------------------------------------------------------------------------
// 编辑参数切换（Tab / R / C / G）
// ---------------------------------------------------------------------------

/// 循环切换摆放图元，返回切换后的类型。
pub fn cycle_kind(world: &mut World) -> Option<PlaceKind> {
    let kind = {
        let mut state = world.try_resource_mut::<EditorState>()?;
        state.place_kind = state.place_kind.next();
        state.place_kind
    };
    set_status(world, format!("KIND {}", kind.label()));
    refresh_hud(world);
    Some(kind)
}

/// 循环切换材质预设，返回切换后的下标。
pub fn cycle_preset(world: &mut World) -> Option<usize> {
    let index = {
        let mut state = world.try_resource_mut::<EditorState>()?;
        state.preset = (state.preset + 1) % MATERIAL_PRESETS.len();
        state.preset
    };
    set_status(world, format!("MAT {}", preset_name(index)));
    refresh_hud(world);
    Some(index)
}

/// 切换「摆放时挂静态碰撞体」，返回切换后的值。
pub fn toggle_collider(world: &mut World) -> Option<bool> {
    let value = {
        let mut state = world.try_resource_mut::<EditorState>()?;
        state.place_collider = !state.place_collider;
        state.place_collider
    };
    set_status(world, format!("COLLIDER {}", on_off(value)));
    refresh_hud(world);
    Some(value)
}

/// 切换网格吸附，返回切换后的值。
pub fn toggle_snap(world: &mut World) -> Option<bool> {
    let value = {
        let mut state = world.try_resource_mut::<EditorState>()?;
        state.snap = !state.snap;
        state.snap
    };
    set_status(world, format!("SNAP {}", on_off(value)));
    refresh_hud(world);
    Some(value)
}

/// 预设名（下标越界回退第 0 项，与 [`ContentMaterial::def`] 同规则）。
pub fn preset_name(index: usize) -> &'static str {
    MATERIAL_PRESETS
        .get(index)
        .map(|preset| preset.name)
        .unwrap_or(MATERIAL_PRESETS[0].name)
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

// ---------------------------------------------------------------------------
// undo / redo（快照栈）
// ---------------------------------------------------------------------------

/// 单个可编辑实体的组件快照（全部拷贝语义，无实体引用——恢复时重建）。
#[derive(Clone, Debug)]
struct EntitySnapshot {
    transform: Transform,
    name: Option<String>,
    mesh: Option<MeshHandle>,
    material: Option<MaterialHandle>,
    visibility: Option<Visibility>,
    collider: Option<Collider>,
    body: Option<RigidBody>,
    light: Option<PointLight>,
    emitter: Option<ParticleEmitter>,
}

/// 可编辑世界的快照（字段集 = R58a 世界 dump 的字段集 + 编辑器句柄缓存）：
/// dump 逐字符对照的前提是这些字段被完整恢复——实体组件、两 Store **整库
/// 克隆**、登记表全文、方向光资源、`level` 段快照。设计论证见模块文档
/// 「undo / redo」。
///
/// Store 记整库克隆而非截断长度：undo 截断丢掉的网格 / 材质数据，redo
/// 无法凭空恢复（长度不可逆、数据可逆）。快照世界里 Store 只含图元 /
/// 灯泡网格（glTF 世界清栈、烘焙清栈——UV2 副本不入快照域），整库体量
/// 有界（单图元 = 数 KB 量级 × [`UNDO_LIMIT`] 深度）。
#[derive(Clone, Debug)]
struct WorldSnapshot {
    /// 可编辑实体（存储序 = spawn 序）。
    entities: Vec<EntitySnapshot>,
    /// `MeshStore` 整库克隆（句柄号 = 下标，恢复整库替换）。
    meshes: Vec<MeshData>,
    /// `MaterialStore` 整库克隆（同上）。
    materials: Vec<MaterialDef>,
    /// 登记表全文（BTree 序收集——恢复重建 `HashMap`，内容恒定）。
    registry_meshes: Vec<(u32, MeshRef)>,
    registry_materials: Vec<(u32, MaterialRef)>,
    registry_gltfs: Vec<(Entity, String)>,
    registry_sky: Option<String>,
    /// 方向光资源（光源编辑的恢复载体，位模式级）。
    directional: Option<DirectionalLight>,
    /// `level` 段快照（写操作不改它，恢复只是复原现场）。
    level: Option<LevelSection>,
    /// 编辑器句柄缓存（「句柄编号对齐」不变量的运行期一半）。
    content_meshes: Vec<(PrimitiveSection, MeshHandle)>,
    content_materials: Vec<(ContentMaterial, MaterialHandle)>,
}

/// 捕获当前可编辑世界快照。存在带子实体的可编辑实体（glTF 挂载根）时
/// 返回 `None`（调用方视情形清栈 / 放弃——挂载子树无法重建）。
fn capture_snapshot(world: &World) -> Option<WorldSnapshot> {
    let mut entities = Vec::new();
    for entity in editable_entities(world) {
        if !world.children(entity).is_empty() {
            return None;
        }
        entities.push(EntitySnapshot {
            transform: *world.get::<Transform>(entity)?,
            name: world.get::<Name>(entity).map(|n| n.0.clone()),
            mesh: world.get::<MeshHandle>(entity).map(|m| *m),
            material: world.get::<MaterialHandle>(entity).map(|m| *m),
            visibility: world.get::<Visibility>(entity).map(|v| *v),
            collider: world.get::<Collider>(entity).map(|c| *c),
            body: world.get::<RigidBody>(entity).map(|b| *b),
            light: world.get::<PointLight>(entity).map(|l| *l),
            emitter: world.get::<ParticleEmitter>(entity).map(|e| e.clone()),
        });
    }
    let (registry_meshes, registry_materials, registry_gltfs, registry_sky) =
        if let Some(registry) = world.try_resource::<SceneAssetRegistry>() {
            let meshes: std::collections::BTreeMap<_, _> = registry.meshes.iter().collect();
            let materials: std::collections::BTreeMap<_, _> = registry.materials.iter().collect();
            let gltfs: std::collections::BTreeMap<_, _> = registry.gltfs.iter().collect();
            (
                meshes.into_iter().map(|(k, v)| (*k, v.clone())).collect(),
                materials
                    .into_iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect(),
                gltfs.into_iter().map(|(k, v)| (*k, v.clone())).collect(),
                registry.sky_source.clone(),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new(), None)
        };
    Some(WorldSnapshot {
        meshes: world
            .try_resource::<MeshStore>()
            .map(|store| store.contents().to_vec())
            .unwrap_or_default(),
        materials: world
            .try_resource::<MaterialStore>()
            .map(|store| store.contents().to_vec())
            .unwrap_or_default(),
        entities,
        registry_meshes,
        registry_materials,
        registry_gltfs,
        registry_sky,
        directional: world
            .try_resource::<DirectionalLight>()
            .map(|l| (*l).clone()),
        level: world
            .try_resource::<LevelRegistry>()
            .map(|registry| registry.section.clone()),
        content_meshes: world
            .try_resource::<EditorState>()
            .map(|state| state.content_meshes.clone())
            .unwrap_or_default(),
        content_materials: world
            .try_resource::<EditorState>()
            .map(|state| state.content_materials.clone())
            .unwrap_or_default(),
    })
}

/// 写操作执行前调用：拍快照入 undo 栈、清 redo 栈。存在挂载根时改为
/// **清空双栈**（挂载根边界，模块文档）——含根世界里 undo 历史恒为空。
fn push_undo(world: &mut World) {
    if world.try_resource::<EditorState>().is_none() {
        return;
    }
    if capture_snapshot(world).is_none() {
        if let Some(mut state) = world.try_resource_mut::<EditorState>() {
            state.undo_stack.clear();
            state.redo_stack.clear();
        }
        return;
    }
    let Some(snapshot) = capture_snapshot(world) else {
        return;
    };
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.redo_stack.clear();
        state.undo_stack.push(snapshot);
        if state.undo_stack.len() > UNDO_LIMIT {
            state.undo_stack.remove(0);
        }
    }
}

/// 清空 undo / redo 双栈（烘焙专用：跨烘焙 undo 的整库恢复会把在用的
/// UV2 副本从库里换掉，理由见模块文档「不入栈」）。
fn clear_undo_history(world: &mut World) {
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.undo_stack.clear();
        state.redo_stack.clear();
    }
}

/// undo 栈深度（面板显示 / 测试断言）。
pub fn undo_depth(world: &World) -> usize {
    world
        .try_resource::<EditorState>()
        .map(|state| state.undo_stack.len())
        .unwrap_or(0)
}

/// redo 栈深度（面板显示 / 测试断言）。
pub fn redo_depth(world: &World) -> usize {
    world
        .try_resource::<EditorState>()
        .map(|state| state.redo_stack.len())
        .unwrap_or(0)
}

/// 撤销上一步写操作（`Ctrl+Z`）：把「当前状态」压入 redo 栈、世界恢复
/// 到 undo 栈顶快照。返回是否执行了撤销（空栈 / 拖拽中 = `false`）。
pub fn undo(world: &mut World) -> bool {
    if world
        .try_resource::<EditorState>()
        .is_some_and(|state| state.dragging)
    {
        return false;
    }
    // 捕获「当前状态」需世界可捕获（栈非空 ⇒ 世界无挂载根，不变量见
    // 模块文档）；捕获失败 = 不变量被破坏，防御性放弃且不动栈
    let Some(current) = capture_snapshot(world) else {
        return false;
    };
    let Some(snapshot) = world
        .try_resource_mut::<EditorState>()
        .and_then(|mut state| state.undo_stack.pop())
    else {
        set_status(world, "NOTHING TO UNDO");
        refresh_hud(world);
        return false;
    };
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.redo_stack.push(current);
    }
    restore_snapshot(world, &snapshot);
    set_status(world, format!("UNDONE ({})", undo_depth(world)));
    refresh_hud(world);
    true
}

/// 重做（`Ctrl+Y` 或 `Ctrl+Shift+Z`）：[`undo`] 的逆操作。返回是否执行。
pub fn redo(world: &mut World) -> bool {
    if world
        .try_resource::<EditorState>()
        .is_some_and(|state| state.dragging)
    {
        return false;
    }
    let Some(current) = capture_snapshot(world) else {
        return false;
    };
    let Some(snapshot) = world
        .try_resource_mut::<EditorState>()
        .and_then(|mut state| state.redo_stack.pop())
    else {
        set_status(world, "NOTHING TO REDO");
        refresh_hud(world);
        return false;
    };
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.undo_stack.push(current);
    }
    restore_snapshot(world, &snapshot);
    set_status(world, format!("REDONE ({})", redo_depth(world)));
    refresh_hud(world);
    true
}

/// 把世界恢复到快照（全删全建——组件存储是 swap_remove 语义，部分重建
/// 无法还原 dump 清单序，论证见模块文档）。选中态清空、选中框重建。
fn restore_snapshot(world: &mut World, snapshot: &WorldSnapshot) {
    // 1) 全删可编辑实体（栈非空 ⇒ 无挂载根 ⇒ 无级联顾虑）
    for entity in editable_entities(world) {
        world.despawn(entity);
    }
    // 2) 两 Store 整库替换（快照克隆即快照时点的完整内容——摆放新增的
    // 条目随之消失、被 undo 掉的条目在 redo 时随库恢复）
    if let Some(mut store) = world.try_resource_mut::<MeshStore>() {
        store.set_contents(snapshot.meshes.clone());
    }
    if let Some(mut store) = world.try_resource_mut::<MaterialStore>() {
        store.set_contents(snapshot.materials.clone());
    }
    // 3) 登记表重建（HashMap 内容恒定，序不影响 dump 的 BTree 对照）
    if world.contains_resource::<SceneAssetRegistry>() {
        let mut registry = world.resource_mut::<SceneAssetRegistry>();
        registry.meshes.clear();
        for (handle, source) in &snapshot.registry_meshes {
            registry.meshes.insert(*handle, source.clone());
        }
        registry.materials.clear();
        for (handle, source) in &snapshot.registry_materials {
            registry.materials.insert(*handle, source.clone());
        }
        registry.gltfs.clear();
        for (entity, path) in &snapshot.registry_gltfs {
            registry.gltfs.insert(*entity, path.clone());
        }
        registry.sky_source = snapshot.registry_sky.clone();
    }
    // 4) 实体按快照序重建（新 spawn 追加 ⇒ 存储序 = 快照序）
    for entry in &snapshot.entities {
        let entity = world.spawn((entry.transform,));
        if let Some(name) = &entry.name {
            world.insert(entity, Name(name.clone()));
        }
        if let Some(mesh) = entry.mesh {
            world.insert(entity, mesh);
        }
        if let Some(material) = entry.material {
            world.insert(entity, material);
        }
        if let Some(visibility) = entry.visibility {
            world.insert(entity, visibility);
        }
        if let Some(collider) = entry.collider {
            world.insert(entity, collider);
        }
        if let Some(body) = entry.body {
            world.insert(entity, body);
        }
        if let Some(light) = entry.light {
            world.insert(entity, light);
        }
        if let Some(emitter) = entry.emitter.clone() {
            world.insert(entity, emitter);
        }
    }
    // 5) 资源复原（方向光 / level 段快照）
    if let Some(light) = snapshot.directional.clone() {
        world.insert_resource(light);
    } else {
        world.remove_resource::<DirectionalLight>();
    }
    if let Some(section) = &snapshot.level {
        world.insert_resource(LevelRegistry {
            section: section.clone(),
        });
    } else {
        world.remove_resource::<LevelRegistry>();
    }
    // 6) 编辑器缓存复原 + 选择清空 + 选中框重建（实体已换编号）
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.content_meshes = snapshot.content_meshes.clone();
        state.content_materials = snapshot.content_materials.clone();
        state.selected = None;
    }
    rebuild_gizmo(world);
}

// ---------------------------------------------------------------------------
// 光源参数编辑（点光四键 / 方向光五键）
// ---------------------------------------------------------------------------

/// 光源编辑模式（由选中态决定，见模块文档「光源参数编辑」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightEditMode {
    /// 未选中实体：编辑方向光资源（`[` `]` `;` `'` `J` `L` `I` `K`）。
    Directional,
    /// 选中的是点光实体：编辑其 [`PointLight`]（`[` `]` `-` `=` `H` `K`）。
    PointLight,
    /// 选中的是非灯光实体：光源键全部 no-op（`L` 回落为摆放点光）。
    Inert,
}

/// 当前光源编辑模式（输入翻译用只读查询；编辑器状态缺席 = `Inert`）。
pub fn light_edit_mode(world: &World) -> LightEditMode {
    let Some(state) = world.try_resource::<EditorState>() else {
        return LightEditMode::Inert;
    };
    match state.selected {
        Some(entity) if world.get::<PointLight>(entity).is_some() => LightEditMode::PointLight,
        Some(_) => LightEditMode::Inert,
        None => LightEditMode::Directional,
    }
}

/// RGB（线性）→ HSV。h ∈ [0, 360)（度）、s / v ∈ [0, 1]。纯函数：
/// 只用 IEEE 确定运算。s = 0（灰）时 h 恒 0（色相无定义——旋转不改变
/// 颜色，见 [`rotate_hue`]）。
pub fn rgb_to_hsv(rgb: Vec3) -> (f32, f32, f32) {
    let (r, g, b) = (rgb.x, rgb.y, rgb.z);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let v = max;
    let s = if max > 0.0 { delta / max } else { 0.0 };
    let h = if delta <= 0.0 {
        0.0
    } else if (max - r).abs() <= f32::EPSILON * max.abs().max(1.0) {
        // max = r：60 × ((g − b)/Δ)，负扇区折回 [0, 360)
        let h = 60.0 * ((g - b) / delta);
        if h < 0.0 { h + 360.0 } else { h }
    } else if (max - g).abs() <= f32::EPSILON * max.abs().max(1.0) {
        60.0 * ((b - r) / delta) + 120.0
    } else {
        60.0 * ((r - g) / delta) + 240.0
    };
    (h, s, v)
}

/// HSV → RGB（线性）。h 以度计（负值 / ≥360 取 `rem_euclid(360)`）、
/// s / v 原样使用。s = 0 时三通道恒逐位等于 v。[`rgb_to_hsv`] 的逆
/// （浮点往返不保证逐位——undo 恢复走快照而非重算）。
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Vec3 {
    let h = h.rem_euclid(360.0) / 60.0;
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    // i ∈ 0..6（rem_euclid 保证非负；i = 6 只在 h 恰为 360° 边界浮点
    // 上探时出现，归入 0 号扇区）
    match i as u32 % 6 {
        0 => Vec3::new(v, t, p),
        1 => Vec3::new(q, v, p),
        2 => Vec3::new(p, v, t),
        3 => Vec3::new(p, q, v),
        4 => Vec3::new(t, p, v),
        _ => Vec3::new(v, p, q),
    }
}

/// 色相旋转（纯函数）：HSV 空间 h += `delta_deg`（mod 360），s / v 保持。
/// 灰色（s = 0）与非有限输入原值逐位返回（旋转无定义 = 不动）。
pub fn rotate_hue(rgb: Vec3, delta_deg: f32) -> Vec3 {
    if !rgb.is_finite() {
        return rgb;
    }
    let (h, s, v) = rgb_to_hsv(rgb);
    if s <= 0.0 {
        return rgb;
    }
    hsv_to_rgb(h + delta_deg, s, v)
}

/// 选中点光（有选中且挂着 [`PointLight`]）。
fn selected_point_light(world: &World) -> Option<(Entity, PointLight)> {
    let state = world.try_resource::<EditorState>()?;
    let entity = state.selected?;
    let light = *world.get::<PointLight>(entity)?;
    Some((entity, light))
}

/// 点光 / 方向光强度步进（`[` = −1、`]` = +1）：点光模式改选中点光，
/// 方向光模式改资源，`Inert` no-op。返回新强度；无操作返回 `None`。
pub fn light_intensity_step(world: &mut World, sign: f32) -> Option<f32> {
    match light_edit_mode(world) {
        LightEditMode::PointLight => {
            let (entity, light) = selected_point_light(world)?;
            push_undo(world);
            let intensity = step_x(
                light.intensity,
                sign,
                LIGHT_INTENSITY_STEP,
                LIGHT_INTENSITY_MIN,
            );
            world.insert(entity, PointLight { intensity, ..light });
            set_status(world, format!("LIGHT INTENSITY {intensity:.3}"));
            refresh_hud(world);
            Some(intensity)
        }
        LightEditMode::Directional => {
            let light = world
                .try_resource::<DirectionalLight>()
                .map(|l| l.clone())?;
            push_undo(world);
            let intensity = step_x(
                light.intensity,
                sign,
                SUN_INTENSITY_STEP,
                LIGHT_INTENSITY_MIN,
            );
            world.insert_resource(DirectionalLight { intensity, ..light });
            set_status(world, format!("SUN INTENSITY {intensity:.3}"));
            refresh_hud(world);
            Some(intensity)
        }
        LightEditMode::Inert => None,
    }
}

/// 点光范围步进（`-` = −0.5 m、`=` = +0.5 m，下限 [`LIGHT_RANGE_MIN`]）。
/// 仅点光模式有效（方向光无范围参数）。
pub fn light_range_step(world: &mut World, sign: f32) -> Option<f32> {
    let (entity, light) = selected_point_light(world)?;
    push_undo(world);
    let range = (light.range + sign * LIGHT_RANGE_STEP).max(LIGHT_RANGE_MIN);
    world.insert(entity, PointLight { range, ..light });
    set_status(world, format!("LIGHT RANGE {range:.3}"));
    refresh_hud(world);
    Some(range)
}

/// 色相旋转一步（`H`，+[`HUE_STEP_DEG`]°）。仅点光模式有效。
pub fn light_hue_step(world: &mut World) -> Option<Vec3> {
    let (entity, light) = selected_point_light(world)?;
    push_undo(world);
    let color = rotate_hue(light.color, HUE_STEP_DEG);
    world.insert(entity, PointLight { color, ..light });
    set_status(world, "LIGHT HUE +30");
    refresh_hud(world);
    Some(color)
}

/// 点光阴影开关（`K`）。仅点光模式有效。
pub fn light_shadow_toggle(world: &mut World) -> Option<bool> {
    let (entity, light) = selected_point_light(world)?;
    push_undo(world);
    let shadows = !light.shadows;
    world.insert(entity, PointLight { shadows, ..light });
    set_status(world, format!("LIGHT SHADOWS {}", on_off(shadows)));
    refresh_hud(world);
    Some(shadows)
}

/// 方向光环境光步进（`;` = ÷1.1、`'` = ×1.1，三通道同乘保色比，下限 0）。
/// 仅方向光模式有效。
pub fn sun_ambient_step(world: &mut World, sign: f32) -> Option<Vec3> {
    if light_edit_mode(world) != LightEditMode::Directional {
        return None;
    }
    let light = world
        .try_resource::<DirectionalLight>()
        .map(|l| l.clone())?;
    push_undo(world);
    let ambient = Vec3::new(
        step_x(light.ambient.x, sign, SUN_AMBIENT_STEP, 0.0),
        step_x(light.ambient.y, sign, SUN_AMBIENT_STEP, 0.0),
        step_x(light.ambient.z, sign, SUN_AMBIENT_STEP, 0.0),
    );
    world.insert_resource(DirectionalLight { ambient, ..light });
    set_status(world, format!("SUN AMBIENT {:.3}", ambient.x));
    refresh_hud(world);
    Some(ambient)
}

/// 方向光方向步进（偏航 `J` = −、`L` = +；俯仰 `I` = +、`K` = −；步长
/// [`SUN_DIR_STEP`] rad，限位 [`SUN_PITCH_LIMIT`]，归一化回写）。仅方向光
/// 模式有效。
pub fn sun_direction_step(world: &mut World, dyaw: f32, dpitch: f32) -> Option<Vec3> {
    if light_edit_mode(world) != LightEditMode::Directional {
        return None;
    }
    let light = world
        .try_resource::<DirectionalLight>()
        .map(|l| l.clone())?;
    push_undo(world);
    let direction = rotate_light_direction(light.direction, dyaw, dpitch);
    world.insert_resource(DirectionalLight { direction, ..light });
    set_status(world, format!("SUN DIR {direction:.3}"));
    refresh_hud(world);
    Some(direction)
}

/// 乘法步进（纯函数）：`sign ≥ 0` 乘 `step`、否则除 `step`（不用 `powi`
/// ——除法的 IEEE 语义确定，`powi` 无逐位保证），下限 `min`。
fn step_x(value: f32, sign: f32, step: f32, min: f32) -> f32 {
    let next = if sign >= 0.0 {
        value * step
    } else {
        value / step
    };
    next.max(min)
}

/// 光方向旋转（纯函数）：当前方向 →（偏航 `atan2(z, x)`、俯仰
/// `asin(y)`）→ 加步进 → 俯仰限位 → 球坐标回写 → 归一化。零方向
/// 原样返回（无意义输入不产生数值）。约定与 [`orbit_eye`] 一致：
/// yaw = 0 指向 +X、yaw = π/2 指向 +Z。
pub fn rotate_light_direction(direction: Vec3, dyaw: f32, dpitch: f32) -> Vec3 {
    let n = direction.normalize_or_zero();
    if n == Vec3::ZERO {
        return direction;
    }
    let yaw = n.z.atan2(n.x);
    let pitch = n.y.clamp(-1.0, 1.0).asin();
    let pitch = (pitch + dpitch).clamp(-SUN_PITCH_LIMIT, SUN_PITCH_LIMIT);
    let yaw = yaw + dyaw;
    Vec3::new(
        pitch.cos() * yaw.cos(),
        pitch.sin(),
        pitch.cos() * yaw.sin(),
    )
    .normalize_or_zero()
}

// ---------------------------------------------------------------------------
// 烘焙入口（B 键）
// ---------------------------------------------------------------------------

/// 一次烘焙的结果报告（状态行 / 测试用）。
#[derive(Clone, Debug, PartialEq)]
pub struct BakeReport {
    /// 覆盖 texel 数（真烘焙结果数，不含 chart 间隙零填充）。
    pub texels: usize,
    /// 挂载实体数（拿到 [`LightmapBinding`] 的实体）。
    pub entities: usize,
    /// 烘焙 + 编码耗时（毫秒，墙钟——仅状态行显示，不入任何确定性路径）。
    pub elapsed_ms: u128,
    /// 产物落盘路径。
    pub artifact: PathBuf,
    /// 参数来源：`true` = 档内 `level.bake` 段，`false` = 引擎默认
    /// （产物走默认名，重载不自动挂载——诚实限制第 3 条）。
    pub from_level_section: bool,
}

/// 烘焙当前静态世界并挂载（`B` 键）：档内 `level.bake` 参数（缺席 =
/// 引擎默认）→ `bake_lightmap` 全链 → 产物 IRISLMAP 落盘 →
/// `install_lightmap` 挂载 → 登记表回补（R57 `attach` 语义，见模块
/// 文档）→ **清空 undo 栈**（跨烘焙 undo 会把在用的 UV2 副本从库里换掉）。
///
/// 烘焙前 despawn 全部瞬态视觉（网格线也是静态几何，不清会进烘焙！），
/// 结束后重建。状态行回报 texel 数 / 耗时 / 挂载实体数。编辑器状态缺席
/// （无档路径可落产物）时 no-op 返回 `None`。
pub fn bake_and_install(world: &mut World) -> Option<BakeReport> {
    world.try_resource::<EditorState>()?;
    let level_path = world
        .try_resource::<EditorState>()
        .map(|state| state.level_path.clone())
        .unwrap_or_default();
    let (params, uv, artifact, from_section) = {
        let section = world
            .try_resource::<LevelRegistry>()
            .and_then(|registry| registry.section.bake.clone());
        match section {
            Some(bake) => match bake.to_bake_params() {
                Ok((params, uv)) => {
                    let file = bake.artifact.clone().unwrap_or_else(|| {
                        level_path
                            .with_extension("ilm")
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "level.ilm".to_string())
                    });
                    (params, uv, file, true)
                }
                Err(err) => {
                    log::error!("level.bake 参数非法：{err}");
                    set_status(world, format!("BAKE FAILED: {err}"));
                    refresh_hud(world);
                    return None;
                }
            },
            None => {
                let (params, uv) = BakeSection::default()
                    .to_bake_params()
                    .expect("BakeSection::default 恒合法");
                // 无 bake 段：档同目录 *.ilm 默认名（状态行提示，档内
                // 无引用——重载不自动挂载，诚实限制第 3 条）
                let file = level_path
                    .with_extension("ilm")
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "level.ilm".to_string());
                (params, uv, file, false)
            }
        }
    };
    // 产物路径相对档目录解析（`level.bake.artifact` 是档内相对路径）
    let artifact_path = match level_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(&artifact),
        _ => PathBuf::from(&artifact),
    };

    let started = std::time::Instant::now();
    // **先存档**：产物按「同一关卡档加载出的世界」的实体位引用实体
    // （R57 契约）——文件是内容的规范事实，烘焙前必须落盘（B 键 =
    // 保存 + 烘焙）。
    if save_level(world).is_err() {
        set_status(world, "BAKE FAILED: level save failed");
        refresh_hud(world);
        return None;
    }
    // **规范影子世界**：fresh World + `load_scene` —— 与游戏加载路径
    // 产出同一套实体位布局（内容实体 0 起连续、代数 0）。在它上面烘焙，
    // 产物位引用对「任何加载本档的世界」都成立；编辑器活动世界因家具
    // （网格线 / HUD）与分配器代数，位布局本就不同构（见 open_level
    // 「内容实体先行」注释），故安装时按**清单序**重映射回活动世界。
    let mut canonical = World::new();
    // 与 `open_level` 同款初始化序（状态 → Store → 基线 → 瞬态**资产
    // 句柄** → 内容）：句柄编号对齐不变量要求影子世界的内容句柄与重载
    // 路径逐位一致（瞬态资产占 Store 前缀）。省略的只有网格线 / HUD
    // 等**实体**家具——规范位布局 = 内容实体 0 起连续。
    canonical.insert_resource(EditorState::new(level_path.clone()));
    ensure_stores(&mut canonical);
    ensure_baseline(&mut canonical);
    build_transient_assets(&mut canonical);
    let canonical_entities = match load_scene(&mut canonical, &level_path) {
        Ok(list) => list,
        Err(err) => {
            log::error!("烘焙的规范世界构建失败（{}）：{err}", level_path.display());
            set_status(world, format!("BAKE FAILED: {err}"));
            refresh_hud(world);
            return None;
        }
    };
    iris_core::hierarchy::propagate_transforms(&mut canonical);
    let baked = bake_lightmap(&canonical, &params, &uv);
    let texels = baked.covered.iter().filter(|covered| **covered).count();
    // 产物先落盘（install 消耗产物），失败 = 安装也不做（世界不动）
    if let Err(err) = save_lightmap_artifact(&baked, &artifact_path) {
        log::error!("光照贴图产物落盘失败（{}）：{err}", artifact_path.display());
        set_status(world, format!("BAKE FAILED: {err}"));
        refresh_hud(world);
        return None;
    }
    // **重映射到活动世界**：规范清单序 == 活动世界可编辑清单序（同一份
    // 文件、同一刻的存储序），按位置一一对应；mesh 句柄改写为活动世界
    // 的当前句柄（两套 Store 的编号各自独立）。之后 install 的全部
    // 断言（实体在世 / 句柄一致 / 三角数相符）都在活动世界上成立。
    let mut remapped = baked;
    {
        let live_entities = editable_entities(world);
        if live_entities.len() != canonical_entities.len() {
            set_status(world, "BAKE FAILED: entity count mismatch after save");
            refresh_hud(world);
            return None;
        }
        for entry in &mut remapped.atlas.entries {
            let pos = canonical_entities
                .iter()
                .position(|&e| e.to_bits() == entry.entity.to_bits())
                .expect("产物表项实体必在规范清单中");
            let live = live_entities[pos];
            entry.entity = live;
            entry.mesh = world.get::<MeshHandle>(live).map(|h| *h);
        }
    }
    // 挂载前抓原句柄（登记表回补：UV2 副本继承原句柄的场景来源——
    // 照抄 R57 attach 语义；否则烘焙后保存的 mesh 段整段省略）
    let originals: Vec<(Entity, MeshHandle)> = editable_entities(world)
        .into_iter()
        .filter_map(|entity| world.get::<MeshHandle>(entity).map(|h| (entity, *h)))
        .collect();
    let install = install_lightmap(world, remapped);
    // 两阶段：先读新句柄（world 不可变借用），再写登记表（可变借用）
    let pairs: Vec<(MeshHandle, Option<MeshHandle>)> = originals
        .iter()
        .map(|&(entity, original)| (original, world.get::<MeshHandle>(entity).map(|h| *h)))
        .collect();
    let mut registry = world.resource_mut::<SceneAssetRegistry>();
    for (original, current) in pairs {
        let Some(current) = current else { continue };
        if current == original {
            continue;
        }
        if let Some(source) = registry.meshes.get(&original.0).cloned() {
            registry.meshes.insert(current.0, source);
        }
    }
    drop(registry);
    // 烘焙不入 undo 栈（理由见模块文档）：清栈，防止跨烘焙 undo 的
    // Store 整库恢复把在用的 UV2 副本换掉
    clear_undo_history(world);
    let elapsed_ms = started.elapsed().as_millis();
    let report = BakeReport {
        texels,
        entities: install.entities,
        elapsed_ms,
        artifact: artifact_path,
        from_level_section: from_section,
    };
    set_status(
        world,
        format!(
            "BAKED {} tx {} ents {}ms -> {}",
            report.texels,
            report.entities,
            report.elapsed_ms,
            file_label(&report.artifact)
        ),
    );
    refresh_hud(world);
    Some(report)
}

// ---------------------------------------------------------------------------
// NavGrid 重建与可视化（N 键）
// ---------------------------------------------------------------------------

/// 一次导航重建的结果报告（面板 / 状态行 / 测试用）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavReport {
    /// 格子总数（cols × rows）。
    pub cells: usize,
    /// blocked 格数（`is_blocked` 逐格统计）。
    pub blocked: usize,
    /// 覆盖域 XZ 尺寸（米，cols × cell_size / rows × cell_size）。
    pub size: [f32; 2],
    /// bounds 来源：`true` = 档内 `level.nav` 段，`false` = 静态 AABB
    /// 并集自动推导。
    pub from_level_section: bool,
}

/// 从静态碰撞体集合推导航覆盖域（纯函数，测试钉住）：轴对齐 AABB 并集
///（Box = half_extents、Sphere = radius、Capsule = 竖直轴 (radius,
/// half_height + radius)——旋转按轴对齐语义忽略，与引擎 `ColliderShape::
/// Box` 的既有简化一致）。空集合或 XZ 退化（min ≥ max）→ `None`。
pub fn nav_bounds_from_statics(statics: &[(Vec3, ColliderShape)]) -> Option<(Vec3, Vec3)> {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for (position, shape) in statics {
        let half = match shape {
            ColliderShape::Box { half_extents } => *half_extents,
            ColliderShape::Sphere { radius } => Vec3::splat(*radius),
            ColliderShape::Capsule {
                radius,
                half_height,
            } => Vec3::new(*radius, half_height + radius, *radius),
        };
        min = min.min(*position - half);
        max = max.max(*position + half);
    }
    if statics.is_empty() || min.x >= max.x || min.z >= max.z {
        return None;
    }
    Some((min, max))
}

/// 收集世界静态碰撞体（与 R57 `apply_nav` 同判据：无 `RigidBody` 或
/// `is_static`、排除传感器；收集序 = 组件存储序）。
fn collect_nav_statics(world: &World) -> Vec<(Vec3, ColliderShape)> {
    let mut statics = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, body)| {
        if collider.is_sensor {
            return;
        }
        if body.is_none() || collider.is_static {
            statics.push((transform.position, collider.shape));
        }
    });
    statics
}

/// blocked 格数（`is_blocked` 逐格统计——与 nav.rs 的位图语义同源，
/// 不走内部数组）。
pub fn nav_blocked_count(grid: &NavGrid) -> usize {
    let mut count = 0usize;
    for row in 0..grid.rows() {
        for col in 0..grid.cols() {
            if grid.is_blocked(col, row) {
                count += 1;
            }
        }
    }
    count
}

/// 重建 NavGrid 资源（参数优先取档内 `level.nav` 段；缺席时从静态
/// 碰撞体 AABB 并集自动推导 bounds + 默认格子参数）。返回报告；无静态体
/// / 参数非法返回 `None` 并在状态行说明。**不入 undo 栈**（幂等可重跑）。
pub fn rebuild_nav(world: &mut World) -> Option<NavReport> {
    world.try_resource::<EditorState>()?;
    let section = world
        .try_resource::<LevelRegistry>()
        .and_then(|registry| registry.section.nav);
    let (params, from_section) = match section {
        Some(nav) => match nav.to_params() {
            Ok(params) => (params, true),
            Err(err) => {
                log::error!("level.nav 参数非法：{err}");
                set_status(world, format!("NAV FAILED: {err}"));
                refresh_hud(world);
                return None;
            }
        },
        None => {
            let statics = collect_nav_statics(world);
            match nav_bounds_from_statics(&statics) {
                Some((min, max)) => (NavGridParams::new(min, max), false),
                None => {
                    set_status(world, "NAV FAILED: no static colliders");
                    refresh_hud(world);
                    return None;
                }
            }
        }
    };
    let statics = collect_nav_statics(world);
    let grid = NavGrid::build(params, &statics);
    let report = NavReport {
        cells: grid.cols() * grid.rows(),
        blocked: nav_blocked_count(&grid),
        size: [
            grid.cols() as f32 * params.cell_size,
            grid.rows() as f32 * params.cell_size,
        ],
        from_level_section: from_section,
    };
    world.insert_resource(grid);
    Some(report)
}

/// `N` 键开关：开 = 重建 + 生成可视化（blocked 红片 + 域边框），关 =
/// 撤掉可视化。返回开后的 [`NavReport`]（关 = `None`；重建失败亦
/// `None`，状态行说明）。可视化实体挂 [`EditorTransient`]——不入档、
/// 不可拾取；**每次开都重建**（关再开 = 手动重建）。
pub fn toggle_nav_viz(world: &mut World) -> Option<NavReport> {
    let turning_on = !world
        .try_resource::<EditorState>()
        .map(|state| state.nav_viz_on)
        .unwrap_or(false);
    if !turning_on {
        clear_nav_viz(world);
        if let Some(mut state) = world.try_resource_mut::<EditorState>() {
            state.nav_viz_on = false;
        }
        set_status(world, "NAV off");
        refresh_hud(world);
        return None;
    }
    build_nav_viz(world)
}

/// 重建 + 生成可视化实体并置开标记（[`toggle_nav_viz`] 的开路径与
/// [`rebuild_nav_viz`] 的重建路径共用；重复调用安全——先撤旧实体）。
fn build_nav_viz(world: &mut World) -> Option<NavReport> {
    let report = rebuild_nav(world)?;
    let Some(assets) = transient_assets(world) else {
        return Some(report);
    };
    clear_nav_viz(world);
    // 只读阶段：抓格子数据与参数（两阶段模式——资源借用与实体 spawn 不
    // 重叠，AGENTS.md 规约）
    let (params, blocked_cells) = {
        let grid = world.try_resource::<NavGrid>()?;
        let params = grid.params();
        let mut cells = Vec::new();
        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                if grid.is_blocked(col, row) {
                    cells.push((
                        col,
                        row,
                        grid.cell_center(col, row),
                        grid.surface_height(col, row),
                    ));
                }
            }
        }
        (params, cells)
    };
    let mut spawned = Vec::new();
    for (col, row, center, surface) in blocked_cells {
        spawned.push(world.spawn((
            Transform {
                position: Vec3::new(
                    center.x,
                    surface + NAV_SLAB_LIFT + NAV_SLAB_THICKNESS * 0.5,
                    center.z,
                ),
                rotation: Quat::IDENTITY,
                scale: Vec3::new(params.cell_size, NAV_SLAB_THICKNESS, params.cell_size),
            },
            assets.line_mesh,
            assets.nav_block,
            Name(format!("navviz/c{col}r{row}")),
            EditorTransient,
        )));
    }
    // 域边框：bounds 四条边（XZ 平面一圈），复用线网格
    let (bmin, bmax) = (params.bounds_min, params.bounds_max);
    let border_y = NAV_BORDER_Y;
    // (中心, 沿 X?, 长度)
    let border_lines: [(Vec3, bool, f32); 4] = [
        (
            Vec3::new((bmin.x + bmax.x) * 0.5, border_y, bmin.z),
            true,
            bmax.x - bmin.x,
        ),
        (
            Vec3::new((bmin.x + bmax.x) * 0.5, border_y, bmax.z),
            true,
            bmax.x - bmin.x,
        ),
        (
            Vec3::new(bmin.x, border_y, (bmin.z + bmax.z) * 0.5),
            false,
            bmax.z - bmin.z,
        ),
        (
            Vec3::new(bmax.x, border_y, (bmin.z + bmax.z) * 0.5),
            false,
            bmax.z - bmin.z,
        ),
    ];
    for (index, (center, along_x, length)) in border_lines.into_iter().enumerate() {
        let scale = if along_x {
            Vec3::new(
                length + NAV_BORDER_THICKNESS,
                NAV_BORDER_THICKNESS,
                NAV_BORDER_THICKNESS,
            )
        } else {
            Vec3::new(
                NAV_BORDER_THICKNESS,
                NAV_BORDER_THICKNESS,
                length + NAV_BORDER_THICKNESS,
            )
        };
        spawned.push(world.spawn((
            Transform {
                position: center,
                rotation: Quat::IDENTITY,
                scale,
            },
            assets.line_mesh,
            assets.nav_border,
            Name(format!("navviz/border{index}")),
            EditorTransient,
        )));
    }
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.nav_viz_on = true;
        state.nav_viz = spawned;
    }
    set_status(
        world,
        format!(
            "NAV on {} cells {} blocked {}x{:.1}m",
            report.cells, report.blocked, report.size[0], report.size[1]
        ),
    );
    refresh_hud(world);
    Some(report)
}

/// 撤掉 Nav 可视化实体（`nav_viz_on` 标记不动——由 [`toggle_nav_viz`] /
/// [`rebuild_nav_viz`] 管理）。
fn clear_nav_viz(world: &mut World) {
    let viz: Vec<Entity> = world
        .try_resource::<EditorState>()
        .map(|state| state.nav_viz.clone())
        .unwrap_or_default();
    for entity in viz {
        world.despawn(entity);
    }
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.nav_viz.clear();
    }
}

/// 按 `nav_viz_on` 标记重建可视化（存档 / 烘焙的瞬态重建路径用；没开过
/// = no-op）。
pub fn rebuild_nav_viz(world: &mut World) {
    let on = world
        .try_resource::<EditorState>()
        .map(|state| state.nav_viz_on)
        .unwrap_or(false);
    if on {
        // 按标记强制重建（不走 toggle——那会把它当成「关」）
        build_nav_viz(world);
    }
}

// ---------------------------------------------------------------------------
// 相机轨道旋转（中键拖拽 / F 聚焦）
// ---------------------------------------------------------------------------

/// 轨道旋转（纯函数）：eye 绕 target 转 `dyaw` / `dpitch`（弧度），返回
/// 新 eye。球坐标解析式：offset = eye − target、r = ‖offset‖、
/// yaw = `atan2(z, x)`（yaw = 0 指向 +X、π/2 指向 +Z）、pitch = `asin(y/r)`
/// → 加步进 → 俯仰钳制 ±[`ORBIT_PITCH_LIMIT`]（防 gimbal 退化：eye 贴
/// 竖直轴时 yaw 无定义）→ 球坐标回写。半径解析上恒保持；零半径 / 非有限
/// offset 原样返回。
pub fn orbit_eye(eye: Vec3, target: Vec3, dyaw: f32, dpitch: f32) -> Vec3 {
    let offset = eye - target;
    let r = offset.length();
    if !(r.is_finite() && r > 0.0) {
        return eye;
    }
    let yaw = offset.z.atan2(offset.x);
    let pitch = (offset.y / r).clamp(-1.0, 1.0).asin();
    let pitch = (pitch + dpitch).clamp(-ORBIT_PITCH_LIMIT, ORBIT_PITCH_LIMIT);
    let yaw = yaw + dyaw;
    target
        + Vec3::new(
            r * pitch.cos() * yaw.cos(),
            r * pitch.sin(),
            r * pitch.cos() * yaw.sin(),
        )
}

/// 中键拖拽的命令层入口：按像素增量旋转编辑器相机（灵敏度
/// [`ORBIT_SENSITIVITY`]，拖右 = 偏航 +、拖上 = 俯仰 +——屏幕 y 向下，
/// 取负号）。相机资源缺席 no-op。相机不入档（§8.33⑥ 语义不变）。
pub fn orbit_camera(world: &mut World, delta_px: (f64, f64)) {
    let Some(mut camera) = world.try_resource_mut::<Camera>() else {
        return;
    };
    let dyaw = delta_px.0 as f32 * ORBIT_SENSITIVITY;
    let dpitch = -delta_px.1 as f32 * ORBIT_SENSITIVITY;
    let eye = orbit_eye(camera.eye, camera.target, dyaw, dpitch);
    camera.eye = eye;
}

/// `F` 聚焦：target 直设为选中实体世界 AABB 中心（eye 不动——规格语义）。
/// 无选中 / 实体无网格 → `None`。
pub fn focus_selected(world: &mut World) -> Option<Vec3> {
    let selected = world
        .try_resource::<EditorState>()
        .and_then(|state| state.selected)?;
    let (min, max) = world_aabb_of(world, selected)?;
    let center = (min + max) * 0.5;
    let mut camera = world.try_resource_mut::<Camera>()?;
    camera.target = center;
    Some(center)
}

// ---------------------------------------------------------------------------
// 瞬态视觉：地面参考网格 + 选中框
// ---------------------------------------------------------------------------

/// 创建瞬态视觉资产（网格 1 + 材质 4），已创建则不动。
///
/// 线段用**单位立方体按 scale 拉细**（引擎无线段图元），选中框同理；
/// 这些句柄刻意**不入登记表**——瞬态实体存前 despawn，`save_scene` 永远
/// 看不到它们，登记只会给登记表添噪声。
fn build_transient_assets(world: &mut World) {
    // 状态缺席 = 无处记账（且此时兜底资源也可能未插）：直接返回，
    // 与命令层「无状态则 no-op」的契约一致
    let Some(state) = world.try_resource::<EditorState>() else {
        return;
    };
    if state.transient.is_some() {
        return;
    }
    drop(state);
    let line_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let grid = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.20, 0.23, 0.29, 1.0));
    let axis_x = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.70, 0.16, 0.13, 1.0));
    let axis_z = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.15, 0.32, 0.72, 1.0));
    let gizmo = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.85, 0.20, 1.0],
        emissive: [1.8, 1.3, 0.25, 1.0],
        roughness: 0.4,
        ..Default::default()
    });
    // Nav blocked 红片：半透明（Blend），叠在几何表面上不遮死
    let nav_block = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.95, 0.12, 0.10, 0.45],
        emissive: [0.55, 0.04, 0.03, 1.0],
        roughness: 0.9,
        alpha_mode: AlphaMode::Blend,
        ..Default::default()
    });
    // 导航域边框：橙（与网格红蓝轴、黄色选中框都可区分）
    let nav_border = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.45, 0.05, 1.0],
        emissive: [0.9, 0.35, 0.02, 1.0],
        roughness: 0.8,
        ..Default::default()
    });
    let assets = TransientAssets {
        line_mesh,
        grid,
        axis_x,
        axis_z,
        gizmo,
        nav_block,
        nav_border,
    };
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.transient = Some(assets);
    }
}

/// despawn 全部瞬态实体（网格线 + 选中框 + Nav 可视化），句柄缓存保留。
/// `nav_viz_on` 标记**不清**（存档后按标记重建可视化）。
pub fn clear_transient(world: &mut World) {
    let mut doomed = Vec::new();
    world.query::<(Entity, &EditorTransient)>(|(entity, _)| doomed.push(entity));
    for entity in doomed {
        world.despawn(entity);
    }
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.grid.clear();
        state.gizmo.clear();
        state.nav_viz.clear();
    }
}

/// 重建地面参考网格（先清旧线）。纯视觉：不进存档（[`clear_transient`]
/// 由 [`save_level`] 在写文件前调用）。
pub fn rebuild_grid(world: &mut World) {
    let Some(assets) = transient_assets(world) else {
        return;
    };
    clear_grid(world);
    let steps = (GRID_EXTENT / GRID_STEP) as i32;
    let length = GRID_EXTENT * 2.0;
    let mut spawned = Vec::new();
    for step in -steps..=steps {
        let offset = step as f32 * GRID_STEP;
        // 沿 X 的线（z = offset）：offset == 0 者是 X 轴（红）
        spawned.push(spawn_line(
            world,
            assets,
            Vec3::new(0.0, GRID_Y, offset),
            Vec3::new(length, GRID_THICKNESS, GRID_THICKNESS),
            if step == 0 {
                assets.axis_x
            } else {
                assets.grid
            },
            format!("grid/x/{offset}"),
        ));
        // 沿 Z 的线（x = offset）：offset == 0 者是 Z 轴（蓝）
        spawned.push(spawn_line(
            world,
            assets,
            Vec3::new(offset, GRID_Y, 0.0),
            Vec3::new(GRID_THICKNESS, GRID_THICKNESS, length),
            if step == 0 {
                assets.axis_z
            } else {
                assets.grid
            },
            format!("grid/z/{offset}"),
        ));
    }
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.grid = spawned;
    }
}

/// 生成一条网格线实体（单位立方体按 scale 拉成细条）。
fn spawn_line(
    world: &mut World,
    assets: TransientAssets,
    position: Vec3,
    scale: Vec3,
    material: MaterialHandle,
    name: String,
) -> Entity {
    world.spawn((
        Transform {
            position,
            rotation: Quat::IDENTITY,
            scale,
        },
        assets.line_mesh,
        material,
        Name(name),
        EditorTransient,
    ))
}

/// 重建选中框（12 条边）：无选中 / 选中实体无网格 → 清空。
pub fn rebuild_gizmo(world: &mut World) {
    let Some(assets) = transient_assets(world) else {
        return;
    };
    clear_gizmo(world);
    let selected = world
        .try_resource::<EditorState>()
        .and_then(|state| state.selected);
    let Some(entity) = selected else { return };
    let Some((min, max)) = world_aabb_of(world, entity) else {
        return;
    };
    let mut spawned = Vec::new();
    for (index, transform) in gizmo_edge_transforms(min, max).iter().enumerate() {
        spawned.push(world.spawn((
            *transform,
            assets.line_mesh,
            assets.gizmo,
            Name(format!("gizmo/{index}")),
            EditorTransient,
        )));
    }
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.gizmo = spawned;
    }
}

/// 拖拽 / 微移 / 缩放后的选中框原位更新（不 despawn 重建——实体编号稳定，
/// 拖拽期每帧调用也无实体churn）。选中框不在位时退回 [`rebuild_gizmo`]。
pub fn update_gizmo(world: &mut World) {
    let gizmo: Vec<Entity> = world
        .try_resource::<EditorState>()
        .map(|state| state.gizmo.clone())
        .unwrap_or_default();
    let selected = world
        .try_resource::<EditorState>()
        .and_then(|state| state.selected);
    if gizmo.len() != GIZMO_EDGE_COUNT {
        rebuild_gizmo(world);
        return;
    }
    let Some(entity) = selected else { return };
    let Some((min, max)) = world_aabb_of(world, entity) else {
        return;
    };
    for (slot, transform) in gizmo.iter().zip(gizmo_edge_transforms(min, max)) {
        if let Some(mut current) = world.get_mut::<Transform>(*slot) {
            *current = transform;
        }
    }
    refresh_hud(world);
}

/// 选中框边数（AABB 的 12 条边）。
pub const GIZMO_EDGE_COUNT: usize = 12;

/// 由世界 AABB 求 12 条边框线的 `Transform`（纯函数，可脱离 World 单测）。
///
/// 角点下标位约定：bit0 = x、bit1 = z、bit2 = y（0 取 lo、1 取 hi）；边表
/// [`GIZMO_EDGES`] 按「4 条沿 X、4 条沿 Z、4 条沿 Y」的固定顺序声明 ⇒
/// 生成序确定（选中框实体的存储序 = 边表序）。外扩 [`GIZMO_MARGIN`] 避免
/// 与实体表面深度冲突，逐轴加 [`GIZMO_THICKNESS`] 让角点相接不留缝。
pub fn gizmo_edge_transforms(min: Vec3, max: Vec3) -> [Transform; GIZMO_EDGE_COUNT] {
    /// 12 条边的角点对（角点下标位约定见 [`gizmo_edge_transforms`]）。
    const GIZMO_EDGES: [[usize; 2]; GIZMO_EDGE_COUNT] = [
        [0, 1],
        [2, 3],
        [4, 5],
        [6, 7],
        [0, 2],
        [1, 3],
        [4, 6],
        [5, 7],
        [0, 4],
        [1, 5],
        [2, 6],
        [3, 7],
    ];
    let lo = min - Vec3::splat(GIZMO_MARGIN);
    let hi = max + Vec3::splat(GIZMO_MARGIN);
    let corners = [
        Vec3::new(lo.x, lo.y, lo.z),
        Vec3::new(hi.x, lo.y, lo.z),
        Vec3::new(lo.x, lo.y, hi.z),
        Vec3::new(hi.x, lo.y, hi.z),
        Vec3::new(lo.x, hi.y, lo.z),
        Vec3::new(hi.x, hi.y, lo.z),
        Vec3::new(lo.x, hi.y, hi.z),
        Vec3::new(hi.x, hi.y, hi.z),
    ];
    let mut out = [Transform::IDENTITY; GIZMO_EDGE_COUNT];
    for (slot, [a, b]) in GIZMO_EDGES.iter().enumerate() {
        let (ca, cb) = (corners[*a], corners[*b]);
        let delta = (cb - ca).abs();
        out[slot] = Transform {
            position: (ca + cb) * 0.5,
            rotation: Quat::IDENTITY,
            scale: delta * 2.0 + Vec3::splat(GIZMO_THICKNESS),
        };
    }
    out
}

fn transient_assets(world: &World) -> Option<TransientAssets> {
    world.try_resource::<EditorState>()?.transient
}

fn clear_grid(world: &mut World) {
    let grid: Vec<Entity> = world
        .try_resource::<EditorState>()
        .map(|state| state.grid.clone())
        .unwrap_or_default();
    for entity in grid {
        world.despawn(entity);
    }
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.grid.clear();
    }
}

fn clear_gizmo(world: &mut World) {
    let gizmo: Vec<Entity> = world
        .try_resource::<EditorState>()
        .map(|state| state.gizmo.clone())
        .unwrap_or_default();
    for entity in gizmo {
        world.despawn(entity);
    }
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.gizmo.clear();
    }
}

/// 重建全部瞬态视觉（网格 + 选中框）。
pub fn refresh_visuals(world: &mut World) {
    build_transient_assets(world);
    rebuild_grid(world);
    rebuild_gizmo(world);
}

// ---------------------------------------------------------------------------
// 属性面板（只读）
// ---------------------------------------------------------------------------

/// 单个实体的只读属性（面板显示用）。
#[derive(Clone, Debug)]
pub struct EntityInfo {
    /// 实体名（无 `Name` 组件 → `"(unnamed)"`）。
    pub name: String,
    /// 类型标签：`primitive(cube)` / `gltf(path)` / `mesh(unregistered)` /
    /// `empty`（无网格）。来源读 [`SceneAssetRegistry`]（与 `save_scene`
    /// 还原 mesh 段同一判据）。
    pub kind: String,
    /// 碰撞体描述：`box(1.00,0.50,1.00)` / `sphere(0.50)` /
    /// `capsule(0.30,0.60)` / `"-"`（无）。
    pub collider: String,
    /// 是否挂 [`PointLight`]。
    pub light: bool,
    /// 点光参数（非灯光实体 = `None`；面板 LIGHT 行的来源）。
    pub point_light: Option<PointLight>,
    /// 位置。
    pub position: Vec3,
    /// 缩放。
    pub scale: Vec3,
}

/// 读单个实体的只读属性。实体不存在 / 无 `Transform` → `None`。
pub fn describe(world: &World, entity: Entity) -> Option<EntityInfo> {
    let transform = *world.get::<Transform>(entity)?;
    let name = world
        .get::<Name>(entity)
        .map(|n| n.0.clone())
        .unwrap_or_else(|| "(unnamed)".to_string());
    let kind = match world.get::<MeshHandle>(entity) {
        Some(handle) => mesh_kind_label(world, entity, *handle),
        None => "empty".to_string(),
    };
    let collider = match world.get::<Collider>(entity) {
        Some(collider) => collider_label(&collider),
        None => "-".to_string(),
    };
    Some(EntityInfo {
        name,
        kind,
        collider,
        light: world.get::<PointLight>(entity).is_some(),
        point_light: world.get::<PointLight>(entity).map(|l| *l),
        position: transform.position,
        scale: transform.scale,
    })
}

/// 网格来源标签（登记表驱动，与 `save_scene` 的 mesh 段还原同判据）。
fn mesh_kind_label(world: &World, entity: Entity, handle: MeshHandle) -> String {
    let registry = world.try_resource::<SceneAssetRegistry>();
    if let Some(path) = registry.as_deref().and_then(|r| r.gltfs.get(&entity)) {
        return format!("gltf({})", ascii_only(path));
    }
    match registry
        .as_deref()
        .and_then(|r| r.meshes.get(&handle.0))
        .cloned()
    {
        Some(MeshRef::Primitive(section)) => format!("primitive({})", primitive_label(&section)),
        Some(MeshRef::Gltf(path)) => format!("gltf({})", ascii_only(&path)),
        None => "mesh(unregistered)".to_string(),
    }
}

/// 图元段的短标签（面板显示）。
pub fn primitive_label(section: &PrimitiveSection) -> &'static str {
    match section {
        PrimitiveSection::Cube => "cube",
        PrimitiveSection::Plane { .. } => "plane",
        PrimitiveSection::Sphere { .. } => "sphere",
        PrimitiveSection::Cylinder { .. } => "cylinder",
    }
}

/// 碰撞体短标签（面板显示，数值 `{:.2}`）。
pub fn collider_label(collider: &Collider) -> String {
    match collider.shape {
        ColliderShape::Sphere { radius } => format!("sphere({radius:.2})"),
        ColliderShape::Box { half_extents } => format!(
            "box({:.2},{:.2},{:.2})",
            half_extents.x, half_extents.y, half_extents.z
        ),
        ColliderShape::Capsule {
            radius,
            half_height,
        } => format!("capsule({radius:.2},{half_height:.2})"),
    }
}

/// 属性面板全文（多行，**纯 ASCII**——8×8 位图字体只有 ASCII 字形，
/// 非 ASCII 折成 `?`，模块文档诚实清单第 7 条）。
///
/// 内容 = 存档文件名 / 实体计数 / 摆放参数 / 选中实体属性 / 状态行 /
/// 键位帮助。**只读**：数值修改走热键（[`nudge_selected`] /
/// [`scale_selected`]）。
pub fn panel_text(world: &World) -> String {
    let Some(state) = world.try_resource::<EditorState>() else {
        return String::from("IRIS EDITOR\nNO EDITOR STATE (call open_level)");
    };
    let mut lines = Vec::with_capacity(24);
    lines.push("IRIS EDITOR  R58b".to_string());
    lines.push(format!("FILE {}", file_label(&state.level_path)));
    lines.push(format!(
        "ENTITIES {}   FORMAT v{}   UNDO {} REDO {}",
        editable_entities(world).len(),
        SCENE_FORMAT_VERSION,
        undo_depth(world),
        redo_depth(world)
    ));
    lines.push(format!(
        "KIND {}   MAT {}   COLLIDER {}",
        state.place_kind.label(),
        preset_name(state.preset),
        on_off(state.place_collider)
    ));
    lines.push(format!(
        "SNAP {} {:.2}m   DRAG {}",
        on_off(state.snap),
        state.snap_size,
        on_off(state.dragging)
    ));
    // Nav 摘要（可视化开 / NavGrid 资源在位时）：格子总数 / blocked / 域尺寸
    if let Some(grid) = world.try_resource::<NavGrid>() {
        let params = grid.params();
        lines.push(format!(
            "NAV {} cells {} blocked {:.1}x{:.1}m",
            grid.cols() * grid.rows(),
            nav_blocked_count(&grid),
            grid.cols() as f32 * params.cell_size,
            grid.rows() as f32 * params.cell_size,
        ));
    }
    match state.selected.and_then(|entity| describe(world, entity)) {
        Some(info) => {
            lines.push(format!("SELECTED {}", ascii_only(&info.name)));
            lines.push(format!("TYPE {}   LIGHT {}", info.kind, on_off(info.light)));
            lines.push(format!("COLLIDER {}", info.collider));
            lines.push(format!(
                "POS  {:.2} {:.2} {:.2}",
                info.position.x, info.position.y, info.position.z
            ));
            lines.push(format!(
                "SCALE {:.2} {:.2} {:.2}",
                info.scale.x, info.scale.y, info.scale.z
            ));
            if let Some(light) = info.point_light {
                lines.push(format!(
                    "LIGHT I{:.2} R{:.1} C({:.2},{:.2},{:.2}) SH {}",
                    light.intensity,
                    light.range,
                    light.color.x,
                    light.color.y,
                    light.color.z,
                    on_off(light.shadows)
                ));
            }
        }
        None => lines.push("SELECTED none".to_string()),
    }
    // 方向光资源（恒显示——方向光是关卡环境的一部分）
    if let Some(light) = world.try_resource::<DirectionalLight>() {
        lines.push(format!(
            "SUN I{:.2} A{:.2} D({:.2},{:.2},{:.2})",
            light.intensity,
            light.ambient.x,
            light.direction.x,
            light.direction.y,
            light.direction.z,
        ));
    }
    lines.push(ascii_only(&state.status));
    lines.push("- - - - - - - - - - - - - - - - - - -".to_string());
    lines.push("LMB pick/drag   RMB place   L light".to_string());
    lines.push("TAB kind   R material   C collider   G snap".to_string());
    lines.push("DEL delete   CTRL+D copy   CTRL+S save".to_string());
    lines.push("CTRL+Z undo   CTRL+Y redo".to_string());
    lines.push("ARROWS nudge 0.1  ALT=Y  SHIFT=x10".to_string());
    lines.push("PGUP/PGDN scale 1.1".to_string());
    lines.push("[ ] light I  -/= range  H hue  K sh".to_string());
    lines.push("no sel: [] sun I  ;' amb  JLIK dir".to_string());
    lines.push("B bake   N nav   F focus   MMB orbit".to_string());
    lines.push("WASD/QE fly  WHEEL zoom  ESC quit".to_string());
    lines.join("\n")
}

/// 生成属性面板 HUD 实体（[`UiText`] + 左上锚定；**无 `Transform`** ⇒
/// `save_scene` 的查询天然不含它）。已存在则不动。
pub fn spawn_hud(world: &mut World) {
    if world.entity_named(HUD_NAME).is_some() {
        return;
    }
    world.spawn((
        Name(HUD_NAME.to_string()),
        UiRect {
            position: [14.0, 12.0],
            size: [0.0, 0.0],
        },
        UiAnchor {
            point: UiAnchorPoint::TopLeft,
            offset: [14.0, 12.0],
        },
        UiText {
            string: String::new(),
            scale: 2.0,
        },
        // 深色板岩：编辑器默认视角的左上区域是浅色天空 / 地面，
        // 白色位图字在其上几乎不可读（验收图目检结论）
        UiColor([0.10, 0.11, 0.15, 1.0]),
    ));
}

/// 把 [`panel_text`] 写进 HUD 实体。
pub fn refresh_hud(world: &mut World) {
    let text = panel_text(world);
    if let Some(entity) = world.entity_named(HUD_NAME) {
        if let Some(mut ui) = world.get_mut::<UiText>(entity) {
            ui.string = text;
        }
    }
}

/// 实体名（无 `Name` 时退化成 `#{index}`；ASCII 折算后面板可直接显示）。
fn name_of(world: &World, entity: Entity) -> String {
    ascii_only(
        &world
            .get::<Name>(entity)
            .map(|name| name.0.clone())
            .unwrap_or_else(|| format!("#{}", entity.index())),
    )
}

// ---------------------------------------------------------------------------
// 纯函数工具
// ---------------------------------------------------------------------------

/// 网格吸附（纯函数）：`(v / grid).round() * grid`。步长非正 / NaN 时原值
/// 返回（= 不吸附）。只用 IEEE 确定运算，无累积状态 ⇒ 同输入逐位同输出。
pub fn snap_value(value: f32, grid: f32) -> f32 {
    if grid.is_nan() || grid <= 0.0 {
        return value;
    }
    (value / grid).round() * grid
}

/// 非 ASCII 字符折成 `?`（8×8 位图字体只有 ASCII 32..=127 字形，渲染器对
/// 非 ASCII 跳过并告警——面板文本先自行折算，避免刷告警且让宽度可预期）。
pub fn ascii_only(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect()
}

/// 屏幕像素（物理像素，原点左上）→ 世界射线 `(origin, dir)`。
///
/// 视口尺寸取 [`Viewport`] 资源（窗口程序由 `WindowPlugin` 插入并在
/// `Resized` 时同步；headless / 离屏路径由调用方预插入），相机取
/// [`Camera`] **资源**（编辑器不挂 `ActiveCamera` 实体，模块文档诚实清单
/// 第 8 条）。像素在视口外同样成立（射线照算，合法性由调用方裁决）。
pub fn screen_ray_at(world: &World, px: f64, py: f64) -> Option<(Vec3, Vec3)> {
    let viewport = world.try_resource::<Viewport>()?;
    let camera = world.try_resource::<Camera>()?;
    let width = viewport.width.max(1) as f32;
    let height = viewport.height.max(1) as f32;
    let ndc = Vec2::new(
        2.0 * px as f32 / width - 1.0,
        1.0 - 2.0 * py as f32 / height,
    );
    let (origin, dir) = camera.screen_ray(ndc, width / height);
    (dir != Vec3::ZERO).then_some((origin, dir))
}

fn set_status(world: &World, text: impl Into<String>) {
    if let Some(mut state) = world.try_resource_mut::<EditorState>() {
        state.status = text.into();
    }
}

// ---------------------------------------------------------------------------
// 测试（纯函数与不依赖 GPU 的小闭环；端到端往返在 tests/edit_headless.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 吸附：0.5 m 步长的精确对照（含负值与已在格点上的值）。
    #[test]
    fn snap_value_matches_exact_grid_points() {
        assert_eq!(snap_value(0.37, 0.5), 0.5);
        assert_eq!(snap_value(0.24, 0.5), 0.0);
        assert_eq!(snap_value(-0.37, 0.5), -0.5);
        assert_eq!(snap_value(-0.24, 0.5), 0.0);
        assert_eq!(snap_value(1.5, 0.5), 1.5, "已在格点上应原值");
        assert_eq!(snap_value(0.25, 0.5), 0.5, "半格向上（round 半值远离零）");
        assert_eq!(snap_value(-0.25, 0.5), -0.5);
    }

    /// 吸附退化：步长 0 / 负 / NaN = 不吸附（原值逐位返回）。
    #[test]
    fn snap_value_degenerates_to_identity() {
        assert_eq!(snap_value(1.234, 0.0).to_bits(), 1.234f32.to_bits());
        assert_eq!(snap_value(1.234, -0.5).to_bits(), 1.234f32.to_bits());
        assert_eq!(
            snap_value(1.234, f32::NAN).to_bits(),
            1.234f32.to_bits(),
            "NaN 步长不满足 grid > 0 ⇒ 原值"
        );
    }

    /// 吸附确定性：同输入两次跑逐位相同（确定性文化）。
    #[test]
    fn snap_value_is_bitwise_deterministic() {
        for value in [-3.7, -0.1, 0.0, 0.1, 2.5, 7.77] {
            let a = snap_value(value, 0.5).to_bits();
            let b = snap_value(value, 0.5).to_bits();
            assert_eq!(a, b, "value = {value}");
        }
    }

    /// 图元循环：四种、顺序固定、绕回。
    #[test]
    fn place_kind_cycles_through_four() {
        assert_eq!(PlaceKind::Cube.next(), PlaceKind::Plane);
        assert_eq!(PlaceKind::Plane.next(), PlaceKind::Sphere);
        assert_eq!(PlaceKind::Sphere.next(), PlaceKind::Cylinder);
        assert_eq!(PlaceKind::Cylinder.next(), PlaceKind::Cube, "循环绕回");
        assert_eq!(PlaceKind::ALL.len(), 4);
    }

    /// 材质预设互不相同（位模式级）：加载侧按位模式去重材质，位相同会
    /// 并成一个句柄、破坏句柄编号对齐不变量（见 [`MATERIAL_PRESETS`] 文档）。
    #[test]
    fn material_presets_are_pairwise_distinct() {
        let defs: Vec<MaterialDef> = MATERIAL_PRESETS.iter().map(|p| p.def()).collect();
        for (i, a) in defs.iter().enumerate() {
            for (j, b) in defs.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    material_bits(a),
                    material_bits(b),
                    "预设 {} 与 {} 位模式相同",
                    MATERIAL_PRESETS[i].name,
                    MATERIAL_PRESETS[j].name
                );
            }
        }
        // 灯泡材质也不得与任何预设位相同
        let bulb = material_bits(&bulb_def());
        for (index, def) in defs.iter().enumerate() {
            assert_ne!(bulb, material_bits(def), "灯泡与预设 {index} 位模式相同");
        }
    }

    fn material_bits(def: &MaterialDef) -> Vec<u32> {
        let mut bits = Vec::new();
        bits.extend(def.base_color.iter().map(|v| v.to_bits()));
        bits.extend(def.emissive.iter().map(|v| v.to_bits()));
        bits.push(def.metallic.to_bits());
        bits.push(def.roughness.to_bits());
        bits.push(def.texture.0);
        bits
    }

    /// 碰撞体派生：平面网格无厚度 ⇒ 盒半厚抬到阈值；立方体 ⇒ 半幅 × scale。
    #[test]
    fn collider_for_derives_from_mesh_bounds() {
        let plane = PrimitiveSection::Plane { size: 4.0 };
        let data = plane.to_mesh_data().expect("平面生成");
        let collider = collider_for(&plane, data.bounds().expect("有顶点"), Vec3::ONE);
        assert_eq!(
            collider.shape,
            ColliderShape::Box {
                half_extents: Vec3::new(2.0, MIN_COLLIDER_HALF_THICKNESS, 2.0)
            },
            "平面盒半厚应抬到阈值（退化盒在物理侧无意义）"
        );
        assert!(collider.is_static, "摆放的碰撞体恒静态");

        let cube = PrimitiveSection::Cube;
        let data = cube.to_mesh_data().expect("立方体生成");
        let collider = collider_for(
            &cube,
            data.bounds().expect("有顶点"),
            Vec3::new(2.0, 3.0, 4.0),
        );
        assert_eq!(
            collider.shape,
            ColliderShape::Box {
                half_extents: Vec3::new(1.0, 1.5, 2.0)
            },
            "半幅 = 局部半幅 × scale 逐轴"
        );

        let sphere = PrimitiveSection::Sphere {
            radius: 0.5,
            segments: 24,
            rings: 16,
        };
        let data = sphere.to_mesh_data().expect("球生成");
        let collider = collider_for(&sphere, data.bounds().expect("有顶点"), Vec3::splat(2.0));
        assert_eq!(
            collider.shape,
            ColliderShape::Sphere { radius: 1.0 },
            "球半径 = 段参数半径 × scale.x"
        );
    }

    /// 选中框：12 条边、边中心 = 角点中点、沿轴边的 scale = 边长 + 粗细。
    #[test]
    fn gizmo_edge_transforms_forms_a_box_frame() {
        let min = Vec3::new(-1.0, 0.0, -2.0);
        let max = Vec3::new(1.0, 3.0, 2.0);
        let edges = gizmo_edge_transforms(min, max);
        assert_eq!(edges.len(), GIZMO_EDGE_COUNT);
        let lo = min - Vec3::splat(GIZMO_MARGIN);
        let hi = max + Vec3::splat(GIZMO_MARGIN);
        // 第 0 条：底面沿 X 的边（z = lo.z、y = lo.y）
        assert_eq!(edges[0].position, Vec3::new(0.0, lo.y, lo.z));
        assert_eq!(
            edges[0].scale,
            Vec3::new(
                2.0 * (hi.x - lo.x) + GIZMO_THICKNESS,
                GIZMO_THICKNESS,
                GIZMO_THICKNESS
            )
        );
        // 第 8 条：沿 Y 的边（x = lo.x、z = lo.z）
        assert_eq!(
            edges[8].position,
            Vec3::new(lo.x, (lo.y + hi.y) * 0.5, lo.z)
        );
        assert_eq!(
            edges[8].scale,
            Vec3::new(
                GIZMO_THICKNESS,
                2.0 * (hi.y - lo.y) + GIZMO_THICKNESS,
                GIZMO_THICKNESS
            )
        );
        // 每条边都恰好只有一个轴是「长边」（其余两轴 = 粗细）
        for (index, edge) in edges.iter().enumerate() {
            let long = [edge.scale.x, edge.scale.y, edge.scale.z]
                .iter()
                .filter(|v| **v > GIZMO_THICKNESS + 1e-6)
                .count();
            assert_eq!(long, 1, "边 {index} 应恰有一个长轴：{:?}", edge.scale);
        }
    }

    /// 局部 AABB → 世界 AABB：无旋转 / 无缩放时逐位等于原盒；绕 Y 转 45°
    /// 的单位立方体 ⇒ 世界 AABB 半幅 = 0.5 × (|cos45| + |sin45|) = 1/√2
    /// ≈ 0.70710678（1e-6 容差覆盖浮点乘加次序差异）。
    #[test]
    fn local_aabb_to_world_handles_identity_and_rotation() {
        let model = Mat4::IDENTITY;
        let (min, max) = local_aabb_to_world([-1.0, -2.0, -3.0], [1.0, 2.0, 3.0], &model);
        assert_eq!(min, Vec3::new(-1.0, -2.0, -3.0));
        assert_eq!(max, Vec3::new(1.0, 2.0, 3.0));

        let rotated = Mat4::from_rotation_y(45.0f32.to_radians());
        let (min, max) = local_aabb_to_world([-0.5; 3], [0.5; 3], &rotated);
        // 半幅 = 0.5 × (|cos45| + |sin45|) = 1/√2
        let expected = std::f32::consts::FRAC_1_SQRT_2;
        assert!((min.x + expected).abs() < 1e-6, "min.x = {}", min.x);
        assert!((max.x - expected).abs() < 1e-6, "max.x = {}", max.x);
        assert!((min.y + 0.5).abs() < 1e-6, "y 不受绕 Y 旋转影响");
        assert!((max.z - expected).abs() < 1e-6, "max.z = {}", max.z);
    }

    /// 面板文本折算：非 ASCII → `?`，ASCII 原样（位图字体只有 ASCII 字形）。
    #[test]
    fn ascii_only_folds_non_ascii() {
        assert_eq!(ascii_only("cube/3"), "cube/3");
        assert_eq!(ascii_only("庭院/3"), "??/3");
        assert_eq!(ascii_only(""), "");
    }

    /// 碰撞体 / 图元标签：面板显示格式钉住（数值 `{:.2}`）。
    #[test]
    fn labels_are_stable_ascii() {
        assert_eq!(primitive_label(&PrimitiveSection::Cube), "cube");
        assert_eq!(
            primitive_label(&PrimitiveSection::Plane { size: 4.0 }),
            "plane"
        );
        assert_eq!(
            collider_label(&Collider::static_sphere(0.5)),
            "sphere(0.50)"
        );
        assert_eq!(
            collider_label(&Collider::static_box(Vec3::new(1.0, 0.25, 2.0))),
            "box(1.00,0.25,2.00)"
        );
    }

    /// HSV 往返：基色相旋转 360° 回到自身（浮点 1e-6）；饱和度 / 明度
    /// 在旋转下保持（解析对照）；灰色（s = 0）旋转原值逐位返回。
    #[test]
    fn hue_rotation_preserves_saturation_and_value() {
        let red = Vec3::new(1.0, 0.25, 0.1);
        let (h, s, v) = rgb_to_hsv(red);
        assert!(
            (h - 10.0).abs() < 0.01,
            "色相（解析 60×(0.25−0.1)/0.9 = 10°）：{h}"
        );
        assert!((s - 0.9).abs() < 1e-6, "饱和度 = (1 − 0.1)/1 = 0.9");
        assert!((v - 1.0).abs() < 1e-6, "明度 = max = 1");
        // 同色相 + 360° = 原色（1e-6，除法 / mod 的浮点误差界）
        let wrapped = rotate_hue(red, 360.0);
        assert!(
            (wrapped - red).abs().max_element() < 1e-6,
            "旋转一整圈应回原色：{wrapped:?}"
        );
        // 旋转保持 s / v
        let rotated = rotate_hue(red, HUE_STEP_DEG);
        let (_, s2, v2) = rgb_to_hsv(rotated);
        assert!((s2 - s).abs() < 1e-5, "饱和度保持");
        assert!((v2 - v).abs() < 1e-6, "明度保持");
        // 灰色：s = 0 ⇒ 原值逐位
        let gray = Vec3::splat(0.6);
        assert_eq!(rotate_hue(gray, 37.0).x.to_bits(), gray.x.to_bits());
        assert_eq!(rotate_hue(gray, 37.0).z.to_bits(), gray.z.to_bits());
        // 非有限输入：原样返回
        let nan = Vec3::NAN;
        assert!(rotate_hue(nan, 30.0).is_nan());
    }

    /// HSV 确定性：同输入两次旋转逐位相同（无累积状态）。
    #[test]
    fn hue_rotation_is_bitwise_deterministic() {
        let color = Vec3::new(1.0, 0.82, 0.58);
        let a = rotate_hue(color, 30.0);
        let b = rotate_hue(color, 30.0);
        let (a_bits, b_bits) = (
            a.to_array().map(|v| v.to_bits()),
            b.to_array().map(|v| v.to_bits()),
        );
        assert_eq!(a_bits, b_bits);
    }

    /// 轨道旋转：绕 target 偏航 +90°，eye 从 +X 象限转到 +Z 象限；
    /// 同式解析对照逐位一致；半径一般情形 1e-5 相对保持、轴向特例逐位。
    #[test]
    fn orbit_eye_yaw_quarter_turn_matches_analytic_form() {
        let target = Vec3::new(1.0, 0.5, -2.0);
        let eye = target + Vec3::new(4.0, 0.0, 0.0);
        let dyaw = std::f32::consts::FRAC_PI_2;
        let turned = orbit_eye(eye, target, dyaw, 0.0);
        // 解析式同式对照（同表达式 ⇒ 逐位一致）
        let (yaw, pitch, r) = (0.0f32.atan2(4.0) + dyaw, 0.0f32, 4.0f32);
        let expected = target
            + Vec3::new(
                r * pitch.cos() * yaw.cos(),
                r * pitch.sin(),
                r * pitch.cos() * yaw.sin(),
            );
        assert_eq!(
            turned.to_array().map(|v| v.to_bits()),
            expected.to_array().map(|v| v.to_bits()),
            "同式解析对照"
        );
        // 象限：偏航 +90° 后 eye 应在 target 的 +Z 侧（x 分量回 target）
        assert!((turned.x - target.x).abs() < 1e-5);
        assert!(turned.z - target.z > 3.9, "eye 转到 +Z 象限：{turned:?}");
        // 半径：1e-5 相对保持
        let radius = (turned - target).length();
        assert!(
            (radius - 4.0).abs() < 1e-5 * 4.0,
            "半径保持：{radius} vs 4.0"
        );
    }

    /// 轨道俯仰钳制：大幅上转让 pitch 停在 ±89°（eye 不越过竖直轴，
    /// gimbal 退化不可达）；半径保持。
    #[test]
    fn orbit_eye_clamps_pitch_at_89_degrees() {
        let target = Vec3::ZERO;
        let eye = Vec3::new(10.0, 0.0, 0.0);
        let turned = orbit_eye(eye, target, 0.0, 10.0);
        let offset = turned;
        // pitch = +89° ⇒ y / r = sin 89°
        let r = offset.length();
        assert!(
            ((offset.y / r) - ORBIT_PITCH_LIMIT.sin()).abs() < 1e-6,
            "俯仰停在 +89°：{}",
            offset.y / r
        );
        assert!((r - 10.0).abs() < 1e-5 * 10.0, "半径保持");
        // 反向同理
        let turned = orbit_eye(eye, target, 0.0, -10.0);
        let r = turned.length();
        assert!(
            ((turned.y / r) + ORBIT_PITCH_LIMIT.sin()).abs() < 1e-6,
            "俯仰停在 −89°"
        );
    }

    /// 轨道退化：eye == target（零半径）与非有限 offset 原样返回。
    #[test]
    fn orbit_eye_degenerates_to_identity() {
        let target = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(orbit_eye(target, target, 1.0, 1.0), target);
        let eye = target + Vec3::new(f32::NAN, 1.0, 0.0);
        assert!(orbit_eye(eye, target, 1.0, 0.0).x.is_nan());
    }

    /// 光方向旋转：+X 方向偏航 +90° → +Z（约定同 orbit）；俯仰步进解析
    /// 对照 + 限位 ±89°（竖直向下再下推一步 = 钳在 −89°，不翻转）；输出
    /// 归一化（‖·‖ = 1 ± 1e-6）。
    #[test]
    fn light_direction_rotation_yaw_and_clamp() {
        let dir = Vec3::X;
        let turned = rotate_light_direction(dir, std::f32::consts::FRAC_PI_2, 0.0);
        assert!(turned.z > 0.99, "偏航 +90°：+X → +Z，实际 {turned:?}");
        assert!(turned.length() > 1.0 - 1e-6 && turned.length() < 1.0 + 1e-6);
        // 竖直向下（pitch = −90°）上抬一步：pitch = −π/2 + 0.05（限位内）
        let down = Vec3::new(0.0, -1.0, 0.0);
        let lifted = rotate_light_direction(down, 0.0, SUN_DIR_STEP);
        let pitch = lifted.y.asin();
        assert!(
            (pitch - (-std::f32::consts::FRAC_PI_2 + SUN_DIR_STEP)).abs() < 1e-5,
            "步进解析对照：{pitch}"
        );
        assert!(pitch.abs() <= SUN_PITCH_LIMIT + 1e-6);
        // 反向下推一步：钳在 −89°（|pitch| 不越过限位 = 翻转不可达）
        let pushed = rotate_light_direction(down, 0.0, -SUN_DIR_STEP);
        assert!(
            (pushed.y.asin() + SUN_PITCH_LIMIT).abs() < 1e-4,
            "俯仰限位 −89°：{}",
            pushed.y.asin().to_degrees()
        );
    }

    /// 导航 bounds 推导（纯函数）：两盒并集的精确对照；空集合 / XZ
    /// 退化返回 `None`。
    #[test]
    fn nav_bounds_from_statics_unions_axis_aligned_extents() {
        let a = (
            Vec3::new(2.0, 0.5, -1.0),
            ColliderShape::Box {
                half_extents: Vec3::new(1.0, 0.5, 1.0),
            },
        );
        let b = (
            Vec3::new(-3.0, 0.0, 4.0),
            ColliderShape::Sphere { radius: 2.0 },
        );
        let (min, max) = nav_bounds_from_statics(&[a, b]).expect("有静态体");
        assert_eq!(min, Vec3::new(-5.0, -2.0, -2.0));
        assert_eq!(max, Vec3::new(3.0, 2.0, 6.0));
        // 空集合
        assert_eq!(nav_bounds_from_statics(&[]), None);
        // XZ 退化：单个体 XZ 半幅为 0
        let degenerate = (
            Vec3::ZERO,
            ColliderShape::Box {
                half_extents: Vec3::new(0.0, 1.0, 0.0),
            },
        );
        assert_eq!(nav_bounds_from_statics(&[degenerate]), None);
    }
}
