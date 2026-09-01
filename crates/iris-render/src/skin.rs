//! 骨骼蒙皮：[`Skin`] 组件与线性混合蒙皮（LBS）数学。
//!
//! 数据模型（与 glTF 2.0 skins 对应）：
//! - `Skin` 挂在带网格的实体上，持有「关节实体列表 + 逆绑定矩阵」；
//! - 关节是普通实体（通常是 [`crate::animation::AnimationPlayer`] 的动画目标），
//!   其 `GlobalTransform` 由层级传播维护；
//! - 渲染时每帧计算**关节矩阵**：
//!   `J_i = inv(蒙皮实体全局变换) × global(joint_i) × inverse_bind_i`，
//!   即把蒙皮定义在网格自身的局部空间——移动蒙皮实体的父级，整具骨骼跟随。
//!
//! GPU 侧：关节矩阵经只读 storage buffer 进入顶点着色器，
//! `skinned = Σ w_i · J_i · v`（见 standard.wgsl `vs_skinned`）。
//!
//! ```ignore
//! let instance = spawn_gltf_instance(&mut world, "assets/character.glb")?;
//! // 加载器已为蒙皮网格实体挂好 Skin，并把兄弟顶层关节树收编为根实体
//! // 的子树（移动根即移动整只蒙皮角色）；挂上播放器即可驱动关节动画
//! world.insert(instance.roots[0], instance.player());
//! ```

use glam::Mat4;
use iris_core::prelude::{Entity, GlobalTransform, World};

/// 蒙皮组件：挂在带网格的实体上，把网格顶点绑定到一组关节实体。
#[derive(Clone, Debug, iris_core::Component)]
pub struct Skin {
    /// 关节实体（顺序与逆绑定矩阵一一对应；顶点属性中的索引指向此表）。
    pub joints: Vec<Entity>,
    /// 逆绑定矩阵：关节**绑定姿势**全局变换的逆，把顶点从网格空间
    /// 变换到「以该关节为原点」的局部空间。
    pub inverse_bind_matrices: Vec<Mat4>,
}

impl Skin {
    /// 构造蒙皮。两表长度必须一致且非空（数据错误应尽早暴露）。
    pub fn new(joints: Vec<Entity>, inverse_bind_matrices: Vec<Mat4>) -> Self {
        assert_eq!(
            joints.len(),
            inverse_bind_matrices.len(),
            "关节数量与逆绑定矩阵数量必须一致"
        );
        assert!(!joints.is_empty(), "蒙皮至少需要一个关节");
        Self {
            joints,
            inverse_bind_matrices,
        }
    }

    /// 关节这一帧的蒙皮矩阵：`J_i = inv(mesh) × global(joint_i) × ibm_i`。
    ///
    /// 失效关节（已销毁）回退为恒等矩阵——该关节影响的顶点停在
    /// 绑定姿势，不产生未定义行为。
    pub fn joint_matrices(&self, world: &World, mesh_global: Mat4, out: &mut Vec<Mat4>) {
        out.clear();
        let mesh_inv = mesh_global.inverse();
        for (joint, ibm) in self.joints.iter().zip(&self.inverse_bind_matrices) {
            let global = world
                .get::<GlobalTransform>(*joint)
                .map(|g| g.matrix())
                // 关节失效：恒等蒙皮 = 顶点保持绑定姿势
                .map_or(Mat4::IDENTITY, |g| mesh_inv * g * *ibm);
            out.push(global);
        }
    }
}

/// 蒙皮数学参考实现（CPU）：对单个顶点应用 4 关节线性混合。
///
/// 与 WGSL `vs_skinned` 的混合公式逐项对应——任何一端改动都应让本函数
/// 同步，二者的一致性由 GPU 端到端测试守护。
pub fn skin_vertex(
    position: [f32; 3],
    joints: [u32; 4],
    weights: [f32; 4],
    joint_matrices: &[Mat4],
) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for k in 0..4 {
        let w = weights[k];
        if w == 0.0 {
            continue;
        }
        let idx = joints[k] as usize;
        let Some(m) = joint_matrices.get(idx) else {
            continue; // 越界关节：权重作废（防御非法资产）
        };
        let p = m.transform_point3(position.into());
        out[0] += w * p.x;
        out[1] += w * p.y;
        out[2] += w * p.z;
    }
    out
}

/// 把平铺的列主序 4x4 矩阵数据（每矩阵 16 个 f32）组装为 `Mat4` 列表。
/// glTF `inverseBindMatrices` accessor 的解码出口。
pub fn mat4_list_from_flat(flat: &[f32]) -> Vec<Mat4> {
    flat.chunks_exact(16)
        .map(|c| {
            Mat4::from_cols_array_2d(&[
                [c[0], c[1], c[2], c[3]],
                [c[4], c[5], c[6], c[7]],
                [c[8], c[9], c[10], c[11]],
                [c[12], c[13], c[14], c[15]],
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::prelude::Transform;
    use iris_core::world::World;

    /// 关节在 bind 姿势 = 当前姿势时应产生恒等蒙皮（顶点不动）。
    #[test]
    fn bind_pose_yields_identity_skinning() {
        let mut world = World::new();
        let joint = world.spawn((Transform::from_xyz(0.0, 1.0, 0.0),));
        propagate_transforms(&mut world);
        let bind = world.get::<GlobalTransform>(joint).unwrap().matrix();
        let skin = Skin::new(vec![joint], vec![bind.inverse()]);
        let mut mats = Vec::new();
        skin.joint_matrices(&world, Mat4::IDENTITY, &mut mats);
        assert!(
            mats[0].abs_diff_eq(Mat4::IDENTITY, 1e-5),
            "bind 姿势关节矩阵应为恒等，实际 {:#?}",
            mats[0]
        );
        // 顶点在 bind 姿势下不动
        let p = skin_vertex([0.3, 1.7, -0.4], [0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0], &mats);
        assert!(
            p.iter()
                .zip([0.3, 1.7, -0.4])
                .all(|(a, b)| (a - b).abs() < 1e-5)
        );
    }

    /// 单关节旋转 90°：权重 1 的顶点应随关节刚体旋转。
    #[test]
    fn rotated_joint_rigidly_transforms_vertices() {
        let mut world = World::new();
        let mesh = world.spawn((Transform::IDENTITY,));
        // 关节 bind 在原点，当前绕 Y 旋转 90°；ibm = 恒等
        let joint = world.spawn((Transform::from_rotation_y(std::f32::consts::FRAC_PI_2),));
        propagate_transforms(&mut world);
        let skin = Skin::new(vec![joint], vec![Mat4::IDENTITY]);
        let mut mats = Vec::new();
        skin.joint_matrices(
            &world,
            world.get::<GlobalTransform>(mesh).unwrap().matrix(),
            &mut mats,
        );
        // (1, 0, 0) → (0, 0, -1)
        let p = skin_vertex([1.0, 0.0, 0.0], [0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0], &mats);
        assert!(
            p.iter()
                .zip([0.0, 0.0, -1.0])
                .all(|(a, b)| (a - b).abs() < 1e-5),
            "实际 {p:?}"
        );
    }

    /// 两关节各 0.5 权重：结果为两个变换的线性混合（LBS 的定义性特征）。
    #[test]
    fn blended_weights_mix_transforms_linearly() {
        let mut world = World::new();
        let a = world.spawn((Transform::IDENTITY,)); // J_a = I
        let b = world.spawn((Transform::from_xyz(0.0, 2.0, 0.0),)); // J_b = T(0,2,0)
        propagate_transforms(&mut world);
        let skin = Skin::new(vec![a, b], vec![Mat4::IDENTITY, Mat4::IDENTITY]);
        let mut mats = Vec::new();
        skin.joint_matrices(&world, Mat4::IDENTITY, &mut mats);
        let p = skin_vertex([0.0, 0.0, 0.0], [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0], &mats);
        assert!(
            p.iter()
                .zip([0.0, 1.0, 0.0])
                .all(|(a, b)| (a - b).abs() < 1e-5),
            "实际 {p:?}"
        );
    }

    /// 关节层级：父关节旋转必须通过 GlobalTransform 传播带动子关节。
    /// 肘关节绕自身 bind 位置 (0,1,0) 旋转：肘点固定，其上方 1 单位的
    /// 顶点 (0,2,0) 绕肘点扫到 (-1,1,0)。
    #[test]
    fn hierarchy_propagates_into_joint_matrices() {
        let mut world = World::new();
        let hip = world.spawn((Transform::IDENTITY,));
        let elbow = world.spawn((Transform {
            position: [0.0, 1.0, 0.0].into(),
            rotation: glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            ..Transform::IDENTITY
        },));
        world.add_child(hip, elbow);
        propagate_transforms(&mut world);

        // 肘 bind 在 (0,1,0)：ibm = T(0,-1,0)
        let ibm = Mat4::from_translation([0.0, -1.0, 0.0].into());
        let skin = Skin::new(vec![hip, elbow], vec![Mat4::IDENTITY, ibm]);
        let mut mats = Vec::new();
        skin.joint_matrices(&world, Mat4::IDENTITY, &mut mats);

        // 解析解：J_肘 = T(0,1,0)·Rz90°·T(0,-1,0) —— 绕肘点的旋转，
        // 旋转部分为 Rz90°，平移部分 = (0,1,0) − Rz90°·(0,1,0) = (1,1,0)
        let j = mats[1];
        let elbow_point = j.transform_point3([0.0, 1.0, 0.0].into());
        assert!(
            elbow_point.abs_diff_eq([0.0, 1.0, 0.0].into(), 1e-5),
            "肘点应固定不动，实际 {elbow_point:?}"
        );
        let swept = j.transform_point3([0.0, 2.0, 0.0].into());
        assert!(
            swept.abs_diff_eq([-1.0, 1.0, 0.0].into(), 1e-5),
            "肘上方顶点应绕肘点扫到 (-1,1,0)，实际 {swept:?}"
        );
        // 髋关节仍为恒等
        let hip_point = mats[0].transform_point3([0.3, 1.4, 0.5].into());
        assert!(
            hip_point.abs_diff_eq([0.3, 1.4, 0.5].into(), 1e-5),
            "髋关节矩阵应为恒等，实际 {hip_point:?}"
        );
    }

    /// 失效关节不 panic：退化为 bind 姿势。
    #[test]
    fn dangling_joint_degrades_to_bind_pose() {
        let mut world = World::new();
        let joint = world.spawn((Transform::from_xyz(5.0, 0.0, 0.0),));
        propagate_transforms(&mut world);
        let bind = world.get::<GlobalTransform>(joint).unwrap().matrix();
        world.despawn(joint);
        let skin = Skin::new(vec![joint], vec![bind.inverse()]);
        let mut mats = Vec::new();
        skin.joint_matrices(&world, Mat4::IDENTITY, &mut mats);
        assert!(
            mats[0].abs_diff_eq(Mat4::IDENTITY, 1e-5),
            "失效关节应退化为 bind 姿势"
        );
    }

    /// 平铺矩阵组装：列主序 16 floats ↔ Mat4。
    #[test]
    fn flat_matrices_roundtrip() {
        let m = Mat4::from_translation([1.0, 2.0, 3.0].into());
        let flat: Vec<f32> = m.to_cols_array_2d().iter().flat_map(|c| *c).collect();
        let back = mat4_list_from_flat(&flat);
        assert_eq!(back.len(), 1);
        assert!(back[0].abs_diff_eq(m, 1e-6));
        // 平移在第 4 列（列主序 floats 12..16）
        assert_eq!(&flat[12..15], &[1.0, 2.0, 3.0]);
    }
}
