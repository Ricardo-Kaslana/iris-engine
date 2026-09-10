//! 异步资产加载骨架：专用加载线程（命令通道 + 结果信箱），主线程
//! 零阻塞轮询；按（优先级，请求序）确定性出队（R59）。
//!
//! # 契约
//!
//! - [`AssetLoader::request`] / [`AssetLoader::request_with_priority`]
//!   **只入队**：native 上把请求丢给后台线程（线程名
//!   `iris-asset-loader`），立刻返回；wasm 上无线程，[`AssetLoader`]
//!   退化为 inline（见下）；
//! - [`AssetLoader::poll`] **绝不阻塞**（`try_recv` / 无作业即时
//!   `None`）：主循环每帧调一次、拿到结果再入库，是既定用法；
//! - **出队序 =（优先级，请求序）升序**（R59）：优先级整数**越小越
//!   先加载**（`i32`，缺省 0 = 既有行为——同优先级时退化为请求 FIFO，
//!   与 R45 行为逐项一致）；请求序 = 入队调用序（`u64` 单调计数，
//!   **在调用线程分配**，与到达加载线程的先后无关）。确定性边界：
//!   已在队列中的请求按此序出队；**在途作业不抢占**（串行加载线程，
//!   后到的高优先级请求等当前作业完成）——与「结果与请求不保证同序」
//!   的旧契约相比，完成顺序在请求集固定时是确定的；
//! - [`AssetLoader`] 不可克隆——单生产者单消费者语义，clone 被刻意
//!   省略；多处入队请共享一个引用。
//!
//! # wasm 姿态（选型写明）
//!
//! wasm32 无线程：[`AssetLoader::inline`] 提供同 API 同语义的同步回退
//! ——`request` 入队、`poll` 出队并**在调用线程执行**读取。R59 起
//! inline 与线程后端共用同一优先级队列与出队序（「同 API 同语义」），
//! 契约差异从「耗时计入 request」变为「耗时计入 poll」（读取发生在
//! 出队时）；同优先级下结果仍按请求序 FIFO 取回，既有调用方无感。
//! [`AssetLoader::spawn`] 在 wasm 上自动退化为 inline——调用方代码
//! 无需 cfg 分叉。
//!
//! # 中断与续载语义（R59）
//!
//! [`AssetLoader`] **无记忆**：无缓存、无去重、无完成登记。丢弃
//! loader（native：命令通道关闭，加载线程排空队列后退出；inline：
//! 未出队的请求随结构体丢弃）即在途请求作废——v1 无 join 语义，
//! 与 R45 一致。**续载 = 消费方持已得结果、对未完成路径重新
//! request**：loader 不感知历史，同路径重请求即幂等重读；已取回的
//! 结果归消费方所有，重建 loader 不会重发、也不会重复投递。该语义
//! 有两后端测试钉住（含「部分完成后中断 → 重建 → 补齐」全流程）。
//!
//! # v1 范围（诚实清单）
//!
//! - [`AssetKind`] 只有两类：**Scene**（读取 + 后台线程上做 JSON 结构
//!   校验——坏文件在加载线程报错而非主线程解析时）与 **RawBytes**
//!   （原样字节透传）；渲染纹理 / 网格的 GPU 上传异步化超出本轮
//!   （GPU 侧提交与生命周期的异步化是独立工程，路线图另列）；
//! - 结果是字节：拿到后走内存解析入口（场景 [`crate::scene::load_scene_from`]
//!   等）；
//! - 无进度回调 / 无取消（队列中的请求不可撤回；不请求即取消——
//!   丢弃 loader 语义见上节）；
//! - 错误经结果信箱回传（与 iris-audio 流式解码「后台错误不可回传」
//!   的教训相反——加载没有实时回调约束，错误必须保真回传）。

use std::collections::BTreeMap;
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

/// 一次加载请求。`priority` / `seq` 由加载器在入队时补齐（调用方经
/// [`AssetLoader::request`] / [`AssetLoader::request_with_priority`]
/// 间接构造）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadRequest {
    /// 资产路径（mount 内的路径约定，如 `"scenes/arena.scene.json"`）。
    pub path: String,
    /// 资产类别。
    pub kind: AssetKind,
    /// 优先级（R59）：越小越先加载；缺省 0 = 既有行为。
    pub priority: i32,
    /// 请求序（R59）：入队调用序的单调计数，出队排序的第二键。
    pub seq: u64,
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

/// 待处理请求的优先级队列（R59）：出队序 =（优先级，请求序）升序。
///
/// `BTreeMap` 选型与 broadphase 同规约——需要迭代序的确定性数据结构
/// 不用哈希容器：键即出队序本身（元组全序），`pop_first` 恒取最小者，
/// 同输入同入队序的出队序列逐项一致（单测守护）。
#[derive(Debug, Default)]
struct PendingQueue(BTreeMap<(i32, u64), LoadRequest>);

impl PendingQueue {
    fn push(&mut self, request: LoadRequest) {
        self.0.insert((request.priority, request.seq), request);
    }

    fn pop_next(&mut self) -> Option<LoadRequest> {
        self.0.pop_first().map(|(_, request)| request)
    }

    // 仅线程主循环消费（inline 后端走 pop_next 的 Option 语义）——
    // wasm 上线程后端不编译，按变体同款 cfg 收窄
    #[cfg(not(target_arch = "wasm32"))]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// 在挂载上执行一次加载（两个后端共用的工作函数）。
fn execute(mount: &AssetMount, request: &LoadRequest) -> JobOutcome {
    let LoadRequest { path, kind, .. } = request;
    let outcome = match mount.read(path) {
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
    (path.clone(), outcome)
}

/// 加载线程主循环（R59）：**排空命令通道 → 等闸门（若有）→ 弹出
/// 最高优先级作业执行**。排空-再弹出的次序是优先级语义的实现核心：
/// 尽最大可能把「已入队」的请求收进同一批排序；串行执行保证「在途
/// 作业不抢占」。命令通道关闭（全部 loader 丢弃）→ 排空队尾后退出；
/// 结果发送失败（消费端已丢弃）→ 同样退出。
#[cfg(not(target_arch = "wasm32"))]
fn loader_thread_loop(
    mount: &AssetMount,
    cmd_rx: &mpsc::Receiver<LoadRequest>,
    result_tx: &mpsc::Sender<JobOutcome>,
    gate: Option<&CondvarGate>,
) {
    let mut queue = PendingQueue::default();
    loop {
        if queue.is_empty() {
            // 队列空才阻塞等待新请求（通道关闭 → 收尾退出）
            match cmd_rx.recv() {
                Ok(first) => queue.push(first),
                Err(_) => break,
            }
        }
        // 排空命令通道：把已入队的请求一并纳入排序
        while let Ok(more) = cmd_rx.try_recv() {
            queue.push(more);
        }
        if let Some(gate) = gate {
            gate.wait();
        }
        if let Some(request) = queue.pop_next() {
            let outcome = execute(mount, &request);
            if result_tx.send(outcome).is_err() {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AssetLoader
// ---------------------------------------------------------------------------

/// 异步资产加载器：native = 后台线程，wasm / 显式 inline = 同步回退。
///
/// 丢弃本句柄不等待在途请求完成（后台线程把队列清空后自然退出；
/// 丢弃前想拿回全部结果，请持续 `poll` 到 `None` 且不再有新请求——
/// v1 无 join 语义，中断 / 续载语义见模块文档）。
pub struct AssetLoader {
    backend: Backend,
    /// 请求序计数器（调用线程侧单调递增——排序键在入队时确定，与
    /// 到达加载线程的先后无关）。`request` 拿 `&self`（多处入队共享
    /// 引用的既定用法），故用原子计数保证序号唯一且按调用序递增。
    next_seq: std::sync::atomic::AtomicU64,
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
    /// 同步回退：request 入队（优先级排队），poll 出队即时执行。
    Inline {
        /// Arc 共享：线程创建失败回退 inline 时源不丢失（见 spawn）。
        mount: std::sync::Arc<AssetMount>,
        queue: std::sync::Mutex<PendingQueue>,
    },
}

/// 测试用启动闸门（native only）：`Some` 时加载线程在每次出队前等待
/// 闸门开放——优先级出队序的**确定性**测试靠它把「请求已全部入队」
/// 与「线程开始消费」两个事件定序（生产路径恒 `None`，零成本）。
#[cfg(not(target_arch = "wasm32"))]
type Gate = std::sync::Arc<CondvarGate>;
#[cfg(not(target_arch = "wasm32"))]
struct CondvarGate {
    open: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}

// new_closed / open 只有测试消费（生产路径闸门恒 None）——lib 构建
// 下显式豁免 dead_code（针对性别名，非全局放宽）
#[cfg(not(target_arch = "wasm32"))]
#[cfg_attr(not(test), allow(dead_code))]
impl CondvarGate {
    fn new_closed() -> Gate {
        std::sync::Arc::new(Self {
            open: std::sync::Mutex::new(false),
            cv: std::sync::Condvar::new(),
        })
    }

    fn open(&self) {
        let mut open = self.open.lock().expect("闸门锁不应中毒");
        *open = true;
        self.cv.notify_all();
    }

    fn wait(&self) {
        let mut open = self.open.lock().expect("闸门锁不应中毒");
        while !*open {
            open = self.cv.wait(open).expect("闸门锁不应中毒");
        }
    }
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
            Self::spawn_threaded(std::sync::Arc::new(mount), None)
        }
    }

    /// 线程后端的实际构造（R59 拆出：测试注入闸门用，生产传 `None`）。
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_threaded(mount: std::sync::Arc<AssetMount>, gate: Option<Gate>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<LoadRequest>();
        let (result_tx, result_rx) = mpsc::channel::<JobOutcome>();
        let thread_mount = std::sync::Arc::clone(&mount);
        let spawn = std::thread::Builder::new()
            .name("iris-asset-loader".into())
            .spawn(move || {
                loader_thread_loop(&thread_mount, &cmd_rx, &result_tx, gate.as_deref());
            });
        match spawn {
            Ok(_join) => Self {
                backend: Backend::Thread {
                    tx: cmd_tx,
                    rx: result_rx,
                },
                next_seq: std::sync::atomic::AtomicU64::new(0),
            },
            // 线程创建失败（罕见：native 线程耗尽）→ 同步回退兜底，
            // 语义与 inline 一致，宁可慢不可坏
            Err(_) => Self::from_arc(mount),
        }
    }

    /// 同步回退：无后台线程，`request` 入队、`poll` 出队并即时执行
    /// （wasm 姿态与两路径行为对齐，见模块文档——R59 起执行时机在
    /// poll，与线程后端同出队序）。
    pub fn inline(mount: AssetMount) -> Self {
        Self::from_arc(std::sync::Arc::new(mount))
    }

    fn from_arc(mount: std::sync::Arc<AssetMount>) -> Self {
        Self {
            backend: Backend::Inline {
                mount,
                queue: std::sync::Mutex::new(PendingQueue::default()),
            },
            next_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 提交一次加载请求（优先级 0 = 既有行为，R59 兼容口）。
    pub fn request(&self, path: impl Into<String>, kind: AssetKind) {
        self.request_with_priority(path, kind, 0);
    }

    /// 提交一次带优先级的加载请求（R59）：`priority` 越小越先加载；
    /// 同优先级按请求序 FIFO。native 只入队（零阻塞）；inline 入队
    /// （读取发生在出队 poll，见模块文档契约说明）。
    pub fn request_with_priority(&self, path: impl Into<String>, kind: AssetKind, priority: i32) {
        let request = LoadRequest {
            path: path.into(),
            kind,
            priority,
            // seq 在调用线程单调分配：出队键 (priority, seq) 完全由
            // 请求集与入队序决定，与线程调度无关（确定性）
            seq: self
                .next_seq
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        };
        match &self.backend {
            Backend::Thread { tx, .. } => {
                // 发送失败 = 加载线程已退出（panic 后果）——不可恢复，
                // panic 让问题在开发期显形（与仓库「冲突即 panic」一致）
                tx.send(request)
                    .expect("加载线程不应提前退出（panic 即引擎 bug）");
            }
            Backend::Inline { queue, .. } => {
                queue.lock().expect("inline 队列锁不应中毒").push(request);
            }
        }
    }

    /// 非阻塞取回一条已完成结果：`None` = 暂无完成项（主循环照常跑，
    /// 下帧再来）。线程后端：已完成结果乱序到达（并行加载天然乱序）；
    /// inline 后端：每次 poll 出队一条（按（优先级，请求序））并在
    /// 调用线程执行读取，结果即该次执行产物。
    pub fn poll(&self) -> Option<(String, Result<Vec<u8>, AssetLoadError>)> {
        match &self.backend {
            Backend::Thread { rx, .. } => rx.try_recv().ok(),
            Backend::Inline { mount, queue } => {
                let request = queue.lock().expect("inline 队列锁不应中毒").pop_next()?;
                Some(execute(mount, &request))
            }
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
    /// 同优先级下结果按请求序 FIFO。
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

        // FIFO：请求序即结果序（同优先级）
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

    // -----------------------------------------------------------------
    // 优先级出队（R59）
    // -----------------------------------------------------------------

    /// 队列结构：出队序 =（优先级，请求序）升序，同优先级 FIFO——
    /// 纯结构单测（无线程，完全确定）。
    #[test]
    fn pending_queue_pops_by_priority_then_request_order() {
        let mut queue = PendingQueue::default();
        let request = |path: &str, priority: i32, seq: u64| LoadRequest {
            path: path.into(),
            kind: AssetKind::RawBytes,
            priority,
            seq,
        };
        // 乱序入队：低优先级先来、高优先级后来；高优先级并列时按
        // 请求序 FIFO（high-a seq=1 先于 high-b seq=3）
        queue.push(request("low", 5, 0));
        queue.push(request("high-a", 1, 1));
        queue.push(request("mid", 3, 2));
        queue.push(request("high-b", 1, 3));
        queue.push(request("default", 0, 4));

        let order: Vec<String> = std::iter::from_fn(|| queue.pop_next().map(|r| r.path)).collect();
        assert_eq!(
            order,
            vec![
                "default".to_string(),
                "high-a".to_string(),
                "high-b".to_string(),
                "mid".to_string(),
                "low".to_string()
            ],
            "出队序应为（优先级，请求序）升序；同优先级 FIFO（high-a 先于 high-b）"
        );
        assert!(queue.is_empty());
    }

    /// inline 后端：优先级出队与线程后端同语义——后请求的高优先级
    /// 先出队（同步执行，结果序即出队序，完全确定）。
    #[test]
    fn inline_backend_dequeues_by_priority() {
        let loader = AssetLoader::inline(mount());
        loader.request_with_priority("raw/first.bin", AssetKind::RawBytes, 5);
        loader.request_with_priority("raw/blob.bin", AssetKind::RawBytes, 1);
        loader.request_with_priority("scenes/ok.scene.json", AssetKind::Scene, 3);
        loader.request_with_priority("raw/never.bin", AssetKind::RawBytes, 9);

        let mut order = Vec::new();
        while let Some((path, _)) = loader.poll() {
            order.push(path);
        }
        assert_eq!(
            order,
            vec![
                "raw/blob.bin".to_string(),
                "scenes/ok.scene.json".to_string(),
                "raw/first.bin".to_string(),
                "raw/never.bin".to_string(),
            ],
            "inline 出队序应为优先级升序（请求序只作并列裁决）"
        );
    }

    /// 线程后端：优先级出队序确定性。闸门在线程首次出队前关闭——
    /// 三条请求全部入队完成后才放行，出队序列（= 串行完成序）因此
    /// 与调度无关，逐项断言（high 先于 mid 先于 low，尽管 low 最早
    /// 请求）。同优先级并列（a/b）按请求序 FIFO。
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn thread_backend_dequeues_by_priority_deterministically() {
        // 优先级测试专用嵌入表（四条路径全部存在，结果恒成功——失败
        // 形态已由其他测试覆盖，此处只看出队序）
        static PRIO_EMBEDDED: &[(&str, &str)] = &[
            ("scenes/ok.scene.json", SCENE_OK),
            ("raw/blob.bin", "raw-bytes-content"),
            ("raw/first.bin", "first"),
            ("raw/never.bin", "never"),
        ];
        let mut prio_mount = AssetMount::new();
        prio_mount.push(AssetSource::Embedded(PRIO_EMBEDDED));

        let gate = CondvarGate::new_closed();
        let loader =
            AssetLoader::spawn_threaded(std::sync::Arc::new(prio_mount), Some(gate.clone()));
        // 请求序：low → mid → high-b → high-a（并列 FIFO 裁决用）
        loader.request_with_priority("raw/first.bin", AssetKind::RawBytes, 5);
        loader.request_with_priority("raw/never.bin", AssetKind::RawBytes, 9);
        loader.request_with_priority("raw/blob.bin", AssetKind::RawBytes, 1);
        loader.request_with_priority("scenes/ok.scene.json", AssetKind::Scene, 1);
        // 全部请求已在通道里，放行
        gate.open();

        let mut order = Vec::new();
        while order.len() < 4 {
            if let Some((path, result)) = loader.poll() {
                result.expect("全部应加载成功");
                order.push(path);
                continue;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            order,
            vec![
                "raw/blob.bin".to_string(),
                "scenes/ok.scene.json".to_string(),
                "raw/first.bin".to_string(),
                "raw/never.bin".to_string(),
            ],
            "线程后端出队序应为（优先级，请求序）升序：闸门定序后与调度无关"
        );
    }

    // -----------------------------------------------------------------
    // 中断 / 续载（R59）
    // -----------------------------------------------------------------

    /// 续载语义（inline）：部分资产完成后中断（drop loader，未出队
    /// 请求作废），重建 loader 对剩余路径重新 request，补齐全部结果；
    /// 已取回的结果归消费方（不重复投递），loader 无记忆（同路径重
    /// 请求 = 幂等重读，字节逐位一致）。
    #[test]
    fn inline_resume_after_partial_completion() {
        let inline = AssetLoader::inline(mount());
        inline.request("scenes/ok.scene.json", AssetKind::Scene);
        inline.request("raw/blob.bin", AssetKind::RawBytes);
        inline.request("scenes/bad.scene.json", AssetKind::Scene);

        // 收一半（模拟收包中断）：前两条已完成并归消费方
        let mut got = Vec::new();
        got.push(inline.poll().expect("第一条应已完成"));
        got.push(inline.poll().expect("第二条应已完成"));
        drop(inline); // 中断：第三条请求作废

        // 重建 loader 续载：对「未完成」的路径重新请求
        let inline2 = AssetLoader::inline(mount());
        inline2.request("scenes/bad.scene.json", AssetKind::Scene);
        let tail = inline2.poll().expect("续载应取出重发请求的结果");
        got.push(tail);

        // 全集对账：三条路径各得一份，字节正确
        let mut paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "raw/blob.bin",
                "scenes/bad.scene.json",
                "scenes/ok.scene.json"
            ]
        );
        let lookup = |path: &str| {
            got.iter()
                .find(|(p, _)| p == path)
                .map(|(_, r)| r.clone())
                .unwrap()
        };
        assert_eq!(
            lookup("raw/blob.bin").unwrap(),
            b"raw-bytes-content".to_vec()
        );
        assert!(matches!(
            lookup("scenes/bad.scene.json"),
            Err(AssetLoadError::Failed(_))
        ));
        assert_eq!(lookup("scenes/ok.scene.json").unwrap(), SCENE_OK.as_bytes());

        // loader 无记忆：新 loader 重请求已得路径 = 幂等重读（字节一致）
        let inline3 = AssetLoader::inline(mount());
        inline3.request("raw/blob.bin", AssetKind::RawBytes);
        let (path, result) = inline3.poll().unwrap();
        assert_eq!(path, "raw/blob.bin");
        assert_eq!(result.unwrap(), b"raw-bytes-content".to_vec());
    }

    /// 续载语义（线程后端）：部分完成后 drop loader（在途请求随通道
    /// 关闭作废、线程自然退出，不悬挂），重建 loader 补齐剩余路径。
    #[test]
    fn thread_resume_after_partial_completion_and_drop() {
        let loader = AssetLoader::spawn(mount());
        loader.request("scenes/ok.scene.json", AssetKind::Scene);
        loader.request("raw/blob.bin", AssetKind::RawBytes);
        loader.request("scenes/bad.scene.json", AssetKind::Scene);

        let mut got = vec![collect(&loader, 1, Duration::from_secs(10)).remove(0)];
        drop(loader); // 中断：剩余在途请求作废（无 join 语义，线程自然退出）

        // 重建续载
        let loader2 = AssetLoader::spawn(mount());
        loader2.request("raw/blob.bin", AssetKind::RawBytes);
        loader2.request("scenes/bad.scene.json", AssetKind::Scene);
        got.extend(collect(&loader2, 2, Duration::from_secs(10)));

        let mut paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "raw/blob.bin",
                "scenes/bad.scene.json",
                "scenes/ok.scene.json"
            ],
            "跨两次 loader 应补齐全集"
        );
    }
}
