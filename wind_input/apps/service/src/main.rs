//! wind_input: 输入法主服务进程
//!
//! 最小可输入服务：启动 Named Pipe 服务器，接收 TSF DLL 按键事件并响应。
//! 与 Go 版本 `wind_input/cmd/service/main.go` 的启动序列对齐。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use file_rotate::compression::Compression;
use file_rotate::{ContentLimit, FileRotate};
use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::ChronoLocal;

use wind_bridge::deferred::DeferredHandler;
use wind_bridge::push::{PushConfig, PushServer};
use wind_bridge::server::{BridgeConfig, BridgeServer};
use wind_webdata::WebDataRpc;

// 启动轨迹下沉在 wind-config，好让 UI 线程等下层也能打点（见该模块文档）。
// 时间戳格式一并取自那里：与主日志、wind_tsf 的 `CFileLogger::_FormatTimestamp`
// 共用一处定义，三份日志才能归并排序，也避免两边各写一份而漂移。
use wind_config::startup_trace::{self, LOG_TIME_FORMAT};

mod backup_cli;
mod cli_util;
mod config_cli;
mod dict_cli;
mod log_rotate;
mod phrase_cli;
mod restart_cli;
mod schema_cli;

/// 顶层 CLI 总览（`wind_input help`）。主动请求的帮助走 stdout（可管道/重定向），
/// 与 `--version` 一致；各子命令的详细用法见 `wind_input <子命令> help`。
fn print_root_usage() {
    println!(
        "WindInput 输入法服务 v{}\n\
         \n\
         用法: wind_input [子命令]   （不带子命令 = 启动输入法服务）\n\
         \n\
         子命令:\n  \
         config    配置查看/读写/导入导出（离线可用，core 在线时热重载）\n  \
         schema    方案配置 / 分类词库开关 / 词库缓存重建（需 core 在线）\n  \
         dict      用户词库按方案导入导出（需 core 在线）\n  \
         phrase    用户短语导入导出 / 系统短语恢复（需 core 在线）\n  \
         backup    整机备份创建/查看/还原（需 core 在线）\n  \
         restart   重启输入法服务（未运行则直接启动）\n  \
         help      显示本帮助；--version 显示版本\n\
         \n\
         各子命令详细用法: wind_input <子命令> help",
        env!("WIND_APP_VERSION")
    );
}

/// GUI 子系统（release profile，`windows_subsystem="windows"`）下进程不附着控制台，
/// 故 CLI 子命令的 `println!` 无处可写。此函数把进程附着到**父控制台**（调用它的 cmd/PowerShell），
/// 并把 `CONOUT$/CONIN$` 设回标准句柄，让 stdout/stderr/stdin 直达该终端。
///
/// 注意：GUI 子系统进程不会让 shell 等待——提示符会先返回、输出随后插入（交错）。
/// 需"等它跑完"时请重定向（`> out.txt 2>&1`）或 `start /wait`。
/// 无父控制台（双击启动/被服务拉起）时静默返回——那种场景本就不该走 CLI。
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows::core::w;

    // GENERIC_READ | GENERIC_WRITE（用裸常量避免跨版本导入路径差异）。
    const GENERIC_RW: u32 = 0xC000_0000;

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return; // 没有父控制台
        }
        // 重新打开控制台读写设备并设为进程标准句柄；否则 GUI 子系统启动时标准句柄为空。
        if let Ok(out) = CreateFileW(
            w!("CONOUT$"),
            GENERIC_RW,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, out);
            let _ = SetStdHandle(STD_ERROR_HANDLE, out);
        }
        if let Ok(inp) = CreateFileW(
            w!("CONIN$"),
            GENERIC_RW,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) {
            let _ = SetStdHandle(STD_INPUT_HANDLE, inp);
        }
    }
}

fn main() {
    // CLI 子命令（config/schema/...）：在服务启动前拦截，处理完即退出。
    // 注意保持在单例检查之前——CLI 进程不该被「另一实例已运行」挡掉。
    let cli_args: Vec<String> = std::env::args().collect();
    // 由 relaunch_self 在重启路径下附加，标记「此次启动是手动重启后的新进程」而非
    // 开机首启/CLI 离线启动——service-ready 后据此决定是否弹「服务已重启」提示。
    let restarted = cli_args.iter().any(|a| a == "--restarted");
    let sub = cli_args.get(1).map(String::as_str);
    if matches!(
        sub,
        Some(
            "config"
                | "schema"
                | "dict"
                | "phrase"
                | "backup"
                | "restart"
                | "help"
                | "--help"
                | "-h"
                | "--version"
                | "-V"
        )
    ) {
        // GUI 子系统下附着父控制台，让输出回到调用的终端（详见 attach_parent_console）。
        #[cfg(windows)]
        attach_parent_console();
        let code = match sub {
            Some("config") => config_cli::run(&cli_args[2..]),
            Some("schema") => schema_cli::run(&cli_args[2..]),
            Some("dict") => dict_cli::run(&cli_args[2..]),
            Some("phrase") => phrase_cli::run(&cli_args[2..]),
            Some("backup") => backup_cli::run(&cli_args[2..]),
            Some("restart") => restart_cli::run(&cli_args[2..]),
            Some("help" | "--help" | "-h") => {
                print_root_usage();
                0
            }
            Some("--version" | "-V") => {
                println!(
                    "wind_input {} (build {} git:{})",
                    env!("WIND_APP_VERSION"),
                    env!("WIND_BUILD_TIME"),
                    env!("WIND_GIT_HASH"),
                );
                0
            }
            _ => unreachable!(),
        };
        // process::exit 不会刷新缓冲，重定向/管道时可能丢尾部输出——显式 flush。
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(code);
    }

    // 0. 设置 DPI 感知（与 Go 版 setDPIAwareness 对齐）
    // 必须在任何窗口创建之前调用，否则坐标会被 Windows DPI 虚拟化
    set_dpi_awareness();

    // 启动轨迹的第一个探针必须早于 init_logger：主日志失效时，
    // 「究竟有没有走到日志初始化」本身就是要回答的问题。
    startup_trace::stage("begin");

    // 0.5 启动上下文自检（B）：核心必须跑在交互用户上下文。若被错误地拉起在
    // SYSTEM/服务账户或 AppContainer 里（TSF DLL 从早期系统/UWP 宿主 CreateProcessW
    // 拉核心时不换令牌），当场退出——绝不带着系统预置（五笔）去占坑、供错配置。
    // 早于单例与日志：错上下文的进程连日志目录都未必写得了。
    guard_process_context();

    // 1. 单例检查（与 Go 版 checkSingleton 对齐）
    //
    // **必须早于 init_logger**：后者会滚动日志（rotate_on_startup），而一个注定要退出的
    // 重复实例不该有权改动正在运行实例的日志。此前顺序相反，于是形成了恶性反馈——
    // 用户遇到故障 → 尝试重启输入法 → 新实例先把故障现场的日志顶到 .1.log、再写下一个
    // 两行空壳、然后被单例挡掉退出。越排查，现场越少。
    //
    // 代价是此处无法用 tracing（subscriber 尚未装），改用启动轨迹 + stderr：
    // 反正 `exit(1)` 也刷不出 tracing 的 non_blocking 缓冲，同步落盘的轨迹反而更可靠。
    let _singleton_guard = match check_singleton() {
        SingletonCheck::Acquired(guard) => guard,
        SingletonCheck::AlreadyRunning => {
            startup_trace::stage("singleton-BLOCKED 另一实例已在运行，本实例退出");
            eprintln!("WindInput: 另一个实例已在运行中");
            std::process::exit(1);
        }
        // 够不着单例对象：本进程多半继承了受限的宿主上下文（TSF DLL 用
        // `CreateProcessW` 拉起服务时不换令牌，宿主是 AppContainer/低完整性，
        // 子进程就跟着受限）。这种进程也建不了命名管道、写不了日志目录，
        // 撑不起服务，退出是对的——但绝不能声称「另一实例已在运行」，
        // 那会把「服务根本没起来」伪装成「服务已经在跑」。
        // 注：改 `Local\` + 默认 DACL 后，同会话进程通常都够得着自己会话的对象，
        // 加上前置的 guard_process_context 已拦掉多数错上下文，此分支已近乎兜底。
        SingletonCheck::Inaccessible(err) => {
            startup_trace::stage(&format!(
                "singleton-INACCESSIBLE err={err} 打不开单例对象（本进程上下文受限），\
                 无从判断另一实例是否存在，本实例退出"
            ));
            eprintln!("WindInput: 无法访问单例对象 (err={err})，本进程上下文受限");
            std::process::exit(1);
        }
    };
    startup_trace::stage("singleton-ok");

    // 2. 初始化日志（含启动滚动：至此才确定本实例会真正运行）
    init_logger();
    startup_trace::stage("logger-ready");

    let pipe_suffix = wind_config::variant::pipe_suffix();
    let variant = if wind_config::variant::is_dev() {
        "dev"
    } else {
        "release"
    };
    info!(
        "WindInput service starting (v{}, {} variant, build {} git:{})",
        env!("WIND_APP_VERSION"),
        variant,
        env!("WIND_BUILD_TIME"),
        env!("WIND_GIT_HASH"),
    );
    info!("Singleton check passed");

    // 2.5 等待用户配置目录就绪。
    //
    // 开机自启时服务可能跑在登录会话很早的阶段，此时漫游 known folder（%APPDATA%）
    // 未必已解析/挂载完成，而日志目录走的是另一个 known folder（%LOCALAPPDATA%，
    // 见 Config::log_dir vs user_config_dir）——两者独立解析，于是会出现
    // 「日志正常写出、用户配置却像不存在一样」，配置静默退化为系统预置，
    // 用户表现为「设置成全拼，重启后工具栏又变回五笔」。
    //
    // 必须在 init_logger() 之后：探测过程本身要留下日志。
    wind_config::Config::wait_user_config_ready(std::time::Duration::from_secs(10));
    startup_trace::stage("config-ready");

    // D：若本用户此刻确有 config.toml（定制过设置），落一个本地标记（幂等）。
    // 下次开机 `probe_user_config` 用它区分「默认用户（永不等）」与「定制用户但漫游
    // 未挂载（要等，别退回系统五笔）」。只在服务启动路径写，不进 load()（见其文档）。
    wind_config::Config::mark_user_config_seen_if_present();

    // D2：清理用户层里的陈旧键（幂等，`Config::prune_user_config` 自带日志），两类：
    //  - **冗余键**：取值与出厂默认（L1⊕L2⊕L2.5）相同。它们是「默认值升级的死区」——写入时与
    //    默认一致、看似无害，日后默认改了用户却一直停在旧值。set_user_value 已收口不再
    //    产生新的，此处清掉存量。
    //  - **退役键**（`RETIRED_KEYS`）：结构体里已删除、serde 读取时静默丢弃，但写回走原始
    //    toml::Value 故会永久留存。留着会误导用户以为它还在生效。
    //
    // 必须在 wait_user_config_ready 之后：漫游未挂载时用户层「看起来是空的」，那时清理
    // 无事可做（返回 0），但也绝不会误删。
    if let Err(e) = wind_config::Config::prune_user_config() {
        warn!("Prune user config failed: {e}");
    }

    // D3：把引导键绑定物化进用户层 `keys.key_actions`（一次性，靠版本号幂等）。
    //
    // 在此之前，五处 `trigger_keys` 的出厂值住在 L2 且每次 load 都折算进 key_actions，
    // 而设置页只写 key_actions ⇒ 用户删掉某个出厂绑定后，下次加载又被折算灌回去，
    // 且备份/还原都带不走「我删过它」。物化之后 key_actions 是唯一真相源。
    //
    // 排在 prune 之后：物化是终态写入，应基于已清理干净的用户层。两道安全闸
    // （漫游未挂载 / data/config.toml 缺席）都退化为「什么都不做」，见函数文档。
    if let Err(e) = wind_config::Config::materialize_key_actions() {
        warn!("Materialize key_actions failed: {e}");
    }

    // 3. 创建 DeferredHandler（启动时返回安全默认值）
    let deferred = DeferredHandler::new();

    // 4. 启动 Push 管道服务器
    let push_config = PushConfig {
        suffix: pipe_suffix.to_string(),
        write_timeout_ms: 30_000,
    };
    let push_server = Arc::new(PushServer::new(push_config));

    if let Err(e) = push_server.start() {
        fatal_exit(
            "push-server-FAILED",
            &format!("Push server failed to start: {e}"),
        );
    }

    // 5. 创建 Bridge 服务器
    let bridge_config = BridgeConfig {
        suffix: pipe_suffix.to_string(),
        request_timeout_ms: 1000,
    };
    // host-render 管理器（Windows）：白名单取自 compat.toml 里 host_render=true 的规则
    // （系统层 + 用户层，与 Coordinator 启动时加载 AppCompat 同一口径）。
    // 同一 Arc 实例同时注入 BridgeServer（连接循环 setup/清理）与 Coordinator（写帧/隐藏）。
    #[cfg(windows)]
    let host_render = {
        let data_dir = wind_config::Config::data_dir();
        let user_dir = wind_config::Config::user_config_dir();
        let whitelist =
            wind_config::app_compat::AppCompat::load(data_dir.as_deref(), user_dir.as_deref())
                .host_render_processes();
        wind_bridge::host_render_windows::HostRenderManager::new(pipe_suffix, whitelist)
    };
    let bridge = BridgeServer::new(bridge_config, deferred.clone());
    #[cfg(windows)]
    let bridge = bridge.with_host_render(host_render.clone());

    // 6. 启动 Bridge 服务器
    if let Err(e) = bridge.start() {
        fatal_exit(
            "bridge-server-FAILED",
            &format!("Bridge server failed to start: {e}"),
        );
    }

    startup_trace::stage("bridge-ready");

    // 8. 创建重启信号通道（须在协调器创建前，使 request_restart 的发送端就绪）
    let restart_rx = wind_coordinator::restart_signal();

    // 9. 创建中央协调器（传入 PushServer 用于激活状态推送）
    // 前后各打一次：候选窗/工具栏的窗口线程在此创建，是「有服务无 GUI」的头号嫌疑段。
    startup_trace::stage("coordinator-begin");
    let coordinator = wind_coordinator::Coordinator::new(push_server.clone());
    startup_trace::stage("coordinator-done");
    // 语言栏图标：状态推送只在状态**变化**时发生，这里补一次初始发布，否则开机后到
    // 用户第一次切换中英/标点之前，DLL 都读不到共享内存、只能本地绘制（图标正常但
    // 没有标点角标）。非 Windows 桌面形态下是空操作。
    coordinator.publish_initial_langbar_icon();
    // 注入 host-render 管理器（与 BridgeServer 共享同一实例），供后续写帧/隐藏使用。
    #[cfg(windows)]
    coordinator.set_host_render(host_render.clone());
    // push 客户端注册回调：host-render 白名单受限宿主（SearchHost 等 transient DocMgr）
    // 服务重启重连时不发任何激活事件，由此回调补推 activation 握手使 DLL 重新 setup。
    #[cfg(windows)]
    {
        let coord = coordinator.clone();
        push_server.set_client_connected_hook(Box::new(move |token| {
            coord.on_push_client_connected(token);
        }));
    }
    let coord_for_web = coordinator.clone();
    let coord_for_restart_toast = coordinator.clone();
    deferred.set_ready(coordinator);

    // 9.5 启动本地控制 / 配置 JSON-RPC 服务（命名管道：..._ctrl + ..._events）。
    // 本地授权靠 OS ACL（SDDL），不再需要 token/Origin/CORS/端口发现。
    // 同步线程模型（与 bridge/push 一致），不引入 tokio 到控制路径。
    let core_rpc: Arc<dyn wind_rpc::CoreRpc> = Arc::new(RpcCore(coord_for_web));
    match wind_rpc::RpcServer::new(core_rpc, variant, pipe_suffix) {
        Ok(rpc_server) => {
            // 活跃方案变更 → RPC 事件广播。EngineManager 位于 wind-rpc 之下，无法直接
            // 持有 EventSink，故在此接线——这里是唯一同时看得见两者的地方。接上后，
            // 快捷键/托盘/RPC 任一路径切方案，设置界面都能收到通知（此前只有候选窗的
            // push 通道收得到，而那条载荷里没有方案 id）。
            let sink = rpc_server.event_sink();
            wind_engine::active_hook::set_active_schema_hook(std::sync::Arc::new({
                let sink = sink.clone();
                move |id: &str| {
                    sink.broadcast("schema.activeChanged", serde_json::json!({ "id": id }));
                }
            }));
            // 配置落盘 → 广播。托盘/右键菜单、快捷键、命令栏改配置都不经 RPC，
            // 设置界面此前无从得知（「显示工具栏」「简入繁出」「主题」「常驻显示」等
            // 都会显示陈旧值）。带上 key 与新值，订阅方可直接更新对应字段，无需回查。
            // 经 RPC setItems 的写入同样会走到这里，属预期内的自我回声：设置端此时
            // 值已相同，更新是幂等的。
            wind_config::change_hook::set_config_change_hook(std::sync::Arc::new(
                move |path: &[&str], value: &toml::Value| {
                    let key = path.join(".");
                    let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
                    sink.broadcast(
                        "config.changed",
                        serde_json::json!({ "reason": "write", "key": key, "value": value }),
                    );
                },
            ));
            if let Err(e) = rpc_server.start() {
                error!("RPC server failed to start: {}", e);
            }
            // 句柄需在进程生命周期内保活（控制/事件 server 已分别在后台线程运行）。
            Box::leak(Box::new(rpc_server));
        }
        Err(e) => error!("RPC server init failed (网页/GUI 设置不可用): {}", e),
    }

    info!(
        "WindInput service ready (pipes: wind_input{}, wind_input_push{}, wind_input_ctrl{})",
        pipe_suffix, pipe_suffix, pipe_suffix
    );
    startup_trace::stage("service-ready");

    // 手动重启（区别于开机首启/CLI 离线启动）：新进程就绪后补一次用户可见反馈，
    // 因为旧进程连同其 UI 窗口线程已在重启前被销毁，反馈只能由新进程接力弹出。
    if restarted {
        coord_for_restart_toast.show_restart_toast();
    }

    // 10. 阻塞主线程，直到菜单触发"重启服务"
    //
    // macOS 例外：主线程改跑 CFRunLoop。Carbon 全局热键的事件只投递到**主线程**的
    // 事件队列，辅助线程自己跑 run loop 收不到（见 wind_ui::global_hotkey_macos 模块头）。
    // 「重启服务」的等待挪到辅助线程，收到信号后停 run loop，主线程回到这里走原重启路径
    // ——语义与其它平台一致，只是等待的姿势不同。
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static RESTART: AtomicBool = AtomicBool::new(false);
        std::thread::Builder::new()
            .name("restart-waiter".into())
            .spawn(move || {
                if restart_rx.recv().is_ok() {
                    RESTART.store(true, Ordering::SeqCst);
                }
                // 通道断开（不应发生）时同样停 loop：RESTART 仍为 false，下方退回挂起，
                // 与其它平台 `Err(_) => park()` 的结局一致。
                wind_ui::global_hotkey_macos::stop_main_loop();
            })
            .expect("spawn restart waiter");

        wind_ui::global_hotkey_macos::run_main_loop();

        if RESTART.load(Ordering::SeqCst) {
            info!("Restart requested, relaunching service...");
            drop(_singleton_guard);
            relaunch_self();
            std::process::exit(0);
        }
        loop {
            std::thread::park();
        }
    }

    #[cfg(not(target_os = "macos"))]
    match restart_rx.recv() {
        Ok(()) => {
            info!("Restart requested, relaunching service...");
            // 先释放单例 Named Mutex（关闭句柄），让新实例可获取所有权，避免竞争
            drop(_singleton_guard);
            relaunch_self();
            std::process::exit(0);
        }
        Err(_) => loop {
            // 通道断开（不应发生）：退回挂起
            std::thread::park();
        },
    }
}

/// 适配 Coordinator 为 RPC 服务的运行时状态来源（解耦 wind-rpc 与 wind-coordinator）。
struct RpcCore(Arc<wind_coordinator::Coordinator>);

impl wind_rpc::CoreRpc for RpcCore {
    fn is_chinese_mode(&self) -> bool {
        self.0.is_chinese_mode()
    }
    fn active_schema_id(&self) -> String {
        self.0.active_schema_id()
    }
    fn apply_config(&self) -> bool {
        self.0.reload_user_config()
    }
    fn data_rpc(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.0.web_data_rpc(method, params)
    }
    fn fonts(&self) -> Vec<(String, String)> {
        self.0.list_font_families()
    }
}

/// 以分离子进程重新启动自身（用于"重启服务"）。
#[cfg(windows)]
fn relaunch_self() {
    match spawn_detached_self(true) {
        Ok(()) => info!("Relaunched service"),
        Err(e) => error!("Failed to relaunch: {}", e),
    }
}

#[cfg(not(windows))]
fn relaunch_self() {
    if let Err(e) = spawn_detached_self(true) {
        error!("Failed to relaunch: {}", e);
    }
}

/// 以脱离父进程的方式拉起自身（服务启动形态）。服务重启（[`relaunch_self`]）与
/// CLI `restart` 的离线启动共用：不脱离时子进程会继承父控制台（用户关终端窗口
/// 广播 CTRL_CLOSE_EVENT 连带杀掉刚起的服务）与 kill-on-job-close 作业对象
/// （IME/TSF 宿主进程常见，父进程退出即连带杀子进程——症状：重启只退出、新进程
/// 不存活）。stdio 一律接 null：服务日志走文件，不该喷进调用方终端。
#[cfg(windows)]
fn spawn_detached_self(restarted: bool) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let exe = std::env::current_exe()?;
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    let try_spawn = |flags: u32| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.creation_flags(flags)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if restarted {
            cmd.arg("--restarted");
        }
        cmd.spawn().map(|_| ())
    };
    // 优先带 breakaway；作业对象不允许 breakaway（spawn 报错）时回退不带该标志再试。
    try_spawn(base | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| try_spawn(base))
}

#[cfg(not(windows))]
fn spawn_detached_self(restarted: bool) -> std::io::Result<()> {
    use std::process::Stdio;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if restarted {
        cmd.arg("--restarted");
    }
    cmd.spawn().map(|_| ())
}

fn init_logger() {
    // 读取配置（失败时用默认值，不阻断日志初始化）
    let cfg_debug = wind_config::Config::load(wind_config::Config::data_dir().as_deref())
        .map(|c| c.debug)
        .unwrap_or_default();

    // RUST_LOG 最优先，其次 debug.log_level，默认 info。
    // info 级别日志不得包含用户输入内容、词库词条等隐私数据。
    let level = std::env::var("RUST_LOG")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let l = &cfg_debug.log_level;
            if l.is_empty() { None } else { Some(l.clone()) }
        })
        .unwrap_or_else(|| "info".to_string());

    // 便携模式：<exe>/userdata/logs；正常模式：%LOCALAPPDATA%\WindInput[Dev]\logs。
    let log_dir = wind_config::Config::log_dir()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("logs")))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("logs"));
    let _ = std::fs::create_dir_all(&log_dir);

    // 滚动命名：wind_input.log → wind_input.1.log → … → wind_input.N.log
    // （序号在扩展名之前，滚动后仍是 .log，编辑器认得、按 *.log 也搜得到）
    let log_path = log_dir.join("wind_input.log");

    // 升级路径：把老方案写下的 wind_input.log.N 迁成新命名，否则新的扫描认不出它们，
    // 会永久滞留在目录里。须在 FileRotate::new 之前——构造时就会扫描既存序号。
    log_rotate::migrate_legacy_suffix(&log_path);

    let mut rotate = FileRotate::new(
        &log_path,
        log_rotate::AppendCountBeforeExt::new(cfg_debug.log_max_files),
        ContentLimit::Bytes((cfg_debug.log_max_size_mb * 1024 * 1024) as usize),
        Compression::None,
        None,
    );

    log_rotate::rotate_on_startup(&mut rotate, &log_path);

    // 顺带回收 TSF DLL 留下的过期日志。它按进程拆文件（每宿主 × pid 一个，落在
    // `logs/tsf_log/`），文件数会累积，而 DLL 自己在 loader lock 下没法做目录遍历。
    // 传的是 logs 根目录，函数会连老版本平铺在这一层的存量一并扫——见 prune_stale_tsf_logs。
    let pruned = log_rotate::prune_stale_tsf_logs(
        &log_dir,
        std::time::Duration::from_secs(60 * 60 * 24 * 7),
    );

    let (writer, _guard) = tracing_appender::non_blocking(rotate);
    // 丢弃计数器：worker 线程一旦出事，channel 断开后 lossy 模式会静默丢掉此后每一条
    // 日志，`wind_input.log` 就永久停在某一行而进程照常运行。守护线程据此留痕。
    let dropped = writer.error_counter();

    // 时间戳用本地时区，且格式与 wind_tsf 的 FileLogger 完全一致
    // （`GetLocalTime` → `%04d-%02d-%02d %02d:%02d:%02d.%03d`），
    // 两份日志才能直接按时间对齐排查。默认的 SystemTime timer 输出 UTC，
    // 与 TSF 日志差一个时区，务必不要退回默认值。
    // 注意：不能用 fmt::time::LocalTime —— 它依赖 time crate，
    // 而 time 在多线程进程中会拒绝获取本地时区偏移，导致时间戳静默变空。
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_timer(ChronoLocal::new(LOG_TIME_FORMAT.to_string()))
        .with_env_filter(EnvFilter::new(&level))
        .init();

    // Box::leak 保持 guard 存活至进程退出，确保缓冲日志全部落盘。
    Box::leak(Box::new(_guard));

    spawn_log_health_watch(dropped);

    info!(
        log_dir = %log_dir.display(),
        // 顺带报出清掉了几个 TSF 旧日志：这是唯一能看出「清理确实在跑」的地方，
        // 不打的话它坏掉了也没人知道（目录里文件变多是很久以后才察觉的事）。
        pruned_tsf_logs = pruned,
        level = %level,
        max_size_mb = cfg_debug.log_max_size_mb,
        max_files = cfg_debug.log_max_files,
        portable = wind_config::variant::is_portable(),
        // 时间戳是本地时间；记一次 UTC 偏移，让日志自描述所处时区
        tz_offset = %chrono::Local::now().format("%:z"),
        "logger initialized"
    );
}

/// 带必达落盘的致命退出。
///
/// `std::process::exit` 不运行析构，也不刷 `tracing_appender` 的 non_blocking 缓冲——
/// 退出原因能否落进主日志纯看 worker 线程有没有抢在进程消失前刷一次盘。这曾直接误导过
/// 排查：同一条退出路径，有的日志里三条俱全，有的只剩一行，让人误以为是两种不同的故障。
///
/// 故退出原因先同步写进启动轨迹（必达），再给 worker 一点时间尽力刷出主日志。
fn fatal_exit(stage: &str, msg: &str) -> ! {
    error!("{}", msg);
    startup_trace::stage(&format!("{stage}: {msg}"));
    eprintln!("WindInput: {msg}");
    // best-effort：让 non_blocking worker 有机会把上面那条 error! 写出去。
    std::thread::sleep(std::time::Duration::from_millis(100));
    std::process::exit(1);
}

/// 守护主日志的健康：丢弃计数一旦增长就写进启动轨迹。
///
/// 解决的是「观测工具自己成了故障的一部分」——`tracing_appender::non_blocking` 的
/// worker 线程出事后，`NonBlocking` 在 lossy 模式下会**静默丢弃**其后每一条日志。
/// 主日志因此停在某一行，而进程仍在正常跑，极易被误读成「进程卡在那一行」。
/// 轨迹文件不经 tracing，故此时仍写得出去。
///
/// 30 秒一次、且只在计数**变化**时落笔：正常运行零写入，不会撑大轨迹文件。
fn spawn_log_health_watch(dropped: tracing_appender::non_blocking::ErrorCounter) {
    std::thread::Builder::new()
        .name("log-health".into())
        .spawn(move || {
            let mut last = 0usize;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                let n = dropped.dropped_lines();
                if n != last {
                    startup_trace::stage(&format!(
                        "LOG-DROPPED lines={n} (主日志已丢日志，其后内容不可信)"
                    ));
                    last = n;
                }
            }
        })
        .ok();
}

/// 单例检查的三种结局。
///
/// **必须分开。** 此前 `CreateMutexW` 返回空句柄与「名字已存在」共用一个出口，
/// 都被翻译成「另一实例已在运行」——可空句柄的含义是*拿不到那个对象*，
/// 与*另一实例存在*根本是两回事。2026-07-23 客户日志里 10 个被挡实例有 8 个
/// 走的是空句柄分支，日志却异口同声说「另一实例已在运行」，
/// 于是"服务到底起没起来"这个最基本的问题反而查不出来。
/// 后两个分支只有 `cfg(windows)` 的 `check_singleton` 会构造（非 Windows 暂不做单例检查，
/// 恒返回 `Acquired`），但消费侧的 match 是跨平台的，不能删。
#[cfg_attr(not(windows), allow(dead_code))]
enum SingletonCheck {
    /// 拿到所有权，本实例可以继续启动。
    Acquired(SingletonGuard),
    /// 确认另一实例持有互斥体。
    AlreadyRunning,
    /// 连最小权限的 `OpenMutexW(SYNCHRONIZE)` 都打不开：本进程够不着这个全局对象
    /// （AppContainer / 低完整性 / 异账户上下文），**无从判断**另一实例是否存在。
    /// 携带 `CreateMutexW` 的 `GetLastError()`（5 = ERROR_ACCESS_DENIED）。
    Inaccessible(u32),
}

/// 启动上下文自检：核心服务被拉起在错误上下文时立即退出。
///
/// 对齐 Weasel `WeaselServer.cpp` 的 `GetUserName()=="SYSTEM"` 守卫，并扩到 AppContainer。
/// 与「单例改 `Local\` 命名空间」互补：命名空间隔离让错上下文实例挡不住正确实例，
/// 本自检再让它**当场自杀**而非常驻占着管道/供错配置。
///
/// 前提假设：核心永远应以交互用户身份运行（每用户登录自启 / 正常宿主拉起）。
/// 目前没有「以 SYSTEM 服务身份运行核心」的部署；若将来引入，需在此放行。
#[cfg(windows)]
fn guard_process_context() {
    if let Some(reason) = wrong_process_context() {
        startup_trace::stage(&format!(
            "context-WRONG {reason} 核心被拉起在非交互用户上下文，退出（避免用系统预置五笔占坑）"
        ));
        eprintln!("WindInput: 进程上下文异常（{reason}），本实例退出");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn guard_process_context() {}

/// 判定当前进程是否处于错误上下文；正常（交互用户）返回 `None`，否则返回原因串。
///
/// 判两类：① AppContainer 令牌（UWP 宿主里被拉起的子进程）；
/// ② TokenUser SID 属于 SYSTEM 家族（LocalSystem/LocalService/NetworkService）。
/// 任一命中即错上下文。任何令牌 API 失败也保守判为错上下文——连自己的令牌都读不了，
/// 说明上下文已异常到无从判断，宁可退出让正确实例接管。
#[cfg(windows)]
fn wrong_process_context() -> Option<&'static str> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        CreateWellKnownSid, EqualSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER,
        TokenIsAppContainer, TokenUser, WinLocalServiceSid, WinLocalSystemSid,
        WinNetworkServiceSid,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return Some("open-token-failed");
        }
        struct Guard(HANDLE);
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
        let _guard = Guard(token);

        // ① AppContainer 令牌？
        let mut is_ac: u32 = 0;
        let mut ret: u32 = 0;
        if GetTokenInformation(
            token,
            TokenIsAppContainer,
            Some(&mut is_ac as *mut u32 as *mut _),
            std::mem::size_of::<u32>() as u32,
            &mut ret,
        )
        .is_ok()
            && is_ac != 0
        {
            return Some("appcontainer");
        }

        // ② TokenUser SID ∈ SYSTEM 家族？先探长度再取。
        let mut len: u32 = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        if len == 0 {
            return Some("token-user-len-0");
        }
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut _),
            len,
            &mut len,
        )
        .is_err()
        {
            return Some("token-user-failed");
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid = tu.User.Sid;

        for kind in [WinLocalSystemSid, WinLocalServiceSid, WinNetworkServiceSid] {
            let mut wk = vec![0u8; 68]; // SECURITY_MAX_SID_SIZE
            let mut wk_len = wk.len() as u32;
            if CreateWellKnownSid(
                kind,
                PSID::default(),
                PSID(wk.as_mut_ptr() as *mut _),
                &mut wk_len,
            )
            .is_ok()
                && EqualSid(sid, PSID(wk.as_ptr() as *mut _)).is_ok()
            {
                return Some("system-account");
            }
        }
    }
    None
}

/// 单例检查：通过 Windows Named Mutex 确保**每个登录会话**只有一个实例运行
///
/// 命名空间从 `Global\` 改为 **`Local\`**（每会话隔离），这是多用户/开机竞态的要害：
/// - `Local\` 由 Windows 按登录会话分隔，错会话（如 SYSTEM 的 session 0）里抢先建的
///   单例落在别的命名空间，**挡不住**用户会话里的正确实例——旧 `Global\` 会被它长期占位。
/// - `Global\` 命名空间的创建需要 `SeCreateGlobalPrivilege`，**普通用户没有**；
///   `Local\` 谁都能建，多用户设备上才不会因权限而失败。
/// - 隔离靠**命名空间**而非放宽 ACL：因此这里回到令牌默认 DACL（不再挂共享安全描述符），
///   与 Weasel 的 `CreateMutex(NULL, FALSE, <per-session name>)` 同构。跨上下文冒占问题
///   由「命名空间隔离 + 启动上下文自检（guard_process_context）」两道解决，不靠宽 ACL。
///
/// 注：管道名仍是机器级、暂未 per-user（见 guard_process_context 附近说明），
/// 那属于另一条针对「多会话跨用户串扰」的后续，需真机 AppContainer 回归。
#[cfg(windows)]
fn check_singleton() -> SingletonCheck {
    // 直接调用 kernel32 的 CreateMutexW，绕过 windows crate 的 Result 包装。
    // Go 版 CreateMutex 在 ERROR_ALREADY_EXISTS 时仍返回有效 handle，
    // 但 windows crate 的 .ok() 会把它转为 Err 丢弃 handle。
    unsafe extern "system" {
        fn CreateMutexW(
            lpMutexAttributes: *const std::ffi::c_void,
            bInitialOwner: i32,
            lpName: *const u16,
        ) -> windows::Win32::Foundation::HANDLE;
    }

    let mutex_name = format!(
        "Local\\WindInputIMEService{}",
        wind_config::variant::pipe_suffix()
    );
    let wide_name: Vec<u16> = mutex_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // 令牌默认 DACL（不挂共享安全描述符）：单例互斥体只由核心进程触碰，
    // 无需对 AppContainer/异账户开放，隔离已由 `Local\` 命名空间承担。
    // 曾经为了让全局对象跨上下文可达而放宽 ACL，恰恰是「错上下文也能占坑」的成因，
    // 改 per-session 命名后不再需要，故回到 NULL 安全属性。
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide_name.as_ptr()) };

    if handle.is_invalid() {
        // GetLastError 必须紧贴失败的调用读，中间不能夹任何 Win32 调用。
        let create_err = unsafe { windows::Win32::Foundation::GetLastError() }.0;
        // 空句柄本身不区分「另一实例存在但本进程够不着」和「全局命名空间不可达」。
        // 退到最小权限 SYNCHRONIZE 再探一次：能打开就说明那个互斥体确实存在。
        // 同会话的普通进程即便被默认 DACL 拒了 MUTEX_ALL_ACCESS，登录会话那条 ACE
        // 通常仍给 SYNCHRONIZE，所以这一步真能把两种情况分开。
        //
        // 本函数早于 init_logger 运行，tracing 尚无 subscriber，只能走启动轨迹。
        if open_existing_mutex(&wide_name) {
            startup_trace::stage(&format!(
                "singleton-CreateMutexW-DENIED err={create_err}（互斥体确实存在，本进程权限不足以参与竞争）"
            ));
            return SingletonCheck::AlreadyRunning;
        }
        return SingletonCheck::Inaccessible(create_err);
    }

    // 检查 ERROR_ALREADY_EXISTS（GetLastError = 183）
    let last_err = unsafe { windows::Win32::Foundation::GetLastError() };

    if last_err == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
        // 另一个实例已在运行，释放 handle
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        return SingletonCheck::AlreadyRunning;
    }

    // 等待获取 mutex 所有权（立即返回）
    let wait_result = unsafe { windows::Win32::System::Threading::WaitForSingleObject(handle, 0) };

    if wait_result == windows::Win32::Foundation::WAIT_OBJECT_0
        || wait_result == windows::Win32::Foundation::WAIT_ABANDONED
    {
        SingletonCheck::Acquired(SingletonGuard { _handle: handle })
    } else {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        SingletonCheck::AlreadyRunning
    }
}

/// 以最小权限 `SYNCHRONIZE` 探测同名互斥体是否存在。仅用于把 `CreateMutexW`
/// 的空句柄拆成「存在但够不着」与「够不着全局命名空间」两种结局，不持有句柄。
#[cfg(windows)]
fn open_existing_mutex(wide_name: &[u16]) -> bool {
    // 与上面的 CreateMutexW 同理直接声明：避开 windows crate 的 Result 包装，
    // 也避开为一个探测函数额外开 feature。
    unsafe extern "system" {
        fn OpenMutexW(
            dwDesiredAccess: u32,
            bInheritHandle: i32,
            lpName: *const u16,
        ) -> windows::Win32::Foundation::HANDLE;
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;

    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, wide_name.as_ptr()) };
    if handle.is_invalid() {
        return false;
    }
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(handle);
    }
    true
}

#[cfg(not(windows))]
fn check_singleton() -> SingletonCheck {
    // 非 Windows 平台：暂不实现单例检查
    SingletonCheck::Acquired(SingletonGuard {})
}

/// 单例守卫：析构时释放 Mutex
#[cfg(windows)]
struct SingletonGuard {
    _handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for SingletonGuard {
    fn drop(&mut self) {
        // Mutex 在 handle 关闭时自动释放
    }
}

#[cfg(not(windows))]
struct SingletonGuard {}

/// 非 Windows 没有 Named Mutex，guard 是空壳；这里仍显式实现 `Drop`，是为了让调用点的
/// `drop(_singleton_guard)`（重启前先释放单例，让新实例能拿到所有权）在两个平台上表达
/// 同一件事——否则那两行在非 Windows 下会被判成「drop 一个没有 Drop 的值」。
#[cfg(not(windows))]
impl Drop for SingletonGuard {
    fn drop(&mut self) {}
}

/// 设置进程 DPI 感知（与 Go 版 setDPIAwareness 对齐）
///
/// 使用 SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE) 设置 Per-Monitor DPI 感知。
/// 必须在任何窗口创建之前调用，否则坐标会被 Windows DPI 虚拟化。
#[cfg(windows)]
fn set_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{PROCESS_PER_MONITOR_DPI_AWARE, SetProcessDpiAwareness};

    let result = unsafe { SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE) };
    if result.is_ok() {
        tracing::info!("DPI awareness set to Per-Monitor DPI Aware");
    } else {
        tracing::warn!("Failed to set DPI awareness: {:?}", result);
    }
}

#[cfg(not(windows))]
fn set_dpi_awareness() {
    // 非 Windows 平台无需设置
}
