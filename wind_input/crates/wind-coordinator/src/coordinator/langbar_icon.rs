//! 语言栏图标发布（Windows 桌面形态）：共享内存单例 + 状态角标。
//!（coordinator 子模块。）

use super::*;

/// 语言栏图标发布器（Windows 桌面形态）。
///
/// 进程级单例：共享内存名固定（`Local\WindInput_IconShm{_dev}`），资源本身进程级唯一。
///
/// 内层 `None` = 创建失败，非致命：DLL 读不到 SHM 会退回本地 DirectWrite 绘制，只是没有角标。
#[cfg(all(feature = "desktop-ui", windows))]
static ICON_PUBLISHER: std::sync::OnceLock<
    std::sync::Mutex<Option<wind_ui::langbar_icon::LangBarIconPublisher>>,
> = std::sync::OnceLock::new();

/// 主字色的两份来源：（用户配置解析结果，主题 token）。合并见 [`effective_text_colors`]。
///
/// 独立于发布器存放：`push_theme` 在构造期就会调，那时为记主题色去建发布器会先发一张没套用户
/// 配置的图。两份各由 `apply_langbar_config` / `push_theme` 写，任一变化按合并结果重发。
#[cfg(all(feature = "desktop-ui", windows))]
static LANGBAR_TEXT_SOURCES: std::sync::Mutex<(
    wind_theme::LangbarTextColors,
    wind_theme::LangbarTextColors,
)> = std::sync::Mutex::new((NO_TEXT_COLORS, NO_TEXT_COLORS));

#[cfg(all(feature = "desktop-ui", windows))]
const NO_TEXT_COLORS: wind_theme::LangbarTextColors = wind_theme::LangbarTextColors {
    cn_light: None,
    cn_dark: None,
    en_light: None,
    en_dark: None,
};

/// 主字色单点合并：用户非空 > 主题 Some > 内置黑白，四格各自独立回落。渲染器不再自行回落。
#[cfg(all(feature = "desktop-ui", any(windows, test)))]
pub(crate) fn effective_text_colors(
    user: &wind_theme::LangbarTextColors,
    theme: &wind_theme::LangbarTextColors,
) -> wind_ui::langbar_icon::TextColors {
    use wind_ui::langbar_icon::TextColors;
    let pick =
        |u: Option<[u8; 4]>, t: Option<[u8; 4]>, builtin: [u8; 4]| u.or(t).unwrap_or(builtin);
    TextColors {
        cn_light: pick(user.cn_light, theme.cn_light, TextColors::BUILTIN_LIGHT),
        cn_dark: pick(user.cn_dark, theme.cn_dark, TextColors::BUILTIN_DARK),
        en_light: pick(user.en_light, theme.en_light, TextColors::BUILTIN_LIGHT),
        en_dark: pick(user.en_dark, theme.en_dark, TextColors::BUILTIN_DARK),
    }
}

/// 解析 `[ui.langbar] text_color_*`：`""` = 跟随主题（None）；非法色值记警告并只让这一格
/// 回落跟随主题。
#[cfg(all(feature = "desktop-ui", any(windows, test)))]
pub(crate) fn parse_user_text_colors(
    cfg: &wind_config::LangBarConfig,
) -> wind_theme::LangbarTextColors {
    let parse = |raw: &str, item: &str| -> Option<[u8; 4]> {
        let t = raw.trim();
        if t.is_empty() {
            return None;
        }
        let c = wind_theme::palette::parse_hex(t);
        if c.is_none() {
            tracing::warn!(value = raw, item, "语言栏主字色无法解析，按跟随主题处理");
        }
        c
    };
    wind_theme::LangbarTextColors {
        cn_light: parse(&cfg.text_color_cn_light, "text_color_cn_light"),
        cn_dark: parse(&cfg.text_color_cn_dark, "text_color_cn_dark"),
        en_light: parse(&cfg.text_color_en_light, "text_color_en_light"),
        en_dark: parse(&cfg.text_color_en_dark, "text_color_en_dark"),
    }
}

/// 一条角标规则某一侧（light/dark）的配置 → 渲染侧的着色。
///
/// - `""`（含解析失败）= 与主字同色；`#RRGGBB` = 指定色相（RGB → BGR）；
/// - `alpha` 原样透传：`None` = 跟随全局 `badge_alpha`，`Some(1.0)` 会切挖空档。
///
/// 定制层未迁移的旧写法只读兼容：`auto` 同 `""`；`#RRGGBBAA` 的末两位只在 `alpha` 为 `None`
/// 时采用（显式新键优先）。判 8 位要看原字符串长度，`parse_hex` 会把 6 位补成 `a = 255`。
///
/// 解析失败只让色相回落「与主字同色」并记警告，`alpha` 照旧生效。
#[cfg(all(feature = "desktop-ui", any(windows, test)))]
pub(crate) fn badge_color_of(color: &str, alpha: Option<f32>) -> wind_ui::langbar_icon::BadgeColor {
    use wind_ui::langbar_icon::BadgeColor;
    let t = color.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("auto") {
        return BadgeColor { rgb: None, alpha };
    }
    match wind_theme::palette::parse_hex(t) {
        Some([r, g, b, a]) => BadgeColor {
            rgb: Some([b, g, r]),
            alpha: alpha
                .or_else(|| (t.trim_start_matches('#').len() == 8).then_some(a as f32 / 255.0)),
        },
        None => {
            tracing::warn!(value = color, "语言栏角标配色无法解析，按与主字同色处理");
            BadgeColor { rgb: None, alpha }
        }
    }
}

/// 演示动画的代际。开/关各 +1，驱动线程每帧核对自己那一代是否仍是当前值，不是就退出。
///
/// 用代际而非停止标志：菜单连点时旧线程尚未退出，标志位会让新旧线程都继续推进相位。
#[cfg(all(feature = "desktop-ui", windows))]
static DEMO_ANIM_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 演示动画帧间隔：一圈 40 帧（`IconRenderer::DEMO_FRAMES_PER_CYCLE`）约 3.2 秒。每帧要重渲
/// 全部档位并跨进程推送，这是 Dev 调试玩具，不为流畅度加码。
#[cfg(all(feature = "desktop-ui", windows))]
const DEMO_ANIM_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

impl Coordinator {
    /// 服务启动后发布一次初始图标。
    ///
    /// [`Self::push_state_update`] 只在状态变化时调用，少了这次补发，首次切换前 SHM 一直是空的、
    /// 图标没有角标。
    ///
    /// 非 Windows 桌面形态下是空操作，故调用方无需自己加 cfg。
    pub fn publish_initial_langbar_icon(&self) {
        // 先套配置再发布，否则开机先发一张代码默认图、图标闪一下。apply 无变化时不重发，故仍要
        // 显式发布一次兜住首发。
        #[cfg(all(feature = "desktop-ui", windows))]
        self.apply_langbar_config();
        self.publish_langbar_icon_now();
    }

    /// 按当前状态立即重渲并发布图标，并让 DLL 重取一次。
    ///
    /// 「位图变了但状态没变」的专用入口（调试菜单、演示动画）：没有状态推送，DLL 也不会
    /// `OnUpdate`，故须自己补一条 [`CMD_REFRESH_ICON`]。只在确实写了新位图时才推。
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
    /// 调试菜单要在发布之外访问它，初始化逻辑只该有一份。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn icon_publisher()
    -> &'static std::sync::Mutex<Option<wind_ui::langbar_icon::LangBarIconPublisher>> {
        use wind_ui::langbar_icon::{BadgeStyle, LangBarIconPublisher};
        ICON_PUBLISHER.get_or_init(|| {
            let suffix = wind_config::variant::pipe_suffix();
            match LangBarIconPublisher::new(suffix, BadgeStyle::default()) {
                Ok(mut p) => {
                    tracing::info!(shm = p.shm_name(), "语言栏图标共享内存已就绪");
                    // 纯调试项从 state.toml 恢复（`None` 保留代码默认）；呈现参数走 config，
                    // 由 apply_langbar_config 套用。
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
    /// 只用于纯调试项（尺寸档标记）。用户可见的呈现参数（形状、配色、大小、透明度）一律走
    /// `[ui.langbar]` 配置，由 [`Self::apply_langbar_config`] 落地，避免两个真相源。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn tweak_langbar_icon(
        &self,
        f: impl FnOnce(&mut wind_ui::langbar_icon::LangBarIconPublisher),
    ) {
        if let Ok(mut guard) = Self::icon_publisher().lock()
            && let Some(p) = guard.as_mut()
        {
            f(p);
            // 经 `state_writer` 落盘：state.toml 多方共用，各写各的会互相吞更新。
            let marks = p.size_marks();
            self.state_writer.schedule("langbar_icon", move |rs| {
                rs.langbar_icon_size_marks = Some(marks);
            });
        }
        // 锁已在块尾释放：发布内部还要取同一把锁。
        self.publish_langbar_icon_now();
    }

    /// 调试菜单改呈现参数：写进用户配置并热重载（这些量本就用户可配，不写 state.toml）。
    /// `set_user_value` 会删掉等于出厂默认的值，点回默认不会在用户层钉死显式值。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn set_langbar_config(&self, key: &str, value: toml::Value) {
        if let Err(e) = Config::set_user_value(&["ui", "langbar", key], value) {
            tracing::warn!(error = %e, key, "语言栏图标配置落盘失败");
            return;
        }
        // 走完整热重载保证内存与文件一致；重载内部会调 apply_langbar_config 重渲重发。
        self.reload_user_config();
    }

    /// 取当前状态，并在返回之前把对应的图标位图投进共享内存。
    ///
    /// DLL 收到状态推送后会 `OnUpdate(TF_LBI_ICON)`、随即 `GetIcon` 读 SHM，新位图必须已在里面；
    /// 先推送后发布是跨进程竞态，表现为切换偶尔不生效。把发布藏进「取状态」，让调用方拿不到
    /// 未发布的 status，顺序由数据依赖保证。
    ///
    /// 非 Windows 桌面形态下退化为 [`Self::build_status`]。
    pub(crate) fn status_with_icon_published(&self) -> StatusUpdateData {
        let s = self.build_status();
        #[cfg(all(feature = "desktop-ui", windows))]
        self.publish_langbar_icon(&s);
        s
    }

    /// 把 `[ui.langbar]` 的呈现参数套到发布器上，有变化就重渲重发。
    ///
    /// 构造后与每次配置重载都要调一次，否则「改了没反应、重启才生效」。
    ///
    /// 单项解析失败只记警告并按字段降级，不整段回落：颜色退到与主字同色、位置退到右下角，
    /// 状态没有默认值，不认识就丢掉整条规则。`alpha_light/dark` 只透传，与全局 `badge_alpha`
    /// 的合流在渲染侧 `active_layers`。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn apply_langbar_config(&self) {
        use wind_ui::langbar_icon::{BadgeRule, BadgeState, BadgeStyle, Corner};

        let cfg = { self.rt().config.ui.langbar.clone() };

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
                    color_light: badge_color_of(&b.color_light, b.alpha_light),
                    color_dark: badge_color_of(&b.color_dark, b.alpha_dark),
                    scale: b.scale,
                })
            })
            .collect();

        // 主字色：记下用户这一份，与主题那份合并。先取完来源锁再取发布器锁，不嵌套。
        let text_colors = {
            let mut src = LANGBAR_TEXT_SOURCES
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            src.0 = parse_user_text_colors(&cfg);
            effective_text_colors(&src.0, &src.1)
        };

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
                Some(text_colors),
            )
        };
        // 锁已释放——发布内部还要取同一把锁。
        if changed {
            self.publish_langbar_icon_now();
        }
    }

    /// 主题换了（切主题 / 切明暗 / 重载）：记下主题给的主字色，有变化就重渲重发。
    ///
    /// 发布器尚未创建（构造期的首次 `push_theme`）时只记不发，首发由
    /// [`Self::publish_initial_langbar_icon`] 负责并合入这份主题色。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn set_langbar_theme_text(&self, theme: wind_theme::LangbarTextColors) {
        let text_colors = {
            let mut src = LANGBAR_TEXT_SOURCES
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            src.1 = theme;
            effective_text_colors(&src.0, &src.1)
        };
        let Some(cell) = ICON_PUBLISHER.get() else {
            return;
        };
        let changed = {
            let Ok(mut guard) = cell.lock() else {
                return;
            };
            let Some(p) = guard.as_mut() else {
                return;
            };
            p.apply_appearance(None, None, None, None, Some(text_colors))
        };
        // 锁已释放——发布内部还要取同一把锁。
        if changed {
            self.publish_langbar_icon_now();
        }
    }

    /// 翻转演示动画（外圈跑马灯）开关，并起停驱动线程。
    ///
    /// 刻意不落盘：它是持续占用 CPU 与 IPC 的演示，不是呈现偏好，重启后应自动关掉。
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

        // 先改代际再发布，让上一个驱动线程作废、不再抢相位；关闭时这一发负责抹掉跑马灯。
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
                    // 关掉后最多再睡一帧就退出，不需要唤醒机制。
                    if DEMO_ANIM_GEN.load(Ordering::Acquire) != generation {
                        return;
                    }
                    // 服务正在退出则收摊。
                    let Some(c) = weak.upgrade() else {
                        return;
                    };
                    // 推进相位与发布分两段取锁：发布内部还要取同一把锁。
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
    /// 状态推送路径一律走 [`Self::status_with_icon_published`]（保证发布先于推送），直接调用的
    /// 只有初始补发与调试菜单。失败只记日志：DLL 会退回本地绘制。
    ///
    /// 返回是否确实写了新位图（`false` = 与上次相同已跳过，或发布器不可用），调用方据此决定
    /// 要不要补一条刷新推送。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(super) fn publish_langbar_icon(&self, s: &StatusUpdateData) -> bool {
        use wind_ui::langbar_icon::{IconSpec, PunctBadge};

        let cell = Self::icon_publisher();

        // 与工具栏同口径：CapsLock 开启时中文模式实际在打英文（见 build_status 的
        // effective_chinese），此时不该显示中文标点角标。
        let effective_chinese = s.chinese_mode && !s.caps_lock;

        // 「不可输入」由协调器单点判定（见 InputBlock）。下面两项都必须在取发布器锁之前算：
        // 它们内部要取 state / gate / 运行时配置的锁，持着发布器锁再取会形成反向持有序。
        let block = self.effective_input_block();
        let english_label = self.rt().config.ui.labels.english_label();

        let Ok(mut guard) = cell.lock() else {
            return false;
        };
        let Some(p) = guard.as_mut() else {
            return false;
        };

        let spec = IconSpec {
            // 只覆盖 spec 而不动 `icon_label`：后者会经 StatusUpdate 写进 TSF 的
            // `_inputTypeLabel`（持久值），烧进随焦点来去的临时态会长期卡「英」。工具栏那侧
            // 直接改 `icon_label` 是因为它不下发 TSF，别把两边统一。
            label: if block.shows_english() {
                english_label
            } else {
                s.icon_label.clone()
            },
            // 英文模式下标点恒为半角，角标没有信息量，不画（同 `toolbar.rs`）。
            punct: if !effective_chinese || block.shows_english() {
                PunctBadge::None
            } else if s.chinese_punct {
                PunctBadge::Chinese
            } else {
                PunctBadge::English
            },
            // 全角标记不看 effective_chinese：英文模式下全角同样生效。
            full_width: s.full_width && !block.shows_english(),
            // 变淡只留给线程级 KEYBOARD_DISABLED。无可编辑上下文是日常状态（点按钮、桌面都会
            // 进），让它变淡会使图标频繁变灰。
            dimmed: block.dims_icon(),
            // 相位取发布器当前值而非 0：演示动画开着时普通状态推送也走这里，归零会让跑马灯跳回起点。
            frame: p.demo_frame(),
            // 主字显示英文态标签（非有效中文态 / 不可输入态，CapsLock 已并进
            // effective_chinese）时取英文那两格主字色。
            english: !effective_chinese || block.shows_english(),
        };

        match p.publish(&spec) {
            // 序号与 DLL 日志的 `GetIcon: from SHM` 对时，可判断 GetIcon 取到的是第几版。
            // label / punct 是模式短称，不含输入内容。
            Ok(Some(seq)) => {
                tracing::debug!(
                    seq,
                    label = %spec.label,
                    punct = ?spec.punct,
                    "语言栏图标已发布"
                );
                true
            }
            // 状态未变，跳过重渲；不记日志，状态推送远比状态变化频繁。
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
/// `wind-config` 不能反向依赖 `wind-ui`，同一个默认值必然写两遍；本 crate 同时依赖两边，
/// 是唯一能做比对的地方。
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

        // 逐条比对：漂移最可能是某一条的颜色或角落被单独改掉。
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
            // 出厂色值都是 6 位，不指定不透明度、跟随全局。
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

    /// 角标默认关（产品决定：不默认改变所有人的任务栏）。与上一条分开，它防的不是漂移。
    #[test]
    fn badges_are_off_by_default() {
        assert_eq!(
            BadgeStyle::from_id(&LangBarConfig::default().badge),
            BadgeStyle::None,
            "角标总开关默认必须是关"
        );
    }
}

#[cfg(all(test, feature = "desktop-ui"))]
mod text_color_tests {
    use super::{effective_text_colors, parse_user_text_colors};
    use wind_theme::LangbarTextColors;
    use wind_ui::langbar_icon::TextColors;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    /// 三档优先级：用户 > 主题 > 内置，且每格独立回落。
    #[test]
    fn user_over_theme_over_builtin_per_cell() {
        let user = LangbarTextColors {
            cn_light: Some(RED),
            ..Default::default()
        };
        let theme = LangbarTextColors {
            cn_light: Some(GREEN),
            en_dark: Some(GREEN),
            ..Default::default()
        };
        let got = effective_text_colors(&user, &theme);
        assert_eq!(got.cn_light, RED, "用户非空压过主题");
        assert_eq!(got.en_dark, GREEN, "用户空则取主题");
        assert_eq!(got.cn_dark, TextColors::BUILTIN_DARK, "都没给回落内置白");
        assert_eq!(got.en_light, TextColors::BUILTIN_LIGHT, "都没给回落内置黑");
    }

    /// 全空 = 改动前的写死黑白。
    #[test]
    fn nothing_configured_is_legacy_black_white() {
        let none = LangbarTextColors::default();
        assert_eq!(effective_text_colors(&none, &none), TextColors::default());
    }

    /// 用户色：空串跟随主题；合法值（含 8 位带 alpha）照用；非法值只让那一格回落主题。
    #[test]
    fn user_colors_parse_and_invalid_falls_back_to_theme() {
        let cfg = wind_config::LangBarConfig {
            text_color_cn_light: "#FF0000".into(),
            text_color_cn_dark: "not-a-color".into(),
            text_color_en_light: " #00FF0080 ".into(),
            text_color_en_dark: String::new(),
            ..Default::default()
        };
        let user = parse_user_text_colors(&cfg);
        assert_eq!(user.cn_light, Some(RED));
        assert_eq!(user.cn_dark, None, "非法色值按跟随主题");
        assert_eq!(user.en_light, Some([0, 255, 0, 0x80]));
        assert_eq!(user.en_dark, None);

        let theme = LangbarTextColors {
            cn_dark: Some(GREEN),
            ..Default::default()
        };
        let got = effective_text_colors(&user, &theme);
        assert_eq!(got.cn_dark, GREEN, "非法那一格回落到主题值");
        assert_eq!(got.cn_light, RED, "其余格不受牵连");
    }
}

#[cfg(all(test, feature = "desktop-ui"))]
mod badge_color_tests {
    use super::badge_color_of;
    use wind_ui::langbar_icon::BadgeColor;

    #[test]
    fn badge_color_of_empty_is_auto() {
        assert_eq!(badge_color_of("", None), BadgeColor::AUTO);
    }

    #[test]
    fn badge_color_of_rgb6_carries_alpha_param() {
        let c = badge_color_of("#112233", Some(0.5));
        assert_eq!(c.rgb, Some([0x33, 0x22, 0x11]), "RGB → BGR");
        assert_eq!(c.alpha, Some(0.5));
        assert_eq!(
            badge_color_of("#112233", None).alpha,
            None,
            "未设置 = 跟随全局"
        );
    }

    #[test]
    fn badge_color_of_empty_keeps_alpha_param() {
        assert_eq!(
            badge_color_of("", Some(1.0)),
            BadgeColor {
                rgb: None,
                alpha: Some(1.0)
            },
            "与主字同色也能单独设不透明度（含切挖空档）"
        );
    }

    /// 定制层里没迁移的旧写法仍能读：`auto` 与 8 位色值（显式 alpha 键优先）。
    #[test]
    fn badge_color_of_reads_legacy_forms() {
        assert_eq!(badge_color_of("auto", None), BadgeColor::AUTO);
        assert_eq!(badge_color_of(" AUTO ", Some(0.3)).alpha, Some(0.3));
        let c = badge_color_of("#112233FF", None);
        assert_eq!(c.rgb, Some([0x33, 0x22, 0x11]));
        assert_eq!(c.alpha, Some(1.0), "未写 alpha 键时用 8 位自带的不透明度");
        assert_eq!(
            badge_color_of("#112233FF", Some(0.5)).alpha,
            Some(0.5),
            "显式 alpha 键优先，与迁移的取舍一致"
        );
    }

    #[test]
    fn badge_color_of_invalid_falls_back_to_follow() {
        assert_eq!(
            badge_color_of("not-a-color", Some(0.5)),
            BadgeColor {
                rgb: None,
                alpha: Some(0.5)
            }
        );
    }
}
