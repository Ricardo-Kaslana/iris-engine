//! 工具：R47 演出时间线视觉验收（离屏渲染，三帧拼图）。
//!
//! 小场景「舞台」：两柱 + 中央门 + 地面。一段 12 秒演出脚本：
//! 相机沿 Catmull-Rom spline 从广角推到门的特写（ease 开），
//! 第 3 秒 `door_open` 事件 → 游戏侧系统消费后开门（`Visibility` 翻转，
//! 事件消费契约示范），第 5–9 秒对话文本（位图字体，ASCII），
//! 全程黑边条（letterbox 0.12）。
//!
//! 采样三个时刻渲染：t=2（门关、无对话、相机仍远）、t=6（门开、
//! 对话显示、相机推进中）、t=10（对话消失、相机贴门）——相机轨迹 /
//! 对话窗口 / 事件开门 / 黑边恒定四件事一图判读。
//!
//! ```sh
//! cargo run --example timeline_snapshot --release
//! ffmpeg -y -i iris_timeline_t2.bmp -i iris_timeline_t6.bmp \
//!     -i iris_timeline_t10.bmp -filter_complex "[0][1][2]hstack=3" \
//!     docs/timeline.png
//! ```

use std::time::Duration;

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

/// 时间线时长（秒）与三个采样时刻。
const DURATION: f32 = 12.0;
const SAMPLES: [(&str, f32); 3] = [
    ("iris_timeline_t2.bmp", 2.0),
    ("iris_timeline_t6.bmp", 6.0),
    ("iris_timeline_t10.bmp", 10.0),
];

fn stage() -> (World, Entity) {
    let mut world = World::new();
    world.insert_resource(Time::default());
    world.register_event::<TimelineEvent>();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(DirectionalLight::default());
    // 黑边 / 对话布局按视口尺寸计算（与离屏渲染目标一致）
    world.insert_resource(Viewport::new(640, 360));

    let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let pillar_material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.62, 0.57, 0.5, 1.0));
    let door_material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.8, 0.32, 0.15, 1.0));
    let ground_material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.33, 0.36, 1.0));
    // 地面（大而扁）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -0.1, 0.0),
            scale: Vec3::new(40.0, 0.2, 40.0),
            ..Default::default()
        },
        mesh,
        ground_material,
    ));
    // 两柱（高柱）
    for x in [-3.5f32, 3.5] {
        world.spawn((
            Transform {
                position: Vec3::new(x, 1.5, 0.0),
                scale: Vec3::new(1.2, 3.0, 1.2),
                ..Default::default()
            },
            mesh,
            pillar_material,
        ));
    }
    // 中央门（初始可见 = 关；`door_open` 事件消费后隐藏 = 开）
    let door = world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.2, 0.0),
            scale: Vec3::new(1.8, 2.4, 0.4),
            ..Default::default()
        },
        mesh,
        door_material,
    ));

    (world, door)
}

/// 12 秒演出脚本：相机广角 → 特写、第 3 秒开门事件、第 5–9 秒对话。
fn script() -> Timeline {
    Timeline {
        duration: DURATION,
        camera: Some(CameraTrack {
            keys: vec![
                CameraKey {
                    time: 0.0,
                    eye: Vec3::new(0.0, 6.0, 18.0),
                    target: Vec3::new(0.0, 1.0, 0.0),
                },
                CameraKey {
                    time: 6.0,
                    eye: Vec3::new(2.5, 2.2, 9.0),
                    target: Vec3::new(0.0, 1.2, 0.0),
                },
                CameraKey {
                    time: 12.0,
                    eye: Vec3::new(0.4, 1.6, 4.5),
                    target: Vec3::new(0.0, 1.2, 0.0),
                },
            ],
            ease: true,
        }),
        events: vec![TimelineEvent {
            time: 3.0,
            name: "door_open".into(),
        }],
        dialogue: vec![DialogueEntry {
            time: 5.0,
            end_time: 9.0,
            text: "The ancient gate awakens...".into(),
        }],
    }
}

/// 渲染采样时刻：从 0 起按 1/30 s 步进推进时间线，每步消费事件
/// （`door_open` → 门隐藏），到目标时刻后传播 + 渲染。
fn render_at(world: &mut World, door: Entity, target_time: f32, out: &str) {
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };
    let dt = 1.0 / 30.0;
    while is_playing(world) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt));
        update_timeline(world);
        // 事件消费（游戏侧每帧恰好一个 drain 方，契约见 iris_core::event）
        let drained: Vec<TimelineEvent> = world.events_mut::<TimelineEvent>().drain().collect();
        for event in drained {
            if event.name == "door_open" {
                world.insert(door, Visibility::HIDDEN);
            }
        }
        if world.resource::<TimelinePlayer>().time >= target_time {
            break;
        }
    }
    propagate_transforms(world);
    renderer.render(world);
    renderer.save_bmp(out).expect("写 BMP");
    println!("已输出 {out}");
}

fn main() {
    env_logger::init();
    for (out, target_time) in SAMPLES {
        // 每个时刻从同一初始状态重放（相机接管 → 演出 → 恢复语义不变）
        let (mut world, door) = stage();
        world.spawn((
            Name("cinema_camera".into()),
            Transform::from_xyz(10.0, 4.0, 20.0), // 演出前的游戏相机位（被接管、播完恢复）
            ActiveCamera::default(),
        ));
        start_timeline(&mut world, script());
        world.resource_mut::<TimelinePlayer>().letterbox = true;
        render_at(&mut world, door, target_time, out);
    }
}
