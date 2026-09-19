//! 场景参数热重载（C5b · 闭合战役格式役 #12，§8.4 场景半边翻转）：
//! **材质 / 光照数值参数子集**的运行期重应用。
//!
//! ## 它是什么
//! 开发期工具：`load_scene` 之后世界与场景文件的关联即断开（既有
//! 事实）；本模块把这条关联重新接上——[`SceneHotReload`] 资源指向
//! 场景文件，[`hot_reload_scene_params`] 系统轮询文件内容（std-only，
//! 不引 notify/fswatch 依赖），内容变化即经 [`crate::scene`] 的
//! `build_scene_param_patch` + `apply_scene_param_patch` 把**具名实体**
//! 的材质数值段、点光段与环境方向光段就地写回世界——不 reload、
//! 不重建实体，游戏状态（位置 / 速度 / 模拟进度）原样保留。
//! 典型用法：手改场景 JSON 调灯 / 调材质 → 半秒内视口生效。
//!
//! ## 内容指纹而非 mtime（为什么轮询可靠）
//! 每次到期轮询读全文件并做 FNV-1a 64 位哈希，与上次所见比对：
//! mtime 粒度（文件系统时钟 + 编辑器原子替换可保留旧 mtime）与
//! 「复制粘贴未改内容」两类噪声都被滤掉；改动检测不依赖时钟精度，
//! 也就不存在「测试里两次写同一 mtime 漏检」的竞态。
//!
//! ## 诚实限制（§8.65 对偶）
//! - 首次轮询只**建立基线不应用**——插入资源到首次轮询之间游戏代码
//!   对材质的运行期改动不会被文件值覆写（「谁最后说话」保持直觉：
//!   文件只在**变化时**赢）；
//! - 只吃「材质数值 + 点光 + 方向光」子集：换贴图源 / 换网格 / 增删
//!   实体 / transform / 粒子 / 碰撞体等结构级编辑一律不生效（需要
//!   完整重载，明说而非半做）；
//! - 匹配键 = 实体 `Name`：无名实体的材质 / 点光段无从匹配，静默跳过；
//! - 材质句柄去重 ⇒ 共享同句柄的实体同步变化（同一处编辑的语义本体）；
//! - wasm 运行时无文件系统：`std::fs::read` 恒失败 ⇒ 系统静默跳过
//!   （与 `vfs` / `scene` 的「编译可用、运行不可用」口径一致）；
//! - 解析失败（编辑到一半的坏 JSON）⇒ 告警一次、该份内容记为已消费
//!   （坏文件修好 = 新内容 = 新哈希 = 再应用），不逐帧刷错误日志。

use std::path::PathBuf;

use iris_core::prelude::{Resource, Time, World};

use crate::scene::{
    ParamReloadReport, SceneError, apply_scene_param_patch, build_scene_param_patch,
};

/// 热重载监听目标（World 资源）：**缺席 = 系统零触碰**——`DefaultPlugins`
/// 恒注册 [`hot_reload_scene_params`]，但只有游戏 / 编辑器插入本资源后
/// 才发生任何文件读取（缺席即逐位不变的兼容形态，与 `TaaSettings` /
/// `MipSettings` 等「资源缺席 = 关」范式同款）。
#[derive(Resource)]
pub struct SceneHotReload {
    /// 被监听的场景 / 关卡文件路径。
    pub path: PathBuf,
    /// 轮询间隔（秒）。`Time` 资源在场时生效；headless / 无 `Time` ⇒
    /// 每次系统调用都检查（测试与专用工具口径）。
    pub poll_interval_secs: f64,
    /// 上次所见内容的 FNV-1a 指纹（`None` = 尚未建立基线）。
    last_hash: Option<u64>,
    /// 上次轮询的 `Time::elapsed_secs`（`None` = 未轮询过或无 `Time`）。
    last_poll: Option<f64>,
}

impl SceneHotReload {
    /// 以默认间隔（0.25 s）监听给定场景文件。
    pub fn watch(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            poll_interval_secs: 0.25,
            last_hash: None,
            last_poll: None,
        }
    }
}

impl Default for SceneHotReload {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            poll_interval_secs: 0.25,
            last_hash: None,
            last_poll: None,
        }
    }
}

/// FNV-1a 64 位（本仓快照测试同款常数；内容指纹只需「变化必翻位」）。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 热重载系统（`DefaultPlugins` 注册到 Update 阶段）：到期轮询 →
/// 内容变化则应用参数补丁。资源缺席 / 未到间隔 / 文件不可读 /
/// 内容未变 ⇒ 零触碰（回执仅测试 / 工具观测用，调度器忽略）。
pub fn hot_reload_scene_params(world: &mut World) {
    let _ = scene_hot_reload_once(world);
}

/// 单次检查的可测核心：返回
/// - `None`：本次无事可做（资源缺席 / 未到间隔 / 不可读 / 内容未变 /
///   基线建立）；
/// - `Some(Ok(report))`：补丁已应用；
/// - `Some(Err(e))`：内容变了但解析失败（该份内容已记为消费，不重试）。
pub fn scene_hot_reload_once(world: &mut World) -> Option<Result<ParamReloadReport, SceneError>> {
    let (path, last_hash, elapsed) = {
        let cfg = world.try_resource::<SceneHotReload>()?;
        // 节流只认 `Time` 时基；无 `Time`（headless）⇒ 每次到期
        let elapsed = world
            .try_resource::<Time>()
            .map(|t| t.elapsed_secs() as f64);
        if let (Some(last), Some(e)) = (cfg.last_poll, elapsed) {
            if e - last < cfg.poll_interval_secs {
                return None;
            }
        }
        (cfg.path.clone(), cfg.last_hash, elapsed)
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            // wasm 运行时 / 文件暂缺（编辑器原子保存的瞬间）：静默跳过，
            // debug 级留痕，不视为错误
            log::debug!("热重载：读取「{}」失败（{e}），本轮跳过", path.display());
            if let Some(mut cfg) = world.try_resource_mut::<SceneHotReload>() {
                cfg.last_poll = elapsed;
            }
            return None;
        }
    };
    let hash = fnv1a64(&bytes);
    // 消费判定与基线推进一次完成：坏内容同样消费（修好的文件 = 新内容）
    let first_sight = last_hash.is_none();
    let changed = last_hash != Some(hash);
    if let Some(mut cfg) = world.try_resource_mut::<SceneHotReload>() {
        cfg.last_poll = elapsed;
        cfg.last_hash = Some(hash);
    }
    if first_sight {
        // 基线建立轮：不应用（游戏侧运行期改动优先于文件旧值，见模块头）
        return None;
    }
    if !changed {
        return None;
    }
    match build_scene_param_patch(&bytes) {
        Ok(patch) => {
            let report = apply_scene_param_patch(world, &patch);
            log::info!(
                "热重载：「{}」已应用（方向光 {} · 材质 {} · 点光 {} · 未命中 {}）",
                path.display(),
                report.directional_applied as u8,
                report.materials_applied,
                report.point_lights_applied,
                report.unmatched.len()
            );
            Some(Ok(report))
        }
        Err(e) => {
            log::warn!(
                "热重载：「{}」新内容解析失败（{e}），该版本已跳过——修好文件会再次生效",
                path.display()
            );
            Some(Err(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::load_scene;
    use iris_render::prelude::{
        DirectionalLight, MaterialHandle, MaterialStore, MeshStore, PointLight, TextureHandle,
        TextureStore,
    };
    use std::path::{Path, PathBuf};

    /// 唯一临时文件（pid + 调用序计数，AGENTS 竞态铁律缺一不可）。
    fn tmp_scene(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join("iris-hotreload-test");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir.join(format!("hr_{tag}_{}_{n}.scene.json", std::process::id()))
    }

    fn scene_json(dir_intensity: f32, rough: f32, lamp: f32) -> String {
        format!(
            r#"{{
                "environment": {{ "directional_light": {{ "intensity": {dir_intensity} }} }},
                "entities": [
                    {{ "name": "lamp", "point_light": {{ "intensity": {lamp} }} }},
                    {{ "name": "ball",
                       "material": {{ "color": [0.9, 0.1, 0.1, 1.0], "roughness": {rough},
                                      "texture": {{ "grass": {{ "size": 8 }} }} }} }}
                ]
            }}"#
        )
    }

    fn loaded_world(path: &Path) -> World {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        load_scene(&mut world, path).expect("加载场景");
        world
    }

    /// 端到端：资源缺席 = 零触碰；插入后首询只建基线（游戏侧运行期
    /// 改动不被覆写）；文件内容变化 ⇒ 材质数值 / 点光 / 方向光就地生效，
    /// 纹理句柄原样（数值子集不含换图）。
    #[test]
    fn hot_reload_applies_param_subset_and_respects_absence() {
        let path = tmp_scene("apply");
        std::fs::write(&path, scene_json(3.0, 0.7, 12.0)).expect("写初始档");
        let mut world = loaded_world(&path);
        // 记下初始状态（加载即文件值）
        let ball = world.entity_named("ball").unwrap();
        let mat_handle = *world.get::<MaterialHandle>(ball).unwrap();
        let tex_before = world
            .resource::<MaterialStore>()
            .get(mat_handle)
            .unwrap()
            .texture;
        assert_ne!(tex_before, TextureHandle(0), "程序化草图应占独立句柄");
        world.insert_resource(SceneHotReload::watch(&path));
        // 游戏侧运行期改材质：首询（基线轮）不得覆写它
        {
            let mut store = world.resource_mut::<MaterialStore>();
            store.get_mut(mat_handle).unwrap().roughness = 0.11;
        }
        assert!(scene_hot_reload_once(&mut world).is_none(), "基线轮不应用");
        {
            let store = world.resource::<MaterialStore>();
            assert!(
                (store.get(mat_handle).unwrap().roughness - 0.11).abs() < 1e-6,
                "首询不得覆写运行期改动"
            );
        }
        // 文件变化：改材质粗度 + 点光强度 + 方向光强度
        std::fs::write(&path, scene_json(9.0, 0.3, 30.0)).expect("写新档");
        let report = scene_hot_reload_once(&mut world)
            .expect("内容变化必应用")
            .expect("合法新内容");
        assert_eq!(report.materials_applied, 1);
        assert_eq!(report.point_lights_applied, 1);
        assert!(report.directional_applied);
        assert!(report.unmatched.is_empty());
        {
            let store = world.resource::<MaterialStore>();
            let def = store.get(mat_handle).unwrap();
            assert!((def.roughness - 0.3).abs() < 1e-6);
            assert_eq!(def.base_color, [0.9, 0.1, 0.1, 1.0]);
            assert_eq!(
                def.texture, tex_before,
                "纹理句柄不随热重载变化（数值子集）"
            );
        }
        let lamp = world.entity_named("lamp").unwrap();
        assert!(
            (world.get::<PointLight>(lamp).unwrap().intensity - 30.0).abs() < 1e-6,
            "点光强度随补丁更新"
        );
        assert!(
            (world.resource::<DirectionalLight>().intensity - 9.0).abs() < 1e-6,
            "方向光资源随补丁更新"
        );
        // 同内容再询：零触碰（指纹未变）
        assert!(scene_hot_reload_once(&mut world).is_none());
        std::fs::remove_file(&path).ok();
    }

    /// 资源缺席：系统函数零分配零读取（不 panic、世界不动）。
    #[test]
    fn system_is_noop_without_resource() {
        let mut world = World::new();
        hot_reload_scene_params(&mut world);
        assert!(!world.contains_resource::<SceneHotReload>());
    }

    /// 坏内容消费不重试：解析失败的这一版记为已见；修好后（新内容）
    /// 再应用。非默认 `poll_interval_secs` 断言：`Time` 在场 + 间隔
    /// 巨大 ⇒ 未到期不读文件。
    #[test]
    fn broken_content_consumed_once_and_throttle_knobs() {
        let path = tmp_scene("broken");
        std::fs::write(&path, scene_json(2.0, 0.5, 8.0)).expect("写初始档");
        let mut world = loaded_world(&path);
        world.insert_resource(Time::default());
        let mut cfg = SceneHotReload::watch(&path);
        cfg.poll_interval_secs = 1e9; // 非默认值：永不到期（elapsed=0 恒定）
        world.insert_resource(cfg);
        scene_hot_reload_once(&mut world); // 基线轮（首询不受节流阻挡）
        // 基线后未到间隔 ⇒ 即使文件变化也不读
        std::fs::write(&path, scene_json(5.0, 0.2, 20.0)).expect("写新档");
        assert!(
            scene_hot_reload_once(&mut world).is_none(),
            "节流生效：未到期零应用"
        );
        {
            let store = world.resource::<MaterialStore>();
            let h = *world
                .get::<MaterialHandle>(world.entity_named("ball").unwrap())
                .unwrap();
            assert!(
                (store.get(h).unwrap().roughness - 0.5).abs() < 1e-6,
                "节流期内文件值不得渗入"
            );
        }
        // 间隔归零 ⇒ 立即放行；先写坏 JSON：Err 一次且被消费
        world
            .try_resource_mut::<SceneHotReload>()
            .unwrap()
            .poll_interval_secs = 0.0;
        std::fs::write(&path, "{ not json").expect("写坏档");
        let bad = scene_hot_reload_once(&mut world).expect("坏内容必有回执");
        assert!(bad.is_err(), "坏 JSON 解析失败");
        assert!(
            scene_hot_reload_once(&mut world).is_none(),
            "同一坏版本不重试（消费）"
        );
        // 修好 = 新内容 ⇒ 再应用
        std::fs::write(&path, scene_json(5.0, 0.2, 20.0)).expect("修好");
        let report = scene_hot_reload_once(&mut world)
            .expect("新内容应用")
            .expect("合法");
        assert_eq!(report.materials_applied, 1);
        {
            let store = world.resource::<MaterialStore>();
            let h = *world
                .get::<MaterialHandle>(world.entity_named("ball").unwrap())
                .unwrap();
            assert!((store.get(h).unwrap().roughness - 0.2).abs() < 1e-6);
        }
        std::fs::remove_file(&path).ok();
    }

    /// 未命中面：档内具名实体在世界中不存在（增删实体属结构级编辑）⇒
    /// 进 `unmatched` 清单、其余照常应用；无名实体的材质段不入补丁表。
    #[test]
    fn unmatched_names_collected_and_unnamed_sections_ignored() {
        let path = tmp_scene("miss");
        std::fs::write(
            &path,
            r#"{
                "entities": [
                    { "name": "ghost", "material": { "color": [0,0,0,1] } },
                    { "material": { "color": [1,1,1,1] } }
                ]
            }"#,
        )
        .expect("写档");
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        let patch = build_scene_param_patch(&std::fs::read(&path).expect("读档")).expect("合法档");
        assert_eq!(patch.entities.len(), 1, "无名段不入表");
        let report = apply_scene_param_patch(&mut world, &patch);
        assert_eq!(report.unmatched, vec!["ghost".to_string()]);
        assert_eq!(report.materials_applied, 0);
        std::fs::remove_file(&path).ok();
    }

    /// 去重共享语义：两实体同材质 ⇒ 同一句柄；按**其一**的名字热改 ⇒
    /// 两者同步生效（写句柄指向的目录项，§8.65④）。
    #[test]
    fn dedup_shared_material_updates_all_earners() {
        let path = tmp_scene("shared");
        std::fs::write(
            &path,
            r#"{
                "entities": [
                    { "name": "a", "material": { "color": [0.5, 0.5, 0.5, 1.0], "roughness": 0.4 } },
                    { "name": "b", "material": { "color": [0.5, 0.5, 0.5, 1.0], "roughness": 0.4 } }
                ]
            }"#,
        )
        .expect("写初始档");
        let mut world = loaded_world(&path);
        let ha = *world
            .get::<MaterialHandle>(world.entity_named("a").unwrap())
            .unwrap();
        let hb = *world
            .get::<MaterialHandle>(world.entity_named("b").unwrap())
            .unwrap();
        assert_eq!(ha, hb, "加载期内容去重共享句柄");
        std::fs::write(
            &path,
            r#"{
                "entities": [
                    { "name": "a", "material": { "color": [0.1, 0.9, 0.2, 1.0], "roughness": 0.8 } },
                    { "name": "b", "material": { "color": [0.5, 0.5, 0.5, 1.0], "roughness": 0.4 } }
                ]
            }"#,
        )
        .expect("写新档");
        world.insert_resource(SceneHotReload::watch(&path));
        scene_hot_reload_once(&mut world); // 基线轮
        std::fs::write(
            &path,
            r#"{
                "entities": [
                    { "name": "a", "material": { "color": [0.1, 0.9, 0.2, 1.0], "roughness": 0.8 } }
                ]
            }"#,
        )
        .expect("写第三版（只列 a）");
        let report = scene_hot_reload_once(&mut world)
            .expect("应用")
            .expect("合法");
        assert_eq!(report.materials_applied, 1);
        let store = world.resource::<MaterialStore>();
        assert!((store.get(ha).unwrap().roughness - 0.8).abs() < 1e-6);
        assert_eq!(store.get(ha).unwrap().base_color, [0.1, 0.9, 0.2, 1.0]);
        std::fs::remove_file(&path).ok();
    }
}
