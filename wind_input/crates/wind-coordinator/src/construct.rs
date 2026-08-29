//! Coordinator 构造器族：生产构造 `new`（desktop-ui）与 headless 家族。
//!
//! 共同核心 `build`（83 字段的结构体字面量装配点）**留在 coordinator.rs**——
//! 23 个私有字段的可见性以 coordinator 模块为界，装配点不外迁。
//! （自 coordinator.rs 平移，纯搬运。）

use std::path::Path;
use std::sync::Arc;
// debug! 只在 desktop-ui 门内的 `new` 用到；headless 形态下无引用。
#[cfg(feature = "desktop-ui")]
use tracing::debug;
use tracing::{info, warn};

use wind_bridge::push::{PushConfig, PushServer};
use wind_config::Config;
use wind_store::Store;
use wind_ui_types::UiCommand;
// UiEvent 仅 macOS forwarder 路径显式命名（其余平台由 UiManager 的通道类型推断）。
#[cfg(all(feature = "desktop-ui", target_os = "macos"))]
use wind_ui_types::UiEvent;
// UiManager 仅 Windows LayeredWindow 路径用；macOS 走 host-render forwarder。
#[cfg(all(feature = "desktop-ui", not(target_os = "macos")))]
use wind_ui::manager::UiManager;

use crate::coordinator::Coordinator;

impl Coordinator {
    /// 生产构造器：从 exe 同目录加载配置，启动候选窗口 UI 线程。
    /// 桌面专属（desktop-ui）：headless/Android 走 `new_headless_with_ui`。
    #[cfg(feature = "desktop-ui")]
    pub fn new(push_server: Arc<PushServer>) -> Arc<Self> {
        let data_dir = Config::data_dir();
        let config = Config::load(data_dir.as_deref()).unwrap_or_default();
        info!("Active schema: {}", config.active_schema());

        // UI 管理器（候选窗口线程）。
        // macOS 无进程内窗口：把 UiCommand 喂给 host-render forwarder，光栅化进 POSIX SHM
        // 再经 push 管道推帧给 .app。其余平台走 Windows LayeredWindow 的 UiManager。
        #[cfg(target_os = "macos")]
        let (ui_tx, event_rx) = {
            let (tx, rx) = std::sync::mpsc::channel::<UiCommand>();
            // 候选/菜单的**鼠标**交互确实经 push/bridge 协议从 .app 回流，不走这里；
            // 但进程内仍有 UiEvent 源——全局热键由服务进程自己注册（语义要求本输入法
            // 未激活时也生效，.app 只在被 IMK 拉起后才在），触发后经本通道回协调器。
            // 后续拖动落点回报（CandidateWindowMoved / StatusTipMoved）等也走这条。
            let (ev_tx, ev_rx) = std::sync::mpsc::channel::<UiEvent>();
            let sink: Arc<dyn wind_bridge::HostRenderSink> = push_server.clone();
            let suffix = push_server.suffix().to_string();
            if let Err(e) = std::thread::Builder::new()
                .name("ui-forwarder-macos".into())
                .spawn(move || wind_ui::manager_macos::forwarder_thread(rx, ev_tx, sink, suffix))
            {
                warn!("Failed to spawn macOS host-render forwarder: {}", e);
            }
            // forwarder 线程阻塞在 `recv()` 上，命令到达本身即唤醒它，无需额外的唤醒通路。
            (crate::UiSender::without_wake(tx), Some(ev_rx))
        };
        #[cfg(not(target_os = "macos"))]
        let (ui_tx, event_rx) = match UiManager::new() {
            Ok(mut ui) => {
                // UI 线程是事件驱动的（睡到有事发生），投递命令后必须唤醒它，否则那条命令
                // 要等下一个计时器到期才被看见。`UiSender` 把两步绑成一次 send。
                //
                // waker 先于 `mem::forget` 取出：它内部持 `Arc`，UiManager 被 forget 之后
                // 唤醒事件照样存活。
                let waker = ui.waker();
                let tx = crate::UiSender::new(ui.sender(), Arc::new(move || waker.wake()));
                let rx = ui.take_event_rx();
                std::mem::forget(ui); // 进程生命周期内保持 UI 线程存活
                (tx, rx)
            }
            Err(e) => {
                warn!("Failed to create UI manager: {}", e);
                // UI 线程没起来，通道无人接收 → 无从唤醒，也无需唤醒。
                let (tx, _rx) = std::sync::mpsc::channel();
                (crate::UiSender::without_wake(tx), None)
            }
        };

        // 用户配置目录（%APPDATA%\WindInput）：config.toml / userdata.redb / 词频等用户偏好。
        let user_dir =
            Config::user_config_dir().or_else(|| data_dir.as_deref().map(|d| d.to_path_buf()));
        // redb 用户数据库（用户偏好数据：词频、自定义词、shadow 规则，应随用户漫游）。
        let store = user_dir.as_deref().and_then(Self::open_user_store);
        let coordinator = Self::build(
            config,
            data_dir.as_deref(),
            push_server,
            ui_tx,
            user_dir,
            store,
            None, // 生产路径：override 目录由 EngineManager 取用户配置目录下的默认值
        );

        // 鼠标事件处理线程：候选窗的点击/悬停/滚轮经此回到协调器
        if let Some(rx) = event_rx {
            let c = Arc::clone(&coordinator);
            std::thread::spawn(move || {
                for ev in rx {
                    c.handle_ui_event(ev);
                }
                debug!("UI event channel closed");
            });
        }

        // 注册 keys.global_hotkeys 全局热键（RegisterHotKey）：启动即注册，
        // 不依赖 IME 激活——全局热键的语义就是在本输入法未激活时也生效。
        coordinator.sync_global_hotkeys();

        // CapsLock 全局钩子：仅当 keys.session_actions 里配了 capslock 才安装。
        // （动作消费线程已在内部构造函数里起好，见那里。）
        coordinator.sync_capslock_hook();

        // 同步 activate_ime 到 DirectSwitchHotkeys 注册表：同样启动即同步（该热键的
        // 语义就是在本输入法未激活时切换过来），且不依赖 UI 线程创建成功
        // （Go 版把同步放在 UI 回调装配里，UI 创建失败会静默跳过——已规避）。
        coordinator.sync_direct_switch_hotkey();

        // 后台预热：提前构建其余方案的引擎与缓存（拼音 merged/unigram、码表 per-dict），
        // 避免首次切换到拼音/临时拼音/码表时同步重熔大词库造成几十秒卡顿。
        // single-flight 构建锁保证预热与用户切换不重复构建；按方案顺序逐个建（后台低频）。
        //
        // ⚠ 移动端可经 `set_eager_prewarm(false)` 关掉这段：手机上「把所有已装方案的
        // 词库都编译一遍」的代价是几秒 CPU + 数十 MB 内存，而启动那几秒正是用户最可能
        // 在打字的时候——预热线程与按键线程抢 state 锁，实测直接把主线程拖到 ANR。
        // 关掉后方案在**首次切换时**按需加载（切换时等一下可以接受，启动卡死不行）。
        {
            let c = Arc::clone(&coordinator);
            std::thread::spawn(move || {
                // 延迟启动有两个作用，缺一不可：
                // ① 让宿主有机会在构造返回后调 `set_eager_prewarm(false)`——本线程在
                //    构造**末尾**就已 spawn，不等就会抢在宿主表态之前读到默认值 true；
                // ② 避开启动期的锁竞争高峰（宿主此时正在建视图、要焦点、可能已在收键）。
                std::thread::sleep(std::time::Duration::from_millis(1500));
                if !c.eager_prewarm.load(std::sync::atomic::Ordering::Relaxed) {
                    debug!("启动预热已关闭（宿主声明按需加载）");
                    return;
                }
                let active = c.engine_mgr.active_schema_id();
                // available_schemas 只含「可切换的方案」。临时拼音 / 临时英文的目标引擎
                // **不在其中**（它们是模式的实现，不是可切换方案），此前因此漏出预热范围：
                // 实测首次按引导键进临拼时才同步加载 52 万词条的拼音库 + 英文库，用户感到
                // 顿一下。两者都只在启用时才预热，不给没开这些功能的用户白付内存。
                let mut targets: Vec<String> = c.engine_mgr.available_schemas().to_vec();
                // ⚠ `temp_pinyin_target()` **自身就会 `ensure_loaded`**（它的语义是「可用才
                // 返回」），故这一行本身即完成了临拼引擎的加载，下面循环里那次只是复查跳过。
                // 看着绕，但比在此复制一份「开关 + 方案适用性 + 目标解析」的判据强——那套判据
                // 是所有临拼入口的公共门卫，抄一份必然漂移。
                if let Some(t) = c.engine_mgr.temp_pinyin_target() {
                    targets.push(t);
                }
                if c.rt().config.input.temp_english.show_candidates {
                    targets.push("english".to_string());
                }
                for id in targets {
                    if id == active || c.engine_mgr.is_loaded(&id) {
                        continue;
                    }
                    let t0 = std::time::Instant::now();
                    if c.engine_mgr.prewarm_schema(&id) {
                        debug!("Prewarmed schema {} in {:?}", id, t0.elapsed());
                    } else {
                        debug!("Prewarm skipped/failed for schema {}", id);
                    }
                }
                debug!("Schema prewarm done");

                // 反查索引（悬停[编码]/编码提示/词语联想的数据源）同样要提前建好。
                //
                // 它此前是**首次按键时**才懒构建的，而对大词库那是秒级操作，恰好落在
                // TSF→服务的同步 IPC 链路上：真机 feihuzj2（253 万条）实测让整机卡了
                // 29.5 秒。放到这里之后，绝大多数用户永远碰不到那次构建。
                //
                // 只预热**当前用得着的方案**（悬停编码段与词语联想各自的来源方案，
                // 混输下通常同为主码表成员）：其余方案的索引切过去时才有意义，而每份
                // 索引对超大词库是百 MB 量级（护栏本就只保留两份），全量预热等于把内存
                // 花在用户未必会用的方案上。
                //
                // 与测试、移动端 prepare() 共用 `prewarm_indexes`，避免三处各写一份。
                c.prewarm_indexes();
            });
        }

        // 恢复持久化的工具栏位置（按前台窗口所在显示器的 key 查找）。
        // 与运行期换屏走同一个函数——判据分成两套迟早漂移。
        coordinator.init_toolbar_pos();

        // 加载并下发初始主题。明暗必须走 resolve_theme_dark（system 实时探测系统明暗），
        // 不能硬编码 false——否则跟随系统在**冷启动**这一刻永远回落亮色（实时跟随另有 WM_SETTINGCHANGE
        // 路径，故只在启动瞬间错、切一次系统主题就"自愈"，与 theme_style 的其余消费点保持同一出口）。
        let name = coordinator
            .theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        coordinator.push_theme(&name, coordinator.resolve_theme_dark());
        // 下发候选布局方向（ui.candidate.layout）。
        let orientation =
            wind_config::Orientation::from_layout_str(&coordinator.rt().config.ui.candidate.layout);
        let _ = coordinator.ui_tx.send(UiCommand::SetCandidateLayout {
            vertical: orientation.vertical,
            rotated: orientation.rotated(),
            upright: orientation.upright(),
        });
        // 下发预编辑内联模式：仅 candidate_inline 需内联候选首单元（app_inline 不显示、candidate_top 独立条）。
        let embedded = coordinator.rt().config.ui.candidate.preedit().embedded();
        let _ = coordinator
            .ui_tx
            .send(UiCommand::SetPreeditEmbedded(embedded));
        // 候选字号覆盖 + 悬停提示延迟初值
        let rt0 = coordinator.rt();
        let _ = coordinator.ui_tx.send(UiCommand::SetCandidateFontSize(
            rt0.config.ui.candidate.font_size,
        ));
        let _ = coordinator.ui_tx.send(UiCommand::SetCandidateFlipWhenAbove(
            rt0.config.ui.candidate.flip_when_above,
        ));
        let _ = coordinator.ui_tx.send(UiCommand::SetCandidateSwapWhenAbove(
            rt0.config.ui.candidate.swap_preedit_when_above,
        ));
        let _ = coordinator.ui_tx.send(UiCommand::SetPagerInPreedit(
            rt0.config.ui.candidate.pager_in_preedit,
        ));
        // 候选窗尺寸下限（抗抖动）；热重载侧同样下发，见 Coordinator::apply_ui_config。
        let _ = coordinator.ui_tx.send(UiCommand::SetCandidateMinSize {
            width_horizontal: rt0.config.ui.candidate.min_window_width_horizontal,
            width_vertical: rt0.config.ui.candidate.min_window_width_vertical,
            height_horizontal: rt0.config.ui.candidate.min_window_height_horizontal,
            height_vertical: rt0.config.ui.candidate.min_window_height_vertical,
            rows: rt0.config.ui.candidate.effective_min_rows(),
        });
        let _ = coordinator
            .ui_tx
            .send(UiCommand::SetTooltipDelay(rt0.config.ui.tooltip.delay));
        // 拆字字根字体（PUA 字根渲染）：路径 + DWrite 家族名取自主码表方案 [engine.chaizi]。
        // 库已在 build 内加载，此处仅补发字体（sync 按变更检测，重复调用幂等）。
        // 快捷输入格式表的用户调整（右键调序/停用）：真相在 store，这里装载运行时镜像。
        // 必须在 store 就位之后——构造体内只能给空初值。
        coordinator.reload_quick_adjust();
        coordinator.sync_chaizi_assets();
        // 注释词库首次加载（`[[ui.comment_dicts]]`，出厂为空数组=不加载任何库）。
        coordinator.sync_comment_dicts();
        // 统一应用外观项（幂等）：补齐上面手动块未含的候选字体族 / 翻页栏 / 页码 / 字号跟随主题，
        // 使首次启动即按 config 应用（与 reload_user_config 同一路径）。
        coordinator.apply_ui_config();
        coordinator
    }

    /// 无头构造器（测试用）：跳过 UI 线程，不做词频持久化（避免污染真实文件）。
    pub fn new_headless(config: Config, data_dir: Option<&Path>) -> Arc<Self> {
        // 无头模式无 UI 消费端：丢弃 rx，notify_ui_* 的 send 会静默失败（已用 `let _ =` 忽略）
        let (ui_tx, _rx) = std::sync::mpsc::channel();
        // 接收端当场丢弃，没有 UI 线程可唤醒。
        let ui_tx = crate::UiSender::without_wake(ui_tx);
        drop(_rx);
        // PushServer::new 零副作用（不起线程不开管道，副作用全在 start()，headless
        // 从不调）；无客户端时 push_* 全是遍历空表的 no-op。勿为 headless 把它
        // feature 门控——40+ 调用点会跟着裂开，得不偿失。
        let push_server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }));
        Self::build(config, data_dir, push_server, ui_tx, None, None, None)
    }

    /// 无头 + **指定方案 override 目录**（测试用）。
    ///
    /// `new_headless` 让 `EngineManager` 自己取 `Config::user_config_dir()/schema_overrides`
    /// ——那是**真实用户目录**，测试写进去会污染用户配置，于是一切「方案级覆盖」的行为都
    /// 没法在集成测试里验证。方案级 `[key_actions]` 的分派 bug 正是因此漏到了真机上。
    pub fn new_headless_with_override(
        config: Config,
        data_dir: Option<&Path>,
        override_dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        let (ui_tx, _rx) = std::sync::mpsc::channel();
        // 接收端当场丢弃，没有 UI 线程可唤醒。
        let ui_tx = crate::UiSender::without_wake(ui_tx);
        drop(_rx);
        let push_server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }));
        Self::build(
            config,
            data_dir,
            push_server,
            ui_tx,
            None,
            None,
            override_dir,
        )
    }

    /// 无头 + **保留 UI 通道接收端**（测试用）。
    ///
    /// `new_headless` 丢弃 rx，于是一切「发给 UI 的内容」在测试里都不可见——而候选的注释段、
    /// 悬停提示这些是在**发送路径上**算出来的，不回写 `state.candidates`。要验证它们只有两条路：
    /// 收这个 rx，或者另写一个「按同样规则再算一遍」的 debug 方法。后者是假测试的经典形态——
    /// 它证明不了生产路径接对了，决策函数写好但消费端没接的情况照样全绿。
    pub fn new_headless_with_ui(
        config: Config,
        data_dir: Option<&Path>,
    ) -> (Arc<Self>, std::sync::mpsc::Receiver<UiCommand>) {
        Self::new_headless_with_ui_at(config, data_dir, None)
    }

    /// 无头 + UI 通道 + **用户数据目录**（Android 生产路径）。
    ///
    /// 与 [`Self::new_headless_with_ui`] 的唯一区别是开不开 redb store，而这个区别不小：
    /// store 为 `None` 时**系统短语层为空**（构造期的 `sync_system_phrases` 整段跳过）、
    /// 用户词频与自造词不落盘。表现是「短语一条也不出」而不是报错，故无头宿主一旦要
    /// 进入生产形态（而非只跑按键逻辑测试），必须走这个入口给出用户目录。
    pub fn new_headless_with_ui_at(
        config: Config,
        data_dir: Option<&Path>,
        user_dir: Option<&Path>,
    ) -> (Arc<Self>, std::sync::mpsc::Receiver<UiCommand>) {
        let (ui_tx, rx) = std::sync::mpsc::channel();
        // rx 交给测试直接读命令，不经 UI 线程，故无需唤醒通路。
        let ui_tx = crate::UiSender::without_wake(ui_tx);
        let push_server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }));
        let user_dir = user_dir.map(|d| d.to_path_buf());
        let store = user_dir.as_deref().and_then(Self::open_user_store);
        (
            Self::build(config, data_dir, push_server, ui_tx, user_dir, store, None),
            rx,
        )
    }

    /// 打开用户目录下的 redb（缺目录时创建）。失败只 warn：store 不可用时协调器
    /// 退化为「不落盘」而非拒绝启动。
    pub(crate) fn open_user_store(dir: &Path) -> Option<Arc<Store>> {
        let _ = std::fs::create_dir_all(dir);
        let p = dir.join("userdata.redb");
        match Store::open(&p) {
            Ok(s) => {
                info!("Opened redb store: {}", p.display());
                Some(Arc::new(s))
            }
            Err(e) => {
                warn!("Failed to open redb store {}: {}", p.display(), e);
                None
            }
        }
    }

    /// 无头 + 注入 redb store（测试用）：用于 web_data_rpc 数据域契约测试。
    pub fn new_headless_with_store(
        config: Config,
        data_dir: Option<&Path>,
        store: Arc<Store>,
    ) -> Arc<Self> {
        Self::new_headless_with_store_override(config, data_dir, store, None)
    }

    /// 无头 + store + **指定方案 override 目录**（测试用）。
    ///
    /// 特殊模式的实例集合来自「带 `[overlay]` 段的已安装方案」，而测试不能往真实
    /// `data/schemas` 里写方案文件。走 override 层即可：`read_schema` 会把它深合并进
    /// 方案，效果等同该方案自带 `[overlay]` 段，同时保住真实词库不动。
    pub fn new_headless_with_store_override(
        config: Config,
        data_dir: Option<&Path>,
        store: Arc<Store>,
        override_dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        let (ui_tx, _rx) = std::sync::mpsc::channel();
        // 接收端当场丢弃，没有 UI 线程可唤醒。
        let ui_tx = crate::UiSender::without_wake(ui_tx);
        drop(_rx);
        let push_server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }));
        Self::build(
            config,
            data_dir,
            push_server,
            ui_tx,
            None,
            Some(store),
            override_dir,
        )
    }
}
