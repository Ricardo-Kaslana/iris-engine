//! 示例：资产打包验收（R45）——把仓库资产（场景 JSON + GLB + OGG 各一）
//! 打包为 PACK1 单文件，演示条目表、磁盘占用与随机读延迟对比。
//!
//! ```sh
//! cargo run -p iris-engine --release --example pack_demo
//! ```

use iris_engine::pack::{PackReader, PackWriter};
use iris_engine::prelude::*;
use std::time::Instant;

/// 待打包资产（场景 JSON / GLB / OGG 各一）。
const ASSETS: [&str; 3] = [
    "assets/arena.scene.json",
    "assets/enemy_worm.glb",
    // OGG 不在 assets/（引擎音频测试资产），路径相对 workspace 根
    "crates/iris-audio/tests/fixtures/tone_2s.ogg",
];

fn main() {
    env_logger::init();

    // ---- 打包 ----
    let mut writer = PackWriter::new();
    let mut raw_total = 0u64;
    for (index, path) in ASSETS.iter().enumerate() {
        // mount 内路径：去掉目录前缀、保内容可读（pack 路径即约定键名）
        let pack_path = format!("{index:02}_{}", file_name(path));
        let bytes = std::fs::read(asset_path(path)).expect("读源资产");
        println!("打包 {path} → \"{pack_path}\"（{} 字节）", bytes.len());
        raw_total += bytes.len() as u64;
        writer.add(pack_path, bytes).expect("条目不重复");
    }
    let pack_path = std::path::Path::new("target").join("pack_demo.ipak");
    let packed = writer.write_file(&pack_path).expect("写 pack");

    println!("\n== 磁盘占用 ==");
    println!("原始文件合计：{raw_total} 字节");
    println!(
        "pack 单文件 ：{packed} 字节（{:.1}%；PACK1 不压缩、条目间无填充）",
        packed as f64 / raw_total as f64 * 100.0
    );
    println!(
        "文件数 3 → 1（含头 + 条目表开销 {} 字节）",
        packed - raw_total
    );

    // ---- 打开：只读头 + 条目表，数据区按需 seek ----
    let pack = PackReader::open(&pack_path).expect("打开 pack");
    println!("\n== 条目表（字典序，CRC-32 校验和）==");
    println!(
        "{:<24} {:>10} {:>10}  {:>10}",
        "路径", "偏移", "长度", "CRC32"
    );
    for path in pack.iter() {
        let bytes = pack.read(path).expect("读条目");
        let checksum = iris_engine::pack::crc32(&bytes) as u32;
        let (offset, _) = locate(&pack_path, path);
        println!(
            "{path:<24} {offset:>10} {:>10}  {checksum:>10}",
            bytes.len()
        );
    }

    // ---- 随机读延迟：pack seek 读 vs 直读文件（各 100 次，确定性序列）----
    const RUNS: u32 = 100;
    let payload: Vec<(String, std::path::PathBuf)> = ASSETS
        .iter()
        .enumerate()
        .map(|(index, path)| (format!("{index:02}_{}", file_name(path)), asset_path(path)))
        .collect();

    let start = Instant::now();
    for run in 0..RUNS {
        let (pack_entry, _) = &payload[(run as usize) % payload.len()];
        let bytes = pack.read(pack_entry).expect("pack 读");
        assert!(!bytes.is_empty());
    }
    let pack_elapsed = start.elapsed();

    let start = Instant::now();
    for run in 0..RUNS {
        let (_, direct_path) = &payload[(run as usize) % payload.len()];
        let bytes = std::fs::read(direct_path).expect("直读");
        assert!(!bytes.is_empty());
    }
    let direct_elapsed = start.elapsed();

    println!("\n== 随机读延迟（{RUNS} 次，混合三资产，Windows 文件缓存预热后）==");
    println!(
        "PackReader（seek + read_exact + CRC）：{:>10.2?} / 次",
        pack_elapsed.as_secs_f64() * 1e6 / RUNS as f64
    );
    println!(
        "std::fs::read 直读                   ：{:>10.2?} / 次",
        direct_elapsed.as_secs_f64() * 1e6 / RUNS as f64
    );
    println!("输出目录：{}", pack_path.display());
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 条目表回读偏移（demo 展示用；正式消费走 PackReader::read）。
fn locate(pack_path: &std::path::Path, entry: &str) -> (u64, u64) {
    let bytes = std::fs::read(pack_path).expect("读 pack 文件");
    let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut cursor = 12usize;
    for _ in 0..count {
        let path_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        let path = std::str::from_utf8(&bytes[cursor + 4..cursor + 4 + path_len]).unwrap();
        let rest = &bytes[cursor + 4 + path_len..cursor + 4 + path_len + 24];
        let offset = u64::from_le_bytes(rest[0..8].try_into().unwrap());
        let length = u64::from_le_bytes(rest[8..16].try_into().unwrap());
        if path == entry {
            return (offset, length);
        }
        cursor += 4 + path_len + 24;
    }
    unreachable!("条目必在表中：{entry}")
}
