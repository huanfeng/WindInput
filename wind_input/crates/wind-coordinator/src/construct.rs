//! Coordinator 构造器族：生产构造 `new`（desktop-ui）与 headless 家族。
//!
//! 共同核心 `build`（结构体字面量装配点）留在 coordinator.rs：私有字段的可见性以
//! coordinator 模块为界，装配点不外迁。

use std::path::Path;
use std::sync::Arc;
// debug! 只在 desktop-ui 门内的 `new` 用到；headless 形态下无引用。
#[cfg(feature = "desktop-ui")]
use tracing::debug;
use tracing::{error, info, warn};

use wind_bridge::push::{PushConfig, PushServer};
use wind_config::Config;
use wind_store::Store;
use wind_ui_types::UiCommand;
// UiEvent 仅 macOS forwarder 路径显式命名（其余平台由 UiManager 的通道类型推断）。
#[cfg(all(feature = "desktop-ui", ext_presenter))]
use wind_ui_types::UiEvent;
// UiManager 仅 Windows LayeredWindow 路径用；macOS 走 host-render forwarder。
#[cfg(all(feature = "desktop-ui", not(ext_presenter)))]
use wind_ui::manager::UiManager;

use crate::coordinator::Coordinator;

/// 把 `format_version` 升到迁移进度够到的值；够到 5（旧库各表迁完）⇒ 删掉旧库链标记、旧库改名
/// `userdata.redb.legacy`（§7.4）。打开用户库后、短语迁移之后各调一次。没迁完不升：认识旧格式的
/// 旧版本回退后旧表仍是完整的真源、仍可写。
pub(crate) fn finish_format_upgrade(store: &Store, user_dir: &Path) {
    let Some(v) = store.format_version_reached() else {
        warn!("P1 迁移未完成，format_version 暂不升级，下次启动重试");
        return;
    };
    if let Err(e) = wind_config::user_meta::raise_format_version(user_dir, v) {
        warn!("format_version 升级失败（旧版本仍可能写 redb）：{e}");
        return;
    }
    if v >= wind_store::migrate_p3::FULLY_MIGRATED_VERSION {
        store.forget_legacy_markers();
        wind_store::legacy_retire::retire_legacy_db(user_dir);
    }
}

/// 生产的运行时状态位置：`<state>/runtime/<dir_id>/`。按用户目录分开——换用户目录（`datadir.conf`、
/// 拷回旧目录副本）时新目录有自己的引擎、迁移标记与锁，旧目录学到的数据不会混进来，新目录的旧表
/// 照常迁移。dir_id 只取字母数字与 `-` `_`（它来自 meta.toml，防路径穿越）。
/// 用户数据文件在用户目录 `<user_dir>/data/` 下（随目录漫游 / 备份）。
pub(crate) fn runtime_state_loc(
    state_base: &Path,
    dir_id: &str,
    user_dir: &Path,
    legacy_done: bool,
) -> wind_store::store::StateLoc {
    let safe: String = dir_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let safe = if safe.is_empty() {
        "default".to_string()
    } else {
        safe
    };
    wind_store::store::StateLoc {
        state_dir: state_base.join("runtime").join(safe),
        dir_id: dir_id.to_string(),
        text_dir: user_dir.join(wind_store::DATA_DIR_NAME),
        legacy_done,
    }
}

/// 运行时引擎的状态目录取哪里。
#[derive(Clone, Copy)]
pub(crate) enum StateChoice {
    /// 生产：`Config::state_dir()`（`local_dir()/state`）+ meta 的 dir_id。
    Production,
    /// 测试 / 无头：redb 旁的派生目录（不碰本机状态目录与它的锁）。
    Derived,
}

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
        #[cfg(ext_presenter)]
        let (ui_tx, event_rx) = {
            let (tx, rx) = std::sync::mpsc::channel::<UiCommand>();
            // 候选/菜单的鼠标交互确实经 push/bridge 协议从 .app 回流，不走这里；
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
        #[cfg(not(ext_presenter))]
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
        let (store, storage_readonly) =
            Self::open_user_store_checked(user_dir.as_deref(), StateChoice::Production);
        let coordinator = Self::build_with_readonly(
            config,
            data_dir.as_deref(),
            push_server,
            ui_tx,
            user_dir,
            store,
            None, // 生产路径：override 目录由 EngineManager 取用户配置目录下的默认值
            storage_readonly,
            crate::input_profile::InputProfile::desktop(),
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

        // 简繁两方向互斥：启动时若配置里两个都开着，关掉 t2s 并落盘。
        //
        // 内部构造那一步只是读时压掉（state 里 t2s 恒 false），配置文件仍留着那个
        // 非法组合。不在这里归一的话，一个从没打开过设置页的用户会一直带着它——每次
        // 启动都被静默压一次，而他从界面上看到的是两个都开着。
        coordinator.normalize_conversion_exclusivity_on_start();

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

        // 后台预热其余方案的引擎与缓存，避免首次切换时同步重熔大词库卡几十秒。single-flight
        // 构建锁保证预热与用户切换不重复构建；按方案顺序逐个建。
        //
        // 移动端经 `set_eager_prewarm(false)` 关掉：启动预热与按键线程抢 state 锁会拖到 ANR，
        // 关掉后方案首次切换时按需加载（切换时等一下可接受，启动卡死不行）。
        {
            let c = Arc::clone(&coordinator);
            std::thread::spawn(move || {
                // 延迟启动有两个作用，缺一不可：
                // ① 让宿主有机会在构造返回后调 `set_eager_prewarm(false)`——本线程在
                //    构造末尾就已 spawn，不等就会抢在宿主表态之前读到默认值 true；
                // ② 避开启动期的锁竞争高峰（宿主此时正在建视图、要焦点、可能已在收键）。
                std::thread::sleep(std::time::Duration::from_millis(1500));
                // 草稿表的启动清理。刻意放在 eager_prewarm 闸之前：移动端关掉的是
                // 「把所有已装方案的词库都编译一遍」那几秒 CPU，不是数据库维护，而草稿
                // 在手机上一样会攒。这一次不碰任何引擎，代价只有一次 redb 事务。
                c.purge_drafts_on_start();
                if !c.eager_prewarm.load(std::sync::atomic::Ordering::Relaxed) {
                    debug!("启动预热已关闭（宿主声明按需加载）");
                    return;
                }
                c.prewarm_on_start();
            });
        }

        // 闲置清扫（每分钟一拍）：方案常驻策略关着时摘掉 30 分钟没用的方案引擎。
        // 与预热一样只在桌面构造器里起——移动端走 headless 构造，不受影响（设计 §9 D3）。
        coordinator.spawn_idle_sweeper();

        // 恢复持久化的工具栏位置（按前台窗口所在显示器的 key 查找）。
        // 与运行期换屏走同一个函数——判据分成两套迟早漂移。
        coordinator.init_toolbar_pos();

        // 加载并下发初始主题。明暗必须走 resolve_theme_dark（system 实时探测系统明暗），
        // 不能硬编码 false——否则跟随系统在冷启动这一刻永远回落亮色（实时跟随另有 WM_SETTINGCHANGE
        // 路径，故只在启动瞬间错、切一次系统主题就"自愈"，与 theme_style 的其余消费点保持同一出口）。
        let name = coordinator
            .theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // 候选字号/字体先于主题下发（理由见 `send_candidate_font`）。
        coordinator.send_candidate_font(&coordinator.rt().config);
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
        // 悬停提示延迟等初值（候选字号/字体已在 push_theme 之前下发）
        let rt0 = coordinator.rt();
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
        coordinator.send_candidate_min_size();
        let _ = coordinator
            .ui_tx
            .send(UiCommand::SetTooltipDelay(rt0.config.ui.tooltip.delay));
        // 快捷输入调整 / 拆字字根字体（PUA 字根渲染，路径 + DWrite 家族名取自主码表方案
        // [engine.chaizi]）/ 注释库 / emoji 的首次装载，见 `load_data_on_start`。
        coordinator.load_data_on_start();
        // 统一应用外观项（幂等）：补齐上面手动块未含的翻页栏 / 页码等外观项，
        // 使首次启动即按 config 应用（与 reload_user_config 同一路径）。
        coordinator.apply_ui_config();
        coordinator
    }

    /// 生产构造器在 `build` 之后的非 UI 数据装载（快捷输入调整、拆字、注释库、emoji）。
    ///
    /// 抽出来是为了让 `debug_desktop_startup_and_prewarm`（本机复现内存的探针入口）与
    /// `new` 走同一份序列，不各抄一份而漂移。
    pub(crate) fn load_data_on_start(&self) {
        // 快捷输入格式表的用户调整（右键调序/停用）：真相在 store，这里装载运行时镜像。
        // 必须在 store 就位之后——构造体内只能给空初值。
        self.reload_quick_adjust();
        // 拆字库已在 build 内加载，此处仅补发字体（sync 按变更检测，重复调用幂等）。
        self.sync_chaizi_assets();
        // 注释词库首次加载（`[[ui.comment_dicts]]`，出厂为空数组=不加载任何库）。
        self.sync_comment_dicts();
        // emoji 扩展表首次加载（`[input.emoji]`，出厂 enabled=false ⇒ 不打开任何文件）。
        self.sync_emoji_dict();
    }

    /// 启动预热线程的主体（`new` 里延迟 1.5 秒后调；阻塞，只可在后台线程 / 测试里调）：
    /// 按方案常驻策略建方案引擎（[`Self::prewarm_schemas_on_start`]：全部常驻，或只建
    /// 用得着的），再 [`Self::prewarm_indexes`]，常驻关着时最后校验其余方案的缓存
    /// （[`Self::refresh_schema_caches_on_start`]）。
    ///
    /// 与 `debug_desktop_startup_and_prewarm` 共用，理由同 [`Self::load_data_on_start`]。
    pub(crate) fn prewarm_on_start(&self) {
        self.prewarm_schemas_on_start();

        // 反查索引（悬停[编码]/编码提示/词语联想的数据源）提前建好：大词库构建是秒级，
        // 懒建会落在 TSF→服务的同步 IPC 链路上卡住宿主。只预热当前用得着的方案——每份
        // 索引对超大词库是百 MB 量级，全量预热是把内存花在未必会用的方案上。
        // 与测试、移动端 prepare() 共用 `prewarm_indexes`。
        self.prewarm_indexes();
        // 常驻关着时，其余方案只校验派生缓存（排在索引之后，见该函数文档）。
        self.refresh_schema_caches_on_start();
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

    /// 无头 + 指定方案 override 目录（测试用）。
    ///
    /// `new_headless` 让 `EngineManager` 自己取 `Config::user_config_dir()/schema_overrides`
    /// ——那是真实用户目录，测试写进去会污染用户配置，于是一切「方案级覆盖」的行为都
    /// 没法在集成测试里验证。方案级 `[key_actions]` 的分派 bug 正是因此漏到了真机上。
    pub fn new_headless_with_override(
        config: Config,
        data_dir: Option<&Path>,
        override_dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        Self::new_headless_with_override_profile(
            config,
            data_dir,
            override_dir,
            crate::input_profile::InputProfile::desktop(),
        )
    }

    /// 同 [`Self::new_headless_with_override`]，另指定输入档位（测试用）。
    pub fn new_headless_with_override_profile(
        config: Config,
        data_dir: Option<&Path>,
        override_dir: Option<std::path::PathBuf>,
        profile: crate::input_profile::InputProfile,
    ) -> Arc<Self> {
        let (ui_tx, _rx) = std::sync::mpsc::channel();
        // 接收端当场丢弃，没有 UI 线程可唤醒。
        let ui_tx = crate::UiSender::without_wake(ui_tx);
        drop(_rx);
        let push_server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }));
        Self::build_with_readonly(
            config,
            data_dir,
            push_server,
            ui_tx,
            None,
            None,
            override_dir,
            None,
            profile,
        )
    }

    /// 无头 + 保留 UI 通道接收端（测试用）。
    ///
    /// 候选注释、悬停提示等是在发送路径上算出来的、不回写 `state.candidates`，测试要验证
    /// 它们就收这个 rx；另写「按同样规则再算一遍」的 debug 方法证明不了生产路径接对了。
    pub fn new_headless_with_ui(
        config: Config,
        data_dir: Option<&Path>,
    ) -> (Arc<Self>, std::sync::mpsc::Receiver<UiCommand>) {
        Self::new_headless_with_ui_at(config, data_dir, None)
    }

    /// 无头 + UI 通道 + 用户数据目录（Android 生产路径）。
    ///
    /// 与 [`Self::new_headless_with_ui`] 的唯一区别是开不开 redb store，而这个区别不小：
    /// store 为 `None` 时系统短语层为空（构造期的 `sync_system_phrases` 整段跳过）、
    /// 用户词频与自造词不落盘。表现是「短语一条也不出」而不是报错，故无头宿主一旦要
    /// 进入生产形态（而非只跑按键逻辑测试），必须走这个入口给出用户目录。
    pub fn new_headless_with_ui_at(
        config: Config,
        data_dir: Option<&Path>,
        user_dir: Option<&Path>,
    ) -> (Arc<Self>, std::sync::mpsc::Receiver<UiCommand>) {
        Self::new_headless_with_profile(
            config,
            data_dir,
            user_dir,
            crate::input_profile::InputProfile::desktop(),
        )
    }

    /// 中立输出通道的构造器（移动端宿主用）：只发 [`crate::SessionEvent`]，不发桌面 `UiCommand`。
    ///
    /// 与 [`Self::new_headless_with_profile`] 同样开 redb store、同样无 UI 线程；区别是
    /// 协调器带着 `session_tx`，于是 `notify_ui_update` 等出口改发中立事件，并跳过首显闸门、
    /// 注释渲染与候选窗布局。内部仍有一条 `UiSender` 给其余约 250 处桌面发送点，接收端当场
    /// 丢弃，那些发送在这里静默失败。
    pub fn new_session(
        config: Config,
        data_dir: Option<&Path>,
        user_dir: Option<&Path>,
        profile: crate::input_profile::InputProfile,
    ) -> (Arc<Self>, std::sync::mpsc::Receiver<crate::SessionEvent>) {
        let (ui_tx, ui_rx) = std::sync::mpsc::channel();
        let ui_tx = crate::UiSender::without_wake(ui_tx);
        drop(ui_rx);
        let (session_tx, session_rx) = std::sync::mpsc::channel();
        let push_server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }));
        let user_dir = user_dir.map(|d| d.to_path_buf());
        let (store, storage_readonly) =
            Self::open_user_store_checked(user_dir.as_deref(), StateChoice::Derived);
        (
            Self::build_with_session(
                config,
                data_dir,
                push_server,
                ui_tx,
                user_dir,
                store,
                None,
                storage_readonly,
                profile,
                Some(session_tx),
            ),
            session_rx,
        )
    }

    /// 无头 + UI 通道 + 用户数据目录 + 指定输入档位。移动端宿主与档位测试走这里；
    /// 其余无头构造器一律是桌面档位。
    pub fn new_headless_with_profile(
        config: Config,
        data_dir: Option<&Path>,
        user_dir: Option<&Path>,
        profile: crate::input_profile::InputProfile,
    ) -> (Arc<Self>, std::sync::mpsc::Receiver<UiCommand>) {
        let (ui_tx, rx) = std::sync::mpsc::channel();
        // rx 交给测试直接读命令，不经 UI 线程，故无需唤醒通路。
        let ui_tx = crate::UiSender::without_wake(ui_tx);
        let push_server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 30_000,
        }));
        let user_dir = user_dir.map(|d| d.to_path_buf());
        // 测试路径：运行时数据放 redb 旁的派生目录，不碰本机状态目录与它的锁
        let (store, storage_readonly) =
            Self::open_user_store_checked(user_dir.as_deref(), StateChoice::Derived);
        (
            Self::build_with_readonly(
                config,
                data_dir,
                push_server,
                ui_tx,
                user_dir,
                store,
                None,
                storage_readonly,
                profile,
            ),
            rx,
        )
    }

    /// 先查 `meta.toml` 的格式版本（用户数据设计 §7.1）：高于本版本或读不出 ⇒ 只读模式，
    /// 不打开 redb、不写任何用户数据；否则照常打开。
    pub(crate) fn open_user_store_checked(
        dir: Option<&Path>,
        choice: StateChoice,
    ) -> (Option<Arc<Store>>, Option<String>) {
        let Some(d) = dir else { return (None, None) };
        match wind_config::user_meta::load_or_init(d) {
            wind_config::user_meta::MetaCheck::Ok(meta) => {
                // 用户目录记为迁完 ⇒ 不再从旧库重迁（state 被清也一样）
                let legacy_done =
                    meta.format_version >= wind_store::migrate_p3::FULLY_MIGRATED_VERSION;
                (
                    Self::open_user_store(d, choice, &meta.dir_id, legacy_done),
                    None,
                )
            }
            wind_config::user_meta::MetaCheck::Incompatible { found, known } => {
                let reason = format!(
                    "用户数据由更新版本写入（格式 {found}，本版本只认识 {known}）：已进入只读模式，\
                     不会修改任何用户数据。请升级到新版本，或从备份还原。"
                );
                error!("{reason}");
                (None, Some(reason))
            }
            wind_config::user_meta::MetaCheck::Unreadable(e) => {
                let reason = format!("用户目录的 meta.toml 读不出（{e}）：按只读模式运行。");
                error!("{reason}");
                (None, Some(reason))
            }
        }
    }

    /// 打开用户目录下的 redb（缺目录时创建）。失败只 warn：store 不可用时协调器
    /// 退化为「不落盘」而非拒绝启动。
    pub(crate) fn open_user_store(
        dir: &Path,
        choice: StateChoice,
        dir_id: &str,
        legacy_done: bool,
    ) -> Option<Arc<Store>> {
        let _ = std::fs::create_dir_all(dir);
        let p = dir.join("userdata.redb");
        let opened = match choice {
            StateChoice::Production => match Config::state_dir() {
                Some(state_base) => Store::open_with_state(
                    &p,
                    runtime_state_loc(&state_base, dir_id, dir, legacy_done),
                ),
                None => {
                    // 文本仍在真实用户目录里：它是真源，不能跟着派生目录走
                    warn!("取不到本机状态目录，运行时数据放在 redb 旁的派生目录");
                    Store::open_with_state(
                        &p,
                        wind_store::store::StateLoc {
                            state_dir: wind_store::store::derived_state_dir(&p),
                            dir_id: dir_id.into(),
                            text_dir: dir.join(wind_store::DATA_DIR_NAME),
                            legacy_done,
                        },
                    )
                }
            },
            StateChoice::Derived => {
                // 运行时状态放 redb 旁的派生目录（新库时清空，测试反复用同一路径不串数据）；用户词
                // 文本是真源，同生产一样在 `<用户目录>/user/`，从不跟着清（移动端也走这条）。
                // 用户目录已记为迁完时本就没有 redb（全新目录不建），不在不代表是新库。
                let state_dir = wind_store::store::derived_state_dir(&p);
                if !p.exists() && !legacy_done {
                    let _ = std::fs::remove_dir_all(&state_dir);
                }
                Store::open_with_state(
                    &p,
                    wind_store::store::StateLoc {
                        state_dir,
                        dir_id: "derived".into(),
                        text_dir: dir.join(wind_store::DATA_DIR_NAME),
                        legacy_done,
                    },
                )
            }
        };
        match opened {
            Ok(s) => {
                info!("Opened user store: {}", dir.display());
                // 迁移完成才升 format_version：此后认识旧格式的版本只读（§7.1）。P3 的短语迁移要等
                // 协调器交来系统短语（之后协调器会再升一次）。
                finish_format_upgrade(&s, dir);
                let s = Arc::new(s);
                // 空闲时把 redb 页缓存还给系统：它只涨不落，读路径（设置页分页要精确 `total`
                // 须扫全表）躲不掉，只能靠这条兜；写路径已在导入/恢复/清空后直接丢。长档与短档
                // 的取值依据见 wind-store `RECLAIM_IDLE` / `SCAN_RECLAIM_IDLE` 的文档。
                // 同一时刻顺带整理堆，否则私有内存停在历史峰值（见 heap_trim.rs）。
                s.spawn_idle_cache_reclaimer_then(
                    wind_store::store::RECLAIM_IDLE,
                    wind_store::store::SCAN_RECLAIM_IDLE,
                    wind_store::store::RECLAIM_TICK,
                    crate::heap_trim::release_free_heap,
                );
                // 用户词改动停手 30 秒后写回 user.dict.yaml：没有后续写入也要有人来折回。
                s.spawn_userdata_ticker(wind_store::store::USERDATA_TICK);
                // 补全历史等小文件只改内存，这里每 2 秒把改动落盘一次（设计 §2.4 去抖）。
                s.spawn_small_file_flusher(wind_store::store::SMALL_FILE_FLUSH);
                Some(s)
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

    /// 无头 + store + 指定方案 override 目录（测试用）。
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

#[cfg(test)]
#[path = "construct_p1_meta_tests.rs"]
mod p1_meta_tests;
