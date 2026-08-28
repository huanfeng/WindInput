//! 推送管道服务器
//!
//! 与 Go 版本 `wind_input/internal/bridge/server_push.go` 对齐。
//! 服务端主动推送状态更新、配置同步等消息给 TSF DLL。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use tracing::{debug, error};
use tracing::{info, warn};
#[cfg(windows)]
use wind_ipc::protocol::*;

#[cfg(windows)]
use crate::server::PipeHandle;

/// 推送管道配置
pub struct PushConfig {
    pub suffix: String,
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

/// 客户端连接信息
pub(crate) struct PushClient {
    /// 客户端 token（PID << 32 | instance_counter；unix 端由服务端发号）
    pub(crate) token: u64,
    /// 发送通道（writer 线程独占管道句柄）
    pub(crate) tx: std::sync::mpsc::Sender<Vec<u8>>,
    /// 是否已触发过 connected_hook。
    ///
    /// 存在的意义是**跨越 hook 注册时刻**：`start()` 在 main.rs 早期就开始 accept，而
    /// `set_client_connected_hook` 要等 Coordinator 构造完才注册（实测窗口约 230ms）。
    /// 窗口内完成握手的客户端曾被静默跳过——对 SearchHost 这类 locked/transient DocMgr
    /// 宿主是致命的：它既不发 FOCUS_GAINED 也不重发 IME_ACTIVATED，hook 是它拿到
    /// activation push 的**唯一**通路，丢一次即永久停留本地渲染（开始菜单候选窗被压在后面）。
    /// 该标志让 hook 注册时能精确补跑这批客户端，且与连接线程的自发触发互斥去重。
    pub(crate) hooked: bool,
}

/// 认领指定 token 的 hook 触发权：返回 `true` 表示本次调用赢得认领、应触发回调。
///
/// 连接线程与 `set_client_connected_hook` 的补跑可能同时盯上同一个客户端
/// （客户端已注册进表、但尚未走到自己的 hook 调用点），认领保证恰好触发一次。
/// 客户端若已断开出表则返回 `false`——不给死连接推帧。
///
/// 触发点在 `cfg(windows)` 的连接线程里；unix 侧不走这条通路，故非 Windows 只在测试中编译。
#[cfg(any(windows, test))]
fn claim_connected_hook(clients: &Mutex<Vec<PushClient>>, token: u64) -> bool {
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
    /// 最近一次**真正上报过 `focus_gained`** 的客户端 token（0 = 还没有过）。
    ///
    /// 与 `active_token` 分开存，是因为后者有**两个**来源：`focus_gained` 与
    /// `ime_activated`，而后者每进程只发一次。两者一旦分叉，就意味着某个宿主的
    /// `focus_gained` 在上游被吃掉了——2026-08-18 任务管理器正是如此（DLL 的
    /// locked/transient 守卫把 WinUI 3 宿主的 gained 全判成 transient），表现为
    /// 「切走再切回工具栏不显示 / per-app 模式不跟随 / 菜单里的应用名是上一个进程」。
    /// 那种缺口逃得过 `is_stale_focus_event`（token 恰等于 active，照常放行），
    /// 只能靠这个字段抓；诊断时它是唯一能把「守卫吃了 gained」与「正常失焦」分开的信号。
    gained_token: Arc<AtomicU64>,
    /// 客户端注册回调（可选，服务侧后置注入）
    connected_hook: Arc<Mutex<Option<ClientConnectedHook>>>,
}

impl PushServer {
    pub fn new(config: PushConfig) -> Self {
        Self {
            config,
            clients: Arc::new(Mutex::new(Vec::new())),
            active_token: Arc::new(AtomicU64::new(0)),
            gained_token: Arc::new(AtomicU64::new(0)),
            connected_hook: Arc::new(Mutex::new(None)),
        }
    }

    /// 注入客户端注册回调（幂等覆盖）。回调在 push accept 线程执行，须自身线程安全且轻量。
    ///
    /// **注册时会对已连接但未触发过回调的客户端补跑一次**：`start()` 早于本方法数百毫秒
    /// 开始 accept，这期间完成握手的客户端此前被静默丢弃（见 `PushClient::hooked`）。
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
    /// 「按事件源计算」的帧（如 activation status 的 hostRenderAvail 位）必须走此方法——
    /// 广播会把按别的进程计算的位污染给无关客户端（真机踩坑：SearchHost 收到兄弟进程的
    /// avail=0 推送后陷入 Band 窗口销毁重建循环）。
    pub fn push_to_token(&self, token: u64, data: &[u8]) -> bool {
        let clients = self.clients.lock().unwrap();
        if let Some(c) = clients.iter().find(|c| c.token == token) {
            let _ = c.tx.send(data.to_vec());
            true
        } else {
            false
        }
    }

    /// 记录活动客户端 token（焦点获取 / IME 激活时调用）
    pub fn set_active_token(&self, token: u64) {
        self.active_token.store(token, Ordering::Relaxed);
    }

    /// 当前活动客户端 token（0 = 尚未有任何客户端获焦）。
    /// 失焦类命令据此做归属校验：TSF 的 `OnKillThreadFocus` 比 DocMgr 级失焦晚约 100ms
    /// 发出 focus_lost，跨宿主切换时必然晚于新宿主的 focus_gained，无校验会清掉新宿主
    /// 刚建立的激活态（工具栏闪一下即消失）。
    pub fn active_token(&self) -> u64 {
        self.active_token.load(Ordering::Relaxed)
    }

    /// 记录「这个 token 真的上报过 `focus_gained`」。**只在 focus_gained 路径调用**，
    /// `ime_activated` 不许调——两者分叉正是本字段要抓的东西（见 `gained_token`）。
    pub fn note_focus_gained(&self, token: u64) {
        self.gained_token.store(token, Ordering::Relaxed);
    }

    /// 最近一次上报过 `focus_gained` 的 token（0 = 还没有过）。
    pub fn gained_token(&self) -> u64 {
        self.gained_token.load(Ordering::Relaxed)
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
    /// 必须与 Go/TSF 一致：后缀插在 `wind_input` 与 `_push` 之间。
    /// Go `endpoint_windows.go`: `\\.\pipe\wind_input` + Suffix + `_push`；
    /// TSF `Globals.h` dev 变体: `\\.\pipe\wind_input_push_dev`。
    /// 此前误写成 `wind_input_push{suffix}` (= wind_input_push_dev)，
    /// 导致 TSF 永远连不上 push 管道、收不到热键白名单 → Shift/Ctrl+Shift+E 不被转发。
    /// 尾部再追加 per-user SID 后缀（`_S-1-...`）：管道名字空间是机器级的，靠它按
    /// 用户隔离（详见 [`crate::pipe_scope`]）。C++ TSF 端算同一名字，顺序须一致：
    /// `wind_input_push{变体后缀}{SID 后缀}`。
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

        std::thread::Builder::new()
            .name("push-server".into())
            .spawn(move || {
                run_push_pipe_server(&pipe_name, clients, hook);
            })?;

        Ok(())
    }

    /// 启动 UDS 推送服务器（macOS / Linux）
    #[cfg(unix)]
    pub fn start(&self) -> anyhow::Result<()> {
        let path = crate::endpoint::push_socket_path(&self.config.suffix);
        info!("Push UDS server starting on {:?}", path);
        let clients = self.clients.clone();
        std::thread::Builder::new()
            .name("push-server".into())
            .spawn(move || crate::push_unix::run_uds_push_server(path, clients))?;
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

    /// 向所有连接客户端广播消息（用于状态/激活同步，幂等无副作用）
    pub fn push_to_active(&self, data: &[u8]) {
        let clients = self.clients.lock().unwrap();
        for client in clients.iter() {
            let _ = client.tx.send(data.to_vec());
        }
    }

    /// 逐客户端生成并投递消息：`make(token)` 按各自的 token 现算内容。
    ///
    /// 用于 **per-app 配置**——不同宿主进程的取值不同（如 `compat.toml` 按进程关掉自动配对），
    /// 拿 [`Self::push_to_active`] 广播同一条会把某个进程的规则套到所有进程头上。
    pub fn push_per_client(&self, make: impl Fn(u64) -> Vec<u8>) {
        let clients = self.clients.lock().unwrap();
        for client in clients.iter() {
            let _ = client.tx.send(make(client.token));
        }
    }

    /// 仅向活动客户端投递（用于 commit 等带副作用的消息，避免广播导致多次上屏）。
    /// 优先按活动 token 匹配；无匹配且仅一个客户端时兜底发它；否则跳过。
    /// 返回是否已投入某客户端的发送队列（false = 无客户端/无匹配/通道已断，
    /// 肯定没投出去）。true 仅表示入队成功，不保证对端最终收到。当前调用点均忽略
    /// 该返回；保留以备需要区分「肯定失败」的场景（撤销上屏曾据此回滚历史，现改为
    /// 读取即复位计数、不再依赖投递结果）。
    pub fn push_commit_to_active(&self, data: &[u8]) -> bool {
        let active = self.active_token.load(Ordering::Relaxed);
        let clients = self.clients.lock().unwrap();
        if clients.is_empty() {
            return false;
        }
        if active != 0
            && let Some(c) = clients.iter().find(|c| c.token == active)
        {
            return c.tx.send(data.to_vec()).is_ok();
        }
        if clients.len() == 1 {
            clients[0].tx.send(data.to_vec()).is_ok()
        } else {
            warn!(
                "push_commit: 无匹配活动客户端 (active=0x{:016X}, clients={})，跳过以防多发",
                active,
                clients.len()
            );
            false
        }
    }

    /// 推送 ActivationStatus 给活跃客户端
    ///
    /// 与 Go 版本 `PushActivationStatusToActiveClient` 对齐。
    /// 激活后 TSF DLL 需要收到此消息才能正常工作。
    ///
    /// 注意：此方法使用 CMD_ACTIVATION_STATUS_PUSH 命令码（0x020C），
    /// 与 CMD_STATUS_UPDATE（0x0202）不同。C++ 端对两者有不同处理路径。
    pub fn push_activation_status(&self, chinese_mode: bool) {
        // ⚠️ 这两个字面量**刻意不接 `[ui.labels]`**：本方法既拿不到 Config，当前也
        // 没有任何调用点——生产路径是协调器那个同名方法（`push_config.rs`），它推的是
        // `build_status` 算好的 `icon_label`，用户配置在那条路上生效。
        //
        // 若哪天要启用本方法，label 必须改成由调用方传入：否则用户把英文态配成 "En"
        // 后，握手瞬间会先闪一下 "英" 再被正式推送改掉。
        let label = if chinese_mode { "中" } else { "英" };
        let resp = wind_ipc::codec::encode_activation_status_push(
            chinese_mode,
            false, // full_width
            true,  // chinese_punct
            true,  // toolbar_visible
            false, // caps_lock
            false, // host_render_avail: 握手早期无焦点信息；真实 avail 位由 coordinator activation push 携带
            false, // soft_keyboard: 握手瞬间面板必然未开（它随焦点切换关闭）
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

        // 握手（写 SERVICE_READY + 读 8 字节 token）**必须离开 accept 循环**。
        //
        // 二者都是无超时阻塞调用，而它们执行期间管道名下**没有任何监听实例**——
        // 此刻敲门的客户端拿到的是 ERROR_FILE_NOT_FOUND，白扣一次重试机会。
        // DLL 侧 `_StartAsyncReader` 只试 3 次就 `return FALSE` 永久放弃，且
        // **重连逻辑活在它没能创建的 async reader 线程里**，于是一次失手 = 终身失联，
        // 之后服务重启多少次它都感知不到（IPCClient.cpp:1940-2021）。
        // 开机/服务重启时 20+ 个宿主 DLL 在数十毫秒内齐冲这条管道，输掉的那个
        // 就此停在无 push 状态——实测 SearchHost 中招后 HostRender 再不激活，
        // 开始菜单候选窗永久压在菜单后方（2026-07-21 定位）。
        //
        // 交给独立线程后：accept 循环立刻回到 CreateNamedPipe，监听实例常驻；
        // 慢客户端最多拖住自己那一个线程，拖不住别人的接纳。
        let clients_c = clients.clone();
        let hook_c = connected_hook.clone();
        let pipe = PipeHandle(pipe_handle);
        if let Err(e) = std::thread::Builder::new()
            .name("push-handshake".into())
            .spawn(move || serve_push_client(pipe, clients_c, hook_c))
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
/// 全程跑在自己的线程上——accept 循环把连接交出来后立刻回去建下一个监听实例，
/// 故本函数里的阻塞调用不会拖住任何其他客户端的接纳（见调用点长注释）。
#[cfg(windows)]
fn serve_push_client(
    pipe: PipeHandle,
    clients: Arc<Mutex<Vec<PushClient>>>,
    connected_hook: Arc<Mutex<Option<ClientConnectedHook>>>,
) {
    use windows::Win32::Foundation::*;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Pipes::*;

    let pipe_handle = pipe.0;

    // 与 Go 版对齐：先发送 CMD_SERVICE_READY，再读取 token。
    // Go 的 push pipe 在 ConnectNamedPipe 后立即写 SERVICE_READY，
    // C++ 端 AsyncReader 收到后触发 _DoFullStateSync(WM_SERVICE_READY)。
    let ready_msg = IpcHeader::new(CMD_SERVICE_READY, 0).to_bytes().to_vec();
    {
        let mut bytes_written: u32 = 0;
        let write_ok = unsafe {
            WriteFile(
                pipe_handle,
                Some(&ready_msg),
                Some(&mut bytes_written),
                None,
            )
        };
        if write_ok.is_err() {
            warn!("Failed to send SERVICE_READY to push client");
            unsafe {
                let _ = DisconnectNamedPipe(pipe_handle);
                let _ = CloseHandle(pipe_handle);
            }
            return;
        }
    }
    debug!("Sent SERVICE_READY to push client");

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

    // 创建发送通道
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

    // 注册客户端（不持有 pipe handle，本线程稍后独占）
    let client = PushClient {
        token,
        tx,
        hooked: false,
    };

    {
        let mut c = clients.lock().unwrap();
        // 清理同 token 的旧连接
        c.retain(|c| c.token != token);
        c.push(client);
    }

    // 注册完成后回调（发送经 tx 入队，下面的 writer loop 写出，顺序安全）。
    // 用途：host-render 白名单宿主（transient DocMgr，如 SearchHost）服务重启重连时
    // 既不发 focus_gained 也不重发 IME_ACTIVATED，无任何 activation push 会到达 →
    // DLL 永不重新 setup。由 coordinator 在此回调中定向补推握手帧。
    //
    // hook 尚未注册时**不能静默跳过**：客户端已带 hooked=false 在表中，
    // set_client_connected_hook 会补跑（见该方法与 PushClient::hooked）。
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
mod tests {
    use super::*;

    /// 锁定 push 管道名格式：变体后缀紧跟 `wind_input_push`，per-user SID 后缀再在其后，
    /// 与 TSF Globals.cpp 一致，否则 TSF 连不上 push 管道。SID 段随机器/用户变化，
    /// 故只断言前缀不变量；Windows 上另断言确有 `_S-` 后缀。
    #[test]
    fn test_push_pipe_name_suffix_position() {
        let dev = PushServer::new(PushConfig {
            suffix: "_dev".into(),
            write_timeout_ms: 30_000,
        });
        let dev_name = dev.pipe_name();
        assert!(
            dev_name.starts_with(r"\\.\pipe\wind_input_push_dev"),
            "变体后缀须紧跟 wind_input_push，实得 {dev_name}"
        );

        let release = PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        });
        let rel_name = release.pipe_name();
        assert!(
            rel_name.starts_with(r"\\.\pipe\wind_input_push"),
            "实得 {rel_name}"
        );

        // Windows：SID 后缀必在变体后缀之后（`..._push_dev_S-...`）。
        #[cfg(windows)]
        {
            assert!(
                dev_name.contains(r"\wind_input_push_dev_S-"),
                "缺 per-user SID 后缀，实得 {dev_name}"
            );
            assert!(
                rel_name.contains(r"\wind_input_push_S-"),
                "缺 per-user SID 后缀，实得 {rel_name}"
            );
        }
    }

    /// push_to_token 必须精确匹配、无兜底：activation push 的 hostRenderAvail 位按事件源
    /// 计算，错发给别的客户端会触发 Band 窗口销毁重建循环（真机踩坑）。
    #[test]
    fn push_to_token_exact_match_no_fallback() {
        let srv = PushServer::new(PushConfig::default());
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        srv.clients_for_test().lock().unwrap().push(PushClient {
            token: 0xAA_0000_0001,
            tx,
            hooked: false,
        });

        // 命中：精确 token 投递
        assert!(srv.push_to_token(0xAA_0000_0001, &[1, 2, 3]));
        assert_eq!(rx.try_recv().unwrap(), vec![1, 2, 3]);

        // 未命中：即使只有一个客户端也不得兜底投递
        assert!(!srv.push_to_token(0xBB_0000_0002, &[9]));
        assert!(rx.try_recv().is_err(), "不匹配的 token 不得收到任何帧");
    }

    fn add_test_client(srv: &PushServer, token: u64) {
        let (tx, _rx) = std::sync::mpsc::channel::<Vec<u8>>();
        srv.clients_for_test().lock().unwrap().push(PushClient {
            token,
            tx,
            hooked: false,
        });
    }

    /// hook 注册前已完成握手的客户端必须被补跑。
    ///
    /// 真机根因（2026-07-21 复现）：`push_server.start()` 早于
    /// `set_client_connected_hook` 约 230ms（中间隔着 `Coordinator::new()`），
    /// SearchHost 的 DLL 在该窗口内重连成功，回调被静默丢弃 → 它是白名单宿主拿到
    /// activation push 的唯一通路 → HostRender 永不激活，开始菜单候选窗被压在后面。
    #[test]
    fn hook_registration_replays_clients_connected_before_it() {
        let srv = PushServer::new(PushConfig::default());
        add_test_client(&srv, 0xAA_0000_0001);
        add_test_client(&srv, 0xBB_0000_0001);

        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_c = seen.clone();
        srv.set_client_connected_hook(Box::new(move |t| seen_c.lock().unwrap().push(t)));

        let mut got = seen.lock().unwrap().clone();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![0xAA_0000_0001, 0xBB_0000_0001],
            "注册前连接的客户端必须全部补跑，否则 SearchHost 永久停留本地渲染"
        );
    }

    /// 补跑与连接线程的自发触发必须互斥去重（同一客户端恰好触发一次）。
    #[test]
    fn connected_hook_fires_exactly_once_per_client() {
        let srv = PushServer::new(PushConfig::default());
        add_test_client(&srv, 0xAA_0000_0001);

        let count = Arc::new(AtomicU64::new(0));
        let count_c = count.clone();
        srv.set_client_connected_hook(Box::new(move |_| {
            count_c.fetch_add(1, Ordering::Relaxed);
        }));
        assert_eq!(count.load(Ordering::Relaxed), 1, "补跑应触发一次");

        // 模拟连接线程随后走到自己的触发点：认领已被补跑拿走，不得重复触发
        assert!(
            !claim_connected_hook(&srv.clients_for_test(), 0xAA_0000_0001),
            "已补跑的客户端不得被连接线程再次认领"
        );

        // 断开的客户端不得被认领（不给死连接推帧）
        assert!(!claim_connected_hook(&srv.clients_for_test(), 0xDEAD_0000));
    }

    /// 重连（同 token 重新入表）必须能再次触发 hook——DLL 重连后需要重新 setup。
    #[test]
    fn reconnected_client_can_be_claimed_again() {
        let srv = PushServer::new(PushConfig::default());
        add_test_client(&srv, 0xAA_0000_0001);
        assert!(claim_connected_hook(
            &srv.clients_for_test(),
            0xAA_0000_0001
        ));

        // 重连：连接路径 retain 掉旧条目后压入新条目（hooked 复位）
        srv.clients_for_test()
            .lock()
            .unwrap()
            .retain(|c| c.token != 0xAA_0000_0001);
        add_test_client(&srv, 0xAA_0000_0001);
        assert!(
            claim_connected_hook(&srv.clients_for_test(), 0xAA_0000_0001),
            "重连后必须可再次认领，否则 DLL 重连不会重新 setup"
        );
    }
}
