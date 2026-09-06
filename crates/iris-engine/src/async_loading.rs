//! 异步资产加载骨架：专用加载线程（命令通道 + 结果信箱），主线程
//! 零阻塞轮询。
//!
//! # 契约
//!
//! - [`AssetLoader::request`] **只入队**：native 上把请求丢给后台线程
//!   （线程名 `iris-asset-loader`），立刻返回；wasm 上无线程，[`AssetLoader`]
//!   退化为 inline（见下）；
//! - [`AssetLoader::poll`] **绝不阻塞**（`try_recv`）：空轮询即时返回
//!   `None`——主循环每帧调一次、拿到结果再入库，是既定用法；
//! - 结果与请求**不保证同序**（并行加载天然乱序）；`poll` 每次取回
//!   一条已完成结果；
//! - [`AssetLoader`] 可克隆计数？**不可以**——单生产者单消费者语义，
//!   clone 被刻意省略；多处入队请共享一个引用。
//!
//! # wasm 姿态（选型写明）
//!
//! wasm32 无线程：[`AssetLoader::inline`] 提供同 API 同语义的同步回退
//! ——`request` 在调用线程**立即执行**读取（耗时计入该次调用，这是
//! 与 native 的唯一契约差异），结果入队、`poll` 按请求序取回。native
//! 上也可显式用 `inline`（调试对齐两路径行为用）。[`AssetLoader::spawn`]
//! 在 wasm 上自动退化为 inline——调用方代码无需 cfg 分叉。
//!
//! # v1 范围（诚实清单）
//!
//! - [`AssetKind`] 只有两类：**Scene**（读取 + 后台线程上做 JSON 结构
//!   校验——坏文件在加载线程报错而非主线程解析时）与 **RawBytes**
//!   （原样字节透传）；渲染纹理 / 网格的 GPU 上传异步化超出本轮
//!   （GPU 侧提交与生命周期的异步化是独立工程，路线图另列）；
//! - 结果是字节：拿到后走内存解析入口（场景 [`crate::scene::load_scene_from`]
//!   等）；
//! - 无进度回调 / 无优先级 / 无取消（v1：队列中的请求不可撤回）；
//! - 错误经结果信箱回传（与 iris-audio 流式解码「后台错误不可回传」
//!   的教训相反——加载没有实时回调约束，错误必须保真回传）。

use std::collections::VecDeque;
use std::sync::mpsc;

use crate::vfs::{AssetError, AssetMount};

// ---------------------------------------------------------------------------
// 请求与结果
// ---------------------------------------------------------------------------

/// 资产类别：决定加载线程在字节之上追加的工作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    /// 场景 JSON：读取后在加载线程做 JSON 结构校验（`serde_json::Value`
    /// 试解析——只验结构不建实体），坏文件在后台报错，主线程拿到的
    /// 字节保证可解析。
    Scene,
    /// 原始字节透传（OGG / GLB / 自定义格式等，解析职责在消费方）。
    RawBytes,
}

/// 一次加载请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadRequest {
    /// 资产路径（mount 内的路径约定，如 `"scenes/arena.scene.json"`）。
    pub path: String,
    /// 资产类别。
    pub kind: AssetKind,
}

/// 加载错误（经结果信箱回传，保真——见模块文档）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetLoadError {
    /// 全部挂载来源未命中。
    NotFound,
    /// 读取或校验失败（IO / pack CRC 不符 / Scene 的 JSON 结构校验未过）。
    Failed(String),
}

impl From<AssetError> for AssetLoadError {
    fn from(e: AssetError) -> Self {
        match e {
            AssetError::NotFound => Self::NotFound,
            AssetError::Failed(what) => Self::Failed(what),
        }
    }
}

impl std::fmt::Display for AssetLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "资产在全部已挂载来源中未命中"),
            Self::Failed(what) => write!(f, "资产加载失败：{what}"),
        }
    }
}

impl std::error::Error for AssetLoadError {}

/// 一条完成的作业：路径 + 结果。类型别名收拢（线程与 inline 两个后端
/// 共用同一结果形态）。
type JobOutcome = (String, Result<Vec<u8>, AssetLoadError>);

/// 在挂载上执行一次加载（两个后端共用的工作函数）。
fn execute(mount: &AssetMount, request: LoadRequest) -> JobOutcome {
    let LoadRequest { path, kind } = request;
    let outcome = match mount.read(&path) {
        Err(e) => Err(e.into()),
        Ok(bytes) => match kind {
            AssetKind::RawBytes => Ok(bytes),
            AssetKind::Scene => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(_) => Ok(bytes),
                Err(e) => Err(AssetLoadError::Failed(format!(
                    "「{path}」不是合法 JSON：{e}"
                ))),
            },
        },
    };
    (path, outcome)
}

// ---------------------------------------------------------------------------
// AssetLoader
// ---------------------------------------------------------------------------

/// 异步资产加载器：native = 后台线程，wasm / 显式 inline = 同步回退。
///
/// 丢弃本句柄不等待在途请求完成（后台线程把队列清空后自然退出；
/// 丢弃前想拿回全部结果，请持续 `poll` 到 `None` 且不再有新请求——
/// v1 无 join 语义，见模块文档诚实清单）。
pub struct AssetLoader {
    backend: Backend,
}

enum Backend {
    /// 专用线程：命令通道入队、结果信箱出队。
    /// wasm 上 `spawn` 恒走 inline，此变体不构造——保留枚举形状跨平台
    /// 对称（针对性别名，非全局放宽）。
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Thread {
        tx: mpsc::Sender<LoadRequest>,
        rx: mpsc::Receiver<JobOutcome>,
    },
    /// 同步回退：request 即时执行，结果排队等 poll。
    Inline {
        /// Arc 共享：线程创建失败回退 inline 时源不丢失（见 spawn）。
        mount: std::sync::Arc<AssetMount>,
        results: std::sync::Mutex<VecDeque<JobOutcome>>,
    },
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Thread { .. } => f.write_str("Thread(iris-asset-loader)"),
            Self::Inline { .. } => f.write_str("Inline(同步回退)"),
        }
    }
}

impl std::fmt::Debug for AssetLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AssetLoader({:?})", self.backend)
    }
}

impl AssetLoader {
    /// 启动专用加载线程挂载 `mount`（线程内独占，主线程经通道交互）。
    ///
    /// wasm32 上自动退化为 [`AssetLoader::inline`]（无线程平台，选型见
    /// 模块文档）。
    pub fn spawn(mount: AssetMount) -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            Self::inline(mount)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Arc 共享挂载：线程创建失败时源还在手里，同步回退不丢源
            let mount = std::sync::Arc::new(mount);
            let (cmd_tx, cmd_rx) = mpsc::channel::<LoadRequest>();
            let (result_tx, result_rx) = mpsc::channel::<JobOutcome>();
            let thread_mount = std::sync::Arc::clone(&mount);
            let spawn = std::thread::Builder::new()
                .name("iris-asset-loader".into())
                .spawn(move || {
                    // 命令通道关闭（全部 AssetLoader 丢弃）→ 收尾退出；
                    // 结果发送失败（消费端已丢弃）→ 同样退出（结果无
                    // 消费方，继续解码纯属浪费）
                    while let Ok(request) = cmd_rx.recv() {
                        let outcome = execute(&thread_mount, request);
                        if result_tx.send(outcome).is_err() {
                            break;
                        }
                    }
                });
            match spawn {
                Ok(_join) => Self {
                    backend: Backend::Thread {
                        tx: cmd_tx,
                        rx: result_rx,
                    },
                },
                // 线程创建失败（罕见：native 线程耗尽）→ 同步回退兜底，
                // 语义与 inline 一致，宁可慢不可坏
                Err(_) => Self::from_arc(mount),
            }
        }
    }

    /// 同步回退：无后台线程，`request` 在调用线程即时执行（wasm 姿态
    /// 与两路径行为对齐用，见模块文档）。
    pub fn inline(mount: AssetMount) -> Self {
        Self::from_arc(std::sync::Arc::new(mount))
    }

    fn from_arc(mount: std::sync::Arc<AssetMount>) -> Self {
        Self {
            backend: Backend::Inline {
                mount,
                results: std::sync::Mutex::new(VecDeque::new()),
            },
        }
    }

    /// 提交一次加载请求。native 只入队（零阻塞）；inline 即时执行
    /// （耗时计入本次调用——无线程平台的契约差异）。
    pub fn request(&self, path: impl Into<String>, kind: AssetKind) {
        let request = LoadRequest {
            path: path.into(),
            kind,
        };
        match &self.backend {
            Backend::Thread { tx, .. } => {
                // 发送失败 = 加载线程已退出（panic 后果）——不可恢复，
                // panic 让问题在开发期显形（与仓库「冲突即 panic」一致）
                tx.send(request)
                    .expect("加载线程不应提前退出（panic 即引擎 bug）");
            }
            Backend::Inline { mount, results } => {
                let outcome = execute(mount, request);
                results
                    .lock()
                    .expect("inline 结果队列锁不应中毒")
                    .push_back(outcome);
            }
        }
    }

    /// 非阻塞取回一条已完成结果：`None` = 暂无完成项（主循环照常跑，
    /// 下帧再来）。native 上结果乱序到达；inline 按请求序。
    pub fn poll(&self) -> Option<(String, Result<Vec<u8>, AssetLoadError>)> {
        match &self.backend {
            Backend::Thread { rx, .. } => rx.try_recv().ok(),
            Backend::Inline { results, .. } => results
                .lock()
                .expect("inline 结果队列锁不应中毒")
                .pop_front(),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::PackWriter;
    use crate::vfs::AssetSource;
    use std::time::{Duration, Instant};

    /// Embedded 源 fixture：一个合法 JSON 场景 + 一个字节资产 + 一个坏
    /// JSON（错误回传用）。
    static SCENE_OK: &str = r#"{ "entities": [{}] }"#;
    static SCENE_BAD: &str = r#"{ "entities": [ "#;
    static EMBEDDED: &[(&str, &str)] = &[
        ("scenes/ok.scene.json", SCENE_OK),
        ("scenes/bad.scene.json", SCENE_BAD),
        ("raw/blob.bin", "raw-bytes-content"),
    ];

    fn mount() -> AssetMount {
        let mut mount = AssetMount::new();
        mount.push(AssetSource::Embedded(EMBEDDED));
        mount
    }

    /// 等待收集 n 条结果（限时兜底——后台线程调度慢不至测试假死；
    /// 每条之间不 busy-spin，1 ms 睡眠让出）。
    fn collect(loader: &AssetLoader, n: usize, timeout: Duration) -> Vec<JobOutcome> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        while out.len() < n {
            if let Some(outcome) = loader.poll() {
                out.push(outcome);
                continue;
            }
            assert!(Instant::now() < deadline, "限时内未收齐 {n} 条结果");
            std::thread::sleep(Duration::from_millis(1));
        }
        out
    }

    /// 线程后端：request → poll 往返，成功 / 未命中 / 坏 JSON 三态；
    /// 结果字节与嵌入源逐位一致。
    #[test]
    fn thread_backend_roundtrip_and_errors() {
        let loader = AssetLoader::spawn(mount());
        loader.request("scenes/ok.scene.json", AssetKind::Scene);
        loader.request("missing.file", AssetKind::RawBytes);
        loader.request("scenes/bad.scene.json", AssetKind::Scene);
        loader.request("raw/blob.bin", AssetKind::RawBytes);

        let results = collect(&loader, 4, Duration::from_secs(10));
        let lookup = |path: &str| {
            results
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| panic!("缺结果 {path}"))
        };

        // 合法 JSON：字节原样返回（校验通过不改写）
        assert_eq!(lookup("scenes/ok.scene.json").unwrap(), SCENE_OK.as_bytes());
        // 未命中
        assert_eq!(lookup("missing.file"), Err(AssetLoadError::NotFound));
        // 坏 JSON：在加载线程报错（不是把坏字节交主线程）
        assert!(matches!(
            lookup("scenes/bad.scene.json"),
            Err(AssetLoadError::Failed(msg)) if msg.contains("JSON")
        ));
        // 原始字节透传
        assert_eq!(
            lookup("raw/blob.bin").unwrap(),
            b"raw-bytes-content".to_vec()
        );
    }

    /// 空轮询即时返回（非阻塞契约）：无请求时 poll 立刻 None，
    /// 耗时远小于帧预算（宽松上限 50 ms 只兜调度抖动，不是精测）。
    #[test]
    fn poll_is_nonblocking() {
        let loader = AssetLoader::spawn(mount());
        let start = Instant::now();
        assert!(loader.poll().is_none(), "空加载器应无结果");
        assert!(loader.poll().is_none());
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "空轮询耗尽 {elapsed:?}，违反非阻塞契约"
        );
    }

    /// inline 回退与线程后端同语义：同请求同结果（成功 + 错误两态），
    /// 结果按请求序 FIFO。
    #[test]
    fn inline_matches_thread_semantics() {
        let inline = AssetLoader::inline(mount());
        inline.request("scenes/ok.scene.json", AssetKind::Scene);
        inline.request("missing.file", AssetKind::RawBytes);
        inline.request("raw/blob.bin", AssetKind::RawBytes);

        let (path_a, result_a) = inline.poll().expect("inline 应立即有结果");
        let (path_b, result_b) = inline.poll().expect("第二条");
        let (path_c, result_c) = inline.poll().expect("第三条");
        assert!(inline.poll().is_none(), "取完应即时 None");

        // FIFO：请求序即结果序
        assert_eq!(path_a, "scenes/ok.scene.json");
        assert_eq!(path_b, "missing.file");
        assert_eq!(path_c, "raw/blob.bin");
        assert_eq!(result_a.unwrap(), SCENE_OK.as_bytes());
        assert_eq!(result_b, Err(AssetLoadError::NotFound));
        assert_eq!(result_c.unwrap(), b"raw-bytes-content".to_vec());

        // 与线程后端逐项对账（同请求同结果）
        let loader = AssetLoader::spawn(mount());
        loader.request("scenes/ok.scene.json", AssetKind::Scene);
        loader.request("missing.file", AssetKind::RawBytes);
        loader.request("raw/blob.bin", AssetKind::RawBytes);
        let threaded = collect(&loader, 3, Duration::from_secs(10));
        assert_eq!(threaded[0].0, "scenes/ok.scene.json");
        assert_eq!(threaded[0].1.as_deref().unwrap(), SCENE_OK.as_bytes());
        assert!(matches!(threaded[1].1, Err(AssetLoadError::NotFound)));
        assert_eq!(
            threaded[2].1.as_deref().unwrap(),
            b"raw-bytes-content".to_vec()
        );
    }

    /// pack 挂载经加载线程可读（mount 异构源 + 异步骨架的接缝）；
    /// CRC 损坏在加载线程报 Failed（错误保真回传）。
    #[test]
    fn thread_backend_reads_pack_and_reports_corruption() {
        use crate::pack::PackReader;

        let mut writer = PackWriter::new();
        writer
            .add("packed/scene.json", br#"{ "entities": [] }"#.to_vec())
            .unwrap();
        writer.add("packed/data.bin", vec![9; 64]).unwrap();
        let mut pack_bytes = Vec::new();
        writer.write_to(&mut pack_bytes).unwrap();

        // 挂载的 Pack 源是磁盘形态；翻坏 data.bin 的数据首字节（数据区
        // 起点 = 12 + 场景条目 45 + 数据条目 43，落在 data.bin 数据区内；
        // scene.json 数据区完好——同一挂载内「好的条目照常读、坏的报错」）
        let dir = std::env::temp_dir().join(format!("iris-async-pack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let pack_path = dir.join("half_broken.ipak");
        pack_bytes[12 + 45 + 43] ^= 0xFF;
        std::fs::write(&pack_path, &pack_bytes).expect("写 pack");

        let mut mount = AssetMount::new();
        mount.push(AssetSource::Pack(PackReader::open(&pack_path).unwrap()));
        let loader = AssetLoader::spawn(mount);
        loader.request("packed/scene.json", AssetKind::Scene);
        loader.request("packed/data.bin", AssetKind::RawBytes);

        let results = collect(&loader, 2, Duration::from_secs(10));
        let scene = results
            .iter()
            .find(|(p, _)| p == "packed/scene.json")
            .unwrap();
        assert_eq!(scene.1.as_deref().unwrap(), b"{ \"entities\": [] }");
        let data = results
            .iter()
            .find(|(p, _)| p == "packed/data.bin")
            .unwrap();
        assert!(
            matches!(data.1, Err(AssetLoadError::Failed(ref msg)) if msg.contains("CRC")),
            "CRC 损坏应保真回传：{data:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
