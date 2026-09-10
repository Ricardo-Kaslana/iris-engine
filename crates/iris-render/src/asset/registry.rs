//! GLB 动画注册表（R59）：资产路径 → 剪辑库（元数据 + 共享数据），
//! 「外部动画资产从导入到运行」的中枢。
//!
//! # 解决的摩擦
//!
//! R59 之前动画剪辑数据藏在每个 [`GltfInstance`] 的播放器里：每 spawn
//! 一次独立解析一份；游戏侧无法「先看有什么动画再决定 spawn」；同一
//! GLB 多实例重复持有剪辑关键帧数据。注册表把三件事合到一处：
//!
//! - **批量导入**（[`AnimationRegistry::import`]）：不 spawn 实例也能
//!   解析 GLB，登记剪辑元数据（名字 / 时长 / 轨道数 / 事件数）与骨架
//!   节点名表——游戏侧先查目录再决定加载；
//! - **多实例共享**（[`AnimationRegistry::spawn_instance`]）：命中注册
//!   表的 spawn 不再重新解析动画，剪辑数据以 `Arc` 引用计数共享——
//!   N 个实例 + 注册表 = 同一份 `Vec<AnimationClip>`（生命周期 =
//!   Arc 语义：注册表与全部播放器都丢弃后才释放，无悬垂风险）。
//!   **引用计数而非克隆**的取舍：克隆语义只省解析 CPU 不省内存，
//!   「多实例重复持有剪辑数据」的摩擦原样保留；Arc 让内存归零共享，
//!   代价仅是 `AnimationPlayer::clips` 字段类型（读路径经 Deref 全
//!   兼容）。未经理注册表的 spawn（[`spawn_gltf_instance`] 路径）行为
//!   不变：每实例独占一份数据；
//! - **按名取用**（[`GltfAnimationLibrary::clip_index_by_name`]）：
//!   游戏侧按剪辑名拿播放器索引（重名**取先**——声明序确定，与骨架
//!   名表的「重名取先」同一约定）。
//!
//! # 迭代序确定性
//!
//! 注册表用 `BTreeMap<String, _>` 按资产路径排序（不用 HashMap）：与
//! `broadphase.rs` 同一选型理由——需要迭代序的确定性数据结构不用哈希
//! 容器，`iter()` 输出恒按路径字典序，同输入两次导入的遍历 / 调试
//! 输出逐位一致（单测守护）。剪辑查找用线性扫描取首个匹配（向量序
//! 确定 ⇒ 结果确定），glTF 不强制剪辑名唯一，唯一性由资产侧保证。
//!
//! # v1 不含（诚实清单）
//!
//! - 剪辑数据只读共享：注册表命中实例的播放器若**增删**共享
//!   `Vec` 的内容会 panic（`Arc` 强计数 > 1 时不可变借用规则）——
//!   播放器对剪辑的全部既有写法是读（采样），需要修改剪辑的游戏侧
//!   应该走 `AnimationPlayer::new` 独占构造；
//! - 不缓存网格 / 材质 / 纹理：spawn 仍逐实例解析实体树（网格句柄
//!   去重是另一个量级的工程，见 §8 资产管线 v1 限制）；
//! - 导入即全量解析关键帧浮点数据（不做「只读名字」的惰性形态——
//!   GLB 动画数据量级下一次解析成本可接受，双形态徒增状态机）；
//! - 无失效 / 驱逐：库随注册表存活到 World 销毁（`clear` 可显式清空）。

use std::collections::BTreeMap;
use std::sync::Arc;

use iris_core::Resource;

use super::gltf::{self, GltfError, GltfInstance};
use crate::animation::AnimationClip;

/// 单条剪辑的元数据（R59 批量导入的登记内容）：游戏侧「先看有什么
/// 动画」的查询载体。关键帧数据本体在 [`GltfAnimationLibrary::clips`]。
#[derive(Clone, Debug, PartialEq)]
pub struct ClipMeta {
    /// 剪辑名（glTF `animations[i].name`，可能为空串——无名剪辑按
    /// 空名登记，`clip_index_by_name("")` 可查但建议资产侧全部命名）。
    pub name: String,
    /// 剪辑时长（秒，取各通道最后关键帧时间的最大值）。
    pub duration: f32,
    /// 轨道（通道）数。
    pub channels: usize,
    /// 事件数（glTF 标准内无事件，GLB 解析恒 0——API 侧构造的事件
    /// 不经注册表，此处仅为元数据形态完整性保留）。
    pub events: usize,
}

/// 一个 GLB 资产的动画库：共享剪辑数据 + 骨架节点名表 + 逐剪辑元数据。
#[derive(Debug)]
pub struct GltfAnimationLibrary {
    /// 全部剪辑（与 GLB `animations` 数组同序）。
    pub clips: Arc<Vec<AnimationClip>>,
    /// 骨架节点名表：下标 = glTF 节点声明序（与 spawn 后的
    /// `GltfInstance::node_names` 同源同序），无名节点为 `None`。
    pub node_names: Vec<Option<String>>,
    /// 逐剪辑元数据（与 `clips` 同下标对齐）。
    pub clip_meta: Vec<ClipMeta>,
}

impl GltfAnimationLibrary {
    /// 按名查剪辑索引：命中首个同名剪辑（声明序确定 ⇒ 重名时结果
    /// 确定）；未命中返回 [`None`]。
    pub fn clip_index_by_name(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|clip| clip.name == name)
    }

    /// 按名查剪辑元数据（语义同 [`Self::clip_index_by_name`]）。
    pub fn clip_meta_by_name(&self, name: &str) -> Option<&ClipMeta> {
        self.clip_index_by_name(name)
            .map(|index| &self.clip_meta[index])
    }
}

/// GLB 动画注册表（World 资源）：资产路径 → 动画库。迭代序 =
/// 路径字典序（`BTreeMap`，见模块文档）。
#[derive(Debug, Default, Resource)]
pub struct AnimationRegistry {
    libraries: BTreeMap<String, Arc<GltfAnimationLibrary>>,
}

impl AnimationRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 批量导入：解析 GLB 字节为动画库并按 `path` 登记（不 spawn 任何
    /// 实体）。解析错误经 [`GltfError`] 报出（不支持的插值 / 损坏容器
    /// 等——导入期即失败，不留半解析的库）。同路径重复导入以新库整体
    /// 替换（幂等重导入，语义与「文件系统最新内容为准」一致）。
    pub fn import(
        &mut self,
        path: impl Into<String>,
        bytes: &[u8],
    ) -> Result<Arc<GltfAnimationLibrary>, GltfError> {
        let path = path.into();
        let library = Arc::new(Self::parse_library(bytes)?);
        self.libraries.insert(path, Arc::clone(&library));
        Ok(library)
    }

    /// 查询已导入的库（路径精确匹配）。
    pub fn get(&self, path: &str) -> Option<&Arc<GltfAnimationLibrary>> {
        self.libraries.get(path)
    }

    /// 已导入的库数量。
    pub fn len(&self) -> usize {
        self.libraries.len()
    }

    /// 是否为空注册表。
    pub fn is_empty(&self) -> bool {
        self.libraries.is_empty()
    }

    /// 按路径字典序遍历（`path, library`）——确定性迭代口。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<GltfAnimationLibrary>)> {
        self.libraries.iter().map(|(p, lib)| (p.as_str(), lib))
    }

    /// 清空注册表（已在播放器中共享的剪辑数据不受影响——`Arc` 语义，
    /// 数据存活到最后一个持有者）。
    pub fn clear(&mut self) {
        self.libraries.clear();
    }

    /// 经注册表 spawn GLB 实例：命中则复用共享剪辑（不重新解析动画），
    /// 未命中则先自动导入再 spawn（单调用便利形态；导入失败即报错，
    /// 不产生实体）。其余实体树 / 网格 / 材质 / 纹理工作与常规 spawn
    /// 完全一致，返回的 [`GltfInstance`] 的 `player()` 与后续实例共享
    /// 同一份剪辑数据。
    pub fn spawn_instance(
        &mut self,
        world: &mut iris_core::world::World,
        path: &str,
        bytes: &[u8],
    ) -> Result<GltfInstance, GltfError> {
        let library = match self.libraries.get(path) {
            Some(library) => Arc::clone(library),
            None => self.import(path, bytes)?,
        };
        gltf::spawn_gltf_instance_from_bytes_shared(
            world,
            bytes.to_vec(),
            Arc::clone(&library.clips),
        )
    }

    /// 解析 GLB 为动画库（无注册表状态的纯解析口）。
    fn parse_library(bytes: &[u8]) -> Result<GltfAnimationLibrary, GltfError> {
        let (clips, node_names) = gltf::parse_library(bytes.to_vec())?;
        let clip_meta = clips
            .iter()
            .map(|clip| ClipMeta {
                name: clip.name.clone(),
                duration: clip.duration,
                channels: clip.channels.len(),
                events: clip.events.len(),
            })
            .collect();
        Ok(GltfAnimationLibrary {
            clips,
            node_names,
            clip_meta,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::gltf::write_glb;

    /// 最小双剪辑 GLB：两个节点（带名），两条剪辑——"idle"（0..1 s
    /// 平移 bob）与 "hit"（0..0.5 s 旋转 jab）。手工写 JSON + 缓冲。
    fn two_clip_glb() -> Vec<u8> {
        let mut bin: Vec<u8> = Vec::new();
        // accessor 0：bob 时间轴（SCALAR ×2）
        bin.extend_from_slice(&[0.0f32, 1.0].map(f32::to_le_bytes).concat());
        // accessor 1：bob 平移（VEC3 ×2）
        bin.extend_from_slice(
            &[0.0f32, 0.0, 0.0, 0.0, 0.3, 0.0]
                .map(f32::to_le_bytes)
                .concat(),
        );
        // accessor 2：hit 时间轴（SCALAR ×2）
        bin.extend_from_slice(&[0.0f32, 0.5].map(f32::to_le_bytes).concat());
        // accessor 3：hit 旋转（VEC4 ×2，yaw 0° 与 yaw 60°；
        // yaw θ 的四元数 = (0, sin(θ/2), 0, cos(θ/2))，单位化）
        bin.extend_from_slice(&[0.0f32, 0.0, 0.0, 1.0].map(f32::to_le_bytes).concat());
        bin.extend_from_slice(
            &[
                0.0f32,
                (60f32.to_radians() * 0.5).sin(),
                0.0,
                (60f32.to_radians() * 0.5).cos(),
            ]
            .map(f32::to_le_bytes)
            .concat(),
        );
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let json = r#"{
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": 88}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 8},
                {"buffer": 0, "byteOffset": 8, "byteLength": 24},
                {"buffer": 0, "byteOffset": 32, "byteLength": 8},
                {"buffer": 0, "byteOffset": 40, "byteLength": 32}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 2, "type": "SCALAR"},
                {"bufferView": 1, "componentType": 5126, "count": 2, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5126, "count": 2, "type": "SCALAR"},
                {"bufferView": 3, "componentType": 5126, "count": 2, "type": "VEC4"}
            ],
            "nodes": [
                {"name": "Root", "children": [1]},
                {"name": "Board", "translation": [0.0, 1.0, 0.0]}
            ],
            "scenes": [{"nodes": [0]}],
            "scene": 0,
            "animations": [
                {
                    "name": "idle",
                    "channels": [{"sampler": 0, "target": {"node": 1, "path": "translation"}}],
                    "samplers": [{"input": 0, "output": 1, "interpolation": "LINEAR"}]
                },
                {
                    "name": "hit",
                    "channels": [{"sampler": 0, "target": {"node": 1, "path": "rotation"}}],
                    "samplers": [{"input": 2, "output": 3, "interpolation": "LINEAR"}]
                }
            ]
        }"#;
        write_glb(json, &bin)
    }

    #[test]
    fn import_registers_metadata_without_spawning() {
        let mut registry = AnimationRegistry::new();
        let bytes = two_clip_glb();
        let library = registry.import("models/actor.glb", &bytes).unwrap();

        // 元数据：名字 / 时长 / 轨道数 / 事件数逐项对账
        assert_eq!(library.clip_meta.len(), 2);
        let idle = library.clip_meta_by_name("idle").unwrap();
        assert_eq!(idle.name, "idle");
        assert!((idle.duration - 1.0).abs() < 1e-6);
        assert_eq!(idle.channels, 1);
        assert_eq!(idle.events, 0, "GLB 解析不产事件");
        let hit = library.clip_meta_by_name("hit").unwrap();
        assert!((hit.duration - 0.5).abs() < 1e-6);

        // 骨架节点名表：声明序，无名节点为 None
        assert_eq!(
            library.node_names,
            vec![Some("Root".to_string()), Some("Board".to_string())]
        );

        // 注册表状态：1 条、可查、未 spawn 不产生任何副作用
        assert_eq!(registry.len(), 1);
        assert!(registry.get("models/actor.glb").is_some());
        assert!(registry.get("别的路径").is_none());
    }

    #[test]
    fn iteration_order_is_path_lexicographic_btreemap() {
        let mut registry = AnimationRegistry::new();
        let bytes = two_clip_glb();
        // 乱序导入：字典序输出
        registry.import("models/c.glb", &bytes).unwrap();
        registry.import("models/a.glb", &bytes).unwrap();
        registry.import("models/b.glb", &bytes).unwrap();
        let paths: Vec<&str> = registry.iter().map(|(p, _)| p).collect();
        assert_eq!(paths, vec!["models/a.glb", "models/b.glb", "models/c.glb"]);

        // 同输入重复导入：库内容逐位一致（确定性）
        let lib_a = registry.get("models/a.glb").unwrap().clone();
        registry.import("models/a.glb", &bytes).unwrap();
        let lib_a2 = registry.get("models/a.glb").unwrap().clone();
        assert_eq!(lib_a.clip_meta, lib_a2.clip_meta);
        assert_eq!(lib_a.node_names, lib_a2.node_names);
        assert_eq!(lib_a.clips.len(), lib_a2.clips.len());
        assert!((lib_a.clips[0].duration - lib_a2.clips[0].duration).abs() < 1e-7);
    }

    #[test]
    fn spawn_instances_share_clip_data_via_arc() {
        let mut world = iris_core::world::World::new();
        world.insert_resource(crate::mesh::MeshStore::new());
        world.insert_resource(crate::material::MaterialStore::new());
        world.insert_resource(crate::texture::TextureStore::new());
        let mut registry = AnimationRegistry::new();
        let bytes = two_clip_glb();

        let first = registry
            .spawn_instance(&mut world, "models/actor.glb", &bytes)
            .unwrap();
        // 首次 spawn 自动导入
        assert_eq!(registry.len(), 1);
        let second = registry
            .spawn_instance(&mut world, "models/actor.glb", &bytes)
            .unwrap();

        // 多实例共享：两实例的剪辑数据与注册表是同一份分配（指针相等）
        let library = registry.get("models/actor.glb").unwrap();
        assert!(Arc::ptr_eq(&first.clips, &library.clips));
        assert!(Arc::ptr_eq(&second.clips, &first.clips));
        assert_eq!(Arc::strong_count(&library.clips), 3, "注册表 + 两实例");
        // 常规（非注册表）spawn 仍独占：与注册表数据不同源
        let solo = gltf::spawn_gltf_instance_from_bytes(&mut world, bytes.clone()).unwrap();
        assert!(!Arc::ptr_eq(&solo.clips, &library.clips));
        assert_eq!(Arc::strong_count(&library.clips), 3, "独占份不增共享计数");
    }

    #[test]
    fn play_clip_by_name_and_assert_pose() {
        use crate::animation::{AnimatedField, update_animations};
        use iris_core::prelude::Transform;

        let mut world = iris_core::world::World::new();
        world.insert_resource(crate::mesh::MeshStore::new());
        world.insert_resource(crate::material::MaterialStore::new());
        world.insert_resource(crate::texture::TextureStore::new());
        let mut registry = AnimationRegistry::new();
        let bytes = two_clip_glb();
        let instance = registry
            .spawn_instance(&mut world, "models/actor.glb", &bytes)
            .unwrap();
        let board = instance
            .node_entities
            .iter()
            .flatten()
            .copied()
            .nth(1)
            .expect("Board 节点应有实体");

        // 游戏侧按名取用：先查注册表拿索引，再驱动播放器
        let library = registry.get("models/actor.glb").unwrap();
        let hit_index = library.clip_index_by_name("hit").expect("应有 hit 剪辑");
        let mut player = instance.player();
        player.transition(hit_index, 0.0); // 硬切换
        player.looping = false;
        player.time = 0.25; // hit 中点 = yaw 30°
        world.insert(instance.roots[0], player);
        update_animations(&world);

        // 姿势断言：Board 局部旋转 = yaw 30°（slerp 中点）
        let expected = glam::Quat::from_rotation_y(30f32.to_radians());
        let transform = world.get::<Transform>(board).unwrap();
        assert!(
            (transform.rotation.dot(expected) - 1.0).abs() < 1e-4,
            "hit@0.25s 应为 yaw 30°，实际 {:?}",
            transform.rotation
        );
        // 采样产物类型正确性（旋转通道）
        let clip = &library.clips[hit_index];
        assert!(matches!(
            clip.channels[0].sample(0.0),
            AnimatedField::Rotation(_)
        ));
    }

    #[test]
    fn import_rejects_broken_glb_with_error() {
        let mut registry = AnimationRegistry::new();
        let err = registry
            .import("broken.glb", b"not a glb at all")
            .unwrap_err();
        assert!(
            matches!(
                err,
                GltfError::Json(_)
                    | GltfError::Unsupported(_)
                    | GltfError::Missing(_)
                    | GltfError::OutOfRange(_)
            ),
            "实际 {err:?}"
        );
        assert!(registry.is_empty(), "失败的导入不留半解析的库");
    }
}
