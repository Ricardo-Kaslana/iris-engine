//! 体素沙盒离屏验收图：森林远景、树冠镂空、洞口坡道与矿脉洞壁。
//!
//! 输出四张 960×540 BMP（已被 `.gitignore` 忽略），用 ffmpeg 拼成 2×2
//! 的入库 `docs/voxel.png`：
//!
//! ```sh
//! cargo run -p iris-voxel --example voxel_snapshot --release
//! ffmpeg -y -i voxel_forest.bmp -i voxel_canopy.bmp \
//!   -i voxel_entrance.bmp -i voxel_cave.bmp \
//!   -filter_complex "[0][1]hstack[t];[2][3]hstack[b];[t][b]vstack" docs/voxel.png
//! ```
//!
//! 洞穴内景的世界是真实生成的：镜头对准一面露出矿脉的洞壁，并在镜头
//! 侧前方的地板补一块荧光石作为拍摄照明。

use glam::{IVec3, Vec3};
use iris_engine::prelude::*;
use iris_voxel::VoxelPlugin;
use iris_voxel::logic::terrain::{column_height, entrance_sites};
use iris_voxel::logic::{Block, Inventory, PlayerState, VoxelWorld};
use iris_voxel::plugin::apply_edit;

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;
const SEED: u64 = 20_261_022;

fn main() {
    let mut app = App::new();
    app.world.insert_resource(Time::default());
    app.world.insert_resource(Input::new());
    app.world.insert_resource(MouseCapture::new());
    app.world.insert_resource(MeshStore::new());
    app.world.insert_resource(MaterialStore::new());
    app.world.insert_resource(TextureStore::new());
    app.world.insert_resource(Camera::default());
    app.world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.45, -1.05, 1.0),
        color: Vec3::new(0.98, 0.96, 0.9),
        intensity: 1.0,
        ambient: Vec3::new(0.16, 0.18, 0.22),
        ..DirectionalLight::default()
    });
    app.world.insert_resource(Viewport::new(WIDTH, HEIGHT));
    // 区块实体挂的是局部 Transform，渲染收集读 GlobalTransform：
    // 与窗口路径一样需要 PostUpdate 的层级传播。
    HierarchyPlugin.build(&mut app);
    VoxelPlugin::new(SEED).build_schedule(&mut app.schedule);
    app.schedule.run(Stage::Startup, &mut app.world);
    app.schedule.run(Stage::PostUpdate, &mut app.world);
    // 验收图里的快捷栏显示一个非零数量，便于看清 HUD 的背包计数。
    app.world.resource_mut::<Inventory>().add(Block::Grass, 12);
    // 抬头看天再跑 Update：目标框只跟随玩家准星，抬头后射线打不到方块，
    // 目标框不会挂在镜头前的墙上。
    app.world.resource_mut::<PlayerState>().pitch = 1.45;
    // 跑一次 Update 让 HUD 完成布局（dt 为 0，玩家不会移动）。
    app.schedule.run(Stage::Update, &mut app.world);

    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };

    // 1. 森林远景：从世界东南角的高空看向出生点，视线略微抬起，
    //    让草场、疏林、天空与方块云同时入画。
    let center = (iris_voxel::logic::WORLD_SIZE / 2) as f32;
    set_camera(
        &mut app.world,
        Vec3::new(center + 32.0, 32.0, center + 54.0),
        Vec3::new(center, 40.0, center - 8.0),
    );
    // 广角把树冠与蓝天收进同一帧。
    app.world.resource_mut::<Camera>().fov_y_radians = 70f32.to_radians();
    shoot(&mut renderer, &app.world, "voxel_forest.bmp");

    // 2. 树冠特写：走到最近的树冠斜下方，让镂空叶隙与层叠的内表面
    //    都进画面——这是裁切树叶的验收点。
    let canopy = {
        let world = app.world.resource::<VoxelWorld>();
        find_canopy_view(&world)
    };
    if let Some((eye, target)) = canopy {
        set_camera(&mut app.world, eye, target);
    } else {
        eprintln!("没有找到合适的树冠机位");
    }
    shoot(&mut renderer, &app.world, "voxel_canopy.bmp");

    // 3. 洞口坡道：站在洞口向坡道下行方向看，露天切口逐步转为有顶
    //    隧道，尽头沉入黑暗。
    let sites = entrance_sites(SEED);
    let site = sites[0];
    let mouth_height = column_height(site.mouth_x, site.mouth_z, SEED) as f32;
    let mouth = Vec3::new(
        site.mouth_x as f32 + 0.5,
        mouth_height + 1.7,
        site.mouth_z as f32 + 0.5,
    );
    set_camera(
        &mut app.world,
        mouth,
        mouth + Vec3::new(site.dir_x as f32 * 18.0, -6.0, site.dir_z as f32 * 18.0),
    );
    shoot(&mut renderer, &app.world, "voxel_entrance.bmp");

    // 4. 矿脉洞壁：找一块暴露在洞穴空气里的矿石，把镜头架在它正前方
    //    的洞穴地板上，再补一块荧光石照亮墙面。
    let shot = {
        let world = app.world.resource::<VoxelWorld>();
        find_ore_wall(&world)
    };
    if let Some((eye, target, ores)) = shot {
        println!("洞穴机位 {eye:?} 对准 {target:?}，墙面矿石 {ores}");
        add_showcase_light(&mut app.world, eye, target);
        set_camera(&mut app.world, eye, target);
    } else {
        eprintln!("没有找到合适的矿脉洞壁");
    }
    shoot(&mut renderer, &app.world, "voxel_cave.bmp");
}

fn set_camera(world: &mut World, eye: Vec3, target: Vec3) {
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = eye;
    camera.target = target;
    camera.up = Vec3::Y;
    camera.fov_y_radians = 55f32.to_radians();
}

fn shoot(renderer: &mut OffscreenRenderer, world: &World, path: &str) {
    renderer.renderer_mut().reset_taa();
    renderer.render(world);
    renderer.save_bmp(path).expect("写 BMP 失败");
    let stats = renderer.renderer().stats();
    assert!(stats.instances > 0, "场景应有可见实例");
    println!(
        "{path}: {} 个实例，{} 个三角形",
        stats.instances, stats.triangles_drawn
    );
}

/// 找一个能看清树冠镂空的机位：在一棵树的树冠周围扫出空气格，取
/// 三到六格外、视线不穿过不透明方块、叶簇最多的那个。
fn find_canopy_view(world: &VoxelWorld) -> Option<(Vec3, Vec3)> {
    let edge = iris_voxel::logic::WORLD_SIZE as i32 - 6;
    let mut bases = 0;
    let mut canopies = 0;
    let mut air_eyes = 0;
    let mut clear_lines = 0;
    let mut best: Option<(i32, Vec3, Vec3)> = None;
    for z in 6..edge {
        for x in 6..edge {
            for y in 8..22 {
                let base = IVec3::new(x, y, z);
                if world.block(base) != Block::Wood || world.block(base - IVec3::Y) == Block::Wood {
                    continue;
                }
                bases += 1;
                let center = base + IVec3::new(0, 4, 0);
                let mut leaves = 0;
                for dy in -2..=3 {
                    for dz in -3..=3 {
                        for dx in -3..=3 {
                            if world.block(center + IVec3::new(dx, dy, dz)) == Block::Leaves {
                                leaves += 1;
                            }
                        }
                    }
                }
                if leaves < 20 {
                    continue;
                }
                canopies += 1;
                let center_f = Vec3::new(
                    center.x as f32 + 0.5,
                    center.y as f32 + 0.3,
                    center.z as f32 + 0.5,
                );
                for dz in -6..=6 {
                    for dx in -6..=6 {
                        let plan = (dx * dx + dz * dz) as f32;
                        if !(64.0..=121.0).contains(&plan) {
                            continue;
                        }
                        // 对准朝向镜头的叶面而不是树心：树心是树干，会把
                        // 连线判成被不透明方块挡住。`inward` 是「从镜头指向
                        // 树心」，要沿它反方向退出到树冠表面。
                        let inward = Vec3::new(-dx as f32, 0.0, -dz as f32).normalize_or_zero();
                        let target = center_f - inward * 2.5;
                        for dy in -1..=2 {
                            let cell = center + IVec3::new(dx, dy, dz);
                            if world.block(cell) != Block::Air
                                || world.block(cell + IVec3::Y) != Block::Air
                            {
                                continue;
                            }
                            air_eyes += 1;
                            let eye = Vec3::new(
                                cell.x as f32 + 0.5,
                                cell.y as f32 + 0.65,
                                cell.z as f32 + 0.5,
                            );
                            // 连线只允许穿过空气与树叶：不透明方块会挡住树冠。
                            let clear = (1..=12).all(|step| {
                                let point = eye.lerp(target, step as f32 / 12.0);
                                !world
                                    .block(IVec3::new(
                                        point.x.floor() as i32,
                                        point.y.floor() as i32,
                                        point.z.floor() as i32,
                                    ))
                                    .is_opaque()
                            });
                            if !clear {
                                continue;
                            }
                            clear_lines += 1;
                            let distance = plan.sqrt() as i32;
                            let score = leaves * 4 - (distance - 9).abs() * 10 - dy.abs() * 6;
                            if best.as_ref().is_none_or(|entry| score > entry.0) {
                                best = Some((score, eye, target));
                            }
                        }
                    }
                }
            }
        }
    }
    println!(
        "树干 {bases}，树冠 {canopies}，机位空气 {air_eyes}，视线通畅 {clear_lines}，命中 {}",
        if best.is_some() { "有" } else { "无" }
    );
    best.map(|(_, eye, target)| (eye, target))
}

/// 找一面露出矿脉的洞壁：矿石至少有一个水平方向的空气邻格，沿该方向
/// 留出六到九格净空作机位，机位脚下有地板、上方有净空、天空光为零。
///
/// 评分优先矿脉成簇的墙面（五格见方、三格高内的矿石数），距离越近
/// 越好。返回机位、矿石中心与墙面矿石数。
fn find_ore_wall(world: &VoxelWorld) -> Option<(Vec3, Vec3, i32)> {
    const DIRECTIONS: [IVec3; 4] = [
        IVec3::new(1, 0, 0),
        IVec3::new(-1, 0, 0),
        IVec3::new(0, 0, 1),
        IVec3::new(0, 0, -1),
    ];
    let edge = iris_voxel::logic::WORLD_SIZE as i32 - 8;
    let mut best: Option<(i32, Vec3, Vec3, i32)> = None;
    for z in 8..edge {
        for x in 8..edge {
            for y in 5..=14 {
                let ore_pos = IVec3::new(x, y, z);
                if !is_ore(world.block(ore_pos)) {
                    continue;
                }
                for direction in DIRECTIONS {
                    let mut distance = 0;
                    for step in 6..=9 {
                        let eye_pos = ore_pos + direction * step;
                        let eye_cell = world.block(eye_pos);
                        let head = world.block(eye_pos + IVec3::Y);
                        let floor = world.block(eye_pos - IVec3::Y);
                        if eye_cell != Block::Air || head != Block::Air || !floor.is_solid() {
                            continue;
                        }
                        let mut clear = true;
                        for between in 1..step {
                            let cell = ore_pos + direction * between;
                            if world.block(cell) != Block::Air
                                || world.block(cell + IVec3::Y) != Block::Air
                            {
                                clear = false;
                                break;
                            }
                        }
                        if clear && world.light().sky(eye_pos) == 0 {
                            distance = step;
                            break;
                        }
                    }
                    if distance == 0 {
                        continue;
                    }
                    // 统计墙面附近的矿脉密度。
                    let mut cluster = 0;
                    for ox in -2..=2 {
                        for oy in -1..=1 {
                            for oz in -2..=2 {
                                if is_ore(world.block(ore_pos + IVec3::new(ox, oy, oz))) {
                                    cluster += 1;
                                }
                            }
                        }
                    }
                    let score = cluster * 40 - distance * 3;
                    if best.as_ref().is_none_or(|entry| score > entry.0) {
                        let eye_pos = ore_pos + direction * distance;
                        let eye = Vec3::new(
                            eye_pos.x as f32 + 0.5,
                            eye_pos.y as f32 + 0.7,
                            eye_pos.z as f32 + 0.5,
                        );
                        let target = Vec3::new(
                            ore_pos.x as f32 + 0.5,
                            ore_pos.y as f32 + 0.4,
                            ore_pos.z as f32 + 0.5,
                        );
                        best = Some((score, eye, target, cluster));
                    }
                }
            }
        }
    }
    best.map(|(_, eye, target, cluster)| (eye, target, cluster))
}

fn is_ore(block: Block) -> bool {
    matches!(block, Block::CoalOre | Block::IronOre | Block::GoldOre)
}

/// 在镜头侧前方的地板嵌一块荧光石，作为洞穴验收图的照明。
///
/// 依次尝试左右两侧与不同距离，找到一处「下方实心、上方两格空气」的
/// 地板；都找不到就放弃，画面只能靠环境底光。
fn add_showcase_light(world: &mut World, eye: Vec3, target: Vec3) {
    let floor = {
        let data = world.resource::<VoxelWorld>();
        find_light_floor(&data, eye, target)
    };
    if let Some(floor) = floor {
        println!("补光荧光石 {floor:?}");
        apply_edit(world, floor, Block::Glowstone);
    }
}

fn find_light_floor(world: &VoxelWorld, eye: Vec3, target: Vec3) -> Option<IVec3> {
    let direction = (target - eye).normalize_or_zero();
    let side = Vec3::new(-direction.z, 0.0, direction.x);
    let base_y = eye.y.floor() as i32;
    for distance in [5.0f32, 4.0, 3.0, 2.0] {
        for lateral in [2.0f32, -2.0, 1.0, -1.0] {
            let point = eye + direction * distance + side * lateral;
            let x = point.x.floor() as i32;
            let z = point.z.floor() as i32;
            for dy in [0i32, -1, 1] {
                let floor_y = base_y + dy - 1;
                let floor = IVec3::new(x, floor_y, z);
                if world.block(floor).is_solid()
                    && world.block(floor + IVec3::Y) == Block::Air
                    && world.block(floor + IVec3::Y * 2) == Block::Air
                {
                    return Some(floor);
                }
            }
        }
    }
    None
}
