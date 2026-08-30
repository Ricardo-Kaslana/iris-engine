//! 渲染器核心：批处理收集、GPU 资源懒上传、帧编码与提交。
//!
//! 数据流：
//! ```text
//! World 资源（MeshStore / MaterialStore / TextureStore，CPU 目录）
//!     -> Renderer 内 GPU 缓存（HashMap<句柄, GPU 资源>，按需懒上传）
//!     -> 查询 (Transform, MeshHandle, MaterialHandle) 收集实例
//!     -> 按 (网格, 材质) 排序合并为批次 -> 一个批次一个 draw call
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use iris_core::prelude::GlobalTransform;

use crate::camera::Camera;
use crate::context::Gpu;
use crate::frustum::Frustum;
use crate::instance::InstanceRaw;
use crate::light::{DirectionalLight, PointLight, MAX_POINT_LIGHTS};
use crate::material::{MaterialHandle, MaterialStore};
use crate::mesh::{upload_mesh, MeshHandle, MeshStore};
use crate::pipeline::create_pipeline;
use crate::texture::{TextureHandle, TextureStore};
use crate::SAMPLE_COUNT;

/// 与 WGSL `PointLight` 结构对齐（32 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PointLightRaw {
    position: [f32; 4],
    color: [f32; 4], // rgb × intensity, w = range
}

/// 与 WGSL `Globals` 结构对齐（400 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlobalsRaw {
    view_proj: [[f32; 4]; 4],
    eye_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    ambient: [f32; 4],
    point_count: [f32; 4],
    point_lights: [PointLightRaw; MAX_POINT_LIGHTS],
}

const _: () = assert!(std::mem::size_of::<GlobalsRaw>() == 400, "Globals 必须与 WGSL 布局一致");

/// 与 WGSL `MaterialParams` 结构对齐（48 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialRaw {
    base_color: [f32; 4],
    emissive: [f32; 4],
    shininess: f32,
    pad: [f32; 3],
}

struct GpuMesh {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    num_indices: u32,
}

struct GpuTexture {
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

struct GpuMaterial {
    bind_group: wgpu::BindGroup,
}

/// 排序键 + 实例数据一体存储，可直接整体上传 GPU。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawItem {
    key: u64,
    instance: InstanceRaw,
}

/// 渲染器：可同时服务窗口表面与离屏目标。
pub struct Renderer {
    gpu: Arc<Gpu>,
    bundle: crate::pipeline::PipelineBundle,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    instance_buf: Option<wgpu::Buffer>,
    instance_capacity: usize,
    mesh_gpu: HashMap<u32, GpuMesh>,
    texture_gpu: HashMap<u32, GpuTexture>,
    material_gpu: HashMap<u32, GpuMaterial>,
    scratch: Vec<DrawItem>,
    instance_scratch: Vec<InstanceRaw>,
    clear_color: wgpu::Color,
    pub(crate) stats: FrameStats,
}

/// 单帧渲染统计。
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub draw_calls: u32,
    pub instances: u32,
}

impl Renderer {
    pub fn new(gpu: Arc<Gpu>, color_format: wgpu::TextureFormat) -> Self {
        let bundle = create_pipeline(&gpu, color_format, SAMPLE_COUNT);
        let globals_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.globals"),
            size: std::mem::size_of::<GlobalsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.globals.bg"),
            layout: &bundle.globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        // 内置 0 号白色兜底纹理：材质未指定贴图时使用
        let white = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.texture.white"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: None },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let white_gpu = GpuTexture {
            view: white.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler: gpu.device.create_sampler(&wgpu::SamplerDescriptor::default()),
        };
        let mut texture_gpu = HashMap::new();
        texture_gpu.insert(0u32, white_gpu);
        Self {
            gpu,
            bundle,
            globals_buf,
            globals_bg,
            instance_buf: None,
            instance_capacity: 0,
            mesh_gpu: HashMap::new(),
            texture_gpu,
            material_gpu: HashMap::new(),
            scratch: Vec::new(),
            instance_scratch: Vec::new(),
            clear_color: wgpu::Color { r: 0.055, g: 0.07, b: 0.10, a: 1.0 },
            stats: FrameStats::default(),
        }
    }

    pub fn set_clear_color(&mut self, color: wgpu::Color) {
        self.clear_color = color;
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// 渲染一帧。
    ///
    /// `color_view` 为 MSAA 附件；`resolve_target` 为解析目标（无 MSAA 时传 None）；
    /// `depth_view` 为深度附件。
    pub fn draw(
        &mut self,
        world: &iris_core::world::World,
        color_view: &wgpu::TextureView,
        resolve_target: Option<&wgpu::TextureView>,
        depth_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        self.stats = FrameStats::default();
        let Some(camera) = world.try_resource::<Camera>().map(|c| (*c).clone()) else {
            log::debug!("世界中没有 Camera 资源，跳过本帧渲染");
            return;
        };
        let light = world
            .try_resource::<DirectionalLight>()
            .map(|l| (*l).clone())
            .unwrap_or_default();
        // 资源目录按引用借用（不复制数据），生命周期覆盖整个 draw 过程
        let Some(mesh_store) = world.try_resource::<MeshStore>() else {
            return;
        };
        let Some(material_store) = world.try_resource::<MaterialStore>() else {
            return;
        };
        let texture_store = world.try_resource::<TextureStore>();

        // 1. 全局 uniform：相机 + 方向光 + 点光
        let aspect = width as f32 / height.max(1) as f32;
        let vp = camera.view_proj(aspect);
        let light_dir = (-light.direction).normalize_or_zero();
        let mut point_lights = [PointLightRaw {
            position: [0.0; 4],
            color: [0.0; 4],
        }; MAX_POINT_LIGHTS];
        let mut point_count = 0usize;
        world.query::<(&GlobalTransform, &PointLight)>(|(gt, pl)| {
            if point_count >= MAX_POINT_LIGHTS {
                return;
            }
            let position = gt.position();
            point_lights[point_count] = PointLightRaw {
                position: [position.x, position.y, position.z, 0.0],
                color: [
                    pl.color.x * pl.intensity,
                    pl.color.y * pl.intensity,
                    pl.color.z * pl.intensity,
                    pl.range,
                ],
            };
            point_count += 1;
        });
        if point_count == MAX_POINT_LIGHTS {
            // 有可能恰好还有更多点光被截断；再探一次数量仅用于日志
            let mut extra = 0;
            world.query::<&PointLight>(|_| extra += 1);
            if extra > MAX_POINT_LIGHTS {
                log::warn!("点光数量 {extra} 超过上限 {MAX_POINT_LIGHTS}，多余的已被忽略");
            }
        }
        let globals = GlobalsRaw {
            view_proj: vp.to_cols_array_2d(),
            eye_pos: [camera.eye.x, camera.eye.y, camera.eye.z, 1.0],
            light_dir: [light_dir.x, light_dir.y, light_dir.z, 0.0],
            light_color: [
                light.color.x * light.intensity,
                light.color.y * light.intensity,
                light.color.z * light.intensity,
                0.0,
            ],
            ambient: [light.ambient.x, light.ambient.y, light.ambient.z, 0.0],
            point_count: [point_count as f32, 0.0, 0.0, 0.0],
            point_lights,
        };
        self.gpu
            .queue
            .write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));

        // 2. 收集实例（ECS 查询 -> 视锥剔除 -> 排序成批）
        let frustum = Frustum::from_view_proj(&vp);
        let mut items = std::mem::take(&mut self.scratch);
        items.clear();
        let mut culled = 0u32;
        world.query::<(&GlobalTransform, &MeshHandle, &MaterialHandle)>(|(gt, m, mat)| {
            let model = gt.matrix();
            // 包围球剔除：局部球心经模型矩阵变换，半径按最大列缩放保守放大
            let culled_now = mesh_store
                .get(*m)
                .and_then(|data| data.bounds())
                .is_some_and(|(min, max)| {
                    let min = Vec3::from(min);
                    let max = Vec3::from(max);
                    let center_local = (min + max) * 0.5;
                    let radius_local = (max - min).length() * 0.5;
                    let center_world = model.transform_point3(center_local);
                    let scale = model
                        .x_axis
                        .truncate()
                        .length()
                        .max(model.y_axis.truncate().length())
                        .max(model.z_axis.truncate().length());
                    !frustum.intersects_sphere(center_world, radius_local * scale)
                });
            if culled_now {
                culled += 1;
                return;
            }
            let key = ((m.0 as u64) << 32) | mat.0 as u64;
            items.push(DrawItem { key, instance: InstanceRaw::from_matrix(model) });
        });
        items.sort_unstable_by_key(|item| item.key);
        let total = items.len();
        if culled > 0 {
            log::trace!("视锥剔除 {culled} / {} 实例", total + culled as usize);
        }

        // 3. 实例缓冲（容量不足时重建）；剔除排序键后按 112B 紧凑布局整体上传
        if total > 0 {
            self.ensure_instance_capacity(total);
            let mut instances = std::mem::take(&mut self.instance_scratch);
            instances.clear();
            instances.extend(items.iter().map(|item| item.instance));
            self.gpu.queue.write_buffer(
                self.instance_buf.as_ref().expect("实例缓冲已就绪"),
                0,
                bytemuck::cast_slice(&instances),
            );
            self.instance_scratch = instances;
        }

        // 4. 预先完成 GPU 资源懒上传（避免与渲染通道编码的借用冲突）
        let mut index = 0;
        while index < total {
            let key = items[index].key;
            let mut count = 1;
            while index + count < total && items[index + count].key == key {
                count += 1;
            }
            let mesh = MeshHandle((key >> 32) as u32);
            let material = MaterialHandle(key as u32);
            self.ensure_mesh(&mesh_store, mesh);
            self.ensure_material(&material_store, texture_store.as_deref(), material);
            index += count;
        }

        // 5. 编码渲染通道
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("iris.frame") });
        let mut draw_calls = 0u32;
        let mut instances = 0u32;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bundle.pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            if total > 0 {
                pass.set_vertex_buffer(
                    1,
                    self.instance_buf
                        .as_ref()
                        .expect("实例缓冲已就绪")
                        .slice(..),
                );
            }
            index = 0;
            while index < total {
                let key = items[index].key;
                let mut count = 1;
                while index + count < total && items[index + count].key == key {
                    count += 1;
                }
                let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                let material = &self.material_gpu[&(key as u32)];
                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                let first = index as u32;
                let last = (index + count) as u32;
                pass.draw_indexed(0..mesh.num_indices, 0, first..last);
                draw_calls += 1;
                instances += count as u32;
                index += count;
            }
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.stats.draw_calls = draw_calls;
        self.stats.instances = instances;
        self.scratch = items;
    }

    fn ensure_instance_capacity(&mut self, needed: usize) {
        if self.instance_capacity >= needed {
            return;
        }
        let capacity = (needed * 2).max(256);
        self.instance_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.instances"),
            size: (capacity * std::mem::size_of::<InstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.instance_capacity = capacity;
        log::debug!("实例缓冲扩容至 {capacity} 实例");
    }

    fn ensure_mesh(&mut self, store: &MeshStore, handle: MeshHandle) {
        if self.mesh_gpu.contains_key(&handle.0) {
            return;
        }
        let Some(data) = store.get(handle) else {
            log::warn!("网格句柄 {handle:?} 不存在");
            return;
        };
        let (vertex_buf, index_buf, num_indices) = upload_mesh(&self.gpu, data);
        self.mesh_gpu.insert(
            handle.0,
            GpuMesh { vertex_buf, index_buf, num_indices },
        );
    }

    fn ensure_texture(&mut self, store: Option<&TextureStore>, handle: TextureHandle) {
        // 0 号纹理是渲染器内置的白色兜底纹理，无需上传
        if handle.0 == 0 || self.texture_gpu.contains_key(&handle.0) {
            return;
        }
        let Some(cpu) = store.and_then(|s| s.get(handle)) else {
            log::warn!("纹理句柄 {handle:?} 不存在，使用白色兜底纹理");
            return;
        };
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.texture"),
            size: wgpu::Extent3d {
                width: cpu.width.max(1),
                height: cpu.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &cpu.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(cpu.width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: cpu.width.max(1),
                height: cpu.height.max(1),
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        self.texture_gpu.insert(handle.0, GpuTexture { view, sampler });
    }

    fn ensure_material(
        &mut self,
        materials: &MaterialStore,
        textures: Option<&TextureStore>,
        handle: MaterialHandle,
    ) {
        if self.material_gpu.contains_key(&handle.0) {
            return;
        }
        let Some(def) = materials.get(handle) else {
            log::warn!("材质句柄 {handle:?} 不存在");
            return;
        };
        self.ensure_texture(textures, def.texture);
        let texture = &self.texture_gpu[&def.texture.0];
        let raw = MaterialRaw {
            base_color: def.base_color,
            emissive: def.emissive,
            shininess: def.shininess,
            pad: [0.0; 3],
        };
        let params_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.material.params"),
            size: std::mem::size_of::<MaterialRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gpu
            .queue
            .write_buffer(&params_buf, 0, bytemuck::bytes_of(&raw));
        let bind_group = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.material.bg"),
            layout: &self.bundle.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                },
            ],
        });
        self.material_gpu.insert(handle.0, GpuMaterial { bind_group });
    }
}
