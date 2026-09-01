//! 材质：PBR 金属度/粗糙度工作流（Cook-Torrance）+ 自发光 + 纹理引用
//! + 透明模式（alpha 混合走独立透明 pass，见 `renderer.rs`）。

use crate::texture::TextureHandle;

/// 材质句柄，指向 `MaterialStore` 中的条目，也作为 ECS 组件挂载到实体上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, iris_core::Component)]
pub struct MaterialHandle(pub u32);

/// 材质透明模式。
///
/// - `Opaque`（默认）：走不透明主 pass——REPLACE 写入 + 深度写入，
///   参与阴影投射；
/// - `Blend`：走透明 pass——标准 alpha 混合（`src α, 1 − src α`）+
///   深度只测不写，按视深降序（远 → 近）绘制，**不投影**。
///
/// 混合系数取 `base_color.a`（与着色器输出 alpha 一致）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Blend,
}

/// 材质定义（CPU 侧）。GPU 侧 uniform 由渲染器按需创建。
#[derive(Clone, Debug)]
pub struct MaterialDef {
    /// 基础色（线性空间，RGB + 透明度）；金属度 1 时作为 F0 反射率。
    pub base_color: [f32; 4],
    /// 自发光（叠加在光照结果上，不受光照与阴影影响）。
    pub emissive: [f32; 4],
    /// 金属度 0（绝缘体）..1（金属），Cook-Torrance 工作流。
    pub metallic: f32,
    /// 粗糙度 0（镜面）..1（完全粗糙）。
    pub roughness: f32,
    /// 基础色贴图（线性采样，值与 base_color 相乘）。`TextureHandle(0)` 为白色兜底。
    pub texture: TextureHandle,
    /// 透明模式：`Opaque` 走不透明主 pass，`Blend` 走透明 pass（见 `AlphaMode`）。
    pub alpha_mode: AlphaMode,
}

impl Default for MaterialDef {
    fn default() -> Self {
        Self {
            base_color: [1.0, 1.0, 1.0, 1.0],
            emissive: [0.0, 0.0, 0.0, 1.0],
            metallic: 0.0,
            roughness: 0.5,
            texture: TextureHandle(0),
            alpha_mode: AlphaMode::Opaque,
        }
    }
}

impl MaterialDef {
    /// 纯色无贴图材质的便捷构造（绝缘体、中等粗糙度、不透明）。
    pub fn solid(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            base_color: [r, g, b, a],
            ..Default::default()
        }
    }

    /// 金属材质的便捷构造（金属度 1，F0 取基础色）。
    pub fn metal(r: f32, g: f32, b: f32, roughness: f32) -> Self {
        Self {
            base_color: [r, g, b, 1.0],
            metallic: 1.0,
            roughness: roughness.clamp(0.0, 1.0),
            ..Default::default()
        }
    }

    /// 玻璃类透明材质的便捷构造（alpha 混合，`a` 为混合系数，
    /// 低粗糙度绝缘体）。走透明 pass：按视深排序、深度只测不写、不投影。
    pub fn glass(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            base_color: [r, g, b, a],
            roughness: 0.08,
            alpha_mode: AlphaMode::Blend,
            ..Default::default()
        }
    }
}

/// CPU 侧材质目录（World 资源）。0 号槽位固定为默认材质。
#[derive(iris_core::Resource)]
pub struct MaterialStore {
    materials: Vec<MaterialDef>,
}

impl Default for MaterialStore {
    fn default() -> Self {
        Self {
            materials: vec![MaterialDef::default()],
        }
    }
}

impl MaterialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册材质；新句柄从 1 开始（0 为默认材质）。
    pub fn add(&mut self, def: MaterialDef) -> MaterialHandle {
        self.materials.push(def);
        MaterialHandle(self.materials.len() as u32 - 1)
    }

    pub fn get(&self, handle: MaterialHandle) -> Option<&MaterialDef> {
        self.materials.get(handle.0 as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_handle_is_default_material() {
        let mut store = MaterialStore::new();
        assert_eq!(
            store.get(MaterialHandle(0)).unwrap().base_color,
            [1.0, 1.0, 1.0, 1.0]
        );
        let handle = store.add(MaterialDef::solid(1.0, 0.0, 0.0, 1.0));
        assert_eq!(handle, MaterialHandle(1));
        assert_eq!(store.get(handle).unwrap().base_color, [1.0, 0.0, 0.0, 1.0]);
        assert!(store.get(MaterialHandle(99)).is_none());
    }

    /// 便捷构造的透明模式：solid / metal / default 恒不透明，
    /// glass 为 Blend 且混合系数取 alpha 分量。
    #[test]
    fn convenience_constructors_set_alpha_mode() {
        assert_eq!(MaterialDef::default().alpha_mode, AlphaMode::Opaque);
        assert_eq!(
            MaterialDef::solid(1.0, 1.0, 1.0, 0.5).alpha_mode,
            AlphaMode::Opaque,
            "solid 即便 alpha < 1 也保持不透明（分区只看 alpha_mode）"
        );
        assert_eq!(
            MaterialDef::metal(1.0, 1.0, 1.0, 0.2).alpha_mode,
            AlphaMode::Opaque
        );
        let glass = MaterialDef::glass(0.6, 0.8, 1.0, 0.35);
        assert_eq!(glass.alpha_mode, AlphaMode::Blend);
        assert_eq!(glass.base_color, [0.6, 0.8, 1.0, 0.35]);
    }
}
