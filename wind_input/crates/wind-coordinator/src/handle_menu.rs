//! 功能菜单与工具栏
//!
//! 主菜单 / 候选右键菜单的构建与分派、工具栏点击/刷新/位置持久化。

use crate::coordinator::ToolbarPush;
use crate::coordinator::{Coordinator, FILTER_MODES};
use crate::theme_style::ThemeStyle;
use wind_bridge::handler::MessageHandler;
use wind_config::Config;
use wind_keys::keymap;
use wind_ui_types::ToolbarState;
use wind_ui_types::{
    CandidateOp, MenuAnchor, MenuCmd, MenuKind, PIN_AT_MAX, ToolbarAction, UiCommand,
};

/// 候选窗定位方式与坐标：菜单一起写，写回目标也按这一组判（见 `menu_writeback_target`）。
const CANDIDATE_POS_FIELDS: [&str; 3] = ["candidate_position_mode", "candidate_x", "candidate_y"];
/// 状态气泡定位方式与坐标，同上。
const STATUS_POS_FIELDS: [&str; 3] = ["status_position_mode", "status_x", "status_y"];

/// 菜单打开后的焦点事件豁免期，见 [`Coordinator::menu_close_on_focus_change`]。
///
/// 下界须盖住跨宿主切换时旧宿主 focus_lost 迟到的约 100ms，上界须远短于用户「点开菜单 →
/// 切走窗口」的最短间隔（几百毫秒）。
pub(crate) const MENU_FOCUS_GUARD: std::time::Duration = std::time::Duration::from_millis(250);

/// Linux 自绘菜单的空闲超时：这么久没有任何菜单操作（打开 / 菜单键 / 指针）就当它已经没了。
///
/// 服务端最后一道兜底，不是正常关闭路径：addon 自己有同样时长的空闲计时，这里防的是它的
/// `menu.dismiss` 报告丢了、`menu_open` 卡在 true 后方向键 / 回车 / Esc 永远被吞（见
/// `docs/design/linux-port.md` §7）。判据放在下一个按键到来时，超时后的第一个键照常处理。
#[cfg(ext_presenter)]
pub(crate) const MENU_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// [`MENU_IDLE_TIMEOUT`] 的判据（纯函数，便于单测）。没有记录时不算超时——打开菜单必然会记。
#[cfg(ext_presenter)]
pub(crate) fn menu_idle_expired(
    touched_at: Option<std::time::Instant>,
    now: std::time::Instant,
    timeout: std::time::Duration,
) -> bool {
    touched_at.is_some_and(|t| now.saturating_duration_since(t) >= timeout)
}

/// 解好的扩展信封 `menu.open`（Linux addon 请求打开自绘菜单），见
/// [`Coordinator::open_menu_from_host`]。
#[cfg(ext_presenter)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MenuOpenRequest {
    /// ≥ 0：候选页内下标；负值见 `wind_ipc::protocol::menu_target`。
    pub(crate) target: i32,
    /// 菜单锚点（屏幕坐标）。
    pub(crate) x: i32,
    pub(crate) y: i32,
    /// 锚点所在工作区 (左, 上, 右, 下)；全 0 = 没报。
    pub(crate) work: [i32; 4],
    /// 右键点在悬停提示位图内的坐标（仅悬停提示菜单带）。
    pub(crate) local: Option<(i32, i32)>,
}

/// `MenuCmd::ToggleToolbar` 的菜单文案，分平台：Windows / Linux 外部宿主显隐的是悬浮工具栏；
/// macOS 下 `UpdateToolbar` 最终落到 `ModeStatusController` 的 `NSStatusItem.isVisible`，
/// 显隐的是菜单栏里的中/英状态图标，压根没有悬浮工具栏。
///
/// 常量而非各处字面量：三处菜单（IMK 输入源 / 候选框右键 / 状态指示器下拉）必须字字一致。
pub(crate) const TOOLBAR_MENU_LABEL: &str = if cfg!(target_os = "macos") {
    "显示状态图标"
} else {
    "显示工具栏"
};

/// 把 (键, 值) 列表拼成设置程序的附加参数串（`--k=v`，空格分隔）。值为空的项跳过
/// （设置端把空串和没传当同一回事）。
///
/// 值含空白时加双引号：参数串经 `ShellExecuteW` 交给目标进程、由 `CommandLineToArgvW`
/// 重新切分，不加引号 `--text=你 好` 会被拆成两个 argv。引号在切分时剥掉。
pub(crate) fn build_settings_args(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (k, v) in pairs {
        if v.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        if v.contains(char::is_whitespace) {
            out.push_str(&format!("--{k}=\"{v}\""));
        } else {
            out.push_str(&format!("--{k}={v}"));
        }
    }
    out
}

/// 把 `page` + [`build_settings_args`] 产出的参数串还原成设置程序的 argv。
///
/// IPC 传的是结构化 argv，故要把参数串切回一个个词。切词放在本文件，与加引号的
/// `build_settings_args` 是一对，引号约定必须同源。仅认双引号、不认转义：值来自本进程
/// 内部拼装，不含引号字面量。
///
/// 仅 ext_presenter 走 IPC argv 通路，但引号往返的单测要在所有平台上跑，故不 `cfg` 掉，
/// 只在其余平台豁免 dead_code。
#[cfg_attr(not(ext_presenter), allow(dead_code))]
pub(crate) fn settings_argv(page: Option<&str>, extra: &str) -> Vec<String> {
    let mut argv = Vec::new();
    if let Some(p) = page {
        argv.push(format!("--page={p}"));
    }
    let (mut cur, mut quoted, mut started) = (String::new(), false, false);
    for ch in extra.chars() {
        if ch == '"' {
            quoted = !quoted;
            started = true;
        } else if !quoted && ch.is_whitespace() {
            if started {
                argv.push(std::mem::take(&mut cur));
            }
            cur.clear();
            started = false;
        } else {
            cur.push(ch);
            started = true;
        }
    }
    if started {
        argv.push(cur);
    }
    argv
}

/// 组装设置程序的完整命令行参数串。
///
/// 附加参数不依附于页（`--dark` / `--soft` 没有页也有意义），`page=None` 时仍原样带上。
/// ext_presenter 走 IPC argv、无命令行概念，故仅其余平台使用。
#[cfg(not(ext_presenter))]
pub(crate) fn settings_cmdline(page: Option<&str>, extra: &str) -> String {
    let mut out = String::new();
    if let Some(p) = page {
        out.push_str("--page ");
        out.push_str(p);
    }
    if !extra.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(extra);
    }
    out
}

/// 全屏探测单飞闸的归还句柄：`Drop` 里必定把闸放回去。
///
/// 闸取在一条线程、还在另一条线程，中间夹着跨进程阻塞查询与一段多早退分支的通知逻辑；
/// 归还点散在各分支上，漏一个就让此后所有探测 early return、全屏缓存永久冻结。收进 `Drop`
/// 后归还点唯一。它不是 panic 安全：release profile 是 `panic = "abort"`，`Drop` 不执行。
/// 持有 `Arc` 而不是 `&AtomicBool`：要跨到 `spawn` 出去的线程里归还。
pub(crate) struct ProbeGate(std::sync::Arc<Coordinator>);

impl Drop for ProbeGate {
    fn drop(&mut self) {
        self.0
            .fullscreen_probing
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl Coordinator {
    /// 候选窗空白处被双击（`UiEvent::CandidateDoubleClick`）：开关打开时截候选窗到剪贴板。
    ///
    /// 与右键菜单「截图到剪贴板」走同一条 UI 命令，Toast 与失败文案也就同一套。
    pub(crate) fn on_candidate_double_click(&self) {
        if self.rt().config.ui.candidate.double_click_screenshot {
            let _ = self.ui_tx.send(UiCommand::ScreenshotCandidateToClipboard);
        }
    }

    /// 菜单项激活：UI 已自管导航/子菜单，这里仅按动作派发。
    pub(crate) fn menu_action(&self, kind: MenuKind) {
        let (page_local, text) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.menu_target_page_local, s.menu_target_text.clone())
        };
        self.menu_close();
        match kind {
            MenuKind::Op(op) => self.candidate_or_quick_format_op(op, page_local),
            MenuKind::Copy => {
                let _ = self.ui_tx.send(UiCommand::CopyToClipboard(text));
            }
            MenuKind::Command(cmd) => self.run_menu_cmd(cmd),
            MenuKind::Submenu | MenuKind::Separator | MenuKind::Label => {}
        }
        // 派发完再解除 tooltip 抑制：Tooltip 截图命令必须先于本次解除被处理，
        // 否则 tooltip 会在截图前被隐藏，见 clear_tooltip_menu_flag。
        self.clear_tooltip_menu_flag();
    }

    /// 执行功能主菜单命令
    pub(crate) fn run_menu_cmd(&self, cmd: MenuCmd) {
        match cmd {
            MenuCmd::SchemaEnglish => {
                // ctrl_held=false：菜单点的。它只被 `ignore_host_ime_switch` 消费，
                // 那条规则只拦 compartment 来源，菜单永远照办。
                self.handle_system_mode_switch(
                    false,
                    wind_ipc::protocol::ModeSwitchSource::Menu,
                    false,
                );
                self.notify_toolbar();
                self.notify_ui_hide();
            }
            MenuCmd::SchemaSelect(i) => self.select_schema(i),
            MenuCmd::TogglePunct => {
                self.handle_menu_command("toggle_punct");
                self.notify_toolbar();
            }
            MenuCmd::ToggleWidth => {
                self.handle_menu_command("toggle_width");
                self.notify_toolbar();
            }
            MenuCmd::ToggleSoftKeyboard => {
                self.softkeyboard_menu_toggle(None);
                self.after_softkeyboard_change();
            }
            // 分格快捷菜单末尾的「更多…」：弹完整主菜单，锚点取光标位（`i32::MIN` 由 UI
            // 侧解释成当前鼠标处），用户刚在那里点过。
            MenuCmd::OpenMainMenu => {
                #[cfg(all(target_os = "linux", ext_presenter))]
                self.send_host_work_area();
                self.show_main_menu(wind_ui_types::MenuAnchor::at_point(i32::MIN, i32::MIN));
            }
            MenuCmd::SoftKeyboardPage(i) => {
                // 菜单选面：面板没开就顺带开出来，否则「选了个面却什么都没发生」。
                if !self.softkeyboard_is_open() {
                    self.softkeyboard_menu_toggle(None);
                    // 开面板这一步自己负责收口，不能指望下面那句：`ui_softkeyboard_page` 下标越界
                    // （菜单按构建时的面表列，配置重载后可能少一面）时只 `warn!` 返回，面板已开却
                    // 不推 STATUS_SOFT_KEYBOARD 位、工具栏不亮。谁改了状态谁收口，两处推送都幂等。
                    self.after_softkeyboard_change();
                }
                self.ui_softkeyboard_page(i);
            }
            MenuCmd::ToggleS2t => {
                self.handle_menu_command("toggle_s2t");
                self.notify_toolbar();
            }
            MenuCmd::FilterMode(i) => self.set_filter_mode(i),
            MenuCmd::ThemeSelect(i) => self.select_theme(i),
            MenuCmd::ThemeStyle(style) => self.set_theme_style(style),
            MenuCmd::ToggleToolbar => self.toggle_toolbar(),
            MenuCmd::ReloadConfig => {
                self.reload_user_config();
            }
            MenuCmd::RestartService => self.restart_service(),
            MenuCmd::OpenSettings => self.open_settings(None),
            MenuCmd::OpenDictionary => self.open_dictionary(),
            MenuCmd::OpenAbout => self.open_settings(Some("about")),
            MenuCmd::TakeScreenshot => {
                if let Some(dir) = screenshots_dir() {
                    let _ = self.ui_tx.send(UiCommand::TakeScreenshot { dir });
                }
            }
            MenuCmd::ScreenshotCandidateToClipboard => {
                let _ = self.ui_tx.send(UiCommand::ScreenshotCandidateToClipboard);
            }
            MenuCmd::OpenConfigDir => self.open_dir(Config::user_config_dir()),
            MenuCmd::OpenAppDir => self.open_dir(
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf())),
            ),
            MenuCmd::OpenLogDir => self.open_dir(Config::log_dir()),
            MenuCmd::ToggleInputDiagnostics => self.toggle_input_diag_hud(),
            MenuCmd::TogglePasswordSuppress => self.toggle_password_suppress(),
            MenuCmd::FirstShowMode(m) => self.set_first_show_mode(m),
            MenuCmd::AutoPairRule(m) => self.set_auto_pair_rule(m),
            MenuCmd::CompatRuleEnabled(m) => self.set_compat_rule_enabled(m == 1),
            MenuCmd::CandidatePositionRule(m) => self.set_candidate_position_rule(m),
            MenuCmd::IgnoreHostImeSwitchRule(m) => self.set_ignore_host_ime_switch_rule(m),
            MenuCmd::PasswordForceEnglishRule(m) => self.set_password_force_english_rule(m),
            MenuCmd::AppSchemaRule(m) => self.set_app_schema_rule(m),
            MenuCmd::StatusPositionRule(m) => self.set_status_position_rule(m),
            MenuCmd::StatusFallbackRule(m) => self.set_status_fallback_rule(m),
            MenuCmd::InitialMode(m) => self.set_initial_state_rule(false, m),
            MenuCmd::InitialPunct(m) => self.set_initial_state_rule(true, m),
            MenuCmd::StatusToggleAlways => self.status_toggle_always(),
            MenuCmd::StatusToggleShowOnFocus => self.status_toggle_show_on_focus(),
            MenuCmd::StatusTogglePinned => self.status_toggle_pinned(),
            MenuCmd::StatusResetPosition => self.status_reset_position(),
            MenuCmd::StatusScreenshot => {
                if let Some(dir) = screenshots_dir() {
                    let _ = self.ui_tx.send(UiCommand::ScreenshotStatusTip {
                        dir: std::path::PathBuf::from(dir),
                    });
                }
            }
            MenuCmd::TooltipCopy
            | MenuCmd::TooltipCopySection
            | MenuCmd::TooltipCommitSection
            | MenuCmd::TooltipCopyLine
            | MenuCmd::TooltipCommitLine => self.tooltip_menu_action(cmd),
            MenuCmd::InputDiagCopy => {
                let _ = self.ui_tx.send(UiCommand::CopyInputDiagText);
            }
            MenuCmd::InputDiagToggleSection(i) => self.toggle_input_diag_section(i),
            MenuCmd::InputDiagToggleFreeze => self.toggle_input_diag_freeze(),
            MenuCmd::InputDiagToggleTopmost => self.toggle_input_diag_topmost(),
            MenuCmd::TooltipScreenshot => {
                if let Some(dir) = screenshots_dir() {
                    let _ = self.ui_tx.send(UiCommand::ScreenshotTooltip {
                        dir: std::path::PathBuf::from(dir),
                    });
                }
            }
            // 语言栏图标：总开关写用户配置（`[ui.langbar]`），纯调试的两项走 state.toml / 内存，
            // 见 set_langbar_config。非 Windows 桌面形态下菜单项不会被构建，故是空操作。
            #[cfg(all(feature = "desktop-ui", windows))]
            MenuCmd::IconBadgeStyle(i) => {
                let id = wind_ui::langbar_icon::BadgeStyle::from_index(i).as_id();
                self.set_langbar_config("badge", toml::Value::String(id.to_string()));
            }
            #[cfg(all(feature = "desktop-ui", windows))]
            MenuCmd::IconToggleSizeMarks => {
                let on = !self.icon_debug_state().map(|s| s.1).unwrap_or(false);
                self.tweak_langbar_icon(|p| p.set_size_marks(on));
            }
            #[cfg(all(feature = "desktop-ui", windows))]
            MenuCmd::IconToggleDemoAnim => self.toggle_icon_demo_animation(),
            MenuCmd::ToggleCaretOverlay => self.toggle_caret_overlay(),
            #[cfg(not(all(feature = "desktop-ui", windows)))]
            MenuCmd::IconBadgeStyle(_)
            | MenuCmd::IconToggleSizeMarks
            | MenuCmd::IconToggleDemoAnim => {}
        }
    }

    /// 图标发布器当前的呈现参数 `(总开关档位下标, 是否烧尺寸档标记)`；
    /// 发布器不可用时返回 `None`。
    ///
    /// 勾选态读渲染器实际生效的值而不是配置文件：读配置会在热重载失败时显示一个并未
    /// 生效的勾。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn icon_debug_state(&self) -> Option<(u8, bool)> {
        let guard = Coordinator::icon_publisher().lock().ok()?;
        let p = guard.as_ref()?;
        Some((p.style().index(), p.size_marks()))
    }

    /// Dev 变体专属的语言栏图标调试子菜单：16×16 上角标可不可辨只能真机看，接上菜单后
    /// 比选只需点几下。
    ///
    /// 只留三样：总开关（写 `[ui.langbar]`）、烧尺寸档标记、演示动画（后两者纯调试，走
    /// state.toml 与内存）。角标规则归 `[ui.langbar.badges]` 与设置页编辑器，菜单不另摆一套。
    #[cfg(all(feature = "desktop-ui", windows))]
    fn build_icon_debug_menu(&self) -> Vec<wind_ui_types::MenuItemSpec> {
        use wind_ui::langbar_icon::BadgeStyle;
        use wind_ui_types::MenuItemSpec as M;
        let cmd = |c: MenuCmd| MenuKind::Command(c);
        let Some((cur_style, marks)) = self.icon_debug_state() else {
            return vec![M::label("图标共享内存不可用")];
        };
        let mut items: Vec<M> = BadgeStyle::ALL
            .iter()
            .map(|&st| {
                M::leaf(
                    st.label(),
                    cmd(MenuCmd::IconBadgeStyle(st.index())),
                    true,
                    st.index() == cur_style,
                )
            })
            .collect();
        items.push(M::separator());
        items.push(M::leaf(
            "烧尺寸档标记",
            cmd(MenuCmd::IconToggleSizeMarks),
            true,
            marks,
        ));
        // 演示动画单独隔一段：它是持续跑的演示、重启不保留，不是会被记住的呈现偏好。
        items.push(M::separator());
        items.push(M::leaf(
            "演示动画（外圈跑马灯）",
            cmd(MenuCmd::IconToggleDemoAnim),
            true,
            self.icon_demo_animation(),
        ));
        items
    }

    /// 状态提示气泡右键菜单「常驻显示」：在 always/temp 间翻转 display_mode 并立即生效。
    /// 变为 always 时立即以常驻方式显示一次当前状态；变为 temp 时立即隐藏。
    pub(crate) fn status_toggle_always(&self) {
        let now_always = !self
            .rt()
            .config
            .ui
            .status
            .display_mode
            .eq_ignore_ascii_case("always");
        let mode = if now_always { "always" } else { "temp" };
        let _ = Config::set_user_string(&["ui", "status", "display_mode"], mode);
        self.refresh_config_in_memory(|c| c.ui.status.display_mode = mode.to_string());
        if now_always {
            self.show_persistent_status_if_always();
        } else {
            self.hide_tip();
        }
    }

    /// 状态提示气泡右键菜单「焦点切换时显示」：翻转 `ui.status.show_on_focus` 并立即生效。
    ///
    /// 这里不立即弹一次气泡（与 `status_toggle_always` 不同）：焦点没动，弹出来像误触发。
    pub(crate) fn status_toggle_show_on_focus(&self) {
        let next = !self.rt().config.ui.status.show_on_focus;
        let _ = Config::set_user_value(
            &["ui", "status", "show_on_focus"],
            toml::Value::Boolean(next),
        );
        self.refresh_config_in_memory(|c| c.ui.status.show_on_focus = next);
    }

    /// 状态提示气泡右键菜单「恢复默认位置」：改回跟随光标，custom_x/y 归零。
    ///
    /// 当前焦点应用配了气泡定位规则时改规则（跟随光标、坐标清零），不碰全局：读取侧规则
    /// 压过全局，只改全局用户看不到变化。
    pub(crate) fn status_reset_position(&self) {
        let name = self.active_process_name();
        if self
            .rule_status_position(&name, &self.active_focus_window())
            .is_some()
        {
            use wind_config::app_compat::StatusPositionMode as SP;
            self.write_status_position_rule(&name, Some(SP::FollowCaret), 0, 0);
            return;
        }
        let _ = Config::set_user_string(&["ui", "status", "position_mode"], "follow_caret");
        let _ = Config::set_user_value(&["ui", "status", "custom_x"], toml::Value::Integer(0));
        let _ = Config::set_user_value(&["ui", "status", "custom_y"], toml::Value::Integer(0));
        self.refresh_config_in_memory(|c| {
            c.ui.status.position_mode = "follow_caret".to_string();
            c.ui.status.custom_x = 0;
            c.ui.status.custom_y = 0;
        });
    }

    /// 拖动状态提示气泡释放后的落位处理，是否持久化取决于当前模式：
    ///
    /// - `fixed`：写回 `custom_x/custom_y`，永久生效。
    /// - `follow_caret` / 锚点：不落盘。拖动只是临时挪开，UI 侧仅在拖动中锁定位置，
    ///   下次 `show()` 照常按光标定位，无需在此清理。
    ///
    /// per-app 规则配了气泡定位方式时落到该应用自己的那份坐标（方式 + 坐标一起写），不碰全局，
    /// 判据与读取侧 `status_position` 同源，照 `save_candidate_pos`。
    pub(crate) fn save_status_tip_pos(&self, x: i32, y: i32) {
        use wind_config::app_compat::StatusPositionMode as SP;
        let name = self.active_process_name();
        if let Some((mode, _, _)) = self.rule_status_position(&name, &self.active_focus_window()) {
            if mode == SP::Fixed {
                let (x, y) = avoid_unset_sentinel(x, y);
                self.write_status_position_rule(&name, Some(SP::Fixed), x, y);
            }
            return;
        }
        if self.rt().config.ui.status.position() != SP::Fixed {
            return;
        }
        // 与候选窗同款哨兵规避：UI 侧用 (0,0) 表示「尚未设定」，缺一处就会拖到主屏左上角后记不住。
        let (x, y) = avoid_unset_sentinel(x, y);
        let _ = Config::set_user_value(
            &["ui", "status", "custom_x"],
            toml::Value::Integer(x as i64),
        );
        let _ = Config::set_user_value(
            &["ui", "status", "custom_y"],
            toml::Value::Integer(y as i64),
        );
        self.refresh_config_in_memory(|c| {
            c.ui.status.custom_x = x;
            c.ui.status.custom_y = y;
        });
    }

    /// 拖动候选窗释放后的落位处理，是否持久化取决于当前定位方式：
    ///
    /// - `fixed`：写回 `ui.candidate.custom_x/custom_y`，永久生效。
    /// - `follow_caret`：不落盘。本次组合内保持不动，组合结束（`hide()` → `reset_drag()`）即恢复。
    ///
    /// per-app 规则命中时落到该应用自己的那份坐标，不碰全局（否则在 A 应用里拖一下所有
    /// 跟随全局的应用全被改掉）。判据与读取侧 `candidate_fixed_pos` 同源。
    pub(crate) fn save_candidate_pos(&self, x: i32, y: i32) {
        let name = self.active_process_name();
        if let Some((rule_fixed, _, _)) =
            self.rule_candidate_fixed_pos(&name, &self.active_focus_window())
        {
            if rule_fixed {
                let (x, y) = avoid_unset_sentinel(x, y);
                self.save_candidate_pos_for_app(&name, x, y);
            }
            // 规则显式配了 follow_caret：与全局跟随模式同义，拖动是临时的，不落盘。
            return;
        }
        if !self.rt().config.ui.candidate.is_fixed_position() {
            return;
        }
        let (x, y) = avoid_unset_sentinel(x, y);
        let _ = Config::set_user_value(
            &["ui", "candidate", "custom_x"],
            toml::Value::Integer(x as i64),
        );
        let _ = Config::set_user_value(
            &["ui", "candidate", "custom_y"],
            toml::Value::Integer(y as i64),
        );
        self.refresh_config_in_memory(|c| {
            c.ui.candidate.custom_x = x;
            c.ui.candidate.custom_y = y;
        });
    }

    /// 把候选窗落点写进该应用自己的 compat 规则（用户层），并让当前应用立即生效。
    ///
    /// 与 [`Self::set_first_show_mode`] 的写盘三步同构，但不弹 toast：拖动是高频手势。
    fn save_candidate_pos_for_app(&self, name: &str, x: i32, y: i32) {
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("save_candidate_pos: 无用户配置目录，无法持久化 process={name}");
            return;
        };
        let target = self.menu_writeback_target(name, &CANDIDATE_POS_FIELDS);
        if let Err(e) =
            wind_config::app_compat::set_user_candidate_fixed_pos(&user_dir, target, x, y)
        {
            tracing::error!("save_candidate_pos: 写用户 compat.toml 失败: {e}");
            return;
        }
        self.reload_app_compat();
        tracing::debug!("候选窗固定位置 for process={name}: ({x},{y})");
    }

    /// 为当前焦点应用设置候选窗定位方式，并写入用户层 compat.toml。
    /// `mode_id`：0=跟随全局（清除规则）1=跟随光标 2=固定位置。
    ///
    /// 三步与 [`Self::set_first_show_mode`] 同构。切到「固定位置」时不预设坐标：`(0,0)` 由 UI
    /// 落到屏幕默认锚点，用户拖一次即定。与状态气泡「以当前实际位置落盘」不同：候选窗此刻
    /// 多半没显示，拿不到当前位置。
    pub(crate) fn set_candidate_position_rule(&self, mode_id: u8) {
        use wind_config::app_compat::CandidatePositionMode as M;
        let mode = match mode_id {
            1 => Some(M::FollowCaret),
            2 => Some(M::Fixed),
            _ => None,
        };
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_candidate_position_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_candidate_position_rule: 无用户配置目录，无法持久化");
            return;
        };
        if let Err(e) = wind_config::app_compat::set_user_candidate_position_mode(
            &user_dir,
            self.menu_writeback_target(&name, &CANDIDATE_POS_FIELDS),
            mode,
        ) {
            tracing::error!("set_candidate_position_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        self.reload_app_compat();
        tracing::info!(
            "候选窗定位方式 for process={name}: {}",
            mode.map(|m| m.as_config()).unwrap_or("(follow-global)")
        );
        self.show_status();
    }

    /// 为当前焦点应用设置「软件切换中英」（忽略 / 采纳宿主写 compartment 引起的中英切换），
    /// 并写入用户层 compat.toml。`mode_id`：0=跟随全局（清除）1=忽略 2=显式采纳。
    ///
    /// 三步与 [`Self::set_first_show_mode`] 同构，没有第四步：判定在服务端
    /// `handle_system_mode_switch`，DLL 照发不误，被拒后由仲裁回路把 compartment 拉回。
    pub(crate) fn set_ignore_host_ime_switch_rule(&self, mode_id: u8) {
        let enabled = match mode_id {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_ignore_host_ime_switch_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_ignore_host_ime_switch_rule: 无用户配置目录，无法持久化");
            return;
        };
        if let Err(e) = wind_config::app_compat::set_user_ignore_host_ime_switch(
            &user_dir,
            self.menu_writeback_target(&name, &["ignore_host_ime_switch"]),
            enabled,
        ) {
            tracing::error!("set_ignore_host_ime_switch_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        self.reload_app_compat();
        tracing::info!(
            "软件切换中英 for process={name}: {}",
            match enabled {
                Some(true) => "忽略",
                Some(false) => "采纳",
                None => "(follow-global)",
            }
        );
        self.show_status();
    }

    /// 状态提示气泡右键菜单「固定位置」：在 fixed / follow_caret 间翻转。
    ///
    /// 打开时以气泡当前实际位置落盘，而不是切到陈旧的 custom_x/custom_y（否则气泡会跳到
    /// 上次保存的、往往是 0,0 的坐标）：先改成 fixed，再请 UI 上报当前位置，回来的
    /// `StatusTipMoved` 经 `save_status_tip_pos` 落盘。当前焦点应用配了气泡定位规则时翻转规则。
    pub(crate) fn status_toggle_pinned(&self) {
        use wind_config::app_compat::StatusPositionMode as SP;
        let name = self.active_process_name();
        if let Some((mode, x, y)) = self.rule_status_position(&name, &self.active_focus_window()) {
            let now_fixed = mode != SP::Fixed;
            if now_fixed {
                // 先落方式（坐标沿用旧值，多半是 0 哨兵），再请 UI 报当前位置覆盖之。
                self.write_status_position_rule(&name, Some(SP::Fixed), x, y);
                let _ = self.ui_tx.send(UiCommand::ReportStatusTipPos);
            } else {
                self.write_status_position_rule(&name, Some(SP::FollowCaret), 0, 0);
            }
            return;
        }
        let now_fixed = self.rt().config.ui.status.position() != SP::Fixed;
        let mode = if now_fixed { "fixed" } else { "follow_caret" };
        let _ = Config::set_user_string(&["ui", "status", "position_mode"], mode);
        self.refresh_config_in_memory(|c| c.ui.status.position_mode = mode.to_string());
        if now_fixed {
            let _ = self.ui_tx.send(UiCommand::ReportStatusTipPos);
        }
    }

    /// 右键状态提示气泡请求的功能菜单：常驻显示 / 焦点切换时显示 / 固定位置（均带勾选）/
    /// 恢复默认位置 / 截图。
    pub(crate) fn show_status_menu(&self, x: i32, y: i32) {
        use wind_ui_types::MenuItemSpec as M;
        let si_always;
        let si_fixed;
        let si_on_focus;
        {
            let si = &self.rt().config.ui.status;
            si_always = si.display_mode.eq_ignore_ascii_case("always");
            // 勾选态看生效的定位（规则优先回落全局），与落盘分流同一判据。
            si_fixed =
                self.status_position().mode == wind_config::app_compat::StatusPositionMode::Fixed;
            si_on_focus = si.show_on_focus;
        }
        // 菜单打开期间抑制气泡自动隐藏，否则临时模式下菜单还开着气泡就没了。
        let _ = self.ui_tx.send(UiCommand::SetStatusMenuOpen(true));
        let cmd = |c: MenuCmd| MenuKind::Command(c);
        let items = vec![
            M::leaf(
                "常驻显示",
                cmd(MenuCmd::StatusToggleAlways),
                true,
                si_always,
            ),
            // 常驻模式下本项无意义，置灰而非隐藏（忽隐忽现更难理解）。
            M::leaf(
                "焦点切换时显示",
                cmd(MenuCmd::StatusToggleShowOnFocus),
                !si_always,
                si_on_focus,
            ),
            M::leaf("固定位置", cmd(MenuCmd::StatusTogglePinned), true, si_fixed),
            M::leaf(
                "恢复默认位置",
                cmd(MenuCmd::StatusResetPosition),
                true,
                false,
            ),
            M::leaf("截图此窗口", cmd(MenuCmd::StatusScreenshot), true, false),
        ];
        self.mark_menu_open(0, String::new());
        let _ = self.ui_tx.send(UiCommand::ShowCandidateMenu {
            items,
            anchor: MenuAnchor::at_point(x, y),
        });
    }

    /// 输入诊断 HUD 上右键请求的菜单：复制 / 显示分类 / 停止刷新 / 置顶 / 关闭。
    ///
    /// 勾选态直接读运行时状态，菜单永远反映当前真值。
    pub(crate) fn show_input_diag_menu(&self, x: i32, y: i32) {
        use std::sync::atomic::Ordering::Relaxed;
        use wind_ui_types::{DiagSections, MenuItemSpec as M};
        let cmd = |c: MenuCmd| MenuKind::Command(c);
        let sections = *self
            .input_diag_sections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let section_items: Vec<M> = DiagSections::ALL
            .iter()
            .map(|&i| {
                M::leaf(
                    DiagSections::label(i),
                    cmd(MenuCmd::InputDiagToggleSection(i)),
                    true,
                    sections.get(i),
                )
            })
            .collect();
        let items = vec![
            M::leaf("复制全部内容", cmd(MenuCmd::InputDiagCopy), true, false),
            M::separator(),
            M::submenu("显示分类", section_items),
            M::separator(),
            M::leaf(
                "停止刷新",
                cmd(MenuCmd::InputDiagToggleFreeze),
                true,
                self.input_diag_frozen.load(Relaxed),
            ),
            M::leaf(
                "窗口置顶",
                cmd(MenuCmd::InputDiagToggleTopmost),
                true,
                self.input_diag_topmost.load(Relaxed),
            ),
            M::separator(),
            M::leaf(
                "关闭诊断 HUD",
                cmd(MenuCmd::ToggleInputDiagnostics),
                true,
                false,
            ),
        ];
        self.mark_menu_open(0, String::new());
        let _ = self.ui_tx.send(UiCommand::ShowCandidateMenu {
            items,
            anchor: MenuAnchor::at_point(x, y),
        });
    }

    /// 切换分区显示。
    ///
    /// 必须强制推一次：冻结中 `push_input_diag_hud_if_visible` 会早退，切分类就毫无变化。
    /// 分区是显示配置而非数据，与冻结正交。
    pub(crate) fn toggle_input_diag_section(&self, idx: u8) {
        {
            let mut s = self
                .input_diag_sections
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            s.toggle(idx);
        }
        self.push_input_diag_hud(true);
    }

    /// 停止/恢复刷新。恢复时立即推一次当前快照，否则要等下一次焦点事件才回到实时值。
    pub(crate) fn toggle_input_diag_freeze(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let now = !self.input_diag_frozen.load(Relaxed);
        self.input_diag_frozen.store(now, Relaxed);
        // 冻结时也推一次：HUD 要立刻显示「已停止刷新」标注。
        self.push_input_diag_hud(true);
    }

    /// 切换窗口置顶。同样强制推——置顶状态由 UI 在渲染时应用。
    pub(crate) fn toggle_input_diag_topmost(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let now = !self.input_diag_topmost.load(Relaxed);
        self.input_diag_topmost.store(now, Relaxed);
        self.push_input_diag_hud(true);
    }

    /// 切换输入诊断 HUD 显隐（高级菜单）：开启时立即推送当前快照，关闭时下发隐藏。
    pub(crate) fn toggle_input_diag_hud(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let now = !self.input_diag_hud_visible.load(Relaxed);
        self.input_diag_hud_visible.store(now, Relaxed);
        // 采集开关随 HUD 显隐下发（广播）。关闭时也必须推，否则 DLL 继续每次焦点切换都采集窗口链。
        self.push_diag_snapshot_config(0);
        if now {
            // 打开时复位置顶与冻结：两个开关都能封死自己的逃生口（非置顶 HUD 沉到宿主下面
            // 右键点不到；冻结中重开看起来像坏了）。分区显示不复位：全关时 HUD 有可右键的提示行。
            self.input_diag_topmost.store(true, Relaxed);
            self.input_diag_frozen.store(false, Relaxed);
            self.push_input_diag_hud_if_visible();
        } else {
            let _ = self.ui_tx.send(UiCommand::HideInputDiag);
        }
    }

    /// 高级菜单「密码框强制英文」：翻转 `input.password_force_english` 并落盘（先写用户层，
    /// 再刷新内存配置），否则重启服务即复原。
    pub(crate) fn toggle_password_suppress(&self) {
        let next = !self.rt().config.input.password_force_english;
        let _ = Config::set_user_value(
            &["input", "password_force_english"],
            toml::Value::Boolean(next),
        );
        self.refresh_config_in_memory(|c| c.input.password_force_english = next);
        self.set_password_suppress_enabled(next);
    }

    /// 把密码框抑制策略开关同步到运行时：关闭时立即解除当前生效的强制英文。
    /// 菜单切换与配置热重载共用，两条路都要回灌。
    pub(crate) fn set_password_suppress_enabled(&self, enabled: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        self.password_suppress_enabled.store(enabled, Relaxed);
        self.relax_password_suppress_for_focus();
        // 同步给 DLL：吃键门控在 TSF 侧本地判定，不推则关掉抑制后 DLL 仍放行所有键、逃生阀
        // 成摆设。逐客户端按各自 pid 现算（per-app 规则优先），见 `push_password_suppress_config`。
        self.push_password_suppress_config(0);
    }

    /// 按最近一次输入诊断（焦点 pid + InputScope 掩码）重算抑制态，只降不升：判定变成
    /// 「不抑制」就立即解除，「抑制」留给下一次诊断上报去置位。
    ///
    /// 不变量是 core.suppress ⊆ C++.suppress，而新开关值经 push 管道异步到达 DLL：解除先于
    /// DLL 生效是安全的；置位若抢在前面就是「DLL 吃键、core 回 PassThrough」，密码框丢键。
    pub(crate) fn relax_password_suppress_for_focus(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let (pid, mask) = {
            let d = self
                .last_input_diag
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            (d.pid, d.mask)
        };
        let want =
            crate::input_diag::is_password_scope(mask) && self.password_force_english_for_pid(pid);
        if !want {
            self.password_suppress.store(false, Relaxed);
        }
    }

    /// 当前焦点进程名（小写，取自 `pid_names` 缓存）。未解析出进程时返回空串。
    pub(crate) fn active_process_name(&self) -> String {
        let pid = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pid;
        if pid == 0 {
            return String::new();
        }
        self.pid_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned()
            .unwrap_or_default()
    }

    /// 「候选窗首显」子菜单的四档：(菜单 id, 档位, 标签)。`None` = 跟随全局。
    ///
    /// 菜单项与 [`Self::set_first_show_mode`] 的解析共用这一张表：两者的对应关系没有编译或
    /// 测试信号，id 写反只表现为点了一档生效另一档。编号按菜单显示顺序排，「0=跟随全局」
    /// 与 `InitialMode` / `AutoPairRule` 一致。
    pub(crate) const FIRST_SHOW_MENU: [(
        u8,
        Option<wind_config::app_compat::FirstShowMode>,
        &'static str,
    ); 4] = {
        use wind_config::app_compat::FirstShowMode as F;
        [
            (0, None, "跟随全局（默认）"),
            (1, Some(F::Fast), "快速显示"),
            (2, Some(F::Wait), "等待精确坐标（较慢）"),
            (3, Some(F::Instant), "立即显示（最快，可能抖动）"),
        ]
    };

    /// 从磁盘整表重载 `app_compat`（系统层 + 定制层 + 用户层，与启动时同一口径），并把所有
    /// 依赖这张表、又在别处缓存了结果的东西一并对齐。菜单写规则、拖动落盘等全部重载点
    /// 都只许调这一个函数。
    ///
    /// 重载拿到的是整份文件的当前内容，不只是本次菜单改的那一项（用户手写的规则随任意一次
    /// 菜单操作生效），所以每次都要重推 DLL 侧的派生值，否则密码框门控会打破
    /// core.suppress ⊆ C++.suppress。对齐项：HostRender 白名单（Windows）、当前焦点的密码框
    /// 抑制态（只降不升，见 [`Self::relax_password_suppress_for_focus`]），以及逐客户端重推
    /// 密码框门控、英文配对、占位字符、多行上屏、标题规则开关等。推送都幂等。
    pub(crate) fn reload_app_compat(&self) {
        let reloaded = wind_config::app_compat::AppCompat::load(
            self.compat_dirs.0.as_deref(),
            self.compat_dirs.1.as_deref(),
        );
        *self.app_compat.lock().unwrap_or_else(|e| e.into_inner()) = reloaded;
        // 按键路径那几项（占位字符 / 候选窗定位 / 收窗 opt-in）是按旧表预提取的。
        self.refresh_active_compat_lookups();
        self.refresh_mode_scope_after_reload();
        #[cfg(windows)]
        self.sync_host_render_whitelist();
        self.relax_password_suppress_for_focus();
        self.push_password_suppress_config(0);
        self.push_english_pair_config(0);
        self.push_pair_via_comp_config(0);
        self.push_composition_placeholder_config(0);
        self.push_space_before_digit_config(0);
        self.push_multiline_commit_config(0);
        self.push_compat_title_match_config(0);
    }

    /// 设置端 `compat.*` 写入之后的收口：重载规则表，再让当前前台进程的 `active_compat`
    /// 立即按新表重算（同 pid 时 `update_active_compat` 提前 return，只重载不刷新要切走再切回
    /// 才生效）。不动 `.pid` / `.has_initial_rule`：它们同时记录上一次真实 FOCUS_GAINED 的落点，
    /// 见 `refresh_active_compat_rule_fields`。
    pub(crate) fn reload_compat_and_refresh(&self) {
        self.reload_app_compat();
        #[cfg(any(windows, test))]
        {
            let pid = self
                .active_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pid;
            self.refresh_active_compat_rule_fields(pid);
        }
    }

    /// 为当前焦点应用设置候选窗首显策略，并写入用户层 compat.toml。
    ///
    /// 三步收口，缺一不可：
    ///   1. 写用户层 compat.toml（持久化）；
    ///   2. 重载规则表：切走再切回时 `update_active_compat` 按这张表重新解析，旧表会把设置回滚；
    ///   3. 刷新当前 `active_compat` 缓存（同 pid 时 `update_active_compat` 提前 return）。
    ///
    /// `mode_id` 的含义见 [`Self::FIRST_SHOW_MENU`]。认不出的编号一律当作「跟随全局」：
    /// per-app 覆盖不该由一个对不上的 id 凭空造出来。
    pub(crate) fn set_first_show_mode(&self, mode_id: u8) {
        let mode = Self::FIRST_SHOW_MENU
            .iter()
            .find(|(id, _, _)| *id == mode_id)
            .and_then(|(_, mode, _)| *mode);
        let name = self.active_process_name();
        if name.is_empty() {
            // 焦点进程未解析。菜单项此时应是禁用态，走到这里说明有别的路径调用，记一条。
            tracing::warn!("set_first_show_mode: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_first_show_mode: 无用户配置目录，无法持久化");
            return;
        };
        let target = self.menu_writeback_target(&name, &["first_show_mode"]);
        if let Err(e) = wind_config::app_compat::set_user_first_show_mode(&user_dir, target, mode) {
            tracing::error!("set_first_show_mode: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表（系统层 + 用户层）。
        self.reload_app_compat();
        // 3）当前应用立即生效：按新表对焦点窗口重新解析，不直接把 `mode` 塞进缓存：
        //    「跟随全局」清掉这一条后，`*` / 更低级规则的值会透上来。
        let resolved = self.with_active_compat_rule(|r| r.and_then(|r| r.first_show_mode));
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first_show_mode = resolved;
        tracing::info!(
            "候选窗首显策略 for process={name}: {}",
            mode.map(|m| m.as_config()).unwrap_or("(follow-global)")
        );
        self.show_status();
    }

    /// 启用 / 禁用当前焦点应用的整条兼容规则，并写入用户层 compat.toml（`disabled`）。
    ///
    /// 与设置端「应用兼容」窗里的「禁用 / 启用」同一件事（字段都留着，整条不生效），用来
    /// 快速判断「是不是兼容规则导致的问题」。写盘走 `app_compat::set_user_rule_disabled`，
    /// 之后 `reload_compat_and_refresh`。
    pub(crate) fn set_compat_rule_enabled(&self, enabled: bool) {
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_compat_rule_enabled: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_compat_rule_enabled: 无用户配置目录，无法持久化");
            return;
        };
        if let Err(e) = wind_config::app_compat::set_user_rule_disabled(&user_dir, &name, !enabled)
        {
            tracing::error!("set_compat_rule_enabled: 写用户 compat.toml 失败: {e}");
            return;
        }
        self.reload_compat_and_refresh();
        tracing::info!(
            "兼容规则 for process={name}: {}",
            if enabled { "启用" } else { "禁用" }
        );
        self.show_status();
    }

    /// 为当前焦点应用设置符号自动配对开关，并写入用户层 compat.toml。
    /// `mode_id`：0=跟随全局（清除规则）1=启用 2=禁用。
    ///
    /// 三步与 [`Self::set_first_show_mode`] 同构。英文配对配置（C++ 侧 `_englishPairEngine`
    /// 只认推过去的值）的重推收在 [`Self::reload_app_compat`]，任何一次整表重载都可能改变它。
    pub(crate) fn set_auto_pair_rule(&self, mode_id: u8) {
        let enabled = match mode_id {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_auto_pair_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_auto_pair_rule: 无用户配置目录，无法持久化");
            return;
        };
        let target = self.menu_writeback_target(&name, &["auto_pair"]);
        if let Err(e) = wind_config::app_compat::set_user_auto_pair(&user_dir, target, enabled) {
            tracing::error!("set_auto_pair_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表。
        self.reload_app_compat();
        // 3）当前应用立即生效：按新表重新解析，理由同 `set_first_show_mode`。
        let resolved = self.with_active_compat_rule(|r| r.and_then(|r| r.auto_pair));
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_pair = resolved;
        tracing::info!(
            "符号自动配对 for process={name}: {}",
            match enabled {
                Some(true) => "启用",
                Some(false) => "禁用",
                None => "跟随全局",
            }
        );
        self.show_status();
    }

    /// 为当前焦点应用设置「密码框强制英文」，并写入用户层 compat.toml。
    /// `mode_id`：0=跟随全局（清除规则）1=开 2=关；认不出的编号按「跟随全局」处理。
    ///
    /// 写盘 → 重载整表。本项不进 `active_compat`：判定按 pid 直查规则表
    /// （`password_force_english_for_pid`）。重算抑制态与重推 DLL 门控收在 [`Self::reload_app_compat`]。
    pub(crate) fn set_password_force_english_rule(&self, mode_id: u8) {
        let enabled = match mode_id {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_password_force_english_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_password_force_english_rule: 无用户配置目录，无法持久化");
            return;
        };
        if let Err(e) = wind_config::app_compat::set_user_password_force_english(
            &user_dir,
            self.menu_writeback_target(&name, &["password_force_english"]),
            enabled,
        ) {
            tracing::error!("set_password_force_english_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表；当前焦点的抑制态重算与逐客户端重推门控都在 `reload_app_compat` 里。
        self.reload_app_compat();
        tracing::info!(
            "密码框强制英文 for process={name}: {}",
            match enabled {
                Some(true) => "开",
                Some(false) => "关",
                None => "跟随全局",
            }
        );
        self.notify_toolbar();
        self.show_status();
    }

    /// 写当前应用的气泡定位规则（方式 + 坐标）并重载规则表。不弹 toast / 气泡：拖动落盘走这里。
    fn write_status_position_rule(
        &self,
        name: &str,
        mode: Option<wind_config::app_compat::StatusPositionMode>,
        x: i32,
        y: i32,
    ) -> bool {
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("status_position: 无用户配置目录，无法持久化 process={name}");
            return false;
        };
        if let Err(e) = wind_config::app_compat::set_user_status_position(
            &user_dir,
            self.menu_writeback_target(name, &STATUS_POS_FIELDS),
            mode,
            x,
            y,
        ) {
            tracing::error!("status_position: 写用户 compat.toml 失败: {e}");
            return false;
        }
        self.reload_app_compat();
        tracing::debug!(
            "状态气泡定位 for process={name}: {} ({x},{y})",
            mode.map(|m| m.as_config()).unwrap_or("(follow-global)")
        );
        true
    }

    /// 为当前焦点应用设置状态气泡定位，并写入用户层 compat.toml。
    /// `code`：0=跟随全局（清除规则）1=跟随光标 2=固定 3+i=`StatusAnchor::ALL[i]`；越界忽略。
    ///
    /// 写盘 → 重载整表。本项不进 `active_compat`（`status_position` 按进程名现查）。
    ///
    /// 「固定」取气泡当前位置：先把方式落成 fixed（坐标沿用已有的，没有就是 0 哨兵），再请 UI
    /// 报当前位置，回来的 `StatusTipMoved` 经 `save_status_tip_pos` 覆盖。气泡不可见时 UI 不回报，
    /// 留在哨兵、拖一次即定。
    pub(crate) fn set_status_position_rule(&self, code: u8) {
        use wind_config::app_compat::{StatusAnchor, StatusPositionMode as SP};
        let mode = match code {
            0 => None,
            1 => Some(SP::FollowCaret),
            2 => Some(SP::Fixed),
            n => match StatusAnchor::ALL.get(n as usize - 3) {
                Some(a) => Some(SP::Anchor(*a)),
                None => {
                    tracing::warn!("set_status_position_rule: 未知编号 {n}，忽略");
                    return;
                }
            },
        };
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_status_position_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let (x, y) = match self.rule_status_position(&name, &self.active_focus_window()) {
            Some((SP::Fixed, x, y)) => (x, y),
            _ => (0, 0),
        };
        if !self.write_status_position_rule(&name, mode, x, y) {
            return;
        }
        tracing::info!(
            "状态气泡定位 for process={name}: {}",
            mode.map(|m| m.as_config()).unwrap_or("(follow-global)")
        );
        if mode == Some(SP::Fixed) {
            let _ = self.ui_tx.send(UiCommand::ReportStatusTipPos);
        }
    }

    /// 为当前焦点应用设置「坐标不可用时」气泡放哪，并写入用户层 compat.toml。
    /// `code`：0=跟随全局（清除规则）1=上次位置 2=不显示 3+i=`StatusAnchor::ALL[i]`；越界忽略。
    pub(crate) fn set_status_fallback_rule(&self, code: u8) {
        use wind_config::app_compat::{StatusAnchor, StatusFallback as SF};
        let fallback = match code {
            0 => None,
            1 => Some(SF::Last),
            2 => Some(SF::Hide),
            n => match StatusAnchor::ALL.get(n as usize - 3) {
                Some(a) => Some(SF::Anchor(*a)),
                None => {
                    tracing::warn!("set_status_fallback_rule: 未知编号 {n}，忽略");
                    return;
                }
            },
        };
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_status_fallback_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_status_fallback_rule: 无用户配置目录，无法持久化");
            return;
        };
        if let Err(e) = wind_config::app_compat::set_user_status_fallback(
            &user_dir,
            self.menu_writeback_target(&name, &["status_fallback_position"]),
            fallback,
        ) {
            tracing::error!("set_status_fallback_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        self.reload_app_compat();
        tracing::info!(
            "状态气泡兜底位置 for process={name}: {}",
            fallback.map(|f| f.as_config()).unwrap_or("(follow-global)")
        );
    }

    /// 为当前焦点应用设置输入方案，并写入用户层 compat.toml。
    /// `code`：0=跟随全局（清除规则）1=记住上次（`@remember`）2+i=固定为可用方案表第 i 个；
    /// 越界的下标忽略（菜单构建与点击之间 available 被热重载缩短了）。
    ///
    /// 写盘 → 重载整表 → 当场对当前焦点生效一次。本项不进 `active_compat`（规则按进程名现查，
    /// 见 `app_schema_rule`）。「记住上次」且记忆表里还没有这个应用时先记下当前方案，否则目标
    /// 回落全局，正在用的方案被切走。
    pub(crate) fn set_app_schema_rule(&self, code: u16) {
        let value = match code {
            0 => None,
            1 => Some(wind_config::app_compat::APP_SCHEMA_REMEMBER.to_string()),
            n => {
                let list = self.engine_mgr.available_schemas();
                match list.get(usize::from(n - 2)) {
                    Some(id) => Some(id.clone()),
                    None => {
                        tracing::warn!("set_app_schema_rule: 方案下标 {} 越界，忽略", n - 2);
                        return;
                    }
                }
            }
        };
        let name = self.active_process_name();
        if name.is_empty() {
            tracing::warn!("set_app_schema_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_app_schema_rule: 无用户配置目录，无法持久化");
            return;
        };
        // 1）写用户层 compat.toml。
        let target = self.menu_writeback_target(&name, &["schema"]);
        if let Err(e) = wind_config::app_compat::set_user_schema(&user_dir, target, value.clone()) {
            tracing::error!("set_app_schema_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表（系统层 + 用户层）。
        self.reload_app_compat();
        // 3）当场对当前焦点生效一次（按新规则算目标方案，不同就轻量切换）。
        if code == 1 && !self.has_remembered_schema(&name) {
            self.remember_app_schema(&name, &self.engine_mgr.active_schema_id());
        }
        self.apply_app_schema_on_focus(&name, &self.active_focus_window());
        tracing::info!(
            "应用独立方案 for process={name}: {}",
            value.as_deref().unwrap_or("(follow-global)")
        );
        self.notify_toolbar();
        self.show_status();
    }

    /// 为当前焦点应用设置初始中英状态（`is_punct=false`）或初始标点（`is_punct=true`），
    /// 并写入用户层 compat.toml。`mode_id`：0=跟随全局（清除规则）1=英文 2=中文。
    ///
    /// 前三步与 [`Self::set_first_show_mode`] 同构。第四步是本项特有：规则只在焦点跨进程切入时
    /// 参与决策，但用户此刻正在当前应用里显式设置它，必须立即生效一次。
    pub(crate) fn set_initial_state_rule(&self, is_punct: bool, mode_id: u8) {
        use wind_config::app_compat::InitialMode as IM;
        let mode = match mode_id {
            1 => Some(IM::English),
            2 => Some(IM::Chinese),
            _ => None, // 0 = 跟随全局：清除该应用在本维度上的规则
        };
        let name = self.active_process_name();
        if name.is_empty() {
            // 与 set_first_show_mode 一致：菜单项此时应是禁用态，走到这里说明有别的调用路径。
            tracing::warn!("set_initial_state_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_initial_state_rule: 无用户配置目录，无法持久化");
            return;
        };
        // 1）写用户层 compat.toml。
        let field = if is_punct {
            "initial_punct"
        } else {
            "initial_mode"
        };
        let target = self.menu_writeback_target(&name, &[field]);
        let written = if is_punct {
            wind_config::app_compat::set_user_initial_punct(&user_dir, target, mode)
        } else {
            wind_config::app_compat::set_user_initial_mode(&user_dir, target, mode)
        };
        if let Err(e) = written {
            tracing::error!("set_initial_state_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表（系统层 + 用户层）。
        self.reload_app_compat();
        // 3）刷新 active 缓存的判据位（同 pid 时 update_active_compat 提前 return），漏了会让
        //    「切出本应用时是否重算」用上过期判据。先取值再持 active_compat 锁，避免与
        //    app_compat 锁嵌套。
        let win = self.active_focus_window();
        let want_mode = self.rule_initial_mode(&name, &win).map(|m| m.is_chinese());
        let want_punct = self.rule_initial_punct(&name, &win).map(|m| m.is_chinese());
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_initial_rule = want_mode.is_some() || want_punct.is_some();
        // 4）立即生效一次。清除规则（None）时不动当前状态：撤销规则不等于要求立刻切换模式。
        let follow = self.rt().config.input.punct.follow_mode;
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = want_mode
                && s.chinese_mode != c
            {
                s.chinese_mode = c;
                if follow {
                    self.set_punct_below_schema_intent(&mut s, c);
                }
            }
            // 与 apply_initial_mode 同序：显式标点规则最后落地，压过 follow 推导。
            if let Some(p) = want_punct {
                s.chinese_punct = p;
            }
        }
        tracing::info!(
            "应用独立初始状态 for process={name}: {}={}",
            if is_punct {
                "initial_punct"
            } else {
                "initial_mode"
            },
            mode.map(|m| m.as_config()).unwrap_or("(follow-global)")
        );
        self.push_state_update();
        self.notify_toolbar();
        self.show_status();
    }

    /// 在文件管理器中打开目录（高级菜单「打开…目录」共用）。
    /// 目录可能尚未创建（如日志目录在首条日志前不存在），先 best-effort 建目录，
    /// 否则资源管理器会弹「找不到路径」。
    fn open_dir(&self, dir: Option<std::path::PathBuf>) {
        let Some(d) = dir else {
            tracing::warn!("open_dir: 目录不可用");
            return;
        };
        let _ = std::fs::create_dir_all(&d);
        let _ = self
            .ui_tx
            .send(UiCommand::OpenPath(d.display().to_string()));
    }

    /// 统一的「打开设置」入口：优先启动同目录的 wind_setting 桌面应用并跳转到指定页
    /// （`--page <name>`，name 为 wind_setting cli 的规范页 id：schema/input/keys/ui/dict/advanced/about）；
    /// 找不到桌面应用再回退到内嵌 web 配置（page 以 `#<name>` 片段附加）。page=None 打开默认页。
    ///
    /// 有 TSF 连接时经 IPC 让宿主进程执行 ShellExecuteW（有前台权限，能把窗口拉到前面）；
    /// 无 TSF 连接时由服务进程直接启动。
    pub(crate) fn open_settings(&self, page: Option<&str>) {
        self.open_settings_with(page, "");
    }

    /// 当前宿主是否要求由服务进程自己拉起外部程序（compat `launch_by_service`）：开始菜单、
    /// 便笺等宿主不执行转发来的启动请求。
    #[cfg(not(ext_presenter))]
    fn launch_by_service(&self) -> bool {
        self.with_active_compat_rule(|r| r.and_then(|r| r.launch_by_service))
            .unwrap_or(false)
    }

    /// 带附加参数的「打开设置」。`extra` 是原样直通给设置程序的命令行参数串
    /// （如 `--schema=wubi86 --type=shadow`），空串=无附加参数。
    ///
    /// 刻意不解析 `extra`：取值合法性由设置端自己判断（会降级并提示）。内部调用方请用
    /// [`build_settings_args`] 构造，含空白的值会被正确加引号。
    pub(crate) fn open_settings_with(&self, page: Option<&str>, extra: &str) {
        #[cfg(not(ext_presenter))]
        let args = settings_cmdline(page, extra);

        // ext_presenter：经扩展信封 `SETTINGS_OPEN` 传结构化 argv，由宿主侧启动/激活设置应用，
        // 不必知道引号约定。`settings_app_path` 在这里恒为 None，不能落到下面的分支。
        #[cfg(ext_presenter)]
        {
            let argv = settings_argv(page, extra);
            let body = serde_json::json!({ "args": argv }).to_string();
            let encoded = wind_ipc::codec::encode_ext(
                wind_ipc::protocol::ext_kind::SETTINGS_OPEN,
                body.as_bytes(),
            );
            self.push_server.push_to_active(&encoded);
        }
        #[cfg(not(ext_presenter))]
        if let Some(app) = crate::coordinator::settings_app_path() {
            if self.push_server.has_clients() && !self.launch_by_service() {
                // 设置程序落到它自己所在目录（app 目录），不继承宿主应用的当前目录。
                let dir = crate::handle_cmdbar::resolve_workdir("setting.open", &app, "");
                self.push_shell_exec(&app, &args, &dir, "", "");
            } else {
                let _ = self.ui_tx.send(UiCommand::OpenApp { path: app, args });
            }
        } else if let Some(url) = crate::coordinator::settings_url() {
            // web 回退没有命令行概念：只带页锚点，附加参数丢弃（页仍能到位）。
            let url = match page {
                Some(p) => format!("{url}#{p}"),
                None => url,
            };
            if self.push_server.has_clients() && !self.launch_by_service() {
                let dir = crate::handle_cmdbar::resolve_workdir("setting.open", &url, "");
                self.push_shell_exec(&url, "", &dir, "", "");
            } else {
                let _ = self.ui_tx.send(UiCommand::OpenPath(url));
            }
        } else {
            tracing::warn!("打开设置失败：未找到 wind_setting 程序，web 服务也未就绪");
        }
    }

    /// 菜单「词库管理」：直接落到当前正在用的方案域，而不是默认的命令短语域。
    /// 方案 id 取不到时退化为不带参数。
    pub(crate) fn open_dictionary(&self) {
        let schema = self.engine_mgr.active_schema_id();
        self.open_settings_with(Some("dict"), &build_settings_args(&[("schema", &schema)]));
    }

    /// 用户开关常驻工具栏（菜单）。仅翻转 toolbar_visible，显隐交 notify_toolbar 单点决策。
    pub(crate) fn toggle_toolbar(&self) {
        let vis = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.toolbar_visible = !s.toolbar_visible;
            s.toolbar_visible
        };
        // 持久化到 config.ui.toolbar.visible（单一源：与设置页统一，reload 不会覆盖菜单选择）。
        let _ = Config::set_user_bool(&["ui", "toolbar", "visible"], vis);
        // 内存 config 同步跟上，否则下一次未经重载的读取拿到陈旧值。
        self.refresh_config_in_memory(|c| c.ui.toolbar.visible = vis);
        self.notify_toolbar();
    }

    /// 构建并显示功能主菜单（方案/主题子菜单 + 勾选态）。
    /// 位置与展开方向全由 `anchor` 描述，见 [`wind_ui_types::MenuPlacement`]。
    pub(crate) fn show_main_menu(&self, anchor: MenuAnchor) {
        let items = self.build_main_menu_items();
        self.mark_menu_open(0, String::new());
        let _ = self
            .ui_tx
            .send(UiCommand::ShowCandidateMenu { items, anchor });
    }

    /// macOS 精简功能菜单（IMK 输入源菜单 + 候选框右键空白菜单共用）。
    /// 只保留必要项且无子菜单（IMK 输入源菜单无法可靠处理嵌套子菜单）：输入方案（英文 +
    /// 各方案单选）/ 中文标点、全角、简入繁出 / 显示状态图标 / 重启服务 / 设置…。
    /// 配置类交由设置应用。
    ///
    /// 状态图标开关是唯一保留的显示类开关，因为入口自锁：它关掉的菜单栏状态指示器正是完整树
    /// 的入口之一，只留在完整树里，关掉图标就把开关本身也藏了；IMK 输入源菜单恒定可达。
    #[cfg(ext_presenter)]
    pub(crate) fn build_menu_items_macos(&self) -> Vec<wind_ui_types::MenuItemSpec> {
        use wind_ui_types::MenuItemSpec as M;
        let (chinese, punct, full, s2t, toolbar_vis) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.chinese_mode,
                s.chinese_punct,
                s.full_width,
                s.s2t_enabled,
                s.toolbar_visible,
            )
        };
        let cmd = |c: MenuCmd| MenuKind::Command(c);
        let active = self.engine_mgr.active_schema_id();
        let schemas = self.engine_mgr.available_schemas().to_vec();

        let mut items = vec![M::leaf("英文", cmd(MenuCmd::SchemaEnglish), true, !chinese)];
        for (i, id) in schemas.iter().enumerate() {
            items.push(M::leaf(
                self.engine_mgr.schema_name(id),
                cmd(MenuCmd::SchemaSelect(i)),
                true,
                chinese && *id == active,
            ));
        }
        items.push(M::separator());
        items.push(M::leaf("中文标点", cmd(MenuCmd::TogglePunct), true, punct));
        items.push(M::leaf("全角", cmd(MenuCmd::ToggleWidth), true, full));
        items.push(M::leaf("简入繁出", cmd(MenuCmd::ToggleS2t), true, s2t));
        items.push(M::separator());
        items.push(M::leaf(
            TOOLBAR_MENU_LABEL,
            cmd(MenuCmd::ToggleToolbar),
            true,
            toolbar_vis,
        ));
        items.push(M::separator());
        items.push(M::leaf(
            "重启服务",
            cmd(MenuCmd::RestartService),
            true,
            false,
        ));
        items.push(M::separator());
        items.push(M::leaf("设置…", cmd(MenuCmd::OpenSettings), true, false));
        items
    }

    /// 构建功能主菜单项树（纯构建，不改状态/不弹窗）。
    /// Windows 经 `show_main_menu` 进程内渲染；macOS 经 `query_main_menu_encoded` 序列化下发给 `.app` 原生 NSMenu。
    pub(crate) fn build_main_menu_items(&self) -> Vec<wind_ui_types::MenuItemSpec> {
        use wind_ui_types::MenuItemSpec as M;
        let (chinese, punct, full, s2t, filter_mode, toolbar_vis) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.chinese_mode,
                s.chinese_punct,
                s.full_width,
                s.s2t_enabled,
                s.filter_mode,
                s.toolbar_visible,
            )
        };
        let cmd = |c: MenuCmd| MenuKind::Command(c);

        // 输入方案子菜单：英文 + 方案单选
        let schema_children = self.schema_menu_children(chinese);

        // 主题子菜单：主题单选 + 亮/暗
        let themes = self.list_themes();
        let cur_theme = self
            .theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let style = *self.theme_style.lock().unwrap_or_else(|e| e.into_inner());
        let mut theme_children = Vec::new();
        for (i, (id, name)) in themes.iter().enumerate() {
            theme_children.push(M::leaf(
                name.clone(),
                cmd(MenuCmd::ThemeSelect(i)),
                true,
                *id == cur_theme,
            ));
        }
        if !theme_children.is_empty() {
            theme_children.push(M::separator());
        }
        for s in [ThemeStyle::System, ThemeStyle::Light, ThemeStyle::Dark] {
            theme_children.push(M::leaf(
                s.label(),
                cmd(MenuCmd::ThemeStyle(s.as_menu_id())),
                true,
                style == s,
            ));
        }

        // 检索范围子菜单：过滤模式单选
        let filter_children: Vec<_> = FILTER_MODES
            .iter()
            .enumerate()
            .map(|(i, (m, label))| {
                M::leaf(*label, cmd(MenuCmd::FilterMode(i)), true, filter_mode == *m)
            })
            .collect();

        // 高级子菜单：截图等不常用功能 + 打开各数据目录（分隔线独立成组）
        #[allow(unused_mut)]
        let mut advanced_children = vec![
            M::leaf(
                "截图所有窗口到文件",
                cmd(MenuCmd::TakeScreenshot),
                true,
                false,
            ),
            M::leaf(
                "截图候选窗口到剪贴板",
                cmd(MenuCmd::ScreenshotCandidateToClipboard),
                true,
                false,
            ),
            M::separator(),
            M::leaf("打开应用程序目录", cmd(MenuCmd::OpenAppDir), true, false),
            M::leaf("打开用户数据目录", cmd(MenuCmd::OpenConfigDir), true, false),
            M::leaf("打开日志目录", cmd(MenuCmd::OpenLogDir), true, false),
            M::separator(),
            // ── 输入行为组 ──
            // 放在诊断类之前、独立成组：它改的是真正的输入行为（密码框里强制走英文），
            // 诊断类只是把内部状态显示出来。
            M::leaf(
                "密码框强制英文",
                cmd(MenuCmd::TogglePasswordSuppress),
                true,
                self.password_suppress_enabled
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            M::separator(),
            // ── 诊断组 ──
            // 「把内部状态显示出来」的工具：HUD 出文字、定位浮窗出几何。
            //
            // 输入诊断 HUD 在 ext_presenter 下未实现（`ShowInputDiag` 落在 forwarder 的兜底臂），
            // 留死菜单项比没有更糟，故按平台摘掉（见 wind_macos/AGENTS.md 差距表）。
            #[cfg(not(ext_presenter))]
            M::leaf(
                "输入诊断 HUD",
                cmd(MenuCmd::ToggleInputDiagnostics),
                true,
                self.input_diag_hud_visible
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
        ];

        // 候选窗定位浮窗：与 HUD 同组，但只在 Dev 变体出现（画的是内部排查用的几何）。
        #[cfg(all(feature = "desktop-ui", windows))]
        if wind_config::variant::is_dev() {
            advanced_children.push(M::leaf(
                "候选窗定位浮窗",
                cmd(MenuCmd::ToggleCaretOverlay),
                true,
                self.caret_overlay_enabled
                    .load(std::sync::atomic::Ordering::Relaxed),
            ));
        }

        // 图标调试项只在 Dev 变体出现：暴露的是还没定下来的呈现参数。
        #[cfg(all(feature = "desktop-ui", windows))]
        if wind_config::variant::is_dev() {
            advanced_children.push(M::separator());
            advanced_children.push(M::submenu("语言栏图标", self.build_icon_debug_menu()));
        }

        // 应用独立配置：所有 per-app 规则（均落在用户层 compat.toml）聚合于此。放顶层而不是
        // 塞进「高级」，维持三层菜单深度。
        //
        // 顶层标签固定，不嵌入进程名：主菜单宽度由最宽一项撑开，进程名长短不一会让主菜单
        // 忽宽忽窄。进程名放进子菜单第一行（禁用的展示行，见 `MenuItemSpec::label`）。
        // 进程未解析时子项禁用而非隐藏（父项 enabled 恒 true），菜单项位置保持稳定。
        let per_app_children = {
            use wind_config::app_compat::CandidatePositionMode as CP;
            use wind_config::app_compat::InitialMode as IM;
            let proc = self.active_process_name();
            let enabled = !proc.is_empty();
            // 整条规则的开关：没有规则（系统层与用户层都没有）时置灰。勾选 = 规则在生效；
            // 参数是目标状态而不是「切换」，菜单快照与实际状态错开时也不会做反。
            let rule_switch = if enabled {
                self.compat_dirs
                    .1
                    .as_deref()
                    .map(|u| wind_config::app_compat::menu_rule_switch(u, &proc))
                    .unwrap_or(wind_config::app_compat::RuleSwitch::NoRule)
            } else {
                wind_config::app_compat::RuleSwitch::NoRule
            };
            // 整条规则被禁用时，下面各项读到的都是「跟随全局」，选了也写得进去却不生效，
            // 故先置灰，让用户先启用这条规则。
            let has_proc = enabled;
            let enabled = enabled && rule_switch != wind_config::app_compat::RuleSwitch::Disabled;
            // 勾选显示的是焦点窗口生效中的值（与 auto_pair 读 active_compat 同口径）；菜单写回
            // 只写纯进程键，见 app_compat 的写回接口。
            let proc_win = self.active_focus_window();
            let (cur_cand_pos, cur_ignore_switch, cur_pfe, cur_schema, cur_status, cur_status_fb) =
                self.with_compat_rule(&proc, &proc_win, |rule| {
                    (
                        rule.and_then(|r| r.candidate_position_mode),
                        rule.and_then(|r| r.ignore_host_ime_switch),
                        rule.and_then(|r| r.password_force_english),
                        rule.and_then(|r| r.schema.clone()),
                        rule.and_then(|r| r.status_position_mode),
                        rule.and_then(|r| r.status_fallback_position),
                    )
                });
            let cur_first_show = self.rule_first_show_mode(&proc, &proc_win);
            let cur_mode = self.rule_initial_mode(&proc, &proc_win);
            let cur_punct = self.rule_initial_punct(&proc, &proc_win);
            let cur_auto_pair = self
                .active_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .auto_pair;
            let header = if has_proc {
                proc.clone()
            } else {
                "当前应用未知".to_string()
            };

            // 三档单选。「跟随全局」必须是独立一档（写盘 None，从 compat.toml 里清掉该字段），
            // 不能靠取消勾选表达，否则设了规则无从撤销。
            let tri = |cur: Option<IM>, mk: fn(u8) -> MenuCmd| {
                vec![
                    M::leaf("跟随全局（默认）", cmd(mk(0)), enabled, cur.is_none()),
                    M::leaf("英文", cmd(mk(1)), enabled, cur == Some(IM::English)),
                    M::leaf("中文", cmd(mk(2)), enabled, cur == Some(IM::Chinese)),
                ]
            };
            // 方案：跟随全局 / 记住上次 / ── / 各可用方案（选中即固定）。勾选看规则而非当前活跃
            // 方案。固定的 id 不在 available 时协调器按未配置处理，这里同样勾「跟随全局」。
            let app_schema_children = {
                use wind_config::app_compat::APP_SCHEMA_REMEMBER;
                let schemas = self.engine_mgr.available_schemas();
                let remember = cur_schema.as_deref() == Some(APP_SCHEMA_REMEMBER);
                let fixed = cur_schema
                    .as_ref()
                    .filter(|id| schemas.contains(id))
                    .cloned();
                let mut v = vec![
                    M::leaf(
                        "跟随全局（默认）",
                        cmd(MenuCmd::AppSchemaRule(0)),
                        enabled,
                        !remember && fixed.is_none(),
                    ),
                    M::leaf(
                        "记住上次",
                        cmd(MenuCmd::AppSchemaRule(1)),
                        enabled,
                        remember,
                    ),
                    M::separator(),
                ];
                for (i, id) in schemas.iter().enumerate() {
                    v.push(M::leaf(
                        self.engine_mgr.schema_name(id),
                        cmd(MenuCmd::AppSchemaRule(i as u16 + 2)),
                        enabled,
                        fixed.as_deref() == Some(id.as_str()),
                    ));
                }
                v
            };
            // 状态提示位置：跟随全局 / 跟随光标 / 固定 / ── / 各锚点 / 子菜单「坐标不可用时」。
            // 勾选看规则（不是生效值），「跟随全局」独立一档，理由见上面 `tri`。
            let status_children = {
                use wind_config::app_compat::{
                    StatusAnchor, StatusFallback as SF, StatusPositionMode as SP,
                };
                let mut v = vec![
                    M::leaf(
                        "跟随全局",
                        cmd(MenuCmd::StatusPositionRule(0)),
                        enabled,
                        cur_status.is_none(),
                    ),
                    M::leaf(
                        "跟随光标",
                        cmd(MenuCmd::StatusPositionRule(1)),
                        enabled,
                        cur_status == Some(SP::FollowCaret),
                    ),
                    M::leaf(
                        "固定（取当前位置）",
                        cmd(MenuCmd::StatusPositionRule(2)),
                        enabled,
                        cur_status == Some(SP::Fixed),
                    ),
                    M::separator(),
                ];
                for (i, a) in StatusAnchor::ALL.iter().enumerate() {
                    v.push(M::leaf(
                        status_anchor_label(*a),
                        cmd(MenuCmd::StatusPositionRule(i as u8 + 3)),
                        enabled,
                        cur_status == Some(SP::Anchor(*a)),
                    ));
                }
                let mut fb = vec![
                    M::leaf(
                        "跟随全局",
                        cmd(MenuCmd::StatusFallbackRule(0)),
                        enabled,
                        cur_status_fb.is_none(),
                    ),
                    M::leaf(
                        "上次位置",
                        cmd(MenuCmd::StatusFallbackRule(1)),
                        enabled,
                        cur_status_fb == Some(SF::Last),
                    ),
                    M::leaf(
                        "不显示",
                        cmd(MenuCmd::StatusFallbackRule(2)),
                        enabled,
                        cur_status_fb == Some(SF::Hide),
                    ),
                    M::separator(),
                ];
                for (i, a) in StatusAnchor::ALL.iter().enumerate() {
                    fb.push(M::leaf(
                        status_anchor_label(*a),
                        cmd(MenuCmd::StatusFallbackRule(i as u8 + 3)),
                        enabled,
                        cur_status_fb == Some(SF::Anchor(*a)),
                    ));
                }
                v.push(M::separator());
                v.push(M::submenu("坐标不可用时", fb));
                v
            };
            vec![
                M::label(header),
                M::separator(),
                M::leaf(
                    "启用此应用规则",
                    cmd(MenuCmd::CompatRuleEnabled(
                        (rule_switch != wind_config::app_compat::RuleSwitch::Enabled) as u8,
                    )),
                    rule_switch != wind_config::app_compat::RuleSwitch::NoRule,
                    rule_switch == wind_config::app_compat::RuleSwitch::Enabled,
                ),
                M::separator(),
                M::submenu("初始输入模式", tri(cur_mode, MenuCmd::InitialMode)),
                M::submenu("初始标点模式", tri(cur_punct, MenuCmd::InitialPunct)),
                M::submenu("方案", app_schema_children),
                M::separator(),
                // 三档互斥，做成子菜单单选（布尔开关能同时打开，instant 抢先放行会让 fast 永不生效）。
                // 括号里写代价而不写机制：用户在选遇到慢宿主时宁可等还是宁可先显示。
                //
                // 「跟随全局」独立一档（理由见 `tri`），「（默认）」标记也只在这一档：本应用显式
                // 配了恰好等于全局值的那一档，不会跟着全局 `ui.candidate.first_show_mode` 一起变。
                M::submenu(
                    "候选窗首显",
                    Self::FIRST_SHOW_MENU
                        .iter()
                        .map(|(id, mode, label)| {
                            M::leaf(
                                *label,
                                cmd(MenuCmd::FirstShowMode(*id)),
                                enabled,
                                cur_first_show == *mode,
                            )
                        })
                        .collect(),
                ),
                // 「跟随全局」独立一档（理由见 `tri`）。禁用一档主要给表格类宿主：Excel / WPS 表格
                // 输入态下方向键 = 确认单元格并移动，配对后的光标回退无法实现。
                M::submenu(
                    "符号自动配对",
                    vec![
                        M::leaf(
                            "跟随全局",
                            cmd(MenuCmd::AutoPairRule(0)),
                            enabled,
                            cur_auto_pair.is_none(),
                        ),
                        M::leaf(
                            "启用",
                            cmd(MenuCmd::AutoPairRule(1)),
                            enabled,
                            cur_auto_pair == Some(true),
                        ),
                        M::leaf(
                            "禁用",
                            cmd(MenuCmd::AutoPairRule(2)),
                            enabled,
                            cur_auto_pair == Some(false),
                        ),
                    ],
                ),
                // 「固定位置」给 caret 坐标报不准的宿主。位置不在这里选：落点由用户拖一次候选窗
                // 定下（存进该应用自己的规则），只给开关不给坐标框。
                M::submenu("状态提示位置", status_children),
                M::submenu(
                    "候选窗定位",
                    vec![
                        M::leaf(
                            "跟随全局",
                            cmd(MenuCmd::CandidatePositionRule(0)),
                            enabled,
                            cur_cand_pos.is_none(),
                        ),
                        M::leaf(
                            "跟随光标",
                            cmd(MenuCmd::CandidatePositionRule(1)),
                            enabled,
                            cur_cand_pos == Some(CP::FollowCaret),
                        ),
                        M::leaf(
                            "固定位置",
                            cmd(MenuCmd::CandidatePositionRule(2)),
                            enabled,
                            cur_cand_pos == Some(CP::Fixed),
                        ),
                    ],
                ),
                // 给自作主张写 compartment 切中英的宿主（WinForms/WPF 焦点落到按钮就关 IME、
                // SSMS 内部焦点切换时打开 IME），开和关两个方向都管。
                //
                // 「跟随全局」= 清除（下层设了就写 `unset`），全局开关是 `input.ignore_host_ime_switch`。
                // 「采纳」与之当前行为可能相同但语义不同：全局开了忽略、某应用（gvim）依赖这个
                // 状态时，只有「采纳」能盖住全局。
                M::submenu(
                    "软件切换中英",
                    vec![
                        M::leaf(
                            "跟随全局",
                            cmd(MenuCmd::IgnoreHostImeSwitchRule(0)),
                            enabled,
                            cur_ignore_switch.is_none(),
                        ),
                        M::leaf(
                            "忽略",
                            cmd(MenuCmd::IgnoreHostImeSwitchRule(1)),
                            enabled,
                            cur_ignore_switch == Some(true),
                        ),
                        M::leaf(
                            "采纳",
                            cmd(MenuCmd::IgnoreHostImeSwitchRule(2)),
                            enabled,
                            cur_ignore_switch == Some(false),
                        ),
                    ],
                ),
                // 给「宿主把普通输入框误报成密码框」的应用单独关掉（全局照旧保护真密码框），
                // 或在全局关掉时只给某个应用开。「跟随全局」独立一档，理由见上面 `tri`。
                M::submenu(
                    "密码框强制英文",
                    vec![
                        M::leaf(
                            "跟随全局",
                            cmd(MenuCmd::PasswordForceEnglishRule(0)),
                            enabled,
                            cur_pfe.is_none(),
                        ),
                        M::leaf(
                            "开",
                            cmd(MenuCmd::PasswordForceEnglishRule(1)),
                            enabled,
                            cur_pfe == Some(true),
                        ),
                        M::leaf(
                            "关",
                            cmd(MenuCmd::PasswordForceEnglishRule(2)),
                            enabled,
                            cur_pfe == Some(false),
                        ),
                    ],
                ),
            ]
        };

        let mut items = vec![
            M::submenu("输入方案", schema_children),
            M::leaf("全角", cmd(MenuCmd::ToggleWidth), true, full),
            M::leaf("中文标点", cmd(MenuCmd::TogglePunct), true, punct),
            M::leaf("简入繁出", cmd(MenuCmd::ToggleS2t), true, s2t),
            M::submenu("检索范围", filter_children),
            M::separator(),
        ];
        // 宿主贴不了面板（Linux 旧 addon）时两项都是点了没反应的死开关，不给。
        if self.host_has_panels() {
            items.extend([
                M::leaf(
                    TOOLBAR_MENU_LABEL,
                    cmd(MenuCmd::ToggleToolbar),
                    true,
                    toolbar_vis,
                ),
                self.soft_keyboard_menu_item(),
            ]);
        }
        items.extend([
            M::submenu("主题", theme_children),
            M::separator(),
            M::leaf("重载配置", cmd(MenuCmd::ReloadConfig), true, false),
            M::leaf("重启服务", cmd(MenuCmd::RestartService), true, false),
            M::separator(),
            M::submenu("应用独立配置", per_app_children),
            M::submenu("高级", advanced_children),
            M::separator(),
            M::leaf("词库管理...", cmd(MenuCmd::OpenDictionary), true, false),
            M::leaf("设置...", cmd(MenuCmd::OpenSettings), true, false),
            M::separator(),
            M::leaf(
                format!(
                    "关于 v{}{}",
                    env!("WIND_APP_VERSION"),
                    if wind_config::variant::is_dev() {
                        " (Dev)"
                    } else {
                        ""
                    }
                ),
                cmd(MenuCmd::OpenAbout),
                true,
                false,
            ),
        ]);
        items
    }

    /// 把 `MenuItemSpec` 树映射为线格式 `MenuNode` 树（id 由 `MenuKind::to_menu_id` 派生）。
    #[cfg(ext_presenter)]
    pub(crate) fn menu_items_to_nodes(
        items: &[wind_ui_types::MenuItemSpec],
    ) -> Vec<wind_ipc::codec::MenuNode> {
        use wind_ui_types::MenuKind;
        items
            .iter()
            .map(|it| wind_ipc::codec::MenuNode {
                id: it.kind.to_menu_id(),
                separator: matches!(it.kind, MenuKind::Separator),
                checked: it.checked,
                disabled: !it.enabled,
                label: it.label.clone(),
                children: Self::menu_items_to_nodes(&it.children),
            })
            .collect()
    }

    pub(crate) fn is_menu_open(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .menu_open
    }

    /// 关闭菜单。单点收口：所有菜单关闭路径（ESC/点击外部/动作执行完毕）都经此函数，
    /// 顺带清除 tooltip 右键菜单的 suppress_hide 抑制标志——不区分是否为 tooltip 菜单，
    /// 非 tooltip 菜单关闭时清除是无操作（tooltip 菜单未打开则标志本就是 false）。
    pub(crate) fn menu_close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(ext_presenter)]
        self.candidate_menu_open
            .store(false, std::sync::atomic::Ordering::Release);
        if state.menu_open {
            state.menu_open = false;
            state.menu_opened_at = None;
            drop(state);
            let _ = self.ui_tx.send(UiCommand::HideMenu);
        }
    }

    /// 菜单打开的状态收口：所有 `show_*_menu` 都必须经此置位。
    ///
    /// `menu_open` 与 `menu_opened_at` 必须成对写入，置位点有多个（主菜单 / 候选右键 /
    /// 状态气泡 / tooltip），漏写表现为「菜单偶尔一弹就没」这种难复现的时序问题。
    pub(crate) fn mark_menu_open(&self, page_local: usize, text: String) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.menu_open = true;
        s.menu_opened_at = Some(std::time::Instant::now());
        #[cfg(ext_presenter)]
        {
            s.menu_touched_at = s.menu_opened_at;
        }
        s.menu_target_page_local = page_local;
        #[cfg(ext_presenter)]
        self.candidate_menu_open
            .store(!text.is_empty(), std::sync::atomic::Ordering::Release);
        s.menu_target_text = text;
    }

    /// 焦点发生变化时关闭菜单（焦点路径专用，与 `menu_close` 的区别只在守卫与日志）。
    ///
    /// 菜单是模态 UI，「任何外部动作都该终结它」；清输入态是破坏性操作，「宁可晚做不能误做」。
    /// 为保护输入态而设的闸门（`CtxLost` 豁免、陈旧失焦丢弃、DLL 侧翻转沿去重）不该把关菜单
    /// 一并吞掉，故本函数自成一路：不看 reason（关菜单幂等、非破坏性）；须在
    /// `is_stale_focus_event` 之前调用（陈旧失焦同样证明用户动了别处）。
    ///
    /// 覆盖面有限，不能替代 UI 层的「点菜单外面就关」：同一个文本框内点击、explorer 里桌面到
    /// 任务栏等不产生 TSF 焦点事件。守卫只保护本函数这条路：`handle_focus_lost` 的
    /// `clears_input` 分支照旧无条件关菜单。
    pub(crate) fn menu_close_on_focus_change(&self, why: &str) {
        {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !s.menu_open {
                return;
            }
            // 守卫期内的焦点事件多半是打开菜单这个动作本身的尾迹：跨宿主切换时旧宿主的
            // focus_lost 晚约 100ms 到达，从任务栏语言栏图标点开菜单正好落在这个窗口里。
            if let Some(at) = s.menu_opened_at
                && at.elapsed() < MENU_FOCUS_GUARD
            {
                tracing::debug!(
                    "menu_close_on_focus_change({why}): 距菜单打开 {:?} < 守卫期，忽略",
                    at.elapsed()
                );
                return;
            }
        }
        tracing::debug!("menu_close_on_focus_change({why}): 关闭菜单");
        self.menu_close();
        // 与 UiEvent::MenuClose 同处置：焦点路径没有后续动作派发，可立即解除隐藏抑制
        // （`menu_action` 那条路必须延后，见 clear_tooltip_menu_flag）。
        self.clear_tooltip_menu_flag();
    }

    /// 解除 Tooltip 的「菜单打开中」隐藏抑制。
    ///
    /// 必须在菜单动作派发之后调用，不能并进 `menu_close()`：`menu_action()` 先 `menu_close()`
    /// 再 `run_menu_cmd()`，在前者里解除的话 UI 线程会先隐藏 tooltip（光标在菜单上），
    /// 再处理 `ScreenshotTooltip`，截图恒定失败。
    pub(crate) fn clear_tooltip_menu_flag(&self) {
        let _ = self.ui_tx.send(UiCommand::SetTooltipMenuOpen(false));
        // 状态气泡同理，恢复自动隐藏计时。它只影响隐藏抑制、不会立即隐藏窗口，不受截图时序制约。
        let _ = self.ui_tx.send(UiCommand::SetStatusMenuOpen(false));
    }

    /// 记一次菜单操作（空闲超时从这里重新起算，见 [`MENU_IDLE_TIMEOUT`]）。
    #[cfg(ext_presenter)]
    pub(crate) fn touch_menu(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.menu_open {
            s.menu_touched_at = Some(std::time::Instant::now());
        }
    }

    /// 直接写 `menu_open = false` 的清理路径（失焦清输入 / 切走输入法 / 组合被终止）调它让 UI
    /// 也收菜单：Windows 那边靠随后的 `HideCandidates` 连带收掉，Linux 的 forwarder 不连带
    /// （见 `notify_ui_hide`）。**不取 `state` 锁**——这几处调用时正持着它。
    #[cfg(ext_presenter)]
    pub(crate) fn hide_menu_ui_unlocked(&self) {
        self.candidate_menu_open
            .store(false, std::sync::atomic::Ordering::Release);
        let _ = self.ui_tx.send(UiCommand::HideMenu);
    }

    /// addon 报「菜单被我收掉了」（`menu.dismiss`）或连接断开：无条件复位并让 UI 收菜单。
    ///
    /// 不走 `menu_close` 的「开着才发 HideMenu」：两端认识错开时（协调器以为关了、UI 还挂着），
    /// 正是这条要对齐的情形。
    #[cfg(ext_presenter)]
    pub(crate) fn menu_dismissed_by_host(&self, why: &str) {
        let was_open = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut s.menu_open, false)
        };
        self.candidate_menu_open
            .store(false, std::sync::atomic::Ordering::Release);
        tracing::debug!("宿主收起菜单（{why}），协调器此前 menu_open={was_open}");
        let _ = self.ui_tx.send(UiCommand::HideMenu);
        self.clear_tooltip_menu_flag();
    }

    /// addon 请求打开菜单（`menu.open`：右键候选 / 候选窗空白处 / 状态气泡 / 悬停提示，或
    /// Fcitx5 状态区入口）。`target` ≥ 0 为候选右键菜单（页内下标），负值见
    /// [`wind_ipc::protocol::menu_target`]，未知负值按主菜单；`work` = (左, 上, 右, 下)。
    #[cfg(ext_presenter)]
    pub(crate) fn open_menu_from_host(&self, req: MenuOpenRequest) {
        use wind_ipc::protocol::menu_target::*;
        let [left, top, right, bottom] = req.work;
        let (x, y) = (req.x, req.y);
        // 工作区先于菜单到 UI 线程：同一条命令通道保序，`show` 定位时已是新值。
        let _ = self.ui_tx.send(UiCommand::SetWorkArea {
            left,
            top,
            right,
            bottom,
        });
        match req.target {
            t if t >= 0 => self.show_candidate_menu(t as usize, x, y),
            MENU_TARGET_STATUS => self.show_status_menu(x, y),
            // 命中（按段 / 按行）要提示文本块的排布，只有渲染端知道：先让 UI 换算，它回
            // `RequestTooltipMenu` 再走与 Windows 同一个 `show_tooltip_menu`。缺位图内坐标按
            // (-1, -1)，等同点在内边距上（只给整体操作）。
            MENU_TARGET_TOOLTIP => {
                let (local_x, local_y) = req.local.unwrap_or((-1, -1));
                let _ = self.ui_tx.send(UiCommand::TooltipMenuAt {
                    x,
                    y,
                    local_x,
                    local_y,
                });
            }
            _ => self.show_main_menu(MenuAnchor::at_point(x, y)),
        }
    }

    /// Linux：面板（工具栏）发起的菜单打开前，把 `host.display` 报的焦点输出工作区下发给 UI。
    /// 菜单定位只认这份工作区，缺了它子菜单直接朝右展开、弹到屏外。没报过就不发。
    ///
    /// 不放进 `show_main_menu`：`open_menu_from_host` 也调它，那里 addon 随请求报的工作区会被覆盖。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub(crate) fn send_host_work_area(&self) {
        let work = *self.host_work.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((left, top, right, bottom)) = work {
            let _ = self.ui_tx.send(UiCommand::SetWorkArea {
                left,
                top,
                right,
                bottom,
            });
        }
    }

    /// 菜单打开时转发导航键给菜单窗口；返回 true 表示已消费。
    pub(crate) fn forward_menu_key(&self, key_code: u32) -> bool {
        if !self.is_menu_open() {
            return false;
        }
        // Linux：空闲超时兜底——菜单多半已不在屏上（addon 的关闭报告丢了），本键照常处理。
        #[cfg(ext_presenter)]
        {
            let touched = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .menu_touched_at;
            if menu_idle_expired(touched, std::time::Instant::now(), MENU_IDLE_TIMEOUT) {
                tracing::info!("菜单空闲超时仍标记为打开，视为已关闭（按键照常处理）");
                self.menu_close();
                self.clear_tooltip_menu_flag();
                return false;
            }
            self.touch_menu();
        }
        match key_code {
            // 方向键/回车/空格/ESC → 菜单窗口处理（导航/下钻/返回/激活/关闭）
            0x26
            | 0x28
            | 0x25
            | 0x27
            | keymap::VK_RETURN
            | keymap::VK_SPACE
            | keymap::VK_ESCAPE => {
                let _ = self.ui_tx.send(UiCommand::MenuKey(key_code));
            }
            // 其它键：关闭菜单并吞掉，一并解除气泡 / 状态气泡的隐藏抑制（否则气泡移出不隐藏、
            // 右键被当成菜单开着）。这里没有菜单命令要派发，不受截图时序约束。
            _ => {
                self.menu_close();
                self.clear_tooltip_menu_flag();
            }
        }
        true
    }

    /// 构建右键候选菜单项并下发给 UI 显示。菜单项本身由 [`Self::candidate_menu_items`] 构造。
    pub(crate) fn show_candidate_menu(&self, page_local: usize, x: i32, y: i32) {
        // 右键落在分类标签上：不是候选，没有候选菜单。
        if self.grid_tab_of_event(page_local).is_some() {
            return;
        }
        // 悬停落到被右键的这一格：菜单开着时鼠标消息到不了候选窗，在另一格上再右键时旧格的
        // 悬停底色会留在原处。
        let ui_idx = page_local as i32;
        let hover_moved = self
            .hover_index
            .swap(ui_idx, std::sync::atomic::Ordering::Relaxed)
            != ui_idx;
        // 展开态下右键的是网格里的某一格：先把高亮切过去（菜单操作的就是高亮那一格），
        // 再按页内下标走下面的原有逻辑。悬停变了而高亮没变时，下面单独补一帧。
        let Some(page_local) = self.resolve_event_index(page_local, !hover_moved) else {
            return;
        };
        if hover_moved {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.candidates.is_empty() {
                self.notify_ui_update(&st);
            }
        }
        let (idx, word) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.candidates.is_empty() {
                return;
            }
            let (start, end) = self.page_range(&state);
            let idx = start + page_local;
            if idx >= end || idx >= state.candidates.len() {
                return;
            }
            (idx, state.candidates[idx].text.clone())
        };
        let Some(items) = self.candidate_menu_items(idx) else {
            return;
        };
        self.mark_menu_open(page_local, word);
        // 候选右键菜单在光标处向下弹出（above=false，y_bottom 不使用）。
        let _ = self.ui_tx.send(UiCommand::ShowCandidateMenu {
            items,
            anchor: MenuAnchor::at_point(x, y),
        });
    }

    /// 按全量绝对下标构造候选菜单项（不弹菜单、不动菜单状态）；无候选 / 越界 → `None`。
    ///
    /// 桌面右键（[`Self::show_candidate_menu`]）与移动端长按菜单共用这一份，文案、启用态、
    /// 顺序天然一致。词条操作的启用态/删除文案按候选来源动态化：
    /// - 置顶/前移：首项禁用；后移：末项禁用；拼音普通候选禁前移/后移（相对邻居，无稳定语义），
    ///   置顶与「固定到第 N 位」照给。
    /// - 删除：拼音 / 英文 ≥2 字的词→「屏蔽该词」并另给「在此编码下隐藏」；短语→「禁用短语」
    ///   （软删可恢复）；码表用户词/临时词→真删；其余系统词→「在此编码下隐藏」（shadow）。
    /// - 特殊模式（快符等）：词条操作照常提供，编码取其独立缓冲、归属取其引用方案。
    /// - 无词库落点者（临拼/临英/混输/网址，以及特殊模式的空码浏览态）：仅提供复制。
    pub fn candidate_menu_items(&self, idx: usize) -> Option<Vec<wind_ui_types::MenuItemSpec>> {
        use wind_ui_types::MenuItemSpec as M;
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let cand = state.candidates.get(idx)?.clone();
        let word = cand.text.clone();
        let pos = self.reorder_positions(&state, idx);
        let per_page = self.page_size(&state);
        let pin_slots = self.pin_at_slots(&state, idx);
        let scope = self.candidate_op_scope(&state);
        // 快捷输入的格式候选：判据独立于 candidate_op_scope（后者问「有没有词库落点」，
        // 混输没有，返回 None 是对的）。与写端 `candidate_or_quick_format_op` 同源。
        let quick = self.quick_format_scope_at(&state, idx);
        let history = self.quick_history_of(&state, idx).is_some();
        // 删除作用的候选（英文头部原文候选换成对应词库词，找不到 = 删除不可用），与写端
        // `candidate_op_in` 同一判据。
        let delete_target = scope
            .as_ref()
            .and_then(|sc| self.delete_target(&state, &sc.schema, &cand));
        // 删除是否按文字屏蔽：决定文案「屏蔽该词」与有无第二项「在此编码下隐藏」。删除目标取不到
        // （只有英文数据方案的头部原文条）时按原文条自身判。不能套 `delete_blocks_word`：头部
        // 原文条没有来源，恒为假。写端在目标为 None 时两项都不执行，这里只影响文案与项数。
        let blocks_word = scope.as_ref().is_some_and(|sc| match &delete_target {
            Some(t) => self.delete_blocks_word(&sc.schema, t),
            None => wind_store::shadow::is_blockable_word(cand.freq_text()),
        });
        drop(state);

        let op = |o: CandidateOp| MenuKind::Op(o);
        // 快捷输入历史候选：只给「删除此历史」与复制（排序按使用次数，不在任何词库里）。
        // 写端见 `candidate_or_quick_format_op`。
        if history {
            let items = vec![
                M::leaf("删除此历史", op(CandidateOp::Delete), true, false),
                M::separator(),
                M::leaf("复制", MenuKind::Copy, true, false),
            ];
            return Some(items);
        }
        // 格式候选优先：它调的是「这种写法排第几」，标签也相应改写。
        if let Some(q) = quick {
            let has_adjust = {
                let a = self.quick_adjust_of(q.kind);
                !a.is_empty()
            };
            let items = vec![
                // 「同类型内」不能省：置顶只在本类（日期/数字/计算）内生效，类间先后由
                // `mix_modes.members` 决定。
                M::leaf(
                    "置顶（同类型内）",
                    op(CandidateOp::MoveTop),
                    q.index_in_kind > 0,
                    false,
                ),
                M::leaf("上移", op(CandidateOp::MoveUp), q.index_in_kind > 0, false),
                M::leaf("下移", op(CandidateOp::MoveDown), true, false),
                // 面向用户说「隐藏」，存储字段叫 `disabled`：设置页的启用开关要让那一行仍可见、能开回来。
                M::leaf("隐藏此格式", op(CandidateOp::Delete), true, false),
                // 整类恢复：被隐藏的格式不出候选、右键点不到，没有这一项就再也开不回来。
                // 与候选调整菜单的同名项语义不同（那边恢复一条），但两个菜单互斥出现。
                M::leaf("恢复默认", op(CandidateOp::Reset), has_adjust, false),
                M::separator(),
                M::leaf("复制", MenuKind::Copy, true, false),
            ];
            return Some(items);
        }
        // 常用/生僻标记：作用域是全局的那个字，与词库落点无关，故对上面「只有复制」的状态同样
        // 成立，只要求 `cand.text` 是单个可登记的字符。刻意不搭 `candidate_op_scope` 的便车：
        // 否则临拼/临英/混输/空码浏览态下的字标不了。
        let common_item = self.common_char_mark(&word).map(|m| {
            // 文案按当前判定二选一，不加「（全局）」后缀：作用域差异写进设置页的说明。
            let label = if m.common {
                "设为生僻字"
            } else {
                "设为常用字"
            };
            M::leaf(label, op(CandidateOp::ToggleCommon), true, false)
        });

        // 有词库落点才给词条操作。没有独立词库归属的 overlay（临拼/临英/混输/网址）与空码浏览态
        // （读端 apply_shadow_in 对空码直接 return）仅保留复制。判据与写端 `candidate_op` 同源，
        // 见 `candidate_op_scope`。
        let mut items = if let Some(scope) = scope {
            let cand_id = (!cand.id.is_empty()).then_some(cand.id.as_str());
            // 规则键是词库原文，与写端同取 `freq_text`（英文大小写投影后 `text` 对不上）。
            let has_rule =
                self.shadow_has_rule(&scope.schema, &scope.code, cand.freq_text(), cand_id);
            // 拼音普通候选放行置顶与固定到第 N 位，前移/后移仍禁：邻居随词频、学词、整句每天在变，
            // 绝对名次则是用户明确的主张。命令候选不受限。引擎类型来自 scope：特殊模式问的是它
            // 引用的方案。
            //
            // 判据必须与写端 `candidate_op` 逐字对应，否则就是点得动却无反应、或能用却是灰的。
            let is_pinyin = matches!(scope.engine_type, Some(wind_engine::EngineType::Pinyin));
            let group_member = candidate_is_group_member(&cand);
            // emoji 扩展候选：五项词条操作全部灰显（不是隐藏）。它不是词库条目，code 恒空、text
            // 按宿主候选查表而来，写 shadow 规则既不会被命中、还会变成孤儿。放开要两端一起
            // （这里与写端 `candidate_op` 的同名守卫），规划见 design/emoji-suggestion.md §9。
            let emoji = cand.is_emoji_suggestion;
            let pinyin_locked = is_pinyin && !cand.is_command;
            // 单字母引导的重复上屏项：不是这个码的候选，调位一律灰（写端同守卫）。
            let repeat = crate::candidate_grid::is_leading_repeat(&cand);
            let can_pin = !group_member && !emoji && !repeat;
            let movable = !pinyin_locked && !group_member && !emoji && !repeat;
            let (delete_label, delete_enabled) = match &delete_target {
                Some(t) => candidate_delete_menu(t, blocks_word),
                None => (candidate_delete_menu(&cand, blocks_word).0, false),
            };
            let has_rule = has_rule && !emoji;

            let mut items = vec![
                // 位置按完整列表计（分类筛选中 `idx` 只是子集下标），与写端同源。
                M::leaf(
                    "置顶",
                    op(CandidateOp::MoveTop),
                    can_pin && pos.here > 0,
                    false,
                ),
                M::leaf(
                    "前移",
                    op(CandidateOp::MoveUp),
                    movable && pos.here > 0,
                    false,
                ),
                M::leaf(
                    "后移",
                    op(CandidateOp::MoveDown),
                    movable && pos.here < pos.last,
                    false,
                ),
                M::leaf(delete_label, op(CandidateOp::Delete), delete_enabled, false),
            ];
            // 英文引擎（英文方案、临英）不给，见 `pin_at_offered`。
            if crate::handle_candidate::pin_at_offered(scope.engine_type) {
                items.insert(1, pin_at_submenu(per_page, &pin_slots, can_pin));
            }
            // 按文字屏蔽的词另给「只在这个码下不出」：写端 `candidate_op_in` 同判据、同目标词，
            // 不可删时一起灰。不绑热键（删除热键仍是上一项）。
            if blocks_word {
                items.push(M::leaf(
                    "在此编码下隐藏",
                    op(CandidateOp::HideHere),
                    delete_enabled,
                    false,
                ));
            }
            items.push(M::leaf("恢复默认", op(CandidateOp::Reset), has_rule, false));
            items.push(M::separator());
            items
        } else {
            Vec::new()
        };
        // 尾段两项对两个分支都成立，统一在这里追加，别在各分支里各写一份。
        items.extend(common_item);
        items.push(M::leaf("复制", MenuKind::Copy, true, false));
        Some(items)
    }

    /// 把工具栏移到「输入焦点所在显示器」上的记忆位置（该屏没记过则落到它的右下角）。
    ///
    /// 仅在显示器发生变化时下发，靠 `current_toolbar_monitor` 去重：notify_toolbar 在每次模式
    /// 切换/焦点事件上都跑，无条件下发会把用户拖过的位置反复重置。调用点必须在 `UpdateToolbar`
    /// 之前，否则切屏会先在旧屏闪一帧。`Toolbar::set_anchor` 受 `visible` 门控，不绕过
    /// `toolbar_gate` 的显示迟滞。
    ///
    /// 有意接受的后果：工具栏无法停在非焦点屏（拖到副屏而焦点仍在主屏，下一次 `notify_toolbar`
    /// 就拽回来）。工具栏放在用户没在看的屏上违背其用途，别「修」成 sticky 或加开关。拖动期间
    /// 前台窗口未变，本函数 early-return，不会拖到一半被拽走。
    fn sync_toolbar_monitor(&self) {
        let Some(mon) = self.current_monitor() else {
            return;
        };
        let key = mon.key;
        let (_, _, work_right, work_bottom) = mon.work;
        // 「判定换屏 + 记新屏 + 取该屏坐标」必须在同一临界区里完成：否则释放锁后、读表前，拖动线程
        // （`save_toolbar_anchor`）写入的新坐标会被本线程读到的旧值覆盖到屏上。
        //
        // 锁序 `current_toolbar_monitor` → `toolbar_anchors`，与 `save_toolbar_anchor` 一致；
        // 新增取这两把锁的代码请沿用同一顺序。
        let dedup = toolbar_dedup_key(&key, mon.work);
        let saved = {
            let mut cur = self
                .current_toolbar_monitor
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cur.as_deref() == Some(dedup.as_str()) {
                return;
            }
            let saved = self
                .toolbar_anchors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .copied();
            // 无记录且工作区未知：不发命令，也就不能记成「已校正」，否则工作区随后报到时会被
            // 去重挡掉、默认落点永远发不出去。
            #[cfg(all(target_os = "linux", ext_presenter))]
            if saved.is_none() && !work_known(mon.work) {
                tracing::debug!("工具栏跟随焦点显示器 key={key}：无记录且工作区未知，不改位置");
                return;
            }
            *cur = Some(dedup);
            saved
        };
        // 记录必须真的落在这块屏上，否则当作「该屏没记过」：旧版本落盘的错屏 key、显示器拓扑
        // 变化后的旧坐标都会把工具栏发到另一块屏。
        //
        // 不在这里删表项：那块屏当前可能没接，`DEFAULTTONEAREST` 会答成某块现有的屏，据此删会误删
        // 合法记录。只忽略，用户在本屏拖一次就覆盖。
        let saved = saved.filter(|&(right, bottom)| {
            let ok = anchor_on_monitor(right, bottom, &key, mon.work);
            if !ok {
                tracing::info!(
                    "工具栏记录 ({},{}) 不在 key={} 这块屏上，按未记录处理",
                    right,
                    bottom,
                    key
                );
            }
            ok
        });
        let cmd = match saved {
            Some((right, bottom)) => UiCommand::SetToolbarAnchor { right, bottom },
            // 工作区未知且无记录的情形已在上面返回（别发按 0 算出的负坐标，宿主会夹到左上角）。
            // 该屏从未拖过：交给 UI 侧按自己的尺寸算右下角（协调器不知道工具栏 w/h）。
            None => UiCommand::SetToolbarCorner {
                work_right,
                work_bottom,
            },
        };
        tracing::debug!("工具栏跟随焦点显示器 key={} saved={:?}", key, saved);
        let _ = self.ui_tx.send(cmd);
    }

    /// 启动时的初始定位：与运行期同一判据（前台窗口所在显示器），并把该 key 记进
    /// `current_toolbar_monitor`，使首个 notify_toolbar 不会重复下发。
    ///
    /// macOS 上 `focus_monitor` 恒为 None，本函数是 no-op（那边没有悬浮工具栏，forwarder 把
    /// `SetToolbarAnchor`/`SetToolbarCorner` 当留桩丢弃）。Linux 外部宿主形态按 addon 报的输出名
    /// 定位（见 [`Self::current_monitor`]）。
    ///
    /// 桌面构造路径（`new`）专用，仅在无 desktop-ui 时放行 dead_code；不要整体 feature 门控
    /// （同 impl 块的运行期工具栏逻辑仍要用）。
    #[cfg_attr(not(feature = "desktop-ui"), allow(dead_code))]
    pub(crate) fn init_toolbar_pos(&self) {
        self.sync_toolbar_monitor();
    }

    /// 持久化工具栏锚点（窗口**右下角**屏幕坐标，按显示器 key 独立存储，best-effort）。
    ///
    /// key 取自工具栏落点自身而非光标：存取两侧共用同一个键空间语义（取侧问「焦点屏上记过
    /// 什么位置」，存侧答「这块屏上工具栏在哪」），只有 key 同源才对得上。
    pub(crate) fn save_toolbar_anchor(&self, right: i32, bottom: i32) {
        #[cfg(all(target_os = "linux", ext_presenter))]
        let key = self.panel_drop_key(wind_ipc::protocol::overlay::OVERLAY_KIND_TOOLBAR);
        #[cfg(not(all(target_os = "linux", ext_presenter)))]
        let Some(key) = monitor_key_from_anchor(right, bottom) else {
            // 查不到显示器就别存：读取侧（`focus_monitor`）同样失败返回 None，兜底 key 永远读不出来。
            tracing::debug!("工具栏位置未保存：查不到 ({},{}) 所在显示器", right, bottom);
            return;
        };
        self.store_toolbar_anchor(key, right, bottom);
    }

    /// [`Self::save_toolbar_anchor`] 的落盘半段：显示器 key 已由调用方定好。Linux 的 key 是 addon
    /// 报的输出名，不经 `monitor_key_from_anchor`。
    fn store_toolbar_anchor(&self, key: String, right: i32, bottom: i32) {
        // 拖到别的屏 = 用户在那块屏上重新定了位；同步当前屏记录，否则下一次
        // sync_toolbar_monitor 会认为「屏没变」而不再校正。
        {
            let mut cur = self
                .current_toolbar_monitor
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cur = Some(self.toolbar_dedup_key_for_drop(&key));
        }
        let snapshot = {
            let mut map = self
                .toolbar_anchors
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            map.insert(key, (right, bottom));
            map.clone()
        };
        // 整份快照交给写入器（覆盖语义要求闭包自带完整目标值）。合并 + 串行化 + 关机 flush
        // 在 `state_writer` 那一侧。
        self.state_writer.schedule("toolbar_anchors", move |rs| {
            rs.toolbar_anchors = snapshot.clone();
        });
    }

    /// 持久化软键盘锚点（面板右下角屏幕坐标）。与 `save_toolbar_anchor` 同构，但不碰
    /// `current_toolbar_monitor`：软键盘由用户显式开关，不参与跟随焦点换屏。
    pub(crate) fn save_softkeyboard_anchor(&self, right: i32, bottom: i32) {
        #[cfg(all(target_os = "linux", ext_presenter))]
        let key = self.panel_drop_key(wind_ipc::protocol::overlay::OVERLAY_KIND_SOFTKEYBOARD);
        #[cfg(not(all(target_os = "linux", ext_presenter)))]
        let Some(key) = monitor_key_from_anchor(right, bottom) else {
            tracing::debug!("软键盘位置未保存：查不到 ({},{}) 所在显示器", right, bottom);
            return;
        };
        self.store_softkeyboard_anchor(key, right, bottom);
    }

    /// [`Self::save_softkeyboard_anchor`] 的落盘半段，key 的来历同 [`Self::store_toolbar_anchor`]。
    fn store_softkeyboard_anchor(&self, key: String, right: i32, bottom: i32) {
        let snapshot = {
            let mut map = self
                .softkeyboard_anchors
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            map.insert(key, (right, bottom));
            map.clone()
        };
        self.state_writer
            .schedule("softkeyboard_anchors", move |rs| {
                rs.softkeyboard_anchors = snapshot.clone();
            });
    }

    /// 焦点屏上记过的软键盘锚点 + 该屏工作区，供 `ShowSoftKeyboard` 携带。
    ///
    /// 查不到显示器时两者都是 `None`：UI 侧据此回退到自己的默认锚点。
    pub(crate) fn softkeyboard_placement(&self) -> SoftKeyboardPlacement {
        let Some(mon) = self.current_monitor() else {
            return SoftKeyboardPlacement {
                anchor: None,
                work: None,
            };
        };
        // 取值与校验分两步：`.lock()` 的临时 guard 活到整条 `let` 语句结束，链在一起会让
        // `monitor_key_from_anchor` 的系统调用在持锁时执行，而拖动落盘要写同一把锁。
        let saved = self
            .softkeyboard_anchors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&mon.key)
            .copied();
        // 与 `sync_toolbar_monitor` 同一道闸门：记录不在这块屏上就当没记过，落回该屏默认位置。
        let anchor =
            saved.filter(|&(right, bottom)| anchor_on_monitor(right, bottom, &mon.key, mon.work));
        // 工作区未知（Linux 旧 addon）时不带：UI 侧回退到自己的默认落点。
        #[cfg(all(target_os = "linux", ext_presenter))]
        let work = work_known(mon.work).then_some(mon.work);
        #[cfg(not(all(target_os = "linux", ext_presenter)))]
        let work = Some(mon.work);
        SoftKeyboardPlacement { anchor, work }
    }

    /// Linux：addon 拖完 `kind` 浮层后的落盘键（`pos.*` 暂存，取一次即清）。没有暂存时按
    /// `on_host_panel_dropped` 同一规则取焦点输出名。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn panel_drop_key(&self, kind: u32) -> String {
        let dropped = self
            .panel_drop_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&kind);
        dropped.unwrap_or_else(|| self.host_focus_output())
    }

    /// Linux：addon 最近一次 `host.display` 报的焦点输出名，没报过 = `"default"`。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn host_focus_output(&self) -> String {
        self.host_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| HOST_OUTPUT_DEFAULT.to_owned())
    }

    /// 拖动落盘时写进 `current_toolbar_monitor` 的去重键。Linux 的键带工作区（见
    /// [`toolbar_dedup_key`]），落点输出就是焦点输出时取它此刻的工作区；落到别的输出则与焦点
    /// 的键必然不同，下一次校正照常拽回焦点屏（同 Windows）。
    fn toolbar_dedup_key_for_drop(&self, key: &str) -> String {
        #[cfg(all(target_os = "linux", ext_presenter))]
        {
            let work = self
                .host_work
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or((0, 0, 0, 0));
            toolbar_dedup_key(key, work)
        }
        #[cfg(not(all(target_os = "linux", ext_presenter)))]
        {
            key.to_owned()
        }
    }

    /// Linux addon 本地拖完工具栏 / 软键盘：记下落点输出名，再让渲染端同步位置。落盘要的是右下角，
    /// 换算要用面板尺寸，所以由渲染端算好后经 `ToolbarMoved` / `SoftKeyboardMoved` 回报，
    /// 走 [`Self::save_toolbar_anchor`] / [`Self::save_softkeyboard_anchor`] 那条与 Windows 相同的路。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub(crate) fn on_host_panel_dropped(&self, kind: u32, x: i32, y: i32, output: Option<String>) {
        // 没报落点输出就记在焦点输出名下：与读取侧（`current_monitor`）同源，只有一边报了
        // output 也不会错桶。
        let key = output.unwrap_or_else(|| self.host_focus_output());
        tracing::debug!("浮层 kind={kind} 拖到 ({x},{y}) output={key}");
        self.panel_drop_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(kind, key);
        let _ = self.ui_tx.send(UiCommand::PanelMoved { kind, x, y });
    }

    /// 输入焦点所在显示器：工具栏 / 软键盘位置记忆的取用单位。
    ///
    /// Windows 问系统（[`focus_monitor`]）。Linux 外部宿主形态问不到屏幕几何，用 addon 经
    /// `host.display` 报的焦点输出名分桶——没报过就落到固定键 `"default"`，退化成只记一个位置；
    /// 工作区同样取它报的，没报则为全 0（见 `work_known`）。
    pub(crate) fn current_monitor(&self) -> Option<MonitorInfo> {
        #[cfg(all(target_os = "linux", ext_presenter))]
        {
            let key = self.host_focus_output();
            let work = self
                .host_work
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or((0, 0, 0, 0));
            Some(MonitorInfo { key, work })
        }
        #[cfg(not(all(target_os = "linux", ext_presenter)))]
        {
            focus_monitor()
        }
    }

    /// 工具栏单元格点击：复用菜单命令切换状态（内部已推送 C++），再刷新工具栏显示。
    pub(crate) fn mouse_toolbar(&self, action: ToolbarAction) {
        match action {
            ToolbarAction::OpenSettings => {
                self.open_settings(None);
                return;
            }
            // 两个转换方向各一格。互斥让一次点击改动两格，故都要 notify_toolbar。
            ToolbarAction::ToggleS2t => {
                self.handle_menu_command("toggle_s2t");
                self.notify_toolbar();
                return;
            }
            ToolbarAction::ToggleT2s => {
                self.handle_menu_command("toggle_t2s");
                self.notify_toolbar();
                return;
            }
            ToolbarAction::Custom(i) => {
                self.run_toolbar_button(i);
                return;
            }
            ToolbarAction::ToggleSoftKeyboard => {
                // 不走 `handle_menu_command` 的动词表：软键盘开启要接管后续按键，
                // 与 `add_word` 同类，不符 `dispatch_hotkey` 的 bool 契约。
                self.toggle_softkeyboard(None);
                self.after_softkeyboard_change();
                return;
            }
            _ => {}
        }
        let cmd = match action {
            ToolbarAction::ToggleMode => "toggle_mode",
            ToolbarAction::SwitchEngine => "switch_engine",
            ToolbarAction::TogglePunct => "toggle_punct",
            ToolbarAction::ToggleWidth => "toggle_width",
            // 上面那个 match 已 return 掉的三支。加 ToolbarAction 变体时必须一并处理上面那个
            // match，漏了就在这里 panic，而不是静默不响应。
            ToolbarAction::ToggleS2t
            | ToolbarAction::ToggleT2s
            | ToolbarAction::OpenSettings
            | ToolbarAction::Custom(_)
            | ToolbarAction::ToggleSoftKeyboard => {
                unreachable!()
            }
        };
        self.handle_menu_command(cmd);
        self.notify_toolbar();
    }

    /// 执行自定义按钮的动作（`ui.toolbar.buttons[i].action`，cmdbar 表达式）。
    ///
    /// 复用短语动作那条链（`run_command_candidate`），故 `open` / `proc.run` / `key.tap` /
    /// `wind.cli` 等全部可用，求值失败会弹 toast。经 `spawn_command` 起独立线程（动作链会回调
    /// 自锁的 coordinator 方法）。下标取不到就忽略：配置刚重载时 UI 侧的 spec 可能一瞬错开。
    fn run_toolbar_button(&self, index: u8) {
        let btn = {
            let cfg = self.rt();
            match cfg.config.ui.toolbar.buttons.get(index as usize) {
                Some(b) => b.clone(),
                None => {
                    tracing::warn!("工具栏自定义按钮下标 {index} 越界（配置刚变？），已忽略");
                    return;
                }
            }
        };
        let action = btn.action.trim();
        if action.is_empty() {
            // 配了按钮却没配动作：点了没反应是最难自查的一类，给一条日志。
            tracing::warn!("工具栏自定义按钮 {:?} 未配置 action", btn.id);
            return;
        }
        self.spawn_user_command(action);
    }

    /// 执行一段用户写的 cmdbar 表达式：工具栏自定义按钮与按键绑定 `command:<表达式>` 共用这一个
    /// 入口，两处对写法的容忍度（裸表达式 / 已带 `$CC(...)`）一致。
    pub(crate) fn spawn_user_command(&self, expr: &str) {
        self.spawn_command(wrap_command_source(expr), String::new());
    }

    /// 焦点/激活切换路径专用：先用缓存值立即同步通知（bridge handler 线程不阻塞），再后台
    /// 刷新全屏缓存，状态变化则再次通知。非焦点路径直接调 notify_toolbar()。
    ///
    /// 副作用：它是全屏复查线程（`coordinator/fullscreen_watch.rs`）的唯一起点，第一次焦点/
    /// 激活事件时懒启动，此后常驻。
    pub(crate) fn notify_toolbar_async(&self) {
        // 立即用缓存值通知，bridge 线程无阻塞
        self.notify_toolbar();
        // 全屏复查线程懒启动：焦点事件只覆盖焦点变化的那一刻，焦点稳定后进出全屏（F11、
        // 全屏播放）没有回调，得靠它。非 Windows 上是空实现。
        self.ensure_fullscreen_watch();
        // 探测不受 `hide_in_fullscreen` 门控：同一次探测还要给候选窗的「D3D 独占全屏不弹窗」
        // 判据（`fullscreen_exclusive_cached`）刷值，那条没有开关。
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        // 单飞：已有探测在途就跳过（探的是同一个全局前台状态，而焦点变化成串来）。复查线程
        // 走同一道闸（`ensure_fullscreen_watch`）。闸在这里取、由起出来的线程持 [`ProbeGate`]
        // 归还：判在途必须发生在起线程之前。
        //
        // 不并入 first-show 那个共享定时器：foreground_fullscreen_kind 会阻塞，会拖垮兜底时限。
        if !self.try_take_probe_gate() {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("fullscreen-probe".into())
            .spawn(move || {
                let Some(c) = weak.upgrade() else {
                    // 协调器已析构，闸随它一起消失，无须归还。
                    return;
                };
                let _gate = c.adopt_probe_gate();
                c.commit_fullscreen_kind(crate::foreground_fullscreen_kind());
            });
        if spawned.is_err() {
            // 线程没起来就得把闸放回去，否则此后永远不再探测
            self.fullscreen_probing
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }

    /// 取全屏探测的单飞闸；已有探测在途返回 `false`。
    ///
    /// 取到之后必须由真正执行探测的那条线程调 [`Self::adopt_probe_gate`] 接管归还（唯一例外是
    /// 线程压根没起来）。分两步是因为判在途要在起线程之前，归还要等探测跑完。
    pub(crate) fn try_take_probe_gate(&self) -> bool {
        !self
            .fullscreen_probing
            .swap(true, std::sync::atomic::Ordering::AcqRel)
    }

    /// 接管单飞闸的归还责任：返回的 [`ProbeGate`] 析构时把闸放回去。
    pub(crate) fn adopt_probe_gate(self: &std::sync::Arc<Self>) -> ProbeGate {
        ProbeGate(std::sync::Arc::clone(self))
    }

    /// 把一次探测的结果写进两个缓存，有变化就重新通知工具栏 / 候选窗。
    ///
    /// 与探测分开：焦点事件那条直采直信，复查线程那条要先连着看两拍（见
    /// `coordinator/fullscreen_watch.rs` 的 `confirmed`）。只能在后台线程调用（内部取 `state` 锁
    /// 并下发 UI 命令），且调用方须持有 [`ProbeGate`]。
    pub(crate) fn commit_fullscreen_kind(&self, kind: crate::FullscreenKind) {
        let is_fs = kind != crate::FullscreenKind::None;
        let is_excl = kind == crate::FullscreenKind::D3dExclusive;
        let prev = self
            .fullscreen_cached
            .swap(is_fs, std::sync::atomic::Ordering::Relaxed);
        let prev_excl = self
            .fullscreen_exclusive_cached
            .swap(is_excl, std::sync::atomic::Ordering::Relaxed);
        if prev_excl != is_excl {
            // 独占全屏态翻转：候选窗该收的收、该弹的弹（见 handle_uielement.rs）。
            tracing::info!(
                "前台 D3D 独占全屏={is_excl}，候选窗{}",
                if is_excl {
                    "改为不弹出"
                } else {
                    "恢复显示"
                }
            );
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.notify_ui_update(&st);
        }
        // 工具栏要在任一位翻转时重算，不能只看 `is_fs`：`notify_toolbar` 的否决项还含经
        // `ui_suppressed_by_host()` 读到的 `fullscreen_exclusive_cached`。`Covering → D3dExclusive`
        // （无边框游戏切独占、PPT 进放映）时 `is_fs` 不翻转，工具栏会留在独占全屏上把游戏踢出
        // 独占态；反向则工具栏回不来。
        if prev != is_fs || prev_excl != is_excl {
            tracing::info!(
                "前台全屏态={kind:?}（此前 fs={prev} excl={prev_excl}），工具栏重算可见性"
            );
            self.notify_toolbar();
        }
    }

    /// 只提交「前台铺满显示器」这一格（判据②），不碰 `fullscreen_exclusive_cached`。
    ///
    /// 复查线程专用：它按固定节拍采样，刻意不问判据①（`SHQueryUserNotificationState` 是跨进程
    /// RPC），没问就不能拿「没问到」去清独占位。只能在后台线程调用，调用方须持有 [`ProbeGate`]。
    /// 生产调用点只在 `cfg(windows)` 的复查线程里。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn commit_fullscreen_covering(&self, covering: bool) {
        let prev = self
            .fullscreen_cached
            .swap(covering, std::sync::atomic::Ordering::Relaxed);
        if prev != covering {
            tracing::info!("前台铺满显示器={covering}（周期复查），工具栏重算可见性");
            self.notify_toolbar();
        }
    }

    /// 工具栏「三项合取」的当前值（不含全屏否决），即「用户此刻停在某个宿主的可编辑控件里，
    /// 且开着工具栏」。全屏复查线程拿它当开销闸。自带取锁，不可在持 `state` 锁时调用；
    /// 与 `notify_toolbar` 共用 [`State::toolbar_conjunction`] 保持同源。生产调用点只在
    /// `cfg(windows)` 的复查线程里。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn toolbar_wants_display(&self) -> bool {
        let hide_in_english = self.rt().config.ui.toolbar.hide_in_english;
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        toolbar_gate_open(s.toolbar_conjunction(), hide_in_english, s.chinese_mode)
    }

    /// 推送当前状态到常驻工具栏（中英/方案/标点/全半角），可见性单点决策：仅当
    /// `ime_active && has_edit_context && toolbar_visible && !全屏否决` 时 UpdateToolbar，否则
    /// HideToolbar。所有调用点（启动/切模式/切方案/激活/失活）经此决策。前三项见
    /// [`State::toolbar_conjunction`]；第四项变量名叫 `hide_fullscreen`，但还含
    /// `ui_suppressed_by_host()`（宿主自绘候选、D3D 独占全屏），日志里 `fullscreen=` 为真时
    /// 未必是全屏惹的。
    pub(crate) fn notify_toolbar(&self) {
        // 前台应用全屏时隐藏工具栏（读缓存，由 notify_toolbar_async 后台刷新）。「宿主接管 UI /
        // D3D 独占全屏」一律压住，不受 hide_in_fullscreen 管：独占全屏下工具栏弹出去会把游戏踢出
        // 独占态，不是偏好。见 handle_uielement.rs。
        let hide_fullscreen = (self.rt().config.ui.toolbar.hide_in_fullscreen
            && self
                .fullscreen_cached
                .load(std::sync::atomic::Ordering::Relaxed))
            || self.ui_suppressed_by_host().is_some();
        let hide_in_english = self.rt().config.ui.toolbar.hide_in_english;
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 四项合取：ime_active、has_edit_context（只看前者会让应用内点到非文本框时不隐藏）、
        // toolbar_visible、未处于全屏。前三项收在 `State::toolbar_conjunction`。
        //
        // 复查线程的唤醒判据取闸（三项合取）而不是四项：工具栏因全屏隐藏、三项合取由假转真时，
        // 按四项判信号发不出去，退出全屏后要等 `PARK_FALLBACK` 才回来（见
        // `watch_gate_stays_open_while_hidden_by_fullscreen`）。「英文状态隐藏」并进闸而不与全屏
        // 否决并列：英文态下工具栏横竖不显示，复查线程不该为它醒着。
        let gate_open = toolbar_gate_open(s.toolbar_conjunction(), hide_in_english, s.chinese_mode);
        if !gate_open || hide_fullscreen {
            // 记录是哪一项否决了显示：四条路径的排查方向完全不同（激活态乱序 / 焦点离开输入框 /
            // 用户关了开关 / 全屏探测）。
            tracing::debug!(
                "notify_toolbar: 隐藏 ime_active={} has_edit_ctx={} toolbar_visible={} fullscreen={} english={}",
                s.ime_active,
                s.has_edit_context,
                s.toolbar_visible,
                hide_fullscreen,
                hide_in_english && !s.chinese_mode
            );
            drop(s);
            // 闸开着却走到隐藏分支 = 三项合取成立、只是被全屏否决，复查线程最该醒着盯退出全屏。
            if gate_open {
                crate::coordinator::fullscreen_watch::wake();
            }
            // 内容没变就不再推：焦点抖动时这条 Hide 会被连发数次，见 `last_toolbar_push`。
            if self.take_toolbar_push_if_changed(ToolbarPush::Hidden) {
                let _ = self.ui_tx.send(UiCommand::HideToolbar);
            }
            // 去重只挡 UI 推送这一条：HUD 与图标各有自己的去重与触发条件，不能跟着跳过。
            self.push_input_diag_hud_if_visible(); // 见函数末尾同一行的说明
            // 语言栏图标同样收口于此，且两个出口都要：「不可输入」恰恰走的是本分支。
            self.publish_langbar_icon_now();
            return;
        }
        let (chinese_mode, caps_lock) = (s.chinese_mode, s.caps_lock);
        drop(s);
        // 走到这里 ⇒ 闸必开，叫醒挂着的复查线程。放在推送去重之前：内容没变但刚从隐藏转为显示时，
        // 去重会挡掉推送、信号却必须发出去。
        crate::coordinator::fullscreen_watch::wake();
        // 必须在取 state 锁之前算：effective_input_block() 内部要读 state，Mutex 不可重入，
        // 写进下面的初始化式就是当场自死锁。
        let input_blocked = self.effective_input_block().shows_english();
        // 不可输入（密码框 / 无编辑上下文 / 系统禁用）时模式格显英文标签：键已全部透传，与英文
        // 半角态同一件事，共用同一个标签、不另设配置键。在协调器算是因为 wind-ui 读不到配置；
        // 语言栏图标那条路只能在 spec 上覆盖（见 publish_langbar_icon）。
        let icon_label = if input_blocked {
            self.rt().config.ui.labels.english_label()
        } else {
            self.mode_icon_label(chinese_mode, caps_lock)
        };
        // 只有移动端事件通道用得上；桌面不多做这次引擎查询
        let latin_input =
            self.session_tx.is_some() && self.latin_input_with(input_blocked, chinese_mode);
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let tb = ToolbarState {
            chinese_mode,
            icon_label,
            caps_lock,
            full_width: s.full_width,
            chinese_punct: s.chinese_punct,
            s2t_enabled: s.s2t_enabled,
            t2s_enabled: s.t2s_enabled,
            // 简繁格：已启用时才在工具栏显示（默认 false 不显示）
            s2t_shown: s.s2t_enabled,
            soft_keyboard_on: self.softkeyboard_is_open(),
            // 不可输入（密码框 / 无编辑上下文 / 系统禁用）：模式格显 "英" 且不高亮。
            // 与语言栏图标读同一个 effective_input_block，两者不会错位。
            input_blocked,
        };
        drop(s);
        // 焦点换屏则先把工具栏挪到那块屏（内部按显示器 key 去重，未换屏时零下发）。
        // 必须先于 UpdateToolbar：反序会先在旧屏渲染一帧再跳。
        self.sync_toolbar_monitor();
        if self.take_toolbar_push_if_changed(ToolbarPush::Shown(Box::new(tb.clone()))) {
            self.emit_status(tb, latin_input);
        }
        // HUD 刷新收口于此（两个出口各一次）：HUD 展示的 ime_active / has_edit_context 正是上面
        // 合取的输入，凡是改动它们的路径都必须调 notify_toolbar，所以这里是唯一不会漏的落点。
        // state 锁已 drop，HUD 关闭时该函数首行即返回。
        self.push_input_diag_hud_if_visible();
        // 语言栏图标收口（与上面那个出口成对）。内部对相同位图跳过重渲与刷新推送，
        // 故多调无副作用；漏调则是「状态变了图标不跟」。
        self.publish_langbar_icon_now();
    }
}

/// 「固定到第 N 位 ▸ 第 1 位…第 K 位」子菜单，K = 每页条数（不超过 [`PIN_AT_MAX`] 与界面
/// 候选条数）。N 是界面名次；已在第 N 位、或那一格落不到（紧跟着变体等后插入项）时灰；
/// 列表里有 emoji 扩展时整组灰（见 `PinAtSlots::has_emoji`）。判据与写端 `candidate_op_in`
/// 的 `PinAt` 分支同用 `pin_at_slots`，失配表现为点得动却毫无反应。
fn pin_at_submenu(
    per_page: usize,
    slots: &crate::candidate_grid::PinAtSlots,
    can_pin: bool,
) -> wind_ui_types::MenuItemSpec {
    use wind_ui_types::MenuItemSpec as M;
    let k = per_page.min(PIN_AT_MAX as usize).min(slots.ui_len);
    let children: Vec<M> = (1..=k as u8)
        .map(|n| {
            M::leaf(
                format!("第 {n} 位"),
                MenuKind::Op(CandidateOp::PinAt(n)),
                can_pin && slots.position_for(n).is_some(),
                false,
            )
        })
        .collect();
    let mut item = M::submenu("固定到第 N 位", children);
    item.enabled = item.children.iter().any(|c| c.enabled);
    item
}

/// 是否 $SS/$AA 展开后的组成员候选：顺序/成员由短语定义决定，禁一切调整（改动走编辑短语
/// 路径，不允许 shadow 双轨漂移）。组导航候选本身（is_group，text 是组名）不算成员，可禁用整组。
pub(crate) fn candidate_is_group_member(cand: &wind_candidate::Candidate) -> bool {
    cand.is_phrase
        && !cand.is_group
        && (cand.is_group_member
            || cand.phrase_template.starts_with("$SS")
            || cand.phrase_template.starts_with("$AA"))
}

/// 右键「删除」菜单项（[`CandidateOp::Delete`]）的动态文案与可用性（按候选来源）：
/// 拼音 / 英文按文字屏蔽的词（`blocks_word`，判据见 `Coordinator::delete_blocks_word`）→
/// 「屏蔽该词」；短语→禁用短语（软删可恢复）；用户词/临时词→真删；系统词→「在此编码下隐藏」
/// （shadow，单字同样允许：只隐藏该编码下的该字，设置页可恢复）。
pub(crate) fn candidate_delete_menu(
    cand: &wind_candidate::Candidate,
    blocks_word: bool,
) -> (&'static str, bool) {
    if candidate_is_group_member(cand) {
        ("删除词条", false)
    } else if cand.is_emoji_suggestion {
        // emoji 扩展候选没有词库落点，删无可删；灰显而非隐藏，理由见菜单装配处。
        ("在此编码下隐藏", false)
    } else if blocks_word {
        ("屏蔽该词", true)
    } else if cand.is_phrase {
        // 静态短语前缀命中（is_prefix 且无完整码）定位不到 store 记录 → 暂禁。
        ("禁用短语", !cand.is_prefix || !cand.group_code.is_empty())
    } else if cand.meta.is_user_dict {
        ("删除用户词", true)
    } else if cand.meta.is_temp_dict {
        ("删除临时词", true)
    } else {
        // 系统词（码表 / 拼音单字 / 混输码表段 / 合成候选）：shadow 软隐藏。
        ("在此编码下隐藏", true)
    }
}

/// 把一段动作源补成 `run_command_candidate` 能执行的短语格式。
///
/// `run_command_candidate` 走 `evaluate_phrase`，命令必须带顶层 `$CC(…)` 标记，裸的
/// `proc.run("x.exe")` 会被当成字面文本、不跑也不报错。按钮的 action 只可能是命令，不该要求
/// 用户为不存在的歧义写标记。已带标记的原样放行（不包成嵌套）。第一个参数是候选显示文本，
/// 工具栏按钮不进候选列表，故给空串。
fn wrap_command_source(action: &str) -> String {
    if action.starts_with("$CC(") {
        return action.to_string();
    }
    format!("$CC(\"\", {action})")
}

/// 返回当前鼠标光标的屏幕坐标；获取失败时返回 (0, 0)。
/// 唯一调用者是 `focus_monitor` 的无前台窗口回退分支，故只在 Windows 下存在。
#[cfg(target_os = "windows")]
fn cursor_pos() -> (i32, i32) {
    use std::mem::zeroed;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt: POINT = unsafe { zeroed() };
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

/// 软键盘开面板时的摆放依据，随 `ShowSoftKeyboard` 下发。
///
/// 两项都是 `Option` 且语义不同：`anchor` 的 `None` 表示这块屏没有记录（落该屏默认位置，
/// 正常的首次使用），`work` 的 `None` 表示查不到显示器（回退主屏，降级路径）。
pub(crate) struct SoftKeyboardPlacement {
    pub anchor: Option<(i32, i32)>,
    pub work: Option<(i32, i32, i32, i32)>,
}

/// 一块显示器的「身份 + 几何」，位置记忆的取用单位。
#[derive(Debug, Clone)]
pub(crate) struct MonitorInfo {
    /// 分桶 key：`"workRight,workBottom@scalePct"`。
    pub key: String,
    /// 工作区 `(left, top, right, bottom)`，物理像素。
    pub work: (i32, i32, i32, i32),
}

/// 位置记忆的分桶 key。
///
/// - `workRight` / `workBottom`：显示器身份 + 分辨率。换屏或改分辨率 ⇒ 记录失配 ⇒ 落回默认
///   位置，比按比例换算旧坐标安全（换算可能整块出界）。
/// - `scalePct`：DPI 缩放。只改缩放时工作区物理像素边界不变，带上它才会失配，坐标本体因此
///   可以一路保持物理像素。
///
/// 存侧（`monitor_key_from_point`）与取侧（`focus_monitor`）共用这一个函数，格式分叉就会
/// 「位置压根没被记住」。
#[cfg(target_os = "windows")]
fn monitor_info(hmon: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<MonitorInfo> {
    use std::mem::{size_of, zeroed};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return None;
        }
        let wa = mi.rcWork;
        // 取不到 DPI 时按 100% 记：退化成不区分缩放，比整条记录作废好。
        let mut dpi_x: u32 = 0;
        let mut dpi_y: u32 = 0;
        let scale_pct = if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok()
            && dpi_y > 0
        {
            dpi_y * 100 / 96
        } else {
            100
        };
        Some(MonitorInfo {
            key: format!("{},{}@{}", wa.right, wa.bottom, scale_pct),
            work: (wa.left, wa.top, wa.right, wa.bottom),
        })
    }
}

/// Linux 外部宿主形态拿不到输出名时的固定显示器键：所有屏共用一个位置。
#[cfg(all(target_os = "linux", ext_presenter))]
pub(crate) const HOST_OUTPUT_DEFAULT: &str = "default";

/// 工作区是否有效（全 0 / 空矩形 = 不知道）。
#[cfg(all(target_os = "linux", ext_presenter))]
fn work_known((left, top, right, bottom): (i32, i32, i32, i32)) -> bool {
    right > left && bottom > top
}

/// 工具栏跟随焦点换屏的去重键（`current_toolbar_monitor` 里存的值）。
///
/// Windows 的显示器 key 本就含工作区右下角与缩放。Linux 的 key 只是输出名，改分辨率、或工作区
/// 晚于输出名报到时名字都不变，单按名字去重会挡掉这两次校正，故拼上工作区。
fn toolbar_dedup_key(key: &str, work: (i32, i32, i32, i32)) -> String {
    #[cfg(all(target_os = "linux", ext_presenter))]
    {
        let (left, top, right, bottom) = work;
        format!("{key}@{left},{top},{right},{bottom}")
    }
    #[cfg(not(all(target_os = "linux", ext_presenter)))]
    {
        let _ = work;
        key.to_owned()
    }
}

/// 记下的右下角锚点还属不属于 `key` 这块屏（`work` 是它的工作区）。
///
/// Windows 按锚点反查显示器 key 比对（理由见 `sync_toolbar_monitor`）。Linux 的记录本就按输出名
/// 分桶，查不了点所在的输出，只能看它是否还落在该输出的工作区里（换了分辨率 / 排列就不在了）；
/// 工作区未知时照单全收，越界由宿主夹回。
fn anchor_on_monitor(right: i32, bottom: i32, key: &str, work: (i32, i32, i32, i32)) -> bool {
    #[cfg(all(target_os = "linux", ext_presenter))]
    {
        let _ = key; // 记录已按输出名取出，key 必然相同
        let (x, y) = anchor_probe_point(right, bottom);
        let (left, top, wr, wb) = work;
        !work_known(work) || (left..wr).contains(&x) && (top..wb).contains(&y)
    }
    #[cfg(not(all(target_os = "linux", ext_presenter)))]
    {
        let _ = work;
        monitor_key_from_anchor(right, bottom).as_deref() == Some(key)
    }
}

/// 窗口右下角锚点所在显示器的 key。查不到时返回 None，与 `focus_monitor` 对称（回落到任何
/// 兜底 key 都会存进读取侧永远问不出来的位置）。
///
/// 参数是排他边界，函数内部退 1px 才拿去查屏（见 [`anchor_probe_point`]），否则贴边的位置会
/// 存到邻屏 key 下，工具栏跑到另一块屏上。新增调用点一律传窗口右下角，不要在调用侧自己减 1。
#[cfg_attr(all(target_os = "linux", ext_presenter), allow(dead_code))]
fn monitor_key_from_anchor(right: i32, bottom: i32) -> Option<String> {
    let (x, y) = anchor_probe_point(right, bottom);
    monitor_key_from_point(x, y)
}

/// 排他的右下角锚点 → 落在窗口内的探针点。
///
/// 窗口占据 `[left, right) × [top, bottom)`（Win32 `RECT` 语义），`(right, bottom)` 在窗口之外；
/// 工具栏贴到工作区右边缘时它正好落在右邻显示器上，而 `MonitorFromPoint` 的
/// `DEFAULTTONEAREST` 不报错只答错。抽成纯函数为可单测。
fn anchor_probe_point(right: i32, bottom: i32) -> (i32, i32) {
    (right - 1, bottom - 1)
}

/// 根据屏幕坐标定位显示器。查不到时返回 None。
///
/// 传进来的必须是落在目标窗口/屏幕内的点。要用窗口右下角定位请走
/// [`monitor_key_from_anchor`]，它负责把排他边界退成屏内点。
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))] // 显示器查询仅 Windows 有
#[cfg_attr(all(target_os = "linux", ext_presenter), allow(dead_code))]
fn monitor_key_from_point(x: i32, y: i32) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
        unsafe {
            let hmon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            monitor_info(hmon).map(|m| m.key)
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 输入焦点所在显示器；查不到返回 None（不动工具栏）。
///
/// 判据取前台窗口而非光标（键盘切窗时光标不动），也不用 caret 坐标（属于 TSF 层，常未就绪，
/// 会在首帧把工具栏定到错屏）。前台窗口是桌面/Shell 时回退到光标所在屏。
#[cfg_attr(all(target_os = "linux", ext_presenter), allow(dead_code))]
pub(crate) fn focus_monitor() -> Option<MonitorInfo> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::{HWND, POINT};
        use windows::Win32::Graphics::Gdi::{
            HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromPoint, MonitorFromWindow,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetDesktopWindow, GetForegroundWindow, GetShellWindow,
        };
        unsafe {
            let hwnd = GetForegroundWindow();
            let hmon: HMONITOR = if hwnd == HWND::default()
                || hwnd == GetDesktopWindow()
                || hwnd == GetShellWindow()
            {
                let (cx, cy) = cursor_pos();
                MonitorFromPoint(POINT { x: cx, y: cy }, MONITOR_DEFAULTTONEAREST)
            } else {
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)
            };
            monitor_info(hmon)
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 截图保存目录：用户配置目录下的 `screenshots/` 子目录。
/// 返回 None 表示无法确定用户目录（portable 模式但找不到 exe 路径等极罕见情况）。
fn screenshots_dir() -> Option<String> {
    Config::user_config_dir().map(|d| d.join("screenshots").display().to_string())
}

/// 状态气泡锚点的菜单文案。
fn status_anchor_label(a: wind_config::app_compat::StatusAnchor) -> &'static str {
    use wind_config::app_compat::StatusAnchor as A;
    match a {
        A::ScreenCenter => "屏幕中央",
        A::ScreenTopLeft => "屏幕左上角",
        A::ScreenTopRight => "屏幕右上角",
        A::ScreenBottomLeft => "屏幕左下角",
        A::ScreenBottomRight => "屏幕右下角",
        A::WindowCenter => "窗口中央",
        A::WindowBottomLeft => "窗口左下角",
    }
}

/// 固定位置落盘前的哨兵规避（候选窗与状态气泡共用）。
///
/// UI 侧用 `(0, 0)` 表示「已开启固定但尚未设定位置」，而主屏工作区左上角往往正是 `(0, 0)`，
/// 拖到最左上角就撞上哨兵、下次跳回默认锚点。这里在落盘侧下移 1px 避开。
fn avoid_unset_sentinel(x: i32, y: i32) -> (i32, i32) {
    if (x, y) == (0, 0) { (0, 1) } else { (x, y) }
}

/// 工具栏的**闸**：三项合取（见 [`State::toolbar_conjunction`]）再减去「英文状态隐藏」。
///
/// `notify_toolbar` 与全屏复查线程的开销闸（`toolbar_wants_display`）共用这一判据，分叉就会
/// 让复查线程白跑或该醒时睡着。
fn toolbar_gate_open(conjunction: bool, hide_in_english: bool, chinese_mode: bool) -> bool {
    let english_veto = hide_in_english && !chinese_mode;
    conjunction && !english_veto
}

#[cfg(test)]
#[path = "handle_menu_tests.rs"]
mod tests;

#[cfg(test)]
mod command_source_tests {
    //! 工具栏按钮 action → 短语格式的补全（`wrap_command_source`）。

    use super::wrap_command_source;

    /// 裸表达式补上标记：不补的话 `evaluate_phrase` 把它当字面文本，不跑也不报错。
    #[test]
    fn bare_expression_gets_wrapped() {
        assert_eq!(
            wrap_command_source(r#"proc.run("x.exe")"#),
            r#"$CC("", proc.run("x.exe"))"#
        );
    }

    /// 已带标记的原样放行——否则会包成 `$CC("", $CC(...))` 的嵌套。
    #[test]
    fn already_marked_source_is_untouched() {
        let src = r#"$CC("切拼音", ime.schema("pinyin"))"#;
        assert_eq!(wrap_command_source(src), src);
    }

    /// 含中文路径与双反斜杠的真实配置原样进包装，转义留给 cmdbar 的 lexer 处理，包装层不得碰
    /// 字符串内容。
    #[test]
    fn windows_path_with_escapes_passes_through_verbatim() {
        let action = r#"proc.run("D:\\Download\\知符\\知符.exe")"#;
        let got = wrap_command_source(action);
        assert!(got.contains(r#"D:\\Download\\知符\\知符.exe"#), "{got}");
        assert_eq!(got, format!(r#"$CC("", {action})"#));
    }
}

#[cfg(test)]
#[path = "handle_menu_toolbar_push_dedup_tests.rs"]
mod toolbar_push_dedup_tests;

#[cfg(test)]
#[path = "handle_menu_fullscreen_veto_tests.rs"]
mod fullscreen_veto_tests;

impl Coordinator {
    /// 主菜单里的「软键盘」项：本身是开关，子菜单直接选面。
    ///
    /// 面多于一个时才给子菜单，只有一面时子菜单孤零零一项、纯属多一层。
    pub(crate) fn soft_keyboard_menu_item(&self) -> wind_ui_types::MenuItemSpec {
        use wind_ui_types::MenuItemSpec as M;
        let on = self.softkeyboard_is_open();
        let table = self.softkeyboard_table();
        let pages = table.pages();
        if pages.len() < 2 {
            return M::leaf(
                "软键盘",
                MenuKind::Command(MenuCmd::ToggleSoftKeyboard),
                !pages.is_empty(),
                on,
            );
        }
        M::submenu("软键盘", self.soft_keyboard_menu_children())
    }

    /// 软键盘的「开关 + 各面单选」项列表。
    ///
    /// 供主菜单的「软键盘」子菜单与右键软键盘格的快捷菜单共用。只有一面时这份列表仍有意义
    /// （开关那一项），「该不该给子菜单」是调用方的问题。
    pub(crate) fn soft_keyboard_menu_children(&self) -> Vec<wind_ui_types::MenuItemSpec> {
        use wind_ui_types::MenuItemSpec as M;
        let on = self.softkeyboard_is_open();
        let table = self.softkeyboard_table();
        let pages = table.pages();
        let cur = self.softkeyboard_page_idx();
        let mut children = vec![M::leaf(
            if on { "关闭面板" } else { "打开面板" },
            MenuKind::Command(MenuCmd::ToggleSoftKeyboard),
            !pages.is_empty(),
            false,
        )];
        if !pages.is_empty() {
            children.push(M::separator());
        }
        for (i, p) in pages.iter().enumerate() {
            children.push(M::leaf(
                p.name.clone(),
                MenuKind::Command(MenuCmd::SoftKeyboardPage(i)),
                true,
                // 勾选当前面，但面板关着时不勾（否则让人以为它开着）。
                on && i == cur,
            ));
        }
        children
    }

    /// 输入方案的「英文 + 各方案单选」项列表（主菜单的「输入方案」子菜单内容）。
    ///
    /// `chinese` 由调用方传入：主菜单已在一次加锁里取出几个状态，这里再读一次是第二次加锁，
    /// 两次之间状态可能变，勾选态会与同一菜单里别的项矛盾。
    pub(crate) fn schema_menu_children(&self, chinese: bool) -> Vec<wind_ui_types::MenuItemSpec> {
        use wind_ui_types::MenuItemSpec as M;
        let cmd = |c: MenuCmd| MenuKind::Command(c);
        let active = self.engine_mgr.active_schema_id();
        let schemas = self.engine_mgr.available_schemas().to_vec();
        let mut children = vec![M::leaf("英文", cmd(MenuCmd::SchemaEnglish), true, !chinese)];
        if !schemas.is_empty() {
            children.push(M::separator());
            for (i, id) in schemas.iter().enumerate() {
                children.push(M::leaf(
                    self.engine_mgr.schema_name(id),
                    cmd(MenuCmd::SchemaSelect(i)),
                    true,
                    chinese && *id == active,
                ));
            }
        }
        children
    }

    /// 右键工具栏某一格时给的精简快捷菜单；这一格没有定制则返回 `None`。
    ///
    /// 右键一个具体的格，意图几乎总是「就在这一格管的事情里换一个」：
    /// - 中英格：整份方案列表（含「英文」），即主菜单「输入方案」子菜单的内容。
    /// - 软键盘格：开关 + 各面单选。
    /// - 标点格 / 全半角格 / 简繁格：共用一份「输出形态」（中文标点、全角、简入繁出）。
    ///
    /// 齿轮格 / 自定义按钮格 / 拖动柄返回 `None` 回落完整主菜单：隐藏齿轮后右键工具栏是主菜单
    /// 仅剩的鼠标入口（`toolbar-customization.md` §2.2 判据③），不能让用户把自己锁在外面。
    pub(crate) fn build_toolbar_cell_menu(
        &self,
        action: ToolbarAction,
    ) -> Option<Vec<wind_ui_types::MenuItemSpec>> {
        use wind_ui_types::MenuItemSpec as M;
        let cmd = |c: MenuCmd| MenuKind::Command(c);
        match action {
            ToolbarAction::ToggleMode | ToolbarAction::SwitchEngine => {
                let chinese = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.chinese_mode
                };
                Some(self.schema_menu_children(chinese))
            }
            ToolbarAction::ToggleSoftKeyboard => Some(self.soft_keyboard_menu_children()),
            // 简繁格也走这一份：它本来就是那三项之一。
            ToolbarAction::TogglePunct | ToolbarAction::ToggleWidth | ToolbarAction::ToggleS2t => {
                let (punct, full, s2t) = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    (s.chinese_punct, s.full_width, s.s2t_enabled)
                };
                // 文案、勾选态与顺序都跟主菜单里那三项一致（见 `build_main_menu_items` 开头）。
                Some(vec![
                    M::leaf("全角", cmd(MenuCmd::ToggleWidth), true, full),
                    M::leaf("中文标点", cmd(MenuCmd::TogglePunct), true, punct),
                    M::leaf("简入繁出", cmd(MenuCmd::ToggleS2t), true, s2t),
                ])
            }
            // 繁入简出刻意不给分格菜单（小众功能，不进上面那张三项表），返回 None 回落主菜单。
            ToolbarAction::ToggleT2s | ToolbarAction::OpenSettings | ToolbarAction::Custom(_) => {
                None
            }
        }
        .map(|mut items| {
            // 每份分格菜单末尾都挂一条回主菜单的路，不可省：分格右键占了功能格后，只剩 12dp 的
            // 拖动柄通向主菜单（§2.2 判据③）。
            items.push(M::separator());
            items.push(M::leaf("更多…", cmd(MenuCmd::OpenMainMenu), true, false));
            items
        })
    }

    /// 右键工具栏：该格有定制就弹定制菜单，否则回落完整主菜单。
    pub(crate) fn show_toolbar_menu(&self, action: Option<ToolbarAction>, anchor: MenuAnchor) {
        let items = action.and_then(|a| self.build_toolbar_cell_menu(a));
        let Some(items) = items else {
            self.show_main_menu(anchor);
            return;
        };
        // 空列表同样回落：弹一个空菜单比不弹更让人以为坏了。（软键盘一面都没有时列表仍有个
        // 禁用的开关项，不为空。）
        if items.is_empty() {
            self.show_main_menu(anchor);
            return;
        }
        self.mark_menu_open(0, String::new());
        let _ = self
            .ui_tx
            .send(UiCommand::ShowCandidateMenu { items, anchor });
    }
}

#[cfg(test)]
mod candidate_double_click_tests {
    //! 双击候选窗截图（`ui.candidate.double_click_screenshot`）。

    use crate::coordinator::Coordinator;
    use wind_config::Config;
    use wind_ui_types::{UiCommand, UiEvent};

    fn screenshot_sent(on: bool) -> bool {
        let mut cfg = Config::default();
        cfg.ui.candidate.double_click_screenshot = on;
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        while rx.try_recv().is_ok() {}
        c.inject_ui_event(UiEvent::CandidateDoubleClick);
        rx.try_iter()
            .any(|cmd| matches!(cmd, UiCommand::ScreenshotCandidateToClipboard))
    }

    #[test]
    fn double_click_copies_candidate_screenshot_when_enabled() {
        assert!(
            screenshot_sent(true),
            "开关打开时双击应请求候选窗截图到剪贴板"
        );
    }

    #[test]
    fn double_click_is_ignored_when_disabled() {
        assert!(!screenshot_sent(false), "出厂关闭时双击不应截图");
    }
}

#[cfg(test)]
#[path = "handle_menu_english_veto_tests.rs"]
mod english_veto_tests;

#[cfg(test)]
mod menu_close_tests {
    use super::*;
    use wind_config::Config;
    use wind_ui_types::UiCommand;

    /// 菜单开着时打字：菜单被收掉的同时要解除气泡的隐藏抑制。只发 `HideMenu` 的话气泡的
    /// `suppress_hide` 残留为 true，从此移出不隐藏、右键被当成「菜单开着」。
    #[test]
    fn typing_closes_menu_and_releases_tooltip_suppress() {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        c.mark_menu_open(0, String::new());
        while rx.try_recv().is_ok() {}
        assert!(c.forward_menu_key(0x41), "菜单开着时其它键被吞");
        let cmds: Vec<UiCommand> = rx.try_iter().collect();
        assert!(cmds.iter().any(|m| matches!(m, UiCommand::HideMenu)));
        assert!(
            cmds.iter()
                .any(|m| matches!(m, UiCommand::SetTooltipMenuOpen(false))),
            "须一并解除气泡抑制：{cmds:?}"
        );
        assert!(!c.is_menu_open());
    }

    /// 菜单刚打开（仍在焦点守卫期内，`menu_close_on_focus_change` 不会动它）时走一条收菜单的
    /// 焦点 / 组合路径，返回这期间下发的命令。
    fn close_via(f: impl FnOnce(&Coordinator)) -> Vec<UiCommand> {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        c.mark_menu_open(0, String::new());
        while rx.try_recv().is_ok() {}
        f(&c);
        rx.try_iter().collect()
    }

    fn releases_suppress(cmds: &[UiCommand]) -> bool {
        cmds.iter()
            .any(|m| matches!(m, UiCommand::SetTooltipMenuOpen(false)))
    }

    /// 失焦（清输入态的 reason）、切走输入法、组合被终止：这三条都经 `HideCandidates` 收掉
    /// 菜单，必须一并解除气泡抑制。
    #[test]
    fn focus_and_composition_closes_release_tooltip_suppress() {
        use wind_bridge::handler::{FocusLostReason, MessageHandler};
        let lost = close_via(|c| c.handle_focus_lost(0, FocusLostReason::Thread));
        assert!(releases_suppress(&lost), "失焦：{lost:?}");
        let deact = close_via(|c| c.handle_ime_deactivated(0));
        assert!(releases_suppress(&deact), "切走输入法：{deact:?}");
        let term = close_via(|c| c.handle_composition_terminated());
        assert!(releases_suppress(&term), "组合被终止：{term:?}");
    }
}

#[cfg(test)]
#[path = "handle_menu_compat_reload_tests.rs"]
mod compat_reload_tests;

/// Linux 自绘菜单的协调器一侧：入口、按平台摘除的菜单项、各条关闭路径与 `menu_open` 的对齐。
/// 渲染 / 命中 / 像素推帧在 wind-ui 的 `menu_host` 里测，addon 与真 X 的整条链在 e2e。
#[cfg(all(test, ext_presenter))]
mod linux_menu_tests {
    use super::*;
    use crate::coordinator::Coordinator;
    use std::sync::mpsc::Receiver;
    use wind_bridge::handler::MessageHandler;
    use wind_ui_types::MenuItemSpec;

    fn coord() -> (std::sync::Arc<Coordinator>, Receiver<UiCommand>) {
        Coordinator::new_headless_with_ui(Config::default(), None)
    }

    fn labels(items: &[MenuItemSpec]) -> Vec<String> {
        let mut out = Vec::new();
        for it in items {
            out.push(it.label.clone());
            out.extend(labels(&it.children));
        }
        out
    }

    fn drain(rx: &Receiver<UiCommand>) -> Vec<UiCommand> {
        rx.try_iter().collect()
    }

    fn with_candidate(c: &Coordinator) {
        let mut st = c.state.lock().unwrap();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.input_buffer = "ce".into();
    }

    // 摘除的是 Linux 没有落点的项；macOS 同走外部宿主形态，但主菜单里这些项都有落点。
    #[cfg(target_os = "linux")]
    #[test]
    fn main_menu_drops_items_without_a_linux_landing() {
        let (c, _rx) = coord();
        let all = labels(&c.build_main_menu_items());
        assert!(
            !all.iter().any(|l| l == "输入诊断 HUD"),
            "Linux 主菜单不该有「输入诊断 HUD」"
        );
        // 邻项还在：cfg 没把整块吞掉。
        for kept in [
            "输入方案",
            "全角",
            "主题",
            "截图所有窗口到文件",
            "截图候选窗口到剪贴板",
            "打开日志目录",
            "应用独立配置",
            "设置...",
            "重启服务",
        ] {
            assert!(
                all.iter().any(|l| l == kept),
                "邻项「{kept}」被误伤：{all:?}"
            );
        }
    }

    /// 候选菜单不挂「更多…」：主菜单的入口是 Fcitx5 状态区的「清风输入法菜单」。屏幕菜单关闭后
    /// 再下发的主菜单在 Wayland 下会落进候选窗的 popup，把候选窗挤走。
    #[test]
    fn candidate_menu_has_no_main_menu_entry() {
        let (c, rx) = coord();
        with_candidate(&c);
        c.show_candidate_menu(0, 10, 20);
        let items = drain(&rx)
            .into_iter()
            .find_map(|m| match m {
                UiCommand::ShowCandidateMenu { items, .. } => Some(items),
                _ => None,
            })
            .expect("候选菜单没弹");
        assert!(
            !items
                .iter()
                .any(|i| i.kind == MenuKind::Command(MenuCmd::OpenMainMenu)),
            "候选菜单不该再有通往主菜单的项：{:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        assert_eq!(items.last().unwrap().label, "复制");
        assert!(c.is_menu_open());
    }

    /// `menu.open`：工作区先于菜单下发（同一通道保序，定位时已是新值）。
    #[test]
    fn host_open_request_sends_work_area_before_menu() {
        let (c, rx) = coord();
        c.handle_ext(
            wind_ipc::protocol::ext_kind::MENU_OPEN,
            br#"{"target":-1,"x":300,"y":420,"work":[0,0,1280,800]}"#,
        );
        let cmds = drain(&rx);
        let wa = cmds.iter().position(|m| {
            matches!(
                m,
                UiCommand::SetWorkArea {
                    right: 1280,
                    bottom: 800,
                    ..
                }
            )
        });
        let show = cmds.iter().position(|m| {
            matches!(m, UiCommand::ShowCandidateMenu { anchor, .. } if (anchor.x, anchor.y) == (300, 420))
        });
        assert!(
            matches!((wa, show), (Some(a), Some(b)) if a < b),
            "{cmds:?}"
        );
        assert!(c.is_menu_open());
    }

    #[test]
    fn host_open_for_candidate_without_candidates_leaves_menu_closed() {
        let (c, _rx) = coord();
        c.handle_ext(
            wind_ipc::protocol::ext_kind::MENU_OPEN,
            br#"{"target":0,"x":1,"y":2,"work":[0,0,100,100]}"#,
        );
        assert!(!c.is_menu_open(), "没有候选就不弹、也不能标记为打开");
    }

    /// 状态气泡右键（target = MENU_TARGET_STATUS）→ 与 Windows `RequestStatusMenu` 同一个菜单。
    #[test]
    fn host_open_on_status_tip_shows_status_menu() {
        let (c, rx) = coord();
        c.handle_ext(
            wind_ipc::protocol::ext_kind::MENU_OPEN,
            br#"{"target":-2,"x":50,"y":60,"work":[0,0,1280,800]}"#,
        );
        let items = drain(&rx)
            .into_iter()
            .find_map(|m| match m {
                UiCommand::ShowCandidateMenu { items, anchor }
                    if (anchor.x, anchor.y) == (50, 60) =>
                {
                    Some(items)
                }
                _ => None,
            })
            .expect("状态气泡菜单没弹");
        assert_eq!(
            labels(&items),
            [
                "常驻显示",
                "焦点切换时显示",
                "固定位置",
                "恢复默认位置",
                "截图此窗口"
            ]
        );
        assert!(c.is_menu_open());
    }

    /// 悬停提示右键（target = MENU_TARGET_TOOLTIP）：命中要 UI 换算，先发 `TooltipMenuAt`
    /// （带位图内坐标），菜单此时还没开；UI 回 `RequestTooltipMenu` 才弹，且菜单随候选收起。
    #[test]
    fn host_open_on_tooltip_asks_ui_for_the_hit_then_opens_candidate_bound_menu() {
        let (c, rx) = coord();
        c.handle_ext(
            wind_ipc::protocol::ext_kind::MENU_OPEN,
            br#"{"target":-3,"x":50,"y":60,"work":[0,0,1280,800],"lx":7,"ly":8}"#,
        );
        let cmds = drain(&rx);
        assert!(
            cmds.iter().any(|m| matches!(
                m,
                UiCommand::TooltipMenuAt {
                    x: 50,
                    y: 60,
                    local_x: 7,
                    local_y: 8
                }
            )),
            "{cmds:?}"
        );
        assert!(!c.is_menu_open(), "命中回来之前不算打开");

        c.handle_ext(
            wind_ipc::protocol::ext_kind::MENU_OPEN,
            br#"{"target":-3,"x":1,"y":2,"work":[0,0,1280,800]}"#,
        );
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::TooltipMenuAt {
                    local_x: -1,
                    local_y: -1,
                    ..
                }
            )),
            "缺位图内坐标按文本块外处理"
        );

        with_candidate(&c);
        drain(&rx);
        c.handle_ui_event(wind_ui_types::UiEvent::RequestTooltipMenu {
            x: 50,
            y: 60,
            candidate: 0,
            hit: None,
            doc_fingerprint: 0,
        });
        assert!(
            drain(&rx)
                .iter()
                .any(|m| matches!(m, UiCommand::ShowCandidateMenu { .. })),
            "UI 回来后照 Windows 同一路弹菜单"
        );
        assert!(c.is_menu_open());
        c.notify_ui_hide();
        assert!(
            drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)),
            "候选收起：提示菜单失去对象，与候选右键菜单一样收掉"
        );
    }

    #[test]
    fn menu_keys_are_forwarded_and_other_keys_close_and_are_swallowed() {
        let (c, rx) = coord();
        c.show_main_menu(MenuAnchor::at_point(0, 0));
        drain(&rx);
        assert!(c.forward_menu_key(keymap::VK_DOWN));
        assert!(
            drain(&rx)
                .iter()
                .any(|m| matches!(m, UiCommand::MenuKey(k) if *k == keymap::VK_DOWN))
        );
        assert!(c.forward_menu_key(keymap::VK_A), "非菜单键也不外泄");
        assert!(!c.is_menu_open());
        assert!(drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)));
    }

    #[test]
    fn pointer_events_are_forwarded_while_open_and_heal_when_closed() {
        use wind_ipc::protocol::menu_pointer::*;
        let (c, rx) = coord();
        c.handle_menu_pointer(MENU_POINTER_MOTION, 0, 5, 6);
        assert!(
            drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)),
            "菜单已关还收到指针事件：让 UI 收菜单"
        );
        c.show_main_menu(MenuAnchor::at_point(0, 0));
        drain(&rx);
        c.handle_menu_pointer(MENU_POINTER_PRESS, 3, 7, 8);
        let cmds = drain(&rx);
        assert!(
            cmds.iter().any(|m| matches!(
                m,
                UiCommand::MenuPointer {
                    event: wind_ui_types::MenuPointerEvent::RightPress,
                    x: 7,
                    y: 8
                }
            )),
            "{cmds:?}"
        );
    }

    /// 宿主报来的皮肤装饰点击原样交给渲染端；载荷坏了就丢。
    #[test]
    fn host_decor_click_reaches_the_ui() {
        let (c, rx) = coord();
        c.handle_ext(
            wind_ipc::protocol::ext_kind::SKIN_DECOR_CLICK,
            br#"{"index":2}"#,
        );
        assert!(
            drain(&rx)
                .iter()
                .any(|m| matches!(m, UiCommand::SkinDecorClick(2)))
        );
        c.handle_ext(
            wind_ipc::protocol::ext_kind::SKIN_DECOR_CLICK,
            br#"{"index":"x"}"#,
        );
        assert!(
            !drain(&rx)
                .iter()
                .any(|m| matches!(m, UiCommand::SkinDecorClick(_)))
        );
    }

    #[test]
    fn host_dismiss_and_disconnect_reset_menu_open() {
        let (c, rx) = coord();
        c.show_main_menu(MenuAnchor::at_point(0, 0));
        c.handle_ext(
            wind_ipc::protocol::ext_kind::MENU_DISMISS,
            br#"{"reason":"idle"}"#,
        );
        assert!(!c.is_menu_open());
        assert!(drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)));
        assert!(
            !c.forward_menu_key(keymap::VK_RETURN),
            "关闭后回车照常交给输入流程"
        );

        c.show_main_menu(MenuAnchor::at_point(0, 0));
        c.handle_client_disconnected();
        assert!(!c.is_menu_open(), "addon 断线：菜单态复位");
    }

    #[test]
    fn candidate_menu_closes_with_candidates_but_main_menu_survives() {
        let (c, rx) = coord();
        with_candidate(&c);
        c.show_candidate_menu(0, 10, 20);
        drain(&rx);
        // 持着 state 锁调用（生产里多处就是这样调的）：不得自锁死。
        {
            let _held = c.state.lock().unwrap();
            c.notify_ui_hide();
        }
        assert!(
            drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)),
            "候选收起：候选菜单失去对象，让 UI 收掉"
        );
        // UI 收掉可见菜单后回送 MenuClose（menu_host::MenuHost::hide），协调器据此复位。
        c.handle_ui_event(wind_ui_types::UiEvent::MenuClose);
        assert!(!c.is_menu_open());

        c.show_main_menu(MenuAnchor::at_point(0, 0));
        drain(&rx);
        c.notify_ui_hide();
        assert!(
            c.is_menu_open(),
            "空闲时的主菜单不受一条无关的 HideCandidates 影响"
        );
        assert!(!drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)));
    }

    /// 失焦清输入 / 切走输入法 / 组合被终止：这些路径直接写 `menu_open = false` 再
    /// `notify_ui_hide`。Windows 靠 `HideCandidates` 连带收菜单窗口，Linux 靠这里补发的 HideMenu。
    #[test]
    fn composition_terminated_hides_the_menu_ui_too() {
        let (c, rx) = coord();
        c.show_main_menu(MenuAnchor::at_point(0, 0));
        drain(&rx);
        c.handle_composition_terminated();
        assert!(!c.is_menu_open());
        assert!(drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)));
    }

    /// 已知竞态（`linux-port.md` §7）：`HideMenu` 回送的 `MenuClose` 不带代际，分不出是哪个菜单的。
    ///
    /// 候选右键菜单 A 开着 → 候选收起，协调器发 `HideMenu`（持锁路径不复位 `menu_open`，靠 UI
    /// 回送收口）→ UI 还没处理，addon 又报 `menu.open` 弹主菜单 B → UI 按序先收 A（回送
    /// `MenuClose`）再弹 B → 协调器把 B 当成被关掉。后果只是 B 刚弹出就被收掉，两端认识仍一致
    /// （协调器复位并补发 `HideMenu`）。本例钉住这条一致性；若日后给 `MenuClose` 加代际，把末段改成
    /// 「B 仍开着」。
    #[test]
    fn stale_menu_close_echo_closes_new_menu_consistently() {
        let (c, rx) = coord();
        with_candidate(&c);
        c.show_candidate_menu(0, 10, 20); // A
        drain(&rx);
        c.notify_ui_hide();
        assert!(drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)));
        c.open_menu_from_host(MenuOpenRequest {
            target: wind_ipc::protocol::menu_target::MENU_TARGET_MAIN,
            x: 0,
            y: 0,
            work: [0, 0, 1280, 800],
            local: None,
        }); // B
        assert!(c.is_menu_open());
        drain(&rx);
        // UI 收 A 时的回送，到得比 B 的任何操作都早。
        c.handle_ui_event(wind_ui_types::UiEvent::MenuClose);
        assert!(!c.is_menu_open(), "已知限制：B 被这条迟到的回送收掉");
        assert!(
            drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)),
            "协调器复位时必须让 UI 也收掉 B，否则屏上留着一个不收键的菜单"
        );
        assert!(
            !c.forward_menu_key(keymap::VK_RETURN),
            "复位之后的回车照常交给输入流程，不被吞"
        );
    }

    #[test]
    fn idle_timeout_predicate() {
        let now = std::time::Instant::now();
        let t = std::time::Duration::from_secs(60);
        assert!(!menu_idle_expired(None, now, t));
        assert!(!menu_idle_expired(Some(now), now, t));
        let old = now.checked_sub(std::time::Duration::from_secs(61));
        assert!(old.is_none() || menu_idle_expired(old, now, t));
    }

    /// 服务端最后一道兜底：空闲超时后菜单仍标记为打开，下一个键照常处理（不被吞）。
    #[test]
    fn stale_open_menu_does_not_swallow_the_next_key() {
        let (c, rx) = coord();
        c.show_main_menu(MenuAnchor::at_point(0, 0));
        // 开机不足两分钟的机器上 Instant 回退不了：此例无从构造，明说而不是静默通过。
        let old = std::time::Instant::now()
            .checked_sub(MENU_IDLE_TIMEOUT * 2)
            .expect("本例需要开机超过 2 分钟（Instant 要能回退两个空闲超时）");
        c.state.lock().unwrap().menu_touched_at = Some(old);
        drain(&rx);
        assert!(!c.forward_menu_key(keymap::VK_DOWN), "超时后的键照常处理");
        assert!(!c.is_menu_open());
        assert!(drain(&rx).iter().any(|m| matches!(m, UiCommand::HideMenu)));
    }

    #[test]
    fn menu_keys_keep_the_idle_timer_fresh() {
        let (c, _rx) = coord();
        c.show_main_menu(MenuAnchor::at_point(0, 0));
        let old = std::time::Instant::now()
            .checked_sub(MENU_IDLE_TIMEOUT / 2)
            .expect("本例需要开机超过 30 秒（Instant 要能回退半个空闲超时）");
        c.state.lock().unwrap().menu_touched_at = Some(old);
        assert!(c.forward_menu_key(keymap::VK_DOWN));
        let touched = c.state.lock().unwrap().menu_touched_at.unwrap();
        assert!(touched > old, "菜单键刷新空闲计时");
    }
}

/// Linux 外部宿主形态的工具栏 / 软键盘入口：主菜单、热键、addon 的位置 / 全屏 / 指针回报。
/// 渲染与鼠标状态机在 wind-ui 的 forwarder 里测，这里只钉协调器这一侧的接线。
#[cfg(all(test, target_os = "linux", ext_presenter))]
mod linux_panel_tests {
    use super::*;
    use crate::coordinator::Coordinator;
    use std::sync::mpsc::Receiver;
    use wind_bridge::handler::{KeyEventData, MessageHandler};
    use wind_ipc::protocol::ext_kind;

    /// 已声明面板能力的 addon（当前版本的 addon 每次 `host.display` 都带 `panels`）。
    fn coord() -> (std::sync::Arc<Coordinator>, Receiver<UiCommand>) {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        c.handle_ext(ext_kind::HOST_DISPLAY, br#"{"panels":true}"#);
        (c, rx)
    }

    fn drain(rx: &Receiver<UiCommand>) -> Vec<UiCommand> {
        rx.try_iter().collect()
    }

    fn labels(items: &[wind_ui_types::MenuItemSpec]) -> Vec<String> {
        let mut out = Vec::new();
        for it in items {
            out.push(it.label.clone());
            out.extend(labels(&it.children));
        }
        out
    }

    /// 文案用 Windows 的「显示工具栏」：Linux 显隐的是悬浮工具栏，不是 macOS 的菜单栏图标。
    #[test]
    fn main_menu_has_toolbar_and_softkeyboard_on_linux() {
        let (c, _rx) = coord();
        let all = labels(&c.build_main_menu_items());
        assert_eq!(TOOLBAR_MENU_LABEL, "显示工具栏");
        for want in ["显示工具栏", "软键盘"] {
            assert!(all.iter().any(|l| l == want), "缺「{want}」：{all:?}");
        }
        assert!(!all.iter().any(|l| l == "显示状态图标"), "{all:?}");
    }

    /// 新服务 + 旧 addon（升级 deb 后没重启 fcitx5）：旧 addon 不报 `panels`，也不认软键盘浮层。
    /// 热键 / 菜单都不能把主键区接管走，菜单里不出两个死开关；声明后照常；addon 断线即复位，
    /// 下一个连上来的实例得重新声明。
    #[test]
    fn softkeyboard_needs_panels_declared_by_host() {
        use wind_bridge::handler::KeyAction;
        use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};
        let (c, _rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        let ctrl_shift_k = KeyEventData {
            key_code: 0x4B,
            scan_code: 0,
            modifiers: MOD_CTRL | MOD_SHIFT,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        };
        let refused = |c: &Coordinator| {
            assert!(matches!(c.open_softkeyboard(None), KeyAction::PassThrough));
            c.handle_key_event(&ctrl_shift_k);
            c.softkeyboard_menu_toggle(None);
            assert!(!c.softkeyboard_is_open(), "未声明面板能力不得开软键盘");
            let all = labels(&c.build_main_menu_items());
            for no in ["显示工具栏", "软键盘"] {
                assert!(!all.iter().any(|l| l == no), "不该有「{no}」：{all:?}");
            }
        };
        refused(&c);
        // 旧 addon 的 host.display 不带 panels：不改现值。
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"caret_free":false,"caret_trusted":true,"inline_preedit":true,"scale":1.00}"#,
        );
        refused(&c);

        c.handle_ext(ext_kind::HOST_DISPLAY, br#"{"panels":true}"#);
        assert!(matches!(c.open_softkeyboard(None), KeyAction::Consumed));
        assert!(c.softkeyboard_is_open(), "声明后应开得出来");
        c.close_softkeyboard();
        c.handle_key_event(&ctrl_shift_k);
        assert!(c.softkeyboard_is_open(), "声明后 ctrl+shift+k 应开得出来");
        c.close_softkeyboard();

        c.handle_client_disconnected();
        refused(&c);
    }

    #[test]
    fn host_fullscreen_forwards_to_ui() {
        let (c, rx) = coord();
        c.handle_ext(ext_kind::HOST_FULLSCREEN, br#"{"fullscreen":true}"#);
        c.handle_ext(ext_kind::HOST_FULLSCREEN, br#"{"fullscreen":false}"#);
        let got: Vec<bool> = drain(&rx)
            .into_iter()
            .filter_map(|m| match m {
                UiCommand::SetHostFullscreen(on) => Some(on),
                _ => None,
            })
            .collect();
        assert_eq!(got, [true, false]);
        // 坏载荷不能当成「退出全屏」：那会让工具栏在全屏应用上冒出来。
        c.handle_ext(ext_kind::HOST_FULLSCREEN, br#"{"fullscreen":1}"#);
        c.handle_ext(ext_kind::HOST_FULLSCREEN, b"oops");
        assert!(
            !drain(&rx)
                .iter()
                .any(|m| matches!(m, UiCommand::SetHostFullscreen(_)))
        );
    }

    /// addon 报的面板指针事件原样转给渲染端，由它翻译成 `WM_*`。
    #[test]
    fn panel_pointer_forwards_to_ui() {
        let (c, rx) = coord();
        c.on_panel_pointer(11, 2, 1, -5, 40);
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::PanelPointer {
                    kind: 11,
                    event: 2,
                    button: 1,
                    x: -5,
                    y: 40
                }
            )),
            "指针事件没转给渲染端"
        );
    }

    /// addon 断线（崩溃）时正按住软键盘的键：它再也报不来松开。就地给两块面板各补一次离开，
    /// 连发当场停（不等新 addon 连上）。
    #[test]
    fn client_disconnect_sends_panel_leave() {
        use wind_ipc::protocol::overlay::{OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR};
        use wind_ipc::protocol::panel_pointer::PANEL_POINTER_LEAVE;
        let (c, rx) = coord();
        c.handle_client_disconnected();
        let mut left: Vec<u32> = drain(&rx)
            .iter()
            .filter_map(|m| match m {
                UiCommand::PanelPointer { kind, event, .. } if *event == PANEL_POINTER_LEAVE => {
                    Some(*kind)
                }
                _ => None,
            })
            .collect();
        left.sort_unstable();
        assert_eq!(left, [OVERLAY_KIND_TOOLBAR, OVERLAY_KIND_SOFTKEYBOARD]);
    }

    fn panel_moved(cmds: &[UiCommand]) -> Vec<(u32, i32, i32)> {
        cmds.iter()
            .filter_map(|m| match m {
                UiCommand::PanelMoved { kind, x, y } => Some((*kind, *x, *y)),
                _ => None,
            })
            .collect()
    }

    /// `pos.toolbar` 按落点输出名分桶：两块屏各记各的，互不覆盖。落盘的右下角由渲染端按面板尺寸
    /// 换算后回报（这里用 `inject_ui_event` 扮演渲染端）；没报输出名的落到 `"default"`。
    #[test]
    fn pos_toolbar_saves_anchor_per_output() {
        use wind_ui_types::UiEvent;
        let (c, rx) = coord();
        c.handle_ext(
            ext_kind::POS_TOOLBAR,
            br#"{"x":100,"y":200,"output":"HDMI-1"}"#,
        );
        assert_eq!(panel_moved(&drain(&rx)), [(10, 100, 200)]);
        c.inject_ui_event(UiEvent::ToolbarMoved {
            right: 300,
            bottom: 240,
        });
        c.handle_ext(
            ext_kind::POS_TOOLBAR,
            br#"{"x":3000,"y":500,"output":"DP-1"}"#,
        );
        assert_eq!(panel_moved(&drain(&rx)), [(10, 3000, 500)]);
        c.inject_ui_event(UiEvent::ToolbarMoved {
            right: 3200,
            bottom: 540,
        });
        {
            let map = c.toolbar_anchors.lock().unwrap();
            assert_eq!(map.get("HDMI-1"), Some(&(300, 240)));
            assert_eq!(map.get("DP-1"), Some(&(3200, 540)));
        }
        c.handle_ext(ext_kind::POS_SOFTKEYBOARD, br#"{"x":5,"y":6}"#);
        assert_eq!(panel_moved(&drain(&rx)), [(11, 5, 6)]);
        c.inject_ui_event(UiEvent::SoftKeyboardMoved {
            right: 805,
            bottom: 306,
        });
        assert_eq!(
            c.softkeyboard_anchors.lock().unwrap().get("default"),
            Some(&(805, 306))
        );

        // 焦点回到哪块屏，就取那块屏的记录。
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"HDMI-1","work":[0,0,1920,1080]}"#,
        );
        c.sync_toolbar_monitor();
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::SetToolbarAnchor {
                    right: 300,
                    bottom: 240
                }
            )),
            "回到 HDMI-1 要取它自己的记录"
        );
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"DP-1","work":[1920,0,3840,1080]}"#,
        );
        c.sync_toolbar_monitor();
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::SetToolbarAnchor {
                    right: 3200,
                    bottom: 540
                }
            )),
            "切到 DP-1 要取 DP-1 的记录"
        );
    }

    /// 记下的位置不在当前输出的工作区里（拔屏、改分辨率）：工具栏回到该屏右下角、软键盘回到
    /// 默认落点，不把面板画到屏外。
    #[test]
    fn stale_anchor_falls_back_to_default() {
        let (c, rx) = coord();
        c.toolbar_anchors
            .lock()
            .unwrap()
            .insert("DP-1".into(), (5000, 3000));
        c.softkeyboard_anchors
            .lock()
            .unwrap()
            .insert("DP-1".into(), (5000, 3000));
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"DP-1","work":[0,0,1920,1080]}"#,
        );
        c.sync_toolbar_monitor();
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::SetToolbarCorner {
                    work_right: 1920,
                    work_bottom: 1080
                }
            )),
            "失效的记录要回落到该屏右下角"
        );
        let p = c.softkeyboard_placement();
        assert_eq!(p.anchor, None, "失效的软键盘记录不能下发");
        assert_eq!(p.work, Some((0, 0, 1920, 1080)), "默认落点按该屏工作区算");
    }

    /// `host.display` 的输出名与工作区成对更新：只报新输出名时不能沿用旧输出的工作区；两者都不报
    /// （只报缩放等）则不改。
    #[test]
    fn host_display_output_and_work_are_a_pair() {
        let (c, _rx) = coord();
        let mon = |c: &Coordinator| {
            let m = c.current_monitor().unwrap();
            (m.key, m.work)
        };
        assert_eq!(mon(&c), ("default".into(), (0, 0, 0, 0)), "没报过 = 固定键");
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"HDMI-1","work":[0,0,1920,1080]}"#,
        );
        c.handle_ext(ext_kind::HOST_DISPLAY, br#"{"scale":1.5}"#);
        assert_eq!(mon(&c), ("HDMI-1".into(), (0, 0, 1920, 1080)));
        c.handle_ext(ext_kind::HOST_DISPLAY, br#"{"output":"DP-1"}"#);
        assert_eq!(mon(&c), ("DP-1".into(), (0, 0, 0, 0)));
    }

    /// 宿主没报工作区（旧 addon）：无从算默认落点，工具栏维持现状，而不是发一个按 0 算的右下角。
    #[test]
    fn unknown_work_area_keeps_toolbar_in_place() {
        let (c, rx) = coord();
        c.handle_ext(ext_kind::HOST_DISPLAY, br#"{"output":"DP-1"}"#);
        c.sync_toolbar_monitor();
        assert!(
            !drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::SetToolbarCorner { .. } | UiCommand::SetToolbarAnchor { .. }
            )),
            "工作区未知时不该改工具栏位置"
        );
        assert_eq!(c.softkeyboard_placement().work, None);
    }

    /// 工作区晚于输出名报到（输出名不变）：前一次「无记录且工作区未知」没发命令，不能记成已校正，
    /// 工作区一到就要下发默认落点。
    #[test]
    fn work_arriving_later_for_same_output_sends_corner() {
        let (c, rx) = coord();
        c.handle_ext(ext_kind::HOST_DISPLAY, br#"{"output":"DP-1"}"#);
        c.sync_toolbar_monitor();
        drain(&rx);
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"DP-1","work":[0,0,1920,1080]}"#,
        );
        c.sync_toolbar_monitor();
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::SetToolbarCorner {
                    work_right: 1920,
                    work_bottom: 1080
                }
            )),
            "工作区报到后要发默认落点"
        );
    }

    /// 会话中途改分辨率：输出名不变、工作区缩小到记录之外，工具栏要回落默认，不能被去重挡掉。
    #[test]
    fn work_change_on_same_output_drops_stale_anchor() {
        let (c, rx) = coord();
        c.toolbar_anchors
            .lock()
            .unwrap()
            .insert("DP-1".into(), (2500, 1400));
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"DP-1","work":[0,0,2560,1440]}"#,
        );
        c.sync_toolbar_monitor();
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::SetToolbarAnchor {
                    right: 2500,
                    bottom: 1400
                }
            )),
            "前置：记录在工作区内时照发"
        );
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"DP-1","work":[0,0,1920,1080]}"#,
        );
        c.sync_toolbar_monitor();
        assert!(
            drain(&rx).iter().any(|m| matches!(
                m,
                UiCommand::SetToolbarCorner {
                    work_right: 1920,
                    work_bottom: 1080
                }
            )),
            "工作区变小、记录越界要回落默认"
        );
    }

    /// `pos.*` 没带落点输出名：记在焦点输出名下，与读取侧同一个桶——只有一边报 output 也不错键。
    #[test]
    fn pos_without_output_saves_under_focus_output() {
        use wind_ui_types::UiEvent;
        let (c, rx) = coord();
        c.handle_ext(
            ext_kind::HOST_DISPLAY,
            br#"{"output":"HDMI-1","work":[0,0,1920,1080]}"#,
        );
        c.handle_ext(ext_kind::POS_TOOLBAR, br#"{"x":100,"y":200}"#);
        c.inject_ui_event(UiEvent::ToolbarMoved {
            right: 300,
            bottom: 240,
        });
        c.handle_ext(ext_kind::POS_SOFTKEYBOARD, br#"{"x":5,"y":6}"#);
        c.inject_ui_event(UiEvent::SoftKeyboardMoved {
            right: 805,
            bottom: 306,
        });
        drain(&rx);
        assert_eq!(
            c.toolbar_anchors.lock().unwrap().get("HDMI-1"),
            Some(&(300, 240))
        );
        assert_eq!(
            c.softkeyboard_anchors.lock().unwrap().get("HDMI-1"),
            Some(&(805, 306))
        );
        assert!(!c.toolbar_anchors.lock().unwrap().contains_key("default"));
    }

    /// 工具栏 / 软键盘窗口产出的 UI 事件在 Linux 上照常落到处理函数：点软键盘格开面板、面板关闭
    /// 按钮收面板、右键工具栏格弹自绘菜单（与 Windows 同一条 `ShowCandidateMenu`）。
    #[test]
    fn toolbar_and_softkeyboard_events_reach_handlers() {
        use wind_ui_types::UiEvent;
        let (c, rx) = coord();
        c.inject_ui_event(UiEvent::Toolbar(ToolbarAction::ToggleSoftKeyboard));
        assert!(c.softkeyboard_is_open(), "工具栏软键盘格没打开面板");
        c.inject_ui_event(UiEvent::SoftKeyboardClose);
        assert!(!c.softkeyboard_is_open(), "面板关闭按钮没收掉面板");
        drain(&rx);
        c.inject_ui_event(UiEvent::RequestToolbarMenu {
            action: Some(ToolbarAction::ToggleSoftKeyboard),
            anchor: MenuAnchor::at_point(10, 20),
        });
        assert!(
            drain(&rx)
                .iter()
                .any(|m| matches!(m, UiCommand::ShowCandidateMenu { .. })),
            "工具栏右键没弹菜单"
        );
        assert!(c.is_menu_open(), "菜单键要能路由给它");
    }

    /// 出厂的 `keys.softkeyboard`（ctrl+shift+k）与 `keys.toggle_toolbar`（ctrl+shift+\）在
    /// Linux 上都编进按键热键表。软键盘那条走完整按键路径、翻转开关；工具栏那条只断言命中的
    /// 动作——它的执行体会写用户目录下的 config.toml，单测碰不得，而分派表本身不分平台。
    #[test]
    fn toggle_hotkeys_work_on_linux() {
        use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT, calc_key_hash};
        let (c, _rx) = coord();
        let key = |vk: u32| KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: MOD_CTRL | MOD_SHIFT,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        };
        let vk_k = 0x4B;
        assert!(!c.softkeyboard_is_open());
        c.handle_key_event(&key(vk_k));
        assert!(c.softkeyboard_is_open(), "ctrl+shift+k 应打开软键盘");
        c.handle_key_event(&key(vk_k));
        assert!(!c.softkeyboard_is_open(), "再按一次关掉");

        let hash = calc_key_hash(MOD_CTRL | MOD_SHIFT, keymap::VK_BACKSLASH);
        assert_eq!(
            c.rt().compiled_hotkeys.match_key_down(hash),
            Some("toggle_toolbar"),
            "ctrl+shift+\\ 应命中 toggle_toolbar"
        );
    }

    /// 面板（工具栏）发起的菜单：先把 `host.display` 报的工作区下发，再弹菜单——否则 UI 侧
    /// 没有工作区，子菜单不判空间直接朝右展开到屏外。三条入口：工具栏右键格菜单、「设置」键
    /// 主菜单、分格菜单末尾的「更多…」。没报过工作区就不发（不能拿全 0 冒充）。
    #[test]
    fn panel_menus_send_host_work_area_first() {
        use wind_ui_types::UiEvent;
        let work = (1920, 0, 3840, 1040);
        let is_work = |m: &UiCommand| {
            matches!(
                m,
                UiCommand::SetWorkArea { left, top, right, bottom }
                    if (*left, *top, *right, *bottom) == work
            )
        };
        let opens: [(&str, fn(&Coordinator)); 3] = [
            ("工具栏右键", |c| {
                c.inject_ui_event(UiEvent::RequestToolbarMenu {
                    action: Some(ToolbarAction::TogglePunct),
                    anchor: MenuAnchor::at_point(10, 20),
                })
            }),
            ("设置键主菜单", |c| {
                c.inject_ui_event(UiEvent::RequestMainMenu(MenuAnchor::at_point(10, 20)))
            }),
            ("更多…", |c| {
                c.inject_ui_event(UiEvent::MenuAction(MenuKind::Command(
                    MenuCmd::OpenMainMenu,
                )))
            }),
        ];
        for (name, open) in opens {
            let (c, rx) = coord();
            c.handle_ext(
                ext_kind::HOST_DISPLAY,
                br#"{"output":"DP-1","work":[1920,0,3840,1040]}"#,
            );
            drain(&rx);
            open(&c);
            let cmds = drain(&rx);
            let wa = cmds.iter().position(is_work);
            let show = cmds
                .iter()
                .position(|m| matches!(m, UiCommand::ShowCandidateMenu { .. }));
            assert!(
                matches!((wa, show), (Some(a), Some(b)) if a < b),
                "{name}：{cmds:?}"
            );

            let (c, rx) = coord();
            drain(&rx);
            open(&c);
            let cmds = drain(&rx);
            assert!(
                cmds.iter()
                    .any(|m| matches!(m, UiCommand::ShowCandidateMenu { .. })),
                "{name}：{cmds:?}"
            );
            assert!(
                !cmds
                    .iter()
                    .any(|m| matches!(m, UiCommand::SetWorkArea { .. })),
                "{name}：没报过工作区不该发 {cmds:?}"
            );
        }
    }
}
