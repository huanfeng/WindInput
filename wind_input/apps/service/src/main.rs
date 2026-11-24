//! wind_input: 输入法主服务进程
//!
//! 最小可输入服务：启动 Named Pipe 服务器，接收 TSF DLL 按键事件并响应。
//! 与 Go 版本 `wind_input/cmd/service/main.go` 的启动序列对齐。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::ChronoLocal;

use wind_bridge::deferred::DeferredHandler;
use wind_bridge::push::{PushConfig, PushServer};
use wind_bridge::server::{BridgeConfig, BridgeServer};
use wind_webdata::WebDataRpc;

// 启动轨迹下沉在 wind-config，好让 UI 线程等下层也能打点。时间戳格式一并取自那里，
// 与 wind_tsf 的 `CFileLogger::_FormatTimestamp` 共用一处定义，三份日志才能归并排序。
use wind_config::startup_trace::{self, LOG_TIME_FORMAT};

mod backup_cli;
mod cli_util;
mod config_cli;
mod dict_cli;
mod extensions;
mod ime_cli;
mod log_rotate;
mod phrase_cli;
mod restart_cli;
mod schema_cli;
mod sync;
mod theme_cli;
mod ui_cli;
// 仅 Windows：它的动作全是写 HKLM 的 TSF 登记项（`wind_coordinator::tsf_profile_name`
// 本身也在 `#[cfg(windows)]` 之下）。少了这道门控，darwin 构建会在 use 处就 E0432。
#[cfg(windows)]
mod system_cli;

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
         theme     主题列表 / 主题包 .wtheme 预览、导入与导出（需 core 在线）\n  \
         dict      用户词库按方案导入导出（需 core 在线）\n  \
         phrase    用户短语导入导出 / 系统短语恢复（需 core 在线）\n  \
         backup    整机备份创建/查看/还原（需 core 在线）\n  \
         restart   重启输入法服务（未运行则直接启动）\n  \
         ui        桌面提示：ui toast <文案>（需 core 在线）\n  \
         ime       输入状态：中英/全半角/简繁/工具栏的查看与切换（需 core 在线）\n  \
         system    系统集成：Dota 2 兼容等（写 HKLM，需管理员权限）\n  \
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
    // `system` 单拎出来用 `cfg!` 判断而不是并进 `matches!` 里：宏参数里的模式加不了
    // `#[cfg]`，而下面的 match 又必须把该分支 cfg 掉（模块本身仅 Windows 存在）。
    // 两处判据必须同真同假，否则非 Windows 上 `wind_input system` 会穿到 `unreachable!()`。
    if matches!(
        sub,
        Some(
            "config"
                | "schema"
                | "theme"
                | "dict"
                | "phrase"
                | "backup"
                | "restart"
                | "ui"
                | "ime"
                | "help"
                | "--help"
                | "-h"
                | "--version"
                | "-V"
        )
    ) || (cfg!(windows) && sub == Some("system"))
    {
        // GUI 子系统下附着父控制台，让输出回到调用的终端。被 `wind.cli` 拉起时必须跳过：
        // 输出该走服务接好的管道（读回来变成 toast），attach 到终端会让管道读空。
        #[cfg(windows)]
        if std::env::var_os(wind_coordinator::handle_cmdbar::WIND_CLI_PIPED_ENV).is_none() {
            attach_parent_console();
        }
        let code = match sub {
            Some("config") => config_cli::run(&cli_args[2..]),
            Some("schema") => schema_cli::run(&cli_args[2..]),
            Some("theme") => theme_cli::run(&cli_args[2..]),
            Some("dict") => dict_cli::run(&cli_args[2..]),
            Some("phrase") => phrase_cli::run(&cli_args[2..]),
            Some("backup") => backup_cli::run(&cli_args[2..]),
            Some("restart") => restart_cli::run(&cli_args[2..]),
            Some("ui") => ui_cli::run(&cli_args[2..]),
            Some("ime") => ime_cli::run(&cli_args[2..]),
            #[cfg(windows)]
            Some("system") => system_cli::run(&cli_args[2..]),
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

    // 拼错的子命令必须当场说"不认识"，不能穿到服务启动路径（会被单例挡掉，报成实例冲突）。
    // 判据只认不以 `-` 开头的首个参数：真正的启动路径要么无参，要么只带 `--restarted`。
    if let Some(first) = sub
        && !first.starts_with('-')
    {
        #[cfg(windows)]
        if std::env::var_os(wind_coordinator::handle_cmdbar::WIND_CLI_PIPED_ENV).is_none() {
            attach_parent_console();
        }
        eprintln!("未知子命令: {first}");
        print_root_usage();
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(2);
    }

    // 0. 设置 DPI 感知（与 Go 版 setDPIAwareness 对齐）
    // 必须在任何窗口创建之前调用，否则坐标会被 Windows DPI 虚拟化
    set_dpi_awareness();

    // 启动轨迹的第一个探针必须早于 init_logger：主日志失效时，
    // 「究竟有没有走到日志初始化」本身就是要回答的问题。
    startup_trace::stage("begin");

    // 0.5 启动上下文自检：被错误地拉起在 SYSTEM/服务账户或 AppContainer 里（TSF DLL 拉核心时
    // 不换令牌）就当场退出，不带着系统预置去占坑。早于单例与日志：错上下文未必写得了日志目录。
    guard_process_context();

    // 1. 单例检查（与 Go 版 checkSingleton 对齐）
    //
    // 必须早于 init_logger：后者会滚动日志，注定要退出的重复实例不该把运行实例的故障现场
    // 日志顶走。代价是此处只能用启动轨迹 + stderr（`exit(1)` 本也刷不出 tracing 的缓冲）。
    let _singleton_guard = match check_singleton() {
        SingletonCheck::Acquired(guard) => guard,
        SingletonCheck::AlreadyRunning => {
            startup_trace::stage("singleton-BLOCKED 另一实例已在运行，本实例退出");
            eprintln!("WindInput: 另一个实例已在运行中");
            std::process::exit(1);
        }
        // 够不着单例对象：多半继承了受限的宿主上下文，撑不起服务，退出是对的；但不能声称
        // 「另一实例已在运行」，那会把服务没起来伪装成已在跑。有 guard_process_context 在前，
        // 此分支近乎兜底。
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
    // 配置在此读一次，日志级别与下面的词库缓存策略共用，不再各读各的。
    let startup_cfg = wind_config::Config::load(wind_config::Config::data_dir().as_deref()).ok();
    init_logger(
        startup_cfg
            .as_ref()
            .map(|c| c.debug.clone())
            .unwrap_or_default(),
    );
    startup_trace::stage("logger-ready");

    // 词库缓存策略（`schema.dict_cache_policy`）须在加载任何词库之前设定；运行期改键不生效
    // （设置页提示需重启）。读不到配置时取出厂值。
    let policy_name = startup_cfg.as_ref().map_or_else(
        || wind_config::Config::default().schema.dict_cache_policy,
        |c| c.schema.dict_cache_policy.clone(),
    );
    drop(startup_cfg);
    let cache_policy = wind_dict::mapping::CachePolicy::from_config(&policy_name);
    wind_dict::mapping::set_policy(cache_policy);
    // 预算：锁定与预加载都占物理内存，按机器内存封顶（算法见 `default_budget`），超出的文件
    // 按普通映射使用，避免超大词库 / 大语法模型把内存吃光。
    let cache_budget = wind_dict::mapping::default_budget();
    wind_dict::mapping::set_budget(cache_budget);
    info!(
        "词库缓存策略：{}（schema.dict_cache_policy），预算 {} MB",
        cache_policy.as_config(),
        cache_budget / (1024 * 1024)
    );

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

    // 定制版身份摘要（非定制版不打）：放在版本行之后，报障日志开头就能看出装的是哪个定制版
    // （定制层能让内置方案整个消失）。文案与 `system.info` 的 `customEdition` 同源；只打元信息，
    // 不打定制层路径。面向用户的定制版 INFO 只有这一处，`load_custom_manifest` 那行刻意是 DEBUG。
    if let Some(summary) = wind_rpc::custom_edition::startup_summary() {
        info!("{summary}");
    }

    // 2.5 等待用户配置目录就绪。
    //
    // 开机自启早期漫游 known folder（%APPDATA%）未必已挂载，而日志目录在 %LOCALAPPDATA%，
    // 不等的话配置会静默退化为系统预置。必须在 init_logger() 之后：探测过程要留日志。
    wind_config::Config::wait_user_config_ready(std::time::Duration::from_secs(10));
    startup_trace::stage("config-ready");

    // 本用户确有 config.toml 时落一个本地标记（幂等），下次开机 `probe_user_config` 据此区分
    // 「默认用户（不等）」与「定制用户但漫游未挂载（要等）」。只在服务启动路径写，不进 load()。
    wind_config::Config::mark_user_config_seen_if_present();

    // 清理用户层里的陈旧键（幂等）：与出厂默认相同的冗余键（否则日后默认改了用户仍停在旧值）
    // 与 `RETIRED_KEYS` 退役键。须在 wait_user_config_ready 之后。
    if let Err(e) = wind_config::Config::prune_user_config() {
        warn!("Prune user config failed: {e}");
    }

    // 把引导键绑定物化进用户层 `keys.key_actions`（一次性，靠版本号幂等），此后它是唯一真相源，
    // 用户删掉的出厂绑定不会被折算灌回。排在 prune 之后：物化是终态写入。
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
    // 语言栏图标：状态推送只在变化时发生，这里补一次初始发布，否则首次切换前 DLL 读不到
    // 共享内存（没有标点角标）。非 Windows 桌面形态下是空操作。
    coordinator.publish_initial_langbar_icon();
    // 注入 host-render 管理器（与 BridgeServer 共享同一实例），供后续写帧/隐藏使用。
    #[cfg(windows)]
    coordinator.set_host_render(host_render.clone());
    #[cfg(windows)]
    let coord_for_push_hook = coordinator.clone();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let coord_for_reconnect_hook = coordinator.clone();
    let coord_for_web = coordinator.clone();
    let coord_for_restart_toast = coordinator.clone();
    let coord_for_exit = coordinator.clone();
    coordinator.install_exit_flush();
    // 扩展程序：按 `[extensions]` 配置拉起扩展宿主，配置热重载时增量调整（注册即按当前值生效）。
    let extensions = extensions::Extensions::start(
        coordinator.hook_tap(),
        {
            let c = coordinator.clone();
            Box::new(move || c.hook_changed())
        },
        {
            let c = coordinator.clone();
            Box::new(move || c.hook_presence())
        },
    );
    let extensions_for_rpc = extensions.clone();
    let extensions_for_exit = extensions.clone();
    // 数据同步：设置启用且同步进程已安装时拉起 `wind_input_sync`（见 sync.rs）。
    let sync_host = sync::SyncHost::start();
    let sync_for_exit = sync_host.clone();
    coordinator.set_extensions_config_hook(Box::new(move |c| extensions.apply_config(c)));
    deferred.set_ready(coordinator);

    // 三步顺序不能动（见 `PushServer::mark_ready`）：set_ready → mark_ready（给就绪前连上的
    // DLL 补发 SERVICE_READY）→ hook 注册（补跑推的握手配置必须排在 SERVICE_READY 之后）。
    push_server.mark_ready();
    // push 客户端注册回调：host-render 白名单受限宿主（SearchHost 等 transient DocMgr）
    // 服务重启重连时不发任何激活事件，由此回调补推 activation 握手使 DLL 重新 setup。
    #[cfg(windows)]
    push_server.set_client_connected_hook(Box::new(move |token| {
        coord_for_push_hook.on_push_client_connected(token);
    }));
    // 外部宿主：push 连接（重）建立后渲染端复位宿主状态（Linux 补推可见的工具栏 / 软键盘，addon
    // 收到 SERVICE_READY 会丢掉手里的面板；macOS 作废皮肤换帧的分层去重）。
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    coord_for_reconnect_hook.install_host_reconnect_hook();

    // 9.5 启动本地控制 / 配置 JSON-RPC 服务（命名管道：..._ctrl + ..._events）。
    // 本地授权靠 OS ACL（SDDL），不再需要 token/Origin/CORS/端口发现。
    // 同步线程模型（与 bridge/push 一致），不引入 tokio 到控制路径。
    // 主题预览缓存键带上构建标识：换一次构建旧图就不再命中。dirty 构建的短哈希不随改动变，
    // 再带上构建时刻。
    let git = env!("WIND_GIT_HASH");
    wind_rpc::set_build_id(git);
    wind_webdata::set_preview_build_id(if git.ends_with("-dirty") {
        format!("{git}@{}", env!("WIND_BUILD_TIME"))
    } else {
        git.to_string()
    });
    let core_rpc: Arc<dyn wind_rpc::CoreRpc> = Arc::new(RpcCore {
        coord: coord_for_web,
        extensions: extensions_for_rpc,
        sync: sync_host.clone(),
    });
    match wind_rpc::RpcServer::new(core_rpc.clone(), variant, pipe_suffix) {
        Ok(rpc_server) => {
            // 活跃方案变更 → RPC 事件广播。EngineManager 位于 wind-rpc 之下、无法持有 EventSink，
            // 这里是唯一同时看得见两者的地方。
            let sink = rpc_server.event_sink();
            // 同步进程的状态广播与受限请求：请求走与设置端同一个分发器，
            // 这样 config.changed / compat.changed 等事件照常发出。
            match wind_rpc::DispatchState::with_events(core_rpc, variant, sink.clone()) {
                Ok(state) => sync_host.set_hooks(
                    Box::new({
                        let sink = sink.clone();
                        move |v| sink.broadcast("sync.statusChanged", v)
                    }),
                    Box::new(move |method, params| {
                        let resp = wind_rpc::dispatch(
                            &state,
                            wind_ipc::rpc::Request {
                                version: 1,
                                id: 0,
                                method: method.to_string(),
                                params: params.clone(),
                            },
                        );
                        match resp.error {
                            Some(e) => Err(e),
                            None => Ok(resp.result.unwrap_or(serde_json::Value::Null)),
                        }
                    }),
                ),
                Err(e) => error!("同步进程的请求分发器初始化失败: {e}"),
            }
            // 右键菜单改兼容规则不经 RPC：写成功后在此广播，设置端的「应用兼容」页据此刷新。
            // 经 `compat.*` RPC 的写入由 dispatch 自己广播，钩子只覆盖菜单路径，不会重复。
            wind_config::change_hook::set_compat_change_hook(std::sync::Arc::new({
                let sink = sink.clone();
                move |source: &str, _user_dir: &std::path::Path| {
                    sink.emit_compat_changed(serde_json::json!({ "method": source }));
                }
            }));
            wind_engine::active_hook::set_active_schema_hook(std::sync::Arc::new({
                let sink = sink.clone();
                move |id: &str| {
                    sink.broadcast("schema.activeChanged", serde_json::json!({ "id": id }));
                }
            }));
            // 配置落盘 → 广播（带 key 与新值）：托盘、快捷键、命令栏改配置都不经 RPC，设置端靠它
            // 刷新。经 RPC setItems 的写入也会回声到这里，订阅方更新是幂等的。
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

    // 配置文件语法不合法 ⇒ 弹一次提示（带文件路径与行号）。排在 service-ready 之后：toast 由
    // 本进程窗口渲染，之前窗口线程未必起得来。写盘一侧由 `ConfigDegradation::unparsable` 防护，
    // 这里负责让用户知道。
    coord_for_restart_toast.notify_config_syntax_error();

    // 当前方案有段被降级（值不在取值范围内，静默回落出厂值）⇒ 同样提示一次。
    {
        let active = coord_for_restart_toast.active_schema_id();
        coord_for_restart_toast.notify_schema_degradation(&active);
    }

    // 10. 阻塞主线程，直到菜单触发"重启服务"
    //
    // macOS 例外：主线程改跑 CFRunLoop（Carbon 全局热键事件只投递到主线程），重启等待挪到
    // 辅助线程，收到信号后停 run loop、主线程回到这里走原重启路径。
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

        // 软键盘面板的主线程宿主，必须在进事件循环之前装：AppKit 只能在主线程碰，而下发命令的
        // forwarder 是工作线程。装配前已发的命令由这一步顺带 drain。
        wind_ui::softkeyboard_host_macos::install_on_main();

        wind_ui::global_hotkey_macos::run_main_loop();

        if RESTART.load(Ordering::SeqCst) {
            info!("Restart requested, relaunching service...");
            extensions_for_exit.stop();
            sync_for_exit.stop();
            coord_for_exit.flush_user_data_before_exit();
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
            extensions_for_exit.stop();
            sync_for_exit.stop();
            coord_for_exit.flush_user_data_before_exit();
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
/// 扩展子系统在服务进程里、不在协调器里，`extensions.*` 在这里截下。
struct RpcCore {
    coord: Arc<wind_coordinator::Coordinator>,
    extensions: Arc<extensions::Extensions>,
    sync: Arc<sync::SyncHost>,
}

impl wind_rpc::CoreRpc for RpcCore {
    fn is_chinese_mode(&self) -> bool {
        self.coord.is_chinese_mode()
    }
    fn active_schema_id(&self) -> String {
        self.coord.active_schema_id()
    }
    fn apply_config(&self) -> bool {
        self.coord.reload_user_config()
    }
    fn data_rpc(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        match method {
            "extensions.inventory" => Ok(self.extensions.inventory()),
            m if m.starts_with("sync.") => self.sync.rpc(m, params),
            _ => self.coord.web_data_rpc(method, params),
        }
    }
    fn fonts(&self) -> Vec<(String, String)> {
        self.coord.list_font_families()
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

/// 以脱离父进程的方式拉起自身，服务重启（[`relaunch_self`]）与 CLI `restart` 共用。不脱离时
/// 子进程会继承父控制台（关终端连带杀掉服务）与 kill-on-job-close 作业对象（父进程退出即连带
/// 杀子进程）。stdio 一律接 null。
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

fn init_logger(cfg_debug: wind_config::config::DebugConfig) {
    // RUST_LOG 最优先，其次 debug.log_level，默认 info。
    // info 级别日志不得包含用户输入内容、词库词条等隐私数据。
    //
    // 优先级链由 `startup_trace::effective_log_level_from` 独家实现，启动轨迹的关闭门控读同一个
    // 函数，两份日志才能互相印证。用 `_from` 变体是因为配置在 main 里刚 load 过。
    let level = startup_trace::effective_log_level_from(&cfg_debug.log_level);

    // 关闭档（level = off）：一个文件都不建（空文件也是痕迹），光靠 `EnvFilter("off")` 不够，
    // `SelfHealingLog::new` 会建出文件。仍装一个 no-op subscriber，免得别处再 `init()` 时 panic。
    if level.eq_ignore_ascii_case("off") {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .with_env_filter(EnvFilter::new("off"))
            .try_init();
        return;
    }

    // 便携模式：<exe>/localdata/logs；正常模式：%LOCALAPPDATA%\WindInput[Dev]\logs。
    let log_dir = wind_config::Config::log_dir()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("logs")))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("logs"));
    let _ = std::fs::create_dir_all(&log_dir);

    // 滚动命名：wind_input.1.log（本次运行）→ .2.log → … → .N.log，见 log_rotate 模块文档。
    let log_path = log_dir.join(format!("wind_input.{}.log", log_rotate::CURRENT_INDEX));

    // 把前两代命名迁成现在这套。须在 SelfHealingLog::new 之前：构造时就会扫描既存序号。
    log_rotate::migrate_legacy_suffix(&log_path);

    // 自愈写端：日志被设置页「清理」或用户手动删掉后自己接回来，见 SelfHealingLog。
    let rotate = log_rotate::SelfHealingLog::new(
        &log_path,
        cfg_debug.log_max_files,
        cfg_debug.log_max_size_mb * 1024 * 1024,
    );

    // 顺带回收 TSF DLL 留下的过期日志，见 prune_stale_tsf_logs。
    let pruned = log_rotate::prune_stale_tsf_logs(
        &log_dir,
        std::time::Duration::from_secs(60 * 60 * 24 * 7),
    );

    let (writer, _guard) = tracing_appender::non_blocking(rotate);
    // 丢弃计数器：worker 线程一旦出事，channel 断开后 lossy 模式会静默丢掉此后每一条
    // 日志，`wind_input.1.log` 就永久停在某一行而进程照常运行。守护线程据此留痕。
    let dropped = writer.error_counter();

    // 时间戳用本地时区、格式与 wind_tsf 的 FileLogger 一致，两份日志才能按时间对齐（默认 timer
    // 输出 UTC）。不能用 fmt::time::LocalTime：time crate 在多线程进程里拒绝取本地时区，时间戳会变空。
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
        // 唯一能看出 TSF 日志清理确实在跑的地方。
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
/// `std::process::exit` 不刷 `tracing_appender` 的 non_blocking 缓冲，故退出原因先同步写进
/// 启动轨迹（必达），再给 worker 一点时间尽力刷出主日志。
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
/// non_blocking 的 worker 线程出事后 lossy 模式会静默丢弃其后每一条日志，主日志停在某一行
/// 而进程照常运行，极易误读成「卡在那一行」。轨迹文件不经 tracing，此时仍写得出去。
/// 30 秒一次、只在计数变化时落笔。
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
/// 必须分开：`CreateMutexW` 返回空句柄是「拿不到那个对象」，与「另一实例存在」是两回事，
/// 混在一起会让「服务到底起没起来」查不出来。`Inaccessible` 只有 Windows 构造，
/// 但消费侧的 match 是跨平台的。
#[cfg_attr(not(windows), allow(dead_code))]
enum SingletonCheck {
    /// 拿到所有权，本实例可以继续启动。
    Acquired(SingletonGuard),
    /// 确认另一实例持有互斥体。
    AlreadyRunning,
    /// 连最小权限的 `OpenMutexW(SYNCHRONIZE)` 都打不开：本进程够不着这个对象
    /// （AppContainer / 低完整性 / 异账户上下文），无从判断另一实例是否存在。
    /// 携带 `CreateMutexW` 的 `GetLastError()`（5 = ERROR_ACCESS_DENIED）。
    Inaccessible(u32),
}

/// 启动上下文自检：核心服务被拉起在错误上下文时立即退出。
///
/// 对齐 Weasel `WeaselServer.cpp` 的 `GetUserName()=="SYSTEM"` 守卫，并扩到 AppContainer。
/// 与单例的 `Local\` 命名空间互补：后者让错上下文实例挡不住正确实例，本自检让它当场退出。
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
/// 用 `Local\` 命名空间（每会话隔离）：错会话（如 SYSTEM 的 session 0）里抢先建的单例挡不住
/// 用户会话里的正确实例；`Global\` 还需要普通用户没有的 `SeCreateGlobalPrivilege`。
/// 隔离靠命名空间而非放宽 ACL，故用令牌默认 DACL，与 Weasel 同构。
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

    // 令牌默认 DACL：单例互斥体只由核心进程触碰。别放宽 ACL，那会让错上下文也能占坑。
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide_name.as_ptr()) };

    if handle.is_invalid() {
        // GetLastError 必须紧贴失败的调用读，中间不能夹任何 Win32 调用。
        let create_err = unsafe { windows::Win32::Foundation::GetLastError() }.0;
        // 空句柄不区分「另一实例存在但够不着」与「命名空间不可达」，退到最小权限 SYNCHRONIZE
        // 再探一次（登录会话 ACE 通常仍给它）。本函数早于 init_logger，只能走启动轨迹。
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

/// macOS 由 launchd 保证单实例，这里不重复检查。
#[cfg(all(not(windows), not(target_os = "linux")))]
fn check_singleton() -> SingletonCheck {
    SingletonCheck::Acquired(SingletonGuard { _lock: None })
}

/// Linux：运行时目录下的 `wind_input.lock` 上持 `flock`（进程退出内核自动释放，崩溃也不留脏锁）。
///
/// 必须有：addon 会在连不上服务时自动拉起它，而服务首次启动要建词库缓存（十几秒）才绑 socket，
/// 期间 addon 可能再拉一次；没有单例的话第二个实例会 `remove_file` 掉第一个的 socket 再自己绑，
/// 把第一个实例架空。
#[cfg(target_os = "linux")]
fn check_singleton() -> SingletonCheck {
    use std::os::fd::AsRawFd;
    // 运行时目录先建好并校验：/tmp 兜底（无 XDG_RUNTIME_DIR）必须是本用户私有的，否则别的用户
    // 抢先建好它、在里面监听 socket，就能收走 addon 发来的全部按键。不合格就拒绝启动。
    let dir = match wind_bridge::endpoint::ensure_runtime_dir(wind_config::variant::pipe_suffix()) {
        Ok(dir) => dir,
        Err(e) => {
            startup_trace::stage(&format!("runtime-dir-UNSAFE {e}，拒绝启动"));
            eprintln!("WindInput: {e}，拒绝启动");
            std::process::exit(1);
        }
    };
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("wind_input.lock"))
    {
        Ok(f) => f,
        // 建不出锁文件（目录只读等）：宁可放行也不要因此起不来；后果仅是失去单例保护。
        Err(_) => return SingletonCheck::Acquired(SingletonGuard { _lock: None }),
    };
    // SAFETY: fd 在 `file` 存活期内有效；flock 只读该 fd。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return SingletonCheck::Acquired(SingletonGuard { _lock: Some(file) });
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK) {
        SingletonCheck::AlreadyRunning
    } else {
        SingletonCheck::Acquired(SingletonGuard { _lock: None })
    }
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
struct SingletonGuard {
    /// 持有即持锁（Linux）；仅靠 Drop 关闭 fd 来释放，故字段本身不被读取。
    _lock: Option<std::fs::File>,
}

/// 显式实现 `Drop`，让调用点的 `drop(_singleton_guard)`（重启前先释放单例）在各平台表达同一件事
/// （Linux 上释放的是 flock，macOS 上是空壳），否则会被判成「drop 一个没有 Drop 的值」。
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
