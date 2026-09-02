//! 语言栏图标发布（Windows 桌面形态）：共享内存单例 + 状态角标。
//!（coordinator 子模块，自 coordinator.rs 平移，纯搬运。）

use super::*;

/// 语言栏图标发布器（Windows 桌面形态）。
///
/// 做成进程级单例而非 [`Coordinator`] 字段，理由是它对应的资源本身就是进程级唯一的：
/// 共享内存名固定（`Local\WindInput_IconShm{_dev}`），一个进程开两份没有意义。
/// 附带好处是不必改动全部构造器。
///
/// 内层 `Option` 为 `None` = 创建失败。这不是致命错误——DLL 侧读不到 SHM 会退回
/// 本地 DirectWrite 绘制，图标照常显示，只是不跟随标点状态。
#[cfg(all(feature = "desktop-ui", windows))]
static ICON_PUBLISHER: std::sync::OnceLock<
    std::sync::Mutex<Option<wind_ui::langbar_icon::LangBarIconPublisher>>,
> = std::sync::OnceLock::new();

/// 演示动画的代际。开/关各 +1，驱动线程每帧核对自己那一代是否仍是当前值，不是就退出。
///
/// 用代际而不是 `JoinHandle` + 停止标志：菜单可以被连点，两次开启之间那个线程还没退出，
/// 用标志位会让新旧两个线程都认为自己该跑（相位于是每帧被推进两次，动画快一倍）。
/// 代际让「谁是当前的驱动」有唯一答案，且无需持有句柄或等待线程结束。
#[cfg(all(feature = "desktop-ui", windows))]
static DEMO_ANIM_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 演示动画帧间隔。一圈 40 帧（`IconRenderer::DEMO_FRAMES_PER_CYCLE`），80ms/帧 ≈ 3.2 秒
/// 转一圈——足够看清转向，又不至于让每帧那套「重渲全部档位 + 跨进程推送 + 宿主重建图标」
/// 变成真实负担。它是 Dev 调试玩具，不为流畅度加码。
#[cfg(all(feature = "desktop-ui", windows))]
const DEMO_ANIM_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

impl Coordinator {
    /// 服务启动后发布一次初始图标。
    ///
    /// [`Self::push_state_update`] 只在状态**变化**时调用。少了这一次补发，开机后到
    /// 用户第一次切换中英或标点之前，共享内存始终是空的，DLL 只能走本地绘制——
    /// 图标显示正常但没有角标，看起来像「功能根本没做」而不是「还没初始化」。
    ///
    /// 非 Windows 桌面形态下是空操作，故调用方无需自己加 cfg。
    pub fn publish_initial_langbar_icon(&self) {
        // 先套配置再发布：反过来会先发一张按代码默认渲染的图，随后又被配置版覆盖——
        // 开机瞬间图标闪一下的来源。apply 内部无变化时不重发，故这里由它兜住首发。
        #[cfg(all(feature = "desktop-ui", windows))]
        self.apply_langbar_config();
        self.publish_langbar_icon_now();
    }

    /// 按当前状态立即重渲并发布图标，并让 DLL 重取一次。
    ///
    /// 这是「位图变了但状态没变」的专用入口——调试菜单改角标形状、演示动画推进相位都走它。
    /// 这类变化不构成状态变化，既有的状态推送不会发生，DLL 那边的 `UpdateFullStatus`
    /// 也会因 `needUpdate` 为假而不发 `OnUpdate`，所以必须自己补一条 [`CMD_REFRESH_ICON`]。
    ///
    /// 只在**确实写了新位图**时才推刷新：`publish` 内部对相同 spec 会跳过，此时 SHM 内容
    /// 没变，推了也只是让每个宿主白重绘一次。
    ///
    /// [`CMD_REFRESH_ICON`]: wind_ipc::protocol::CMD_REFRESH_ICON
    pub fn publish_langbar_icon_now(&self) {
        #[cfg(all(feature = "desktop-ui", windows))]
        if self.publish_langbar_icon(&self.build_status()) {
            self.push_refresh_icon();
        }
    }

    /// 图标发布器单例。首次访问时创建；创建失败缓存为 `None`（DLL 会退回本地绘制）。
    ///
    /// 抽出来是因为调试菜单要在**发布之外**访问它（读当前形状画勾选、改形状），
    /// 而 `get_or_init` 的初始化逻辑只该有一份。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn icon_publisher()
    -> &'static std::sync::Mutex<Option<wind_ui::langbar_icon::LangBarIconPublisher>> {
        use wind_ui::langbar_icon::{BadgeStyle, LangBarIconPublisher};
        ICON_PUBLISHER.get_or_init(|| {
            let suffix = wind_config::variant::pipe_suffix();
            match LangBarIconPublisher::new(suffix, BadgeStyle::default()) {
                Ok(mut p) => {
                    tracing::info!(shm = p.shm_name(), "语言栏图标共享内存已就绪");
                    // 纯调试项（不进用户配置）从 state.toml 恢复；呈现参数走 config，
                    // 由 apply_langbar_config 在构造后与每次配置重载时套用。
                    // `None`（从未设过）一律不动，保留构造函数给的代码默认。
                    if let Some(dir) = Config::state_dir() {
                        let rs = wind_config::RuntimeState::load(&dir);
                        if let Some(on) = rs.langbar_icon_size_marks {
                            p.set_size_marks(on);
                        }
                    }
                    std::sync::Mutex::new(Some(p))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "语言栏图标共享内存创建失败，DLL 将退回本地绘制");
                    std::sync::Mutex::new(None)
                }
            }
        })
    }

    /// 对发布器做一次改动，随后落盘并立即重发。发布器不可用时是空操作。
    ///
    /// 收成一个函数而不是每个调试项各写一遍「取锁 → 改 → 落盘 → 发布」：漏掉重发那步
    /// 的症状是「点了菜单毫无变化」、漏掉落盘那步是「重启就忘」，而调试菜单存在的意义
    /// 恰恰是反复比选——两种症状都直接毁掉它。
    ///
    /// ⚠ **只用于纯调试项**（烧尺寸档标记）。会影响用户可见呈现的那些参数（形状、配色、
    /// 大小、透明度）一律走 `[ui.langbar]` 配置，由 [`Self::apply_langbar_config`] 落地——
    /// 两处都能改同一个量就等于有两个真相源，重启后谁赢取决于加载顺序。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn tweak_langbar_icon(
        &self,
        f: impl FnOnce(&mut wind_ui::langbar_icon::LangBarIconPublisher),
    ) {
        if let Ok(mut guard) = Self::icon_publisher().lock()
            && let Some(p) = guard.as_mut()
        {
            f(p);
            // load-modify-save，与 toolbar_positions / record_last_state 同一模式：
            // state.toml 是多方共用的文件，整体覆盖会抹掉别人的字段。
            if let Some(dir) = Config::state_dir() {
                let mut rs = wind_config::RuntimeState::load(&dir);
                rs.langbar_icon_size_marks = Some(p.size_marks());
                if let Err(e) = rs.save(&dir) {
                    tracing::warn!(error = %e, "语言栏图标偏好落盘失败");
                }
            }
        }
        // 锁已在上面的块尾释放——发布内部还要再取一次同一把锁，留在块内会自锁。
        self.publish_langbar_icon_now();
    }

    /// 调试菜单改呈现参数：写进**用户配置**并热重载。
    ///
    /// 走 `Config::set_user_value` 而不是像纯调试项那样写 state.toml——这些量本就是
    /// 用户可配的，菜单只是个更顺手的入口。同一个落点也就不存在「菜单改了、配置文件
    /// 还是旧值」的分裂，以及随之而来的「重启后谁赢取决于加载顺序」。
    ///
    /// 顺带白拿一条：`set_user_value` 会把等于出厂默认的值**删掉而非写入**，所以点回
    /// 默认位不会在用户层留下钉死的显式值（那正是 `auto_commit_block_on_pinyin` 那颗雷）。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn set_langbar_config(&self, key: &str, value: toml::Value) {
        if let Err(e) = Config::set_user_value(&["ui", "langbar", key], value) {
            tracing::warn!(error = %e, key, "语言栏图标配置落盘失败");
            return;
        }
        // 走完整热重载而不是只改内存：配置的真相在文件里，重载才能保证内存与文件一致。
        // 重载内部会调 apply_langbar_config 完成重渲重发。
        self.reload_user_config();
    }

    /// 取当前状态，并在**返回之前**把对应的图标位图投进共享内存。
    ///
    /// 存在的唯一理由是强制两件事的先后：DLL 收到状态推送后会 `OnUpdate(TF_LBI_ICON)`，
    /// 系统随即回调 `GetIcon` 去读 SHM——那时新位图必须已经在里面。反过来（先推送、后发布）
    /// 是一个跨进程竞态：发布要重渲全部尺寸档 × 明暗两档，是毫秒级工作，而「推送 → DLL 读线程
    /// → PostMessage → OnUpdate → GetIcon」同样是毫秒级，谁先到取决于调度，表现为
    /// **切换偶尔不生效**（图标停在上一个状态，下次切换才追上）。
    ///
    /// 把发布藏进「取状态」这一步，是为了让调用方**拿不到**一个尚未发布的 status——
    /// 顺序由数据依赖保证，而不是靠每个推送函数各自记得先调一次发布。此前
    /// `push_state_update` 里的注释就写对了这条要求、代码却是反的，正是这个原因。
    ///
    /// 非 Windows 桌面形态下退化为纯粹的 [`Self::build_status`]，故调用方无需自己加 cfg。
    pub(crate) fn status_with_icon_published(&self) -> StatusUpdateData {
        let s = self.build_status();
        #[cfg(all(feature = "desktop-ui", windows))]
        self.publish_langbar_icon(&s);
        s
    }

    /// 把 `[ui.langbar]` 的呈现参数套到发布器上，有变化就重渲重发。
    ///
    /// 构造后与**每次配置重载**都要调一次：配置改了却不重新发布，症状是「改了没反应、
    /// 重启才生效」——本仓已按这个形态栽过（见运行时镜像态回灌那条）。
    ///
    /// 单项解析失败只记警告并降级，**不整段回落**：改错一个色值若连带把位置、大小
    /// 一起打回默认，用户根本对不上因果。降级的粒度按字段有没有合理默认值来定——
    /// 颜色退到 `auto`、位置退到右下角，而**状态没有默认值**，不认识就丢掉整条规则。
    ///
    /// 色值里的不透明度（`#RRGGBBAA` 末两位）在这里与全局 `badge_alpha` 合流：
    /// 配置侧只表达「这一条说了没有」，合并发生在渲染侧的 `active_layers`。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn apply_langbar_config(&self) {
        use wind_ui::langbar_icon::{BadgeColor, BadgeRule, BadgeState, BadgeStyle, Corner};

        let cfg = { self.rt().config.ui.langbar.clone() };

        // 色值 → 渲染侧的着色。`auto`（含解析失败）= 与主字同色 + 全局不透明度。
        //
        // ⚠ 「有没有指定不透明度」只能看**原字符串的长度**：`parse_hex` 会把 6 位补成
        // `alpha = 255`，那一步就把「没写」和「写了 FF」抹平了，而这两者含义完全不同
        // ——后者会把这一条切到挖空档（角标实心 + 周围切掉一圈主字）。
        let parse_color = |raw: &str, what: &str| -> BadgeColor {
            let t = raw.trim();
            if t.eq_ignore_ascii_case("auto") {
                return BadgeColor::AUTO;
            }
            match wind_theme::palette::parse_hex(t) {
                Some([r, g, b, a]) => BadgeColor {
                    rgb: Some([b, g, r]),
                    alpha: (t.trim_start_matches('#').len() == 8).then_some(a as f32 / 255.0),
                },
                None => {
                    tracing::warn!(
                        value = raw,
                        item = what,
                        "语言栏角标配色无法解析，按 auto（与主字同色）处理"
                    );
                    BadgeColor::AUTO
                }
            }
        };

        // 关掉的规则整条不进渲染器：那边只需回答"画哪些"，不必再处理"配了但不画"。
        let rules: Vec<BadgeRule> = cfg
            .badges
            .iter()
            .filter(|b| b.enabled)
            .filter_map(|b| {
                let Some(state) = BadgeState::from_id(&b.state) else {
                    tracing::warn!(
                        value = %b.state,
                        "语言栏角标规则的状态无法识别，该条已忽略"
                    );
                    return None;
                };
                Some(BadgeRule {
                    state,
                    corner: Corner::from_id(&b.corner),
                    color_light: parse_color(&b.color_light, "color_light"),
                    color_dark: parse_color(&b.color_dark, "color_dark"),
                    scale: b.scale,
                })
            })
            .collect();

        let changed = {
            let Ok(mut guard) = Self::icon_publisher().lock() else {
                return;
            };
            let Some(p) = guard.as_mut() else {
                return;
            };
            p.apply_appearance(
                Some(BadgeStyle::from_id(&cfg.badge)),
                Some(cfg.badge_scale),
                Some(cfg.badge_alpha),
                Some(rules),
            )
        };
        // 锁已释放——发布内部还要取同一把锁。
        if changed {
            self.publish_langbar_icon_now();
        }
    }

    /// 翻转演示动画（外圈跑马灯）开关，并起停驱动线程。
    ///
    /// **刻意不落盘**（对比形状 / 彩色 / 尺寸档三项）：它不是一个呈现偏好，而是一段持续
    /// 占用 CPU 与 IPC 的演示；服务重启后自己关掉才是对的默认，否则用户下次开机会看到一个
    /// 一直转圈的图标，还找不到它是什么。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn toggle_icon_demo_animation(&self) {
        use std::sync::atomic::Ordering;

        let on = {
            let Ok(mut guard) = Self::icon_publisher().lock() else {
                return;
            };
            let Some(p) = guard.as_mut() else {
                return;
            };
            let next = !p.demo_animation();
            p.set_demo_animation(next);
            next
        };

        // 先改代际再发布：这一步让**上一个**驱动线程（若还在）作废，随后的发布才不会
        // 与它抢相位。关闭时这一发同时负责把跑马灯从图标上抹掉。
        let generation = DEMO_ANIM_GEN.fetch_add(1, Ordering::AcqRel) + 1;
        self.publish_langbar_icon_now();

        if !on {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            tracing::warn!("演示动画：拿不到 Coordinator 弱引用，动画不启动");
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("langbar-icon-demo".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(DEMO_ANIM_FRAME_INTERVAL);
                    // 代际核对放在最前：关掉开关后最多再多睡一帧就退出，不需要唤醒机制。
                    if DEMO_ANIM_GEN.load(Ordering::Acquire) != generation {
                        return;
                    }
                    // 服务正在退出 ⇒ 一并收摊。弱引用同时兼作生命周期闸门。
                    let Some(c) = weak.upgrade() else {
                        return;
                    };
                    // 推进相位与发布必须分两段取锁：publish_langbar_icon_now 内部还要取
                    // 同一把锁，握着它调过去就是自锁。
                    {
                        let Ok(mut guard) = Coordinator::icon_publisher().lock() else {
                            return;
                        };
                        let Some(p) = guard.as_mut() else {
                            return;
                        };
                        p.advance_demo_frame();
                    }
                    c.publish_langbar_icon_now();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "演示动画驱动线程启动失败");
        }
    }

    /// 演示动画当前是否开着（菜单画勾用）。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn icon_demo_animation(&self) -> bool {
        Self::icon_publisher()
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|p| p.demo_animation()))
            .unwrap_or(false)
    }

    /// 把当前状态渲染成语言栏图标并投送共享内存。
    ///
    /// ⚠ **不要直接调用**：状态推送路径一律走 [`Self::status_with_icon_published`]，
    /// 那里保证了发布先于推送。直接调用的只有初始补发与调试菜单——它们不伴随状态推送。
    ///
    /// 失败一律只记日志：DLL 侧在读不到 SHM 时会退回本地 DirectWrite 绘制，
    /// 图标不会消失，只是不跟随标点状态——不值得为此中断状态推送。
    ///
    /// 返回是否**确实写了新位图**（`false` = 状态与上次相同已跳过，或发布器不可用）。
    /// 调用方据此决定要不要补一条刷新推送。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(super) fn publish_langbar_icon(&self, s: &StatusUpdateData) -> bool {
        use wind_ui::langbar_icon::{IconSpec, PunctBadge};

        let cell = Self::icon_publisher();

        // 与工具栏同口径：CapsLock 开启时中文模式实际在打英文（见 build_status 的
        // effective_chinese），此时不该显示中文标点角标。
        let effective_chinese = s.chinese_mode && !s.caps_lock;

        // 「不可输入」由协调器单点判定（见 InputBlock）。此前这一档在 DLL 本地算、
        // 本地绘制，服务端渲的图根本不参与——于是同一件事有两个负责者、各带一份迟滞。
        //
        // ⚠ 必须在取发布器锁**之前**算：它内部要取 state 与 gate 两把锁，持着发布器锁
        // 再去拿别的锁就是给自己留一条反向持有序。同类事故刚在 notify_toolbar 里发生过
        // （在 state 锁内调它 → Mutex 不可重入 → 当场卡死）。
        let block = self.effective_input_block();
        // ⚠ 与 `block` 同一条纪律：必须在取发布器锁**之前**算。`rt()` 要取运行时配置的
        // 读锁，持着发布器锁再去拿别的锁就是给自己留一条反向持有序。
        let english_label = self.rt().config.ui.labels.english_label();

        let Ok(mut guard) = cell.lock() else {
            return false;
        };
        let Some(p) = guard.as_mut() else {
            return false;
        };

        let spec = IconSpec {
            // 覆盖成英文标签而**不动 `icon_label` 本身**：后者是「当前方案标签」的单一语义，
            // 且会经 StatusUpdate 下发写进 TSF 的 `_inputTypeLabel`（持久值）。把这种随焦点
            // 来去的临时态烧进标签，离开时就得指望下一次状态推送改回来，漏一次即长期卡「英」。
            //
            // ⚠️ 这是本状态与工具栏那侧**处理方式不同**的原因：工具栏直接改 `icon_label`
            // （ToolbarState 不下发 TSF、无持久值），这里只能覆盖 spec。别把两边"统一"了。
            label: if block.shows_english() {
                english_label
            } else {
                s.icon_label.clone()
            },
            // 英文模式下标点恒为半角且不可切换（`toolbar.rs` 的渲染同样这么处理），
            // 角标此时没有信息量，故不画。
            punct: if !effective_chinese || block.shows_english() {
                PunctBadge::None
            } else if s.chinese_punct {
                PunctBadge::Chinese
            } else {
                PunctBadge::English
            },
            // 全角标记（右上角小方点）。与标点角标不同，它**不看 effective_chinese**：
            // 全半角在英文模式下同样生效（英文全角是真实可用的状态），所以只要是全角就画。
            full_width: s.full_width && !block.shows_english(),
            // 变淡**只留给线程级 KEYBOARD_DISABLED**（输入法整个被禁用）：那才配得上
            // 「输入法本身不可用」这种强呈现。无可编辑上下文是日常状态（点按钮/列表/桌面
            // 都会进），2026-08-04 实测让它变淡被否——图标频繁变灰，用户无从理解。
            dimmed: block.dims_icon(),
            // 相位取发布器持有的当前值，**不写死 0**：演示动画开着时，一次普通的状态推送
            // （切中英/切标点）也会走到这里，若在此归零，跑马灯每被状态变化打断一次就
            // 跳回起点。相位归发布器所有、只由动画定时器推进，是这两件事互不干扰的前提。
            frame: p.demo_frame(),
        };

        match p.publish(&spec) {
            // 记序号是排查「图标落后一帧」的唯一抓手：本行的时刻与 DLL 日志里
            // `GetIcon: from SHM` 的时刻一对，就能判断那次 GetIcon 取到的是第几版。
            // label / punct 都是模式状态（「中」「拼」这类短称），不含输入内容。
            Ok(Some(seq)) => {
                tracing::debug!(
                    seq,
                    label = %spec.label,
                    punct = ?spec.punct,
                    "语言栏图标已发布"
                );
                true
            }
            // 状态未变，SHM 里已经是这张图，跳过重渲。不记日志：状态推送远比状态
            // 变化频繁，每次都记会把这条日志变成噪声。
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(error = %e, "发布语言栏图标失败");
                false
            }
        }
    }
}

/// 配置层与渲染层各存了一份默认值，这里把它们钉在一起。
///
/// 起因是一处不可消除的重复：`wind-config` 不能反向依赖 `wind-ui`（层次颠倒），
/// 而本仓的配置约定要求每个可配置项都有具体默认值并在 `data/config.toml` 里完整列出
/// （守门测试强制）。于是同一个默认值必然写两遍。
///
/// 本 crate 同时依赖两边，是唯一能做这个比对的地方。漂移的症状——「设置页显示的默认
/// 与图标实际长的样子不一致」——不会自己暴露，只能靠测试拦。
#[cfg(all(test, feature = "desktop-ui", windows))]
mod default_parity_tests {
    use wind_config::LangBarConfig;
    use wind_ui::langbar_icon::{BadgeState, BadgeStyle, Corner, IconRenderer};

    /// `#RRGGBB` → BGR，与 `apply_langbar_config` 的换序保持一致。
    fn hex_to_bgr(s: &str) -> [u8; 3] {
        let [r, g, b, _] = wind_theme::palette::parse_hex(s).expect("默认色必须能解析");
        [b, g, r]
    }

    #[test]
    fn langbar_config_defaults_match_renderer() {
        let cfg = LangBarConfig::default();

        assert_eq!(
            BadgeStyle::from_id(&cfg.badge),
            BadgeStyle::default(),
            "配置默认总开关与 BadgeStyle::default() 不一致"
        );
        assert_eq!(
            cfg.badge_alpha,
            IconRenderer::DEFAULT_BADGE_ALPHA,
            "配置默认不透明度与渲染器不一致"
        );

        // 规则表逐条比对。比"两边条数一样"更进一步是必要的：漂移最可能的形态是
        // 某一条的颜色或角落被单独改掉，条数不变而表现变了。
        let rendered = IconRenderer::default_rules();
        assert_eq!(
            cfg.badges.len(),
            rendered.len(),
            "配置与渲染器的出厂规则条数不一致"
        );
        for (i, (c, r)) in cfg.badges.iter().zip(&rendered).enumerate() {
            assert!(c.enabled, "第 {i} 条出厂规则应是启用的");
            assert_eq!(
                BadgeState::from_id(&c.state),
                Some(r.state),
                "第 {i} 条规则的状态不一致"
            );
            assert_eq!(
                Corner::from_id(&c.corner),
                r.corner,
                "第 {i} 条规则的角落不一致"
            );
            assert_eq!(
                Some(hex_to_bgr(&c.color_light)),
                r.color_light.rgb,
                "第 {i} 条规则的浅色任务栏配色不一致"
            );
            assert_eq!(
                Some(hex_to_bgr(&c.color_dark)),
                r.color_dark.rgb,
                "第 {i} 条规则的深色任务栏配色不一致"
            );
            // 出厂色值都是 6 位 ⇒ 不指定不透明度、跟随全局。写死这条是为了让
            // 「有人给出厂色补了末两位」变成一次失败，而不是悄悄改变出厂画法。
            assert_eq!(
                r.color_light.alpha, None,
                "第 {i} 条出厂规则不该自带不透明度"
            );
            assert_eq!(
                r.color_dark.alpha, None,
                "第 {i} 条出厂规则不该自带不透明度"
            );
            assert_eq!(c.scale, r.scale, "第 {i} 条规则的条目倍率不一致");
        }
    }

    /// 角标默认关：它是加在系统图标上的新东西，默认改变所有人的任务栏是过界的。
    ///
    /// 单独钉一条而不是并进上面：上面那条防的是「两处默认值漂移」，这条防的是
    /// 「有人把默认改成开」——后者不是不一致，而是一个产品决定被悄悄推翻。
    #[test]
    fn badges_are_off_by_default() {
        assert_eq!(
            BadgeStyle::from_id(&LangBarConfig::default().badge),
            BadgeStyle::None,
            "角标总开关默认必须是关"
        );
    }
}
