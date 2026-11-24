//! 推送管道服务器
//!
//! 与 Go 版本 `wind_input/internal/bridge/server_push.go` 对齐。
//! 服务端主动推送状态更新、配置同步等消息给 TSF DLL。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use tracing::{debug, error};
use tracing::{info, warn};
use wind_ipc::protocol::*;

#[cfg(windows)]
use crate::server::PipeHandle;

/// 推送管道配置
pub struct PushConfig {
    pub suffix: String,
    /// 单次写超时。当前没接上：写线程是无限期阻塞写，卡住的对端靠 [`PUSH_QUEUE_CAP`] 兜住内存。
    pub write_timeout_ms: u64,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }
    }
}

/// 每个客户端发送队列的帧数上限。
///
/// 写线程是阻塞 `WriteFile`：对端挂起但不断开时管道缓冲一满就停住，无界队列会攒下全部广播。
/// 握手时一批配置推送约 14 帧，64 帧足够吸收突发；真满了说明对端不读了，见 [`offer`]。
/// 不取更大：`sync_channel` 按容量预分配槽位，而每个加载了 TSF 的进程都有客户端。
pub(crate) const PUSH_QUEUE_CAP: usize = 64;

pub(crate) fn push_channel() -> (
    std::sync::mpsc::SyncSender<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    std::sync::mpsc::sync_channel(PUSH_QUEUE_CAP)
}

/// 投一帧进客户端队列，返回该客户端是否已卡死（队列满）。
///
/// 卡死的客户端由调用方移出客户端表：发送端析构后写线程终将断开管道，DLL 重连时做全量状态
/// 同步，比丢帧更能保证对端恢复后拿到当前状态。对端已断开不算卡死（写线程自己清理表项）。
fn offer(c: &PushClient, data: Vec<u8>) -> bool {
    matches!(
        c.tx.try_send(data),
        Err(std::sync::mpsc::TrySendError::Full(_))
    )
}

/// 把 [`offer`] 判为卡死的客户端移出表。
fn drop_stalled(clients: &mut Vec<PushClient>, stalled: &[u64]) {
    if stalled.is_empty() {
        return;
    }
    clients.retain(|c| !stalled.contains(&c.token));
    for t in stalled {
        warn!(
            "Push client 0x{:016X} 发送队列已满（{} 帧，对端不读），断开待其重连",
            t, PUSH_QUEUE_CAP
        );
    }
}

/// 客户端连接信息
pub(crate) struct PushClient {
    /// 客户端 token（PID << 32 | instance_counter；unix 端由服务端发号）
    pub(crate) token: u64,
    /// 发送通道（writer 线程独占管道句柄）
    pub(crate) tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// 是否已触发过 connected_hook。
    ///
    /// `start()` 早于 `set_client_connected_hook` 开始 accept，这段窗口内握手的客户端要在 hook
    /// 注册时补跑；对 SearchHost 这类既不发 FOCUS_GAINED 也不重发 IME_ACTIVATED 的宿主，hook 是
    /// 拿到 activation push 的唯一通路。该标志让补跑与连接线程的自发触发互斥去重。
    pub(crate) hooked: bool,
    /// 是否已给它发过 CMD_SERVICE_READY。
    ///
    /// Windows 侧要等服务真正就绪（[`PushServer::mark_ready`]）才发，见那里的说明；
    /// unix 侧连接即发，注册时就是 `true`。
    pub(crate) ready_sent: bool,
}

fn service_ready_frame() -> Vec<u8> {
    IpcHeader::new(CMD_SERVICE_READY, 0).to_bytes().to_vec()
}

/// 把握手完成的客户端登记进表；服务已就绪则顺带投一帧 SERVICE_READY，否则留给
/// [`PushServer::mark_ready`] 补发。
///
/// 「读就绪标志 → 入表」与 `mark_ready` 的「置标志 → 扫表」都在 `clients` 锁内，
/// 二者串行：不会出现「这边读到未就绪、那边扫表时它还没入表」而两头都没发的空档。
/// 同 token 的旧连接一并清掉（DLL 重连）。
#[cfg(any(windows, test))]
fn register_client(
    clients: &Mutex<Vec<PushClient>>,
    ready: &AtomicBool,
    token: u64,
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
) {
    let mut c = clients.lock().unwrap();
    c.retain(|c| c.token != token);
    let ready_now = ready.load(Ordering::Acquire);
    let client = PushClient {
        token,
        tx,
        hooked: false,
        ready_sent: ready_now,
    };
    // 新建的队列是空的，投不满
    if ready_now {
        let _ = offer(&client, service_ready_frame());
    }
    c.push(client);
}

/// 认领指定 token 的 hook 触发权：返回 `true` 表示本次调用赢得认领、应触发回调。
///
/// 连接线程与 `set_client_connected_hook` 的补跑可能同时盯上同一个客户端
/// （客户端已注册进表、但尚未走到自己的 hook 调用点），认领保证恰好触发一次。
/// 客户端若已断开出表则返回 `false`——不给死连接推帧。
///
/// 触发点在各平台的连接线程里（Windows 管道 / unix 的 `push_unix::handle_push_conn`）。
pub(crate) fn claim_connected_hook(clients: &Mutex<Vec<PushClient>>, token: u64) -> bool {
    let mut guard = clients.lock().unwrap();
    match guard.iter_mut().find(|c| c.token == token) {
        Some(c) if !c.hooked => {
            c.hooked = true;
            true
        }
        _ => false,
    }
}

/// 认领全部未触发过 hook 的客户端，返回其 token 列表。
fn claim_all_unhooked(clients: &Mutex<Vec<PushClient>>) -> Vec<u64> {
    let mut guard = clients.lock().unwrap();
    guard
        .iter_mut()
        .filter(|c| !c.hooked)
        .map(|c| {
            c.hooked = true;
            c.token
        })
        .collect()
}

/// push 客户端完成 token 握手注册后的回调（参数 = 客户端 token，高 32 位为 PID）。
/// Windows 用于 host-render 白名单宿主的重连补推握手（见 coordinator）。
pub type ClientConnectedHook = Box<dyn Fn(u64) + Send + Sync>;

/// 推送管道服务器
pub struct PushServer {
    config: PushConfig,
    clients: Arc<Mutex<Vec<PushClient>>>,
    /// 当前活动（有焦点）客户端 token；commit 仅投递给它，避免广播多发
    active_token: Arc<AtomicU64>,
    /// 最近一次真正上报过 `focus_gained` 的客户端 token（0 = 还没有过）。
    ///
    /// 与 `active_token` 分开存：后者还会被 `ime_activated` 设置。两者分叉意味着某个宿主的
    /// `focus_gained` 在上游被吃掉了（如 DLL 守卫把 WinUI 3 宿主误判成 transient），这是诊断时
    /// 唯一能把它与正常失焦分开的信号。
    gained_token: Arc<AtomicU64>,
    /// 客户端注册回调（可选，服务侧后置注入）
    connected_hook: Arc<Mutex<Option<ClientConnectedHook>>>,
    /// 服务是否已就绪（消息处理器已切到真实的协调器），见 [`Self::mark_ready`]。
    ready: Arc<AtomicBool>,
}

impl PushServer {
    pub fn new(config: PushConfig) -> Self {
        Self {
            config,
            clients: Arc::new(Mutex::new(Vec::new())),
            active_token: Arc::new(AtomicU64::new(0)),
            gained_token: Arc::new(AtomicU64::new(0)),
            connected_hook: Arc::new(Mutex::new(None)),
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 宣告服务就绪：给就绪前连上的客户端补发 CMD_SERVICE_READY（幂等）。
    ///
    /// 不能连上就发：DLL 收到 SERVICE_READY 会立刻做全量同步，协调器未就绪时这些请求被
    /// `DeferredHandler` 静默丢弃，DLL 却以为同步过了（重建词库缓存时这段窗口可达数秒）。
    ///
    /// 必须在 `DeferredHandler::set_ready` 之后、[`Self::set_client_connected_hook`] 之前调用：
    /// DLL 收到 SERVICE_READY 会把会话级开关清零，它必须排在 hook 补跑推来的握手配置前面。
    ///
    /// unix 侧连接即发（客户端注册时 `ready_sent` 已是 `true`），这里对它们是空操作。
    pub fn mark_ready(&self) {
        let mut clients = self.clients.lock().unwrap();
        if self.ready.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut stalled = Vec::new();
        let mut sent = 0usize;
        for c in clients.iter_mut().filter(|c| !c.ready_sent) {
            c.ready_sent = true;
            sent += 1;
            if offer(c, service_ready_frame()) {
                stalled.push(c.token);
            }
        }
        drop_stalled(&mut clients, &stalled);
        if sent > 0 {
            info!("服务就绪：给 {sent} 个就绪前连上的 push 客户端补发 SERVICE_READY");
        }
    }

    /// 注入客户端注册回调（幂等覆盖）。回调在 push accept 线程执行，须自身线程安全且轻量。
    ///
    /// 注册时会对已连接但未触发过回调的客户端补跑一次（见 `PushClient::hooked`）。
    ///
    /// 锁序固定为 `connected_hook` → `clients`，与连接线程的触发路径一致，不会反转。
    /// 回调在持 `connected_hook` 锁期间执行（与连接线程同构）——回调内只准取 `clients` 锁，
    /// 不得回调本方法。
    pub fn set_client_connected_hook(&self, hook: ClientConnectedHook) {
        let mut guard = self.connected_hook.lock().unwrap();
        *guard = Some(hook);

        let pending = claim_all_unhooked(&self.clients);
        if !pending.is_empty() {
            info!(
                "connected_hook 注册：补跑 {} 个早于注册完成握手的 push 客户端",
                pending.len()
            );
            if let Some(hook) = guard.as_ref() {
                for token in pending {
                    hook(token);
                }
            }
        }
    }

    /// 定向投递给指定 token 的客户端（精确匹配，无兜底无广播）。返回是否命中。
    /// 按事件源计算的帧（如 activation status 的 hostRenderAvail 位）必须走此方法，广播会把
    /// 按别的进程算的位污染给无关客户端。
    pub fn push_to_token(&self, token: u64, data: &[u8]) -> bool {
        let mut clients = self.clients.lock().unwrap();
        let Some(c) = clients.iter().find(|c| c.token == token) else {
            return false;
        };
        if offer(c, data.to_vec()) {
            drop_stalled(&mut clients, &[token]);
        }
        true
    }

    /// 记录活动客户端 token（焦点获取 / IME 激活时调用）
    pub fn set_active_token(&self, token: u64) {
        self.active_token.store(token, Ordering::Relaxed);
    }

    /// 当前活动客户端 token（0 = 尚未有任何客户端获焦）。
    /// 失焦类命令据此做归属校验：跨宿主切换时 focus_lost 会晚于新宿主的 focus_gained 到达。
    pub fn active_token(&self) -> u64 {
        self.active_token.load(Ordering::Relaxed)
    }

    /// 记录「这个 token 真的上报过 `focus_gained`」。只在 focus_gained 路径调用，
    /// `ime_activated` 不许调——两者分叉正是本字段要抓的东西（见 `gained_token`）。
    pub fn note_focus_gained(&self, token: u64) {
        self.gained_token.store(token, Ordering::Relaxed);
    }

    /// 最近一次上报过 `focus_gained` 的 token（0 = 还没有过）。
    pub fn gained_token(&self) -> u64 {
        self.gained_token.load(Ordering::Relaxed)
    }

    /// 已连接的客户端（宿主应用进程）数。
    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// 是否有已连接的 TSF 客户端（用于决定是否经 IPC 让宿主执行前台操作）
    pub fn has_clients(&self) -> bool {
        !self.clients.lock().unwrap().is_empty()
    }

    /// 管道/SHM 后缀（变体隔离）。macOS forwarder 据此派生 SHM 名，须与 .app 读端一致。
    pub fn suffix(&self) -> &str {
        &self.config.suffix
    }

    /// 获取推送管道名称
    ///
    /// 格式 `wind_input_push{变体后缀}{SID 后缀}`，C++ TSF 端（`Globals.h`）算同一名字，顺序须一致。
    /// SID 后缀按用户隔离机器级的管道名字空间（见 [`crate::pipe_scope`]）。
    pub fn pipe_name(&self) -> String {
        format!(
            r"\\.\pipe\wind_input_push{}{}",
            self.config.suffix,
            crate::pipe_scope::user_scope_suffix()
        )
    }

    /// 启动推送管道服务器
    #[cfg(windows)]
    pub fn start(&self) -> anyhow::Result<()> {
        let pipe_name = self.pipe_name();
        info!("Push server starting on {:?}", pipe_name);

        let clients = self.clients.clone();
        let hook = self.connected_hook.clone();
        let ready = self.ready.clone();

        std::thread::Builder::new()
            .name("push-server".into())
            .spawn(move || {
                run_push_pipe_server(&pipe_name, clients, hook, ready);
            })?;

        Ok(())
    }

    /// 启动 UDS 推送服务器（macOS / Linux）
    #[cfg(unix)]
    pub fn start(&self) -> anyhow::Result<()> {
        // 同 BridgeServer::start：运行时目录不可信就不 bind。
        #[cfg(all(target_os = "linux", ext_presenter))]
        crate::endpoint::ensure_runtime_dir(&self.config.suffix)?;
        let path = crate::endpoint::push_socket_path(&self.config.suffix);
        info!("Push UDS server starting on {:?}", path);
        let clients = self.clients.clone();
        let hook = self.connected_hook.clone();
        std::thread::Builder::new()
            .name("push-server".into())
            .spawn(move || crate::push_unix::run_uds_push_server(path, clients, hook))?;
        Ok(())
    }

    #[cfg(not(any(windows, unix)))]
    pub fn start(&self) -> anyhow::Result<()> {
        warn!("Push server not supported on this platform");
        Ok(())
    }

    /// 仅供测试：返回 clients Arc 克隆，允许测试向 push_unix 注入并断言 fanout。
    #[cfg(test)]
    pub(crate) fn clients_for_test(&self) -> Arc<Mutex<Vec<PushClient>>> {
        self.clients.clone()
    }

    /// 仅供测试：connected_hook 的 Arc 克隆，交给测试里直接起的 `run_uds_push_server`。
    #[cfg(all(test, unix))]
    pub(crate) fn connected_hook_for_test(&self) -> Arc<Mutex<Option<ClientConnectedHook>>> {
        self.connected_hook.clone()
    }

    /// 挂一个内存客户端并设为活动客户端，返回它收到的全部推送（测试接缝）。
    ///
    /// 给下游 crate 的测试截获经 push 投递的上屏（`#[cfg(test)]` 跨不了 crate）。生产代码不调用。
    #[doc(hidden)]
    pub fn attach_capture_client(&self, token: u64) -> std::sync::mpsc::Receiver<Vec<u8>> {
        // 测试可能攒很多帧才读，给足余量，别让 [`PUSH_QUEUE_CAP`] 把它当卡死踢掉。
        let (tx, rx) = std::sync::mpsc::sync_channel(4096);
        self.clients.lock().unwrap().push(PushClient {
            token,
            tx,
            hooked: true,
            // 截获器只该看到被测代码推的帧
            ready_sent: true,
        });
        self.active_token.store(token, Ordering::Relaxed);
        rx
    }

    /// 向所有连接客户端广播消息（用于状态/激活同步，幂等无副作用）
    pub fn push_to_active(&self, data: &[u8]) {
        self.push_per_client(|_| data.to_vec());
    }

    /// 逐客户端生成并投递消息：`make(token)` 按各自的 token 现算内容。
    ///
    /// 用于 per-app 配置：不同宿主进程的取值不同（如 `compat.toml` 按进程关掉自动配对），
    /// 拿 [`Self::push_to_active`] 广播同一条会把某个进程的规则套到所有进程头上。
    pub fn push_per_client(&self, make: impl Fn(u64) -> Vec<u8>) {
        let mut clients = self.clients.lock().unwrap();
        let stalled: Vec<u64> = clients
            .iter()
            .filter(|c| offer(c, make(c.token)))
            .map(|c| c.token)
            .collect();
        drop_stalled(&mut clients, &stalled);
    }

    /// 某个宿主进程的全部推送客户端 token（token 高 32 位是 pid）。一个进程可能有多个
    /// TSF 实例、各连一条，故返回列表。用于「只重推给这个进程」的 per-app 配置。
    pub fn tokens_of_pid(&self, pid: u32) -> Vec<u64> {
        self.clients
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.token)
            .filter(|t| (t >> 32) as u32 == pid)
            .collect()
    }

    /// 仅向活动客户端投递（用于 commit 等带副作用的消息，避免广播导致多次上屏）。
    /// 优先按活动 token 匹配；无匹配且仅一个客户端时兜底发它；否则跳过。
    /// 返回是否已入队（false = 肯定没投出去；true 不保证对端最终收到）。当前调用点均忽略返回值。
    pub fn push_commit_to_active(&self, data: &[u8]) -> bool {
        let active = self.active_token.load(Ordering::Relaxed);
        let mut clients = self.clients.lock().unwrap();
        if clients.is_empty() {
            return false;
        }
        let target = if active != 0
            && let Some(i) = clients.iter().position(|c| c.token == active)
        {
            Some(i)
        } else if clients.len() == 1 {
            Some(0)
        } else {
            None
        };
        if let Some(i) = target {
            let token = clients[i].token;
            return match clients[i].tx.try_send(data.to_vec()) {
                Ok(()) => true,
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    // 这一帧没入队，对端重连后的全量同步也补不回上屏这类副作用帧。
                    warn!("push_commit: 活动客户端 0x{token:016X} 卡死，本条上屏帧丢弃");
                    drop_stalled(&mut clients, &[token]);
                    false
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => false,
            };
        }
        warn!(
            "push_commit: 无匹配活动客户端 (active=0x{:016X}, clients={})，跳过以防多发",
            active,
            clients.len()
        );
        false
    }

    /// 推送 ActivationStatus 给活跃客户端
    ///
    /// 与 Go 版本 `PushActivationStatusToActiveClient` 对齐。
    /// 激活后 TSF DLL 需要收到此消息才能正常工作。
    ///
    /// 注意：此方法使用 CMD_ACTIVATION_STATUS_PUSH 命令码（0x020C），
    /// 与 CMD_STATUS_UPDATE（0x0202）不同。C++ 端对两者有不同处理路径。
    pub fn push_activation_status(&self, chinese_mode: bool) {
        // 字面量刻意不接 `[ui.labels]`：本方法当前没有调用点，生产路径是协调器同名方法
        // （`push_config.rs`）。若要启用本方法，label 必须改由调用方传入。
        let label = if chinese_mode { "中" } else { "英" };
        let resp = wind_ipc::codec::encode_activation_status_push(
            chinese_mode,
            false, // full_width
            true,  // chinese_punct
            true,  // toolbar_visible
            false, // caps_lock
            false, // host_render_avail: 握手早期无焦点信息；真实 avail 位由 coordinator activation push 携带
            false, // soft_keyboard: 握手瞬间面板必然未开（它随焦点切换关闭）
            false, // soft_keyboard_keys: 同上，面板没开就没有"当前面"
            false, // hotkey_session: 握手瞬间不可能有热键模式活着（它随焦点/按键产生）
            &[],   // key_down_hotkeys
            &[],   // key_up_hotkeys
            label,
        );
        self.push_to_active(&resp);
    }
}

/// host-render 帧 sink：forwarder 把候选/状态帧广播给已连接 push 客户端（macOS .app）。
impl crate::host_render_sink::HostRenderSink for PushServer {
    fn push_frame(&self, frame: &[u8]) {
        self.push_to_active(frame);
    }
}

/// 推送管道服务器主循环
#[cfg(windows)]
fn run_push_pipe_server(
    pipe_name: &str,
    clients: Arc<Mutex<Vec<PushClient>>>,
    connected_hook: Arc<Mutex<Option<ClientConnectedHook>>>,
    ready: Arc<AtomicBool>,
) {
    use std::ffi::CString;
    use windows::Win32::Foundation::*;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Pipes::*;

    let pipe_name_c = match CString::new(pipe_name) {
        Ok(s) => s,
        Err(e) => {
            error!("Invalid push pipe name: {}", e);
            return;
        }
    };

    // 解析 SDDL 安全描述符，允许 AppContainer/UWP 进程连接
    let sd = crate::security::create_pipe_security_attributes();
    let sa = sd.as_ref().map(|s| {
        use windows::Win32::Security::SECURITY_ATTRIBUTES;
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: s.as_ptr() as *mut _,
            bInheritHandle: false.into(),
        }
    });

    loop {
        let pipe_handle = unsafe {
            CreateNamedPipeA(
                windows::core::PCSTR(pipe_name_c.as_ptr() as *const u8),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                0,
                sa.as_ref().map(|s| s as *const _),
            )
        };

        let pipe_handle = match pipe_handle {
            Ok(h) => h,
            Err(e) => {
                error!("CreateNamedPipe (push) failed: {}", e);
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };

        let connected = unsafe { ConnectNamedPipe(pipe_handle, None) };
        if connected.is_err() {
            let err = windows::core::Error::from_win32();
            if err.code() != ERROR_PIPE_CONNECTED.into() {
                warn!("ConnectNamedPipe (push) failed: {}", err);
                unsafe {
                    let _ = CloseHandle(pipe_handle);
                }
                continue;
            }
        }

        debug!("Push client connected to push pipe");

        // 握手（读 8 字节 token）必须离开 accept 循环：阻塞读期间管道名下没有监听实例，
        // 此刻连接的客户端拿到 ERROR_FILE_NOT_FOUND；DLL `_StartAsyncReader` 只试 3 次就永久放弃
        // （重连逻辑在它没能创建的线程里）。开机时众多宿主齐冲这条管道，输掉的会终身失联。
        let clients_c = clients.clone();
        let hook_c = connected_hook.clone();
        let ready_c = ready.clone();
        let pipe = PipeHandle(pipe_handle);
        if let Err(e) = std::thread::Builder::new()
            .name("push-handshake".into())
            .spawn(move || serve_push_client(pipe, clients_c, hook_c, ready_c))
        {
            error!("spawn push-handshake 线程失败: {e}；关闭该连接");
            unsafe {
                let _ = DisconnectNamedPipe(pipe_handle);
                let _ = CloseHandle(pipe_handle);
            }
        }
    }
}

/// 单个 push 客户端的完整生命周期：握手 → 注册 → 触发 connected_hook → writer loop。
///
/// 全程跑在自己的线程上，阻塞调用不会拖住其他客户端的接纳（见调用点注释）。
#[cfg(windows)]
fn serve_push_client(
    pipe: PipeHandle,
    clients: Arc<Mutex<Vec<PushClient>>>,
    connected_hook: Arc<Mutex<Option<ClientConnectedHook>>>,
    ready: Arc<AtomicBool>,
) {
    use windows::Win32::Foundation::*;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Pipes::*;

    let pipe_handle = pipe.0;

    // CMD_SERVICE_READY 不在这里发：服务还没就绪时发出去，DLL 据此做的全量同步会被吞掉。
    // 入表时按就绪与否决定立刻投递还是留给 `mark_ready` 补发（见 `register_client`）。
    // DLL 连上即写 token、不等 SERVICE_READY，故先读 token 不会互等。

    // 读取客户端 token（8 字节）
    let mut token_buf = [0u8; 8];
    let mut bytes_read: u32 = 0;
    let read_ok = unsafe {
        ReadFile(
            pipe_handle,
            Some(&mut token_buf),
            Some(&mut bytes_read),
            None,
        )
    };

    if read_ok.is_err() || bytes_read != 8 {
        warn!("Failed to read push client token");
        unsafe {
            let _ = DisconnectNamedPipe(pipe_handle);
            let _ = CloseHandle(pipe_handle);
        }
        return;
    }

    let token = u64::from_le_bytes(token_buf);
    debug!("Push client token: 0x{:016X}", token);

    // 注册客户端（不持有 pipe handle，本线程稍后独占）
    let (tx, rx) = push_channel();
    register_client(&clients, &ready, token, tx);

    // 注册完成后回调（经 tx 入队，顺序安全）：coordinator 借它给 host-render 白名单宿主
    // 定向补推握手帧。hook 尚未注册时客户端已带 hooked=false 在表中，由注册时补跑。
    {
        let guard = connected_hook.lock().unwrap();
        match guard.as_ref() {
            // 认领失败 = 补跑路径已抢先触发，跳过以免重复推送
            Some(hook) if claim_connected_hook(&clients, token) => hook(token),
            Some(_) => {}
            None => warn!(
                "Push client 0x{:016X} 握手完成时 connected_hook 尚未注册，\
                 待 hook 注册时补跑（服务启动期竞态；曾致 SearchHost 永久停留本地渲染）",
                token
            ),
        }
    }

    // 本线程转为该客户端的 writer loop（无需再 spawn：accept 循环早已脱身）
    push_writer_loop(pipe, rx, token, clients);
}

/// 推送写入循环
#[cfg(windows)]
fn push_writer_loop(
    pipe: PipeHandle,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    token: u64,
    clients: Arc<Mutex<Vec<PushClient>>>,
) {
    use windows::Win32::Foundation::*;
    use windows::Win32::Storage::FileSystem::*;

    let pipe = pipe.0;
    // recv 出错（发送端全部析构）即退出循环，与写失败同样走下面的清理。
    while let Ok(data) = rx.recv() {
        let mut bytes_written: u32 = 0;
        let write_ok = unsafe { WriteFile(pipe, Some(&data), Some(&mut bytes_written), None) };
        if write_ok.is_err() {
            debug!("Push client 0x{:016X} write failed, removing", token);
            break;
        }
    }

    unsafe {
        let _ = windows::Win32::System::Pipes::DisconnectNamedPipe(pipe);
        let _ = CloseHandle(pipe);
    }

    let mut clients = clients.lock().unwrap();
    clients.retain(|c| c.token != token);
    debug!("Push client 0x{:016X} disconnected", token);
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
