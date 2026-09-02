//! 功能菜单与工具栏
//!
//! 主菜单 / 候选右键菜单的构建与分派、工具栏点击/刷新/位置持久化。
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。

use crate::coordinator::ToolbarPush;
use crate::coordinator::{Coordinator, FILTER_MODES};
use crate::theme_style::ThemeStyle;
use wind_bridge::handler::MessageHandler;
use wind_config::Config;
use wind_keys::keymap;
use wind_ui_types::ToolbarState;
use wind_ui_types::{CandidateOp, MenuAnchor, MenuCmd, MenuKind, ToolbarAction, UiCommand};

/// 菜单打开后的焦点事件豁免期，见 [`Coordinator::menu_close_on_focus_change`]。
///
/// 取 250ms 的依据：下界须盖住跨宿主切换时旧宿主 focus_lost 迟到的约 100ms（实测
/// 97~111ms，见 `project_toolbar_flash_stale_focus_lost` 的时序），上界须远短于用户
/// 「点开菜单 → 切走窗口」的最短间隔（看清菜单内容至少几百毫秒）。
pub(crate) const MENU_FOCUS_GUARD: std::time::Duration = std::time::Duration::from_millis(250);

/// `MenuCmd::ToggleToolbar` 的菜单文案。两平台**同一个命令、不同的 UI 实体**，故文案分平台：
/// Windows 下它显隐的是跟随光标的悬浮工具栏窗口；macOS 下 `UpdateToolbar` 被
/// `manager_macos` 编码成 mode_status 帧，最终落到 `ModeStatusController` 的
/// `NSStatusItem.isVisible` —— 显隐的是菜单栏里那个中/英状态图标，压根没有悬浮工具栏。
/// 照搬「显示工具栏」会让 mac 用户去找一个不存在的东西。
///
/// 常量而非各处字面量：三处菜单（IMK 输入源 / 候选框右键 / 状态指示器下拉）必须字字一致，
/// 这正是本次统一要解决的问题，散成字面量迟早再次跑偏。
pub(crate) const TOOLBAR_MENU_LABEL: &str = if cfg!(target_os = "macos") {
    "显示状态图标"
} else {
    "显示工具栏"
};

/// 把 (键, 值) 列表拼成设置程序的附加参数串（`--k=v`，空格分隔）。值为空的项跳过
/// ——设置端把"传了空串"和"没传"当同一回事，少一个参数更省事。
///
/// 值含空白时加双引号：参数串最终经 `ShellExecuteW` 的 params 交给目标进程，由
/// `CommandLineToArgvW` 重新切分，不加引号的 `--text=你 好` 会被拆成两个 argv，
/// 设置端只收得到 `--text=你`。引号在切分时会被剥掉，故设置端拿到的仍是裸值。
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
/// 与命令行不同，IPC 传的是**结构化 argv**，故必须在此把参数串切回一个个词。切词逻辑
/// 刻意放在本文件——它和上面加引号的 `build_settings_args` 是一对，两者的引号约定必须
/// 同源。此前这一步在 Swift 侧重做了一遍，等于让另一门语言去猜 Rust 的引号规则。
///
/// 仅认双引号、不认转义：值来自本进程内部拼装，不含引号字面量。
///
/// 仅 macOS 走 IPC argv 通路，非 macOS 下无调用点；但引号往返的单元测试要在所有平台上跑
/// （切词规则与 `build_settings_args` 是一对，任一平台改坏都该被拦住），故不加 `cfg` 编译
/// 掉本函数，只在非 macOS 下豁免 dead_code。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
/// `--page <p>` 与附加参数各自独立成段：附加参数**不依附于页**（`--dark` / `--soft`
/// 这类没有页也有意义），故 `page=None` 时仍原样带上，不能因为没页就丢掉。
/// macOS 走 IPC 裸串、无命令行概念，故仅非 macOS 使用。
#[cfg(not(target_os = "macos"))]
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

impl Coordinator {
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
        // 否则 tooltip 会在截图前被隐藏。详见 clear_tooltip_menu_flag 的说明。
        self.clear_tooltip_menu_flag();
    }

    /// 执行功能主菜单命令
    pub(crate) fn run_menu_cmd(&self, cmd: MenuCmd) {
        match cmd {
            MenuCmd::SchemaEnglish => {
                self.handle_system_mode_switch(false);
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
                self.toggle_softkeyboard(None);
                self.after_softkeyboard_change();
            }
            // 分格快捷菜单末尾的「更多…」：弹完整主菜单。锚点取光标位（`i32::MIN` 由 UI
            // 侧解释成"当前鼠标处"）——用户刚在那里点过，比回头去算工具栏几何更贴合，
            // 也不必把上一个菜单的锚点一路带过来。
            MenuCmd::OpenMainMenu => {
                self.show_main_menu(wind_ui_types::MenuAnchor::at_point(i32::MIN, i32::MIN));
            }
            MenuCmd::SoftKeyboardPage(i) => {
                // 菜单选面：面板没开就顺带开出来，否则「选了个面却什么都没发生」。
                if !self.softkeyboard_is_open() {
                    self.open_softkeyboard(None);
                    // ⚠️ **开面板这一步自己负责收口，不能指望下面那句**：
                    // `ui_softkeyboard_page` 在下标越界时只 `warn!` 就返回，走不到收口。
                    // 而菜单是照**构建时**的面表列的，配置一重载就可能少一面——那时面板
                    // 已经开着并接管按键，C++ 却收不到 STATUS_SOFT_KEYBOARD 位、工具栏
                    // 图标也不亮，正是这组提交要消灭的那对症状。
                    // 判据：**谁改了状态谁负责收口**。成功路径会多推一次，两处推送都幂等
                    // （工具栏有 PartialEq 去重），比漏推划算得多。
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
            MenuCmd::TooltipCopy => {
                let _ = self.ui_tx.send(UiCommand::CopyTooltipText);
            }
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
            // 语言栏图标：总开关写用户配置（`[ui.langbar]`，热重载后重渲重发），
            // 纯调试的两项走 state.toml / 内存。两类落点不同，见 set_langbar_config 的说明。
            // 非 Windows 桌面形态下压根没有发布器，菜单项也不会被构建出来，故是空操作。
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
            #[cfg(not(all(feature = "desktop-ui", windows)))]
            MenuCmd::IconBadgeStyle(_)
            | MenuCmd::IconToggleSizeMarks
            | MenuCmd::IconToggleDemoAnim => {}
        }
    }

    /// 图标发布器当前的呈现参数 `(总开关档位下标, 是否烧尺寸档标记)`；
    /// 发布器不可用时返回 `None`。
    ///
    /// 勾选态一律读**渲染器实际生效的值**而不是配置文件：配置写入到生效之间隔着一次
    /// 热重载，读配置会在重载失败时显示一个并未生效的勾。菜单勾选与实际行为不同步，
    /// 用户的反应是反复点同一项。
    #[cfg(all(feature = "desktop-ui", windows))]
    pub(crate) fn icon_debug_state(&self) -> Option<(u8, bool)> {
        let guard = Coordinator::icon_publisher().lock().ok()?;
        let p = guard.as_ref()?;
        Some((p.style().index(), p.size_marks()))
    }

    /// Dev 变体专属的语言栏图标调试子菜单。
    ///
    /// 为什么值得做：16×16 上角标可不可辨只能真机看，而每改一次就得提权部署 + 重启
    /// 输入法，成本高到根本比不动。渲染搬到服务端后这些本就是运行时参数，接上菜单后
    /// 比选退化成点几下——这正是当初把渲染从 DLL 挪到服务端换来的东西。
    ///
    /// **这里只留三样**：总开关（写用户配置 `[ui.langbar]`）、烧尺寸档标记、演示动画
    /// （后两者是纯调试项，走 state.toml 与内存）。角标画哪些状态、什么颜色、在哪个
    /// 角，是 `[ui.langbar.badges]` 那张规则表的事——设置页有专门的编辑器，菜单再摆
    /// 一套就是第二个真相源，且在 16px 的比选场景里也帮不上忙。
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
        // 演示动画单独隔一段：前面几项都是「图标长什么样」的偏好并会被记住，它却是一段
        // 持续跑的演示、且重启不保留，混在一起会让人以为它也是个呈现选项。
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
    /// 与 `status_toggle_always` 不同，这里**不立即弹一次气泡**：用户此刻正对着菜单操作，
    /// 焦点没动，弹出来反而像误触发。下一次真的切换输入框时自然会显示。
    pub(crate) fn status_toggle_show_on_focus(&self) {
        let next = !self.rt().config.ui.status.show_on_focus;
        let _ = Config::set_user_value(
            &["ui", "status", "show_on_focus"],
            toml::Value::Boolean(next),
        );
        self.refresh_config_in_memory(|c| c.ui.status.show_on_focus = next);
    }

    /// 状态提示气泡右键菜单「恢复默认位置」：改回跟随光标，custom_x/y 归零。
    pub(crate) fn status_reset_position(&self) {
        let _ = Config::set_user_string(&["ui", "status", "position_mode"], "follow_caret");
        let _ = Config::set_user_value(&["ui", "status", "custom_x"], toml::Value::Integer(0));
        let _ = Config::set_user_value(&["ui", "status", "custom_y"], toml::Value::Integer(0));
        self.refresh_config_in_memory(|c| {
            c.ui.status.position_mode = "follow_caret".to_string();
            c.ui.status.custom_x = 0;
            c.ui.status.custom_y = 0;
        });
    }

    /// 拖动状态提示气泡释放后的落位处理——**是否持久化取决于当前模式**：
    ///
    /// - `fixed`（固定坐标）：写回 `custom_x/custom_y`，永久生效。
    /// - `follow_caret`（跟随光标）：**不落盘**。拖动只是把气泡临时挪开，
    ///   下次状态变化重新显示时自然回到光标旁——UI 侧仅在拖动进行中锁定位置，
    ///   松手后的 `show()` 会照常按光标重新定位，无需在此做任何清理。
    ///
    /// 这样两种模式各自语义自洽：跟随模式拖动是临时的，固定模式拖动才是"重新摆放"。
    pub(crate) fn save_status_tip_pos(&self, x: i32, y: i32) {
        if !self
            .rt()
            .config
            .ui
            .status
            .position_mode
            .eq_ignore_ascii_case("fixed")
        {
            return;
        }
        // 与候选窗同款哨兵规避：状态气泡的 UI 侧同样用 (0,0) 表示"尚未设定"。
        // 两处共用同一约定，缺一处就会出现"拖到主屏左上角后位置记不住"。
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

    /// 拖动候选窗释放后的落位处理——**是否持久化取决于当前定位方式**：
    ///
    /// - `fixed`（固定位置）：写回 `ui.candidate.custom_x/custom_y`，永久生效。
    /// - `follow_caret`（跟随光标）：**不落盘**。拖动只是把候选窗临时挪开，
    ///   本次组合内保持不动，组合结束（`hide()` → `reset_drag()`）即恢复跟随光标。
    ///
    /// 与 `save_status_tip_pos` 同构：两种模式各自语义自洽，跟随模式的拖动是临时的，
    /// 固定模式的拖动才是"重新摆放"。
    pub(crate) fn save_candidate_pos(&self, x: i32, y: i32) {
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

    /// 状态提示气泡右键菜单「固定位置」：在 fixed / follow_caret 间翻转。
    ///
    /// 打开时**以气泡当前实际位置**落盘，而不是直接切到陈旧的 custom_x/custom_y——
    /// 否则用户拖到某处后点「固定位置」，气泡会跳到上次保存的（往往是 0,0）坐标。
    /// 做法：先把模式改成 fixed，再请 UI 上报当前位置，回来的 `StatusTipMoved`
    /// 经 `save_status_tip_pos` 落盘（该函数只在 fixed 模式下持久化，此时条件已满足）。
    pub(crate) fn status_toggle_pinned(&self) {
        let now_fixed = !self
            .rt()
            .config
            .ui
            .status
            .position_mode
            .eq_ignore_ascii_case("fixed");
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
            si_fixed = si.position_mode.eq_ignore_ascii_case("fixed");
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
            // 常驻模式下本项无意义（获焦本就会显示），置灰而非隐藏——项忽隐忽现比置灰更难理解，
            // 用户会以为功能没了。
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

    /// 右键悬停提示（编码反查气泡）请求的功能菜单：复制内容 / 截图此窗口。
    /// **先**发 SetTooltipMenuOpen(true) 抑制 tooltip 的 WM_MOUSELEAVE 自动隐藏——
    /// 右键弹出菜单后鼠标会移到菜单窗口上，若不抑制 tooltip 会当场消失，菜单就指向一个
    /// 已不存在的窗口，「截图此窗口」会截空。抑制标志在菜单关闭时由 menu_close 统一清除。
    pub(crate) fn show_tooltip_menu(&self, x: i32, y: i32) {
        use wind_ui_types::MenuItemSpec as M;
        let _ = self.ui_tx.send(UiCommand::SetTooltipMenuOpen(true));
        let cmd = |c: MenuCmd| MenuKind::Command(c);
        let items = vec![
            M::leaf("复制内容", cmd(MenuCmd::TooltipCopy), true, false),
            M::leaf("截图此窗口", cmd(MenuCmd::TooltipScreenshot), true, false),
        ];
        self.mark_menu_open(0, String::new());
        let _ = self.ui_tx.send(UiCommand::ShowCandidateMenu {
            items,
            anchor: MenuAnchor::at_point(x, y),
        });
    }

    /// 输入诊断 HUD 上右键请求的菜单：复制 / 显示分类 / 停止刷新 / 置顶 / 关闭。
    ///
    /// 勾选态直接读运行时状态，故菜单永远反映当前真值——这类"开关型"菜单最忌讳
    /// 勾选态与实际行为不同步，那会让用户反复点同一项。
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
    /// ⚠ **必须强制推一次**：冻结中 `push_input_diag_hud_if_visible` 会早退，此时切分类
    /// 屏幕上毫无变化，用户只能判断为"菜单坏了"。分区是显示配置而非数据，与冻结正交。
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
        // 冻结时也推一次：HUD 要立刻显示"⏸ 已停止刷新"这行标注，否则用户无从确认开关生效。
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
        // 采集开关随 HUD 显隐下发（广播）。关闭时也必须推——否则 DLL 会在 HUD 早已关掉
        // 之后继续每次焦点切换都采集窗口链，白付开销且无人消费。
        self.push_diag_snapshot_config(0);
        if now {
            // ⚠ 打开时复位置顶与冻结——这两个开关都能把自己的逃生口关上：
            //   · 非置顶 → HUD 沉到宿主窗口之下 → 右键菜单点不到 → 没法再打开置顶；
            //   · 冻结中关掉再打开 → 内容停在旧快照，看起来就是「HUD 坏了不刷新」。
            // 「重新打开」是用户表达「重来一次」的动作，复位到默认最不意外。
            // 分区显示不复位：它是纯显示偏好，且全关时 HUD 会给出可右键的提示行，不封死。
            self.input_diag_topmost.store(true, Relaxed);
            self.input_diag_frozen.store(false, Relaxed);
            self.push_input_diag_hud_if_visible();
        } else {
            let _ = self.ui_tx.send(UiCommand::HideInputDiag);
        }
    }

    /// 切换密码框强制英文抑制策略（高级菜单，临时测试入口）：关闭时立即解除当前生效的强制英文。
    pub(crate) fn toggle_password_suppress(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let now = !self.password_suppress_enabled.load(Relaxed);
        self.password_suppress_enabled.store(now, Relaxed);
        if !now {
            self.password_suppress.store(false, Relaxed);
        }
        // 同步给 DLL：吃键门控在 TSF 侧本地判定（早于 IPC），不推则开关对 DLL 无效——
        // 关掉抑制后 DLL 仍会放行所有键，这个「误置位时用来救场」的逃生阀就成了摆设。
        self.push_password_suppress_config(0);
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

    /// 为当前焦点应用设置候选窗首显策略，并写入用户层 compat.toml。
    ///
    /// 三步收口，缺一不可：
    ///   1. 写用户层 compat.toml（持久化，跨重启保留）；
    ///   2. **重载规则表**——只改运行时缓存不够，切到别的应用再切回来时
    ///      `update_active_compat` 会拿这张表重新解析，旧表会把本次设置悄悄回滚；
    ///   3. 刷新当前 `active_compat` 缓存，使本次设置对当前应用立即生效
    ///      （同 pid 时 `update_active_compat` 提前 return，不会自己刷）。
    pub(crate) fn set_first_show_mode(&self, mode_id: u8) {
        use wind_config::app_compat::FirstShowMode;
        let mode = match mode_id {
            1 => FirstShowMode::Fast,
            2 => FirstShowMode::Instant,
            _ => FirstShowMode::Wait,
        };
        let name = self.active_process_name();
        if name.is_empty() {
            // 焦点进程未解析（尚无焦点 / OpenProcess 失败）。菜单项此时应是禁用态，
            // 走到这里说明有别的路径调用，记一条便于排查——静默返回会让用户以为点了没反应。
            tracing::warn!("set_first_show_mode: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_first_show_mode: 无用户配置目录，无法持久化");
            return;
        };
        if let Err(e) = wind_config::app_compat::set_user_first_show_mode(&user_dir, &name, mode) {
            tracing::error!("set_first_show_mode: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表（系统层 + 用户层），与启动时同一口径。
        let reloaded = wind_config::app_compat::AppCompat::load(
            self.compat_dirs.0.as_deref(),
            Some(user_dir.as_path()),
        );
        *self.app_compat.lock().unwrap_or_else(|e| e.into_inner()) = reloaded;
        #[cfg(windows)]
        self.sync_host_render_whitelist();
        // 3）当前应用立即生效。
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first_show_mode = mode;
        tracing::info!("候选窗首显策略 for process={name}: {}", mode.as_config());
        self.show_status();
    }

    /// 为当前焦点应用设置符号自动配对开关，并写入用户层 compat.toml。
    /// `mode_id`：0=跟随全局（清除规则）1=启用 2=禁用。
    ///
    /// 前三步与 [`Self::set_first_show_mode`] 完全同构，缺一不可，理由见那里的注释。
    /// **第四步是本项特有**：还要把英文配对配置重推给 DLL——纯英文模式的配对由 C++ 侧
    /// `_englishPairEngine` 独立处理，它只认握手/配置变更时推过去的那份值。不重推的症状是
    /// 「中文模式关掉了，切到英文又配上了」，且要等下次重连才好，极难归因。
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
        if let Err(e) = wind_config::app_compat::set_user_auto_pair(&user_dir, &name, enabled) {
            tracing::error!("set_auto_pair_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表（系统层 + 用户层），与启动时同一口径。
        let reloaded = wind_config::app_compat::AppCompat::load(
            self.compat_dirs.0.as_deref(),
            Some(user_dir.as_path()),
        );
        *self.app_compat.lock().unwrap_or_else(|e| e.into_inner()) = reloaded;
        #[cfg(windows)]
        self.sync_host_render_whitelist();
        // 3）当前应用立即生效（同 pid 时 `update_active_compat` 提前 return，不会自己刷）。
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_pair = enabled;
        // 4）重推英文配对配置：逐客户端按各自 PID 现算，本进程拿到新值、别的进程不受影响。
        self.push_english_pair_config(0);
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

    /// 为当前焦点应用设置初始中英状态（`is_punct=false`）或初始标点（`is_punct=true`），
    /// 并写入用户层 compat.toml。`mode_id`：0=跟随全局（清除规则）1=英文 2=中文。
    ///
    /// 前三步与 [`Self::set_first_show_mode`] 完全同构，缺一不可，理由见那里的注释。
    /// 第四步是本项特有：规则语义是「初始状态」，只在焦点跨进程切入时参与决策，但用户
    /// 此刻正是在**当前**应用里显式设置它，必须立即生效一次——否则得切走再切回才看得到
    /// 效果，会被当成"设了没反应"。
    pub(crate) fn set_initial_state_rule(&self, is_punct: bool, mode_id: u8) {
        use wind_config::app_compat::InitialMode as IM;
        let mode = match mode_id {
            1 => Some(IM::English),
            2 => Some(IM::Chinese),
            _ => None, // 0 = 跟随全局：清除该应用在本维度上的规则
        };
        let name = self.active_process_name();
        if name.is_empty() {
            // 与 set_first_show_mode 一致：菜单项此时应是禁用态，走到这里说明有别的调用
            // 路径，记一条便于排查——静默返回会让用户以为点了没反应。
            tracing::warn!("set_initial_state_rule: 当前焦点进程未知，忽略本次设置");
            return;
        }
        let Some(user_dir) = self.compat_dirs.1.clone() else {
            tracing::warn!("set_initial_state_rule: 无用户配置目录，无法持久化");
            return;
        };
        // 1）写用户层 compat.toml。
        let written = if is_punct {
            wind_config::app_compat::set_user_initial_punct(&user_dir, &name, mode)
        } else {
            wind_config::app_compat::set_user_initial_mode(&user_dir, &name, mode)
        };
        if let Err(e) = written {
            tracing::error!("set_initial_state_rule: 写用户 compat.toml 失败: {e}");
            return;
        }
        // 2）重载整表（系统层 + 用户层），与启动时同一口径。
        let reloaded = wind_config::app_compat::AppCompat::load(
            self.compat_dirs.0.as_deref(),
            Some(user_dir.as_path()),
        );
        *self.app_compat.lock().unwrap_or_else(|e| e.into_inner()) = reloaded;
        #[cfg(windows)]
        self.sync_host_render_whitelist();
        // 3）刷新 active 缓存的判据位：同 pid 时 update_active_compat 提前 return，不会自己刷。
        //    漏掉这步会让「切出本应用时是否重算」用上过期的判据。
        //    注意先取值再持 active_compat 锁，避免与 app_compat 锁形成嵌套顺序。
        let want_mode = self.rule_initial_mode(&name).map(|m| m.is_chinese());
        let want_punct = self.rule_initial_punct(&name).map(|m| m.is_chinese());
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_initial_rule = want_mode.is_some() || want_punct.is_some();
        // 4）立即生效一次。清除规则（None）时刻意不动当前状态：撤销规则不等于要求立刻
        //    切换模式，下次从别的应用切进来时自然走回全局逻辑。
        let follow = self.rt().config.input.punct.follow_mode;
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = want_mode
                && s.chinese_mode != c
            {
                s.chinese_mode = c;
                if follow {
                    s.chinese_punct = c;
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
    /// （`--page <name>`，name 为 wind_setting cli 的规范页 id：
    /// schema/input/keys/ui/dict/advanced/about，旧 web 别名如 dictionary 不被识别）；
    /// 找不到桌面应用再回退到内嵌 web 配置（签发 token 构造 URL，page 以 `#<name>` 片段附加）。
    /// page=None 打开默认页。设置/词库管理/关于等菜单项统一经此函数。
    ///
    /// 执行路径：有 TSF 连接时经 IPC 让宿主进程执行 ShellExecuteW（有前台权限，能拉窗口到前面）；
    /// 无 TSF 连接时回退到服务进程侧直接启动。
    pub(crate) fn open_settings(&self, page: Option<&str>) {
        self.open_settings_with(page, "");
    }

    /// 带附加参数的「打开设置」。`extra` 是**原样直通**给设置程序的命令行参数串
    /// （如 `--schema=wubi86 --type=shadow`），空串=无附加参数。
    ///
    /// 刻意不解析 `extra`：设置端每加一个参数就要同步改一遍宿主，才是真正难维护的。
    /// 宿主只负责拼接与投递，取值合法性由设置端自己判断（它会降级并提示，不会崩）。
    /// 内部调用方请用 [`build_settings_args`] 构造，含空白的值会被正确加引号。
    pub(crate) fn open_settings_with(&self, page: Option<&str>, extra: &str) {
        #[cfg(not(target_os = "macos"))]
        let args = settings_cmdline(page, extra);

        // macOS：经 CmdOpenSettings(0x0507) 让 .app 用 LaunchServices 按 bundleID 启动/激活
        // 设置应用（app 侧 ModeStatusController.openSettings 已实现）。settings_app_path 拼 .exe，
        // macOS 恒为 None，旧路径会误落到已废弃的 web 分支并 WARN 失败，故此处直接短路。
        // payload 沿用「页名后接参数」的裸串形态（既有 add-word 路径就是这样传的），
        // Swift 侧解析方式不变。
        #[cfg(target_os = "macos")]
        {
            // 走扩展信封传结构化 argv：Swift 侧直接拿数组用，不必知道引号约定
            // （旧路径传的是「页名 + 参数」空格串，切词在 Swift 侧重做了一遍）。
            let argv = settings_argv(page, extra);
            let body = serde_json::json!({ "args": argv }).to_string();
            let encoded = wind_ipc::codec::encode_ext(
                wind_ipc::protocol::ext_kind::SETTINGS_OPEN,
                body.as_bytes(),
            );
            self.push_server.push_to_active(&encoded);
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(app) = crate::coordinator::settings_app_path() {
            if self.push_server.has_clients() {
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
            if self.push_server.has_clients() {
                let dir = crate::handle_cmdbar::resolve_workdir("setting.open", &url, "");
                self.push_shell_exec(&url, "", &dir, "", "");
            } else {
                let _ = self.ui_tx.send(UiCommand::OpenPath(url));
            }
        } else {
            tracing::warn!("打开设置失败：未找到 wind_setting 程序，web 服务也未就绪");
        }
    }

    /// 菜单「词库管理」：直接落到当前正在用的方案域，而不是默认的快捷短语域。
    /// 用户从输入法菜单进词库，十有八九是要管当前这套方案的词。
    /// 方案 id 取不到时退化为不带参数，行为与从前一致。
    pub(crate) fn open_dictionary(&self) {
        let schema = self.engine_mgr.active_schema_id();
        self.open_settings_with(Some("dict"), &build_settings_args(&[("schema", &schema)]));
    }

    /// 用户开关常驻工具栏（菜单）。仅翻转 toolbar_visible，显隐交 notify_toolbar
    /// 单点决策（结合 ime_active）。
    pub(crate) fn toggle_toolbar(&self) {
        let vis = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.toolbar_visible = !s.toolbar_visible;
            s.toolbar_visible
        };
        // 持久化到 config.ui.toolbar.visible(单一源:与设置页统一,reload 不会覆盖菜单选择)。
        let _ = Config::set_user_bool(&["ui", "toolbar", "visible"], vis);
        // 内存 config 同步跟上（同 status_toggle_always）：落盘与内存不同步时，下一次
        // 未经重载的读取会拿到陈旧值。
        self.refresh_config_in_memory(|c| c.ui.toolbar.visible = vis);
        self.notify_toolbar();
    }

    /// 循环切换到下一个主题，重绘并持久化选择。
    /// 构建并显示功能主菜单（对齐 Go 统一菜单：方案/主题子菜单 + 勾选态）。
    /// 位置与展开方向全由 `anchor` 描述，见 [`wind_ui_types::MenuPlacement`]。
    pub(crate) fn show_main_menu(&self, anchor: MenuAnchor) {
        let items = self.build_main_menu_items();
        self.mark_menu_open(0, String::new());
        let _ = self
            .ui_tx
            .send(UiCommand::ShowCandidateMenu { items, anchor });
    }

    /// macOS 精简功能菜单（IMK 输入源菜单 + 候选框右键空白菜单共用）。
    /// 相比 Windows 完整菜单，只保留必要项、且【无子菜单】（IMK 输入源菜单无法可靠处理嵌套子菜单）：
    ///   组1 输入方案（展开）：英文 + 各方案单选
    ///   组2 中文标点 / 全角 / 简入繁出
    ///   组3 显示状态图标
    ///   组4 重启服务
    ///   设置…
    /// 主题/检索范围/重载配置/高级/词库/关于 移除（配置类交由设置应用）。
    ///
    /// 状态图标开关是**唯一从精简树里保留的显示类开关**，理由是入口自锁：它关掉的正是
    /// 菜单栏状态指示器，而那个指示器的下拉菜单是完整树的两个入口之一；只留在完整树里
    /// 的话，用户一旦关掉图标就把开关本身也藏了（只剩候选框右键这个得先打字才碰得到的
    /// 入口）。IMK 输入源菜单不依赖任何 UI 可见性，是恒定可达的那个。
    #[cfg(target_os = "macos")]
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
            // 输入诊断 HUD 在 macOS 上整套未实现（`ShowInputDiag` 落在 forwarder 的兜底臂），
            // 点了没有任何反应。留一个死菜单项比没有更糟，故按平台摘掉。
            // 要在 macOS 做它得把整个浮层 UI 建在 `.app` 侧，见 wind_macos/AGENTS.md 差距表。
            #[cfg(not(target_os = "macos"))]
            M::leaf(
                "输入诊断 HUD",
                cmd(MenuCmd::ToggleInputDiagnostics),
                true,
                self.input_diag_hud_visible
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            M::leaf(
                "密码框强制英文",
                cmd(MenuCmd::TogglePasswordSuppress),
                true,
                self.password_suppress_enabled
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
        ];

        // 图标调试项**只在 Dev 变体出现**：它暴露的是"还没定下来的呈现参数"，
        // 正式用户看到只会困惑（何况其中两种形状已被否决）。
        #[cfg(all(feature = "desktop-ui", windows))]
        if wind_config::variant::is_dev() {
            advanced_children.push(M::separator());
            advanced_children.push(M::submenu("语言栏图标", self.build_icon_debug_menu()));
        }

        // 应用独立配置：所有 per-app 规则（均落在用户层 compat.toml）聚合于此。
        //
        // 放**顶层**而不是塞进「高级」是为了不增加层级深度——「高级 ▸ 应用独立配置 ▸ 初始
        // 输入模式 ▸ 三选一」是四层，而此前的「高级 ▸ 候选窗首显 ▸ 三选一」是三层；提到顶层
        // 后维持三层不变。这些项也比截图/打开目录更常用。
        //
        // 顶层标签固定为「应用独立配置」，**不嵌入进程名**：进程名长度不一（如
        // "Everything.exe" vs "chrome.exe"）曾导致主菜单整体宽度随焦点应用忽宽忽窄，
        // 观感很差——主菜单的宽度由其中最宽的一项撑开，顶层项不该背这个不确定性。
        // 进程名改放进子菜单的第一行（禁用的展示行，见 `MenuItemSpec::label`），宽度
        // 波动被限制在这个子菜单自己弹出的窗口里，不影响主菜单。
        //
        // 进程未解析时**子项禁用而非隐藏**（父项 enabled 恒 true，见
        // `MenuItemSpec::submenu`），菜单项位置保持稳定。
        let per_app_children = {
            use wind_config::app_compat::{FirstShowMode as F, InitialMode as IM};
            let proc = self.active_process_name();
            let enabled = !proc.is_empty();
            let cur_first_show = self
                .active_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .first_show_mode;
            let cur_mode = self.rule_initial_mode(&proc);
            let cur_punct = self.rule_initial_punct(&proc);
            let cur_auto_pair = self
                .active_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .auto_pair;
            let header = if enabled {
                proc.clone()
            } else {
                "当前应用未知".to_string()
            };
            // 三档单选。「跟随全局」必须是独立一档，不能靠"取消勾选"表达——否则用户设了
            // 规则之后无从撤销。它对应写盘时的 None，即从 compat.toml 里清掉该字段。
            let tri = |cur: Option<IM>, mk: fn(u8) -> MenuCmd| {
                vec![
                    M::leaf("跟随全局（默认）", cmd(mk(0)), enabled, cur.is_none()),
                    M::leaf("英文", cmd(mk(1)), enabled, cur == Some(IM::English)),
                    M::leaf("中文", cmd(mk(2)), enabled, cur == Some(IM::Chinese)),
                ]
            };
            vec![
                M::label(header),
                M::separator(),
                M::submenu("初始输入模式", tri(cur_mode, MenuCmd::InitialMode)),
                M::submenu("初始标点模式", tri(cur_punct, MenuCmd::InitialPunct)),
                M::separator(),
                // 三档**互斥**，做成子菜单单选：布尔开关时代它们能同时打开，实测就因此出过
                // 「fast 配了却从未生效」——instant 抢先放行，fast 的判据根本没机会跑。
                // 文案按「快 → 慢」以外的另一个维度排：用户真正在选的是**遇到慢宿主时
                // 宁可等还是宁可先显示**，故括号里写代价而不写机制。
                M::submenu(
                    "候选窗首显",
                    vec![
                        M::leaf(
                            "快速显示（默认）",
                            cmd(MenuCmd::FirstShowMode(1)),
                            enabled,
                            cur_first_show == F::Fast,
                        ),
                        M::leaf(
                            "等待精确坐标（较慢）",
                            cmd(MenuCmd::FirstShowMode(0)),
                            enabled,
                            cur_first_show == F::Wait,
                        ),
                        M::leaf(
                            "立即显示（最快，可能抖动）",
                            cmd(MenuCmd::FirstShowMode(2)),
                            enabled,
                            cur_first_show == F::Instant,
                        ),
                    ],
                ),
                // 「跟随全局」同样必须是独立一档（理由见上面 `tri`）。禁用一档主要给表格类
                // 宿主：Excel / WPS 表格「输入态」下方向键 = 确认单元格并移动，配对后的
                // 光标回退在那里无法实现，关掉配对是唯一可行的兼容策略。
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
            ]
        };

        let items = vec![
            M::submenu("输入方案", schema_children),
            M::leaf("全角", cmd(MenuCmd::ToggleWidth), true, full),
            M::leaf("中文标点", cmd(MenuCmd::TogglePunct), true, punct),
            M::leaf("简入繁出", cmd(MenuCmd::ToggleS2t), true, s2t),
            M::submenu("检索范围", filter_children),
            M::separator(),
            M::leaf(
                TOOLBAR_MENU_LABEL,
                cmd(MenuCmd::ToggleToolbar),
                true,
                toolbar_vis,
            ),
            self.soft_keyboard_menu_item(),
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
        ];
        items
    }

    /// 把 `MenuItemSpec` 树映射为线格式 `MenuNode` 树（id 由 `MenuKind::to_menu_id` 派生）。
    #[cfg(target_os = "macos")]
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

    // macOS 用 IMK 原生菜单, 不走协调器弹出菜单键转发 (见 coordinator handle_key_event 门控)。
    #[cfg_attr(target_os = "macos", allow(dead_code))]
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
        if state.menu_open {
            state.menu_open = false;
            state.menu_opened_at = None;
            drop(state);
            let _ = self.ui_tx.send(UiCommand::HideMenu);
        }
    }

    /// 菜单打开的状态收口：所有 `show_*_menu` 都必须经此置位。
    ///
    /// 单独抽出来是因为 `menu_open` 与 `menu_opened_at` **必须成对写入**，而置位点有四个
    /// （主菜单 / 候选右键 / 状态气泡 / tooltip）。靠"记得两行都写"在第五个入口出现时必然
    /// 失守，且失守的表现是「菜单偶尔一弹就没」这种极难复现的时序问题。
    fn mark_menu_open(&self, page_local: usize, text: String) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.menu_open = true;
        s.menu_opened_at = Some(std::time::Instant::now());
        s.menu_target_page_local = page_local;
        s.menu_target_text = text;
    }

    /// 焦点发生变化时关闭菜单（焦点路径专用，与 `menu_close` 的区别只在守卫与日志）。
    ///
    /// 菜单是**模态 UI**，语义是「任何外部动作都该终结它」；而输入态清理是**破坏性操作**，
    /// 语义是「宁可晚做也不能误做」。此前关菜单寄生在 `FocusLostReason::clears_input` 上，
    /// 于是被按后者的标准整定——`CtxLost` 豁免、陈旧失焦整条丢弃、DLL 侧翻转沿去重，三道
    /// 为保护输入态而设的闸门各自都会顺带把关菜单一并吞掉。故本函数自成一路：
    ///
    /// - **不看 reason**：关菜单幂等且非破坏性，放在 DocMgr 噪声层是安全的（同理于
    ///   `has_edit_context` 只翻可见性标志——真正不能放在噪声层的是清 buffer）。
    /// - **须在 `is_stale_focus_event` 之前调用**：「这条失焦不该动激活态」不等于「没发生
    ///   焦点变动」；对菜单而言，陈旧失焦同样证明用户动了别处。
    ///
    /// ⚠️ 覆盖面有限，**不能替代 UI 层的"点菜单外面就关"**：焦点通路只在宿主真的换了
    /// DocMgr 时才响。同一个文本框内点一下（焦点没变）、或在 explorer 里从桌面点到任务栏
    /// （两侧都无可编辑上下文）都不会产生任何 TSF 事件，那些情形本函数无能为力。
    ///
    /// ⚠️ 守卫**只保护本函数这条路**。`handle_focus_lost` 的 `clears_input` 分支照旧无条件
    /// 关菜单（那里 `notify_ui_hide` 会连带隐藏菜单窗口，拦不住也不该拦），故「菜单刚弹出
    /// 就被一条未被判陈旧的 `Thread` 失焦关掉」这个既有行为不变。
    pub(crate) fn menu_close_on_focus_change(&self, why: &str) {
        {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !s.menu_open {
                return;
            }
            // 守卫期内的焦点事件多半是「打开菜单这个动作本身」的尾迹，而非用户切走：
            // 跨宿主切换时旧宿主的 focus_lost 实测晚约 100ms 到达（97~111ms），从任务栏
            // 语言栏图标点开菜单正好落在这个窗口里，不设守卫会表现为「菜单弹出即消失」。
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
        // 与 UiEvent::MenuClose 同处置：焦点路径没有后续动作派发，可立即解除 tooltip /
        // 状态气泡的隐藏抑制（`menu_action` 那条路必须延后，理由见 clear_tooltip_menu_flag）。
        self.clear_tooltip_menu_flag();
    }

    /// 解除 Tooltip 的「菜单打开中」隐藏抑制。
    ///
    /// **必须在菜单动作派发之后调用，不能并进 `menu_close()`**：`menu_action()` 是先
    /// `menu_close()` 再 `run_menu_cmd()`，若在前者里解除，UI 线程会按序先处理解除
    /// （此时光标在菜单窗口上、不在 tooltip 上 → 立即隐藏 tooltip），再处理
    /// `ScreenshotTooltip`，于是截图恒定失败在「未显示」上。复制不受影响（文本已留存），
    /// 表现为「复制能用、截图不能用」。
    pub(crate) fn clear_tooltip_menu_flag(&self) {
        let _ = self.ui_tx.send(UiCommand::SetTooltipMenuOpen(false));
        // 状态气泡同理：菜单关掉后恢复自动隐藏计时。这里解除是安全的——它只影响
        // 隐藏抑制，不像 tooltip 那样会立即隐藏窗口，故不受"截图命令尚未处理"的时序制约。
        let _ = self.ui_tx.send(UiCommand::SetStatusMenuOpen(false));
    }

    /// 菜单打开时转发导航键给菜单窗口；返回 true 表示已消费。
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(crate) fn forward_menu_key(&self, key_code: u32) -> bool {
        if !self.is_menu_open() {
            return false;
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
            // 其它键：关闭菜单并吞掉
            _ => self.menu_close(),
        }
        true
    }

    /// 构建右键候选菜单项并下发给 UI 显示。
    /// 词条操作的启用态/删除文案按候选来源动态化（对齐 Go window_mouse 菜单状态规则）：
    /// - 置顶/前移：首项禁用；后移：末项禁用；拼音普通候选禁全部调位（无稳定位置语义）。
    /// - 删除：短语→「禁用短语」（软删可恢复）；用户词/临时词→真删；系统词→「隐藏候选」（shadow）。
    /// - 特殊模式（快符等）：词条操作**照常提供**，编码取其独立缓冲、归属取其引用方案。
    /// - 无词库落点者（临拼/临英/混输/网址，以及特殊模式的空码浏览态）：仅提供复制。
    pub(crate) fn show_candidate_menu(&self, page_local: usize, x: i32, y: i32) {
        use wind_ui_types::MenuItemSpec as M;
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            return;
        }
        let (start, end) = self.page_range(&state);
        let idx = start + page_local;
        if idx >= end || idx >= state.candidates.len() {
            return;
        }
        let cand = state.candidates[idx].clone();
        let word = cand.text.clone();
        let total = state.candidates.len();
        let scope = self.candidate_op_scope(&state);
        // 快捷输入的格式候选：判据独立于 candidate_op_scope（后者问「有没有词库落点」，
        // 混输没有，返回 None 是对的）。与写端 `candidate_or_quick_format_op` 同源。
        let quick = self.quick_format_scope(&state, page_local);
        drop(state);

        let op = |o: CandidateOp| MenuKind::Op(o);
        // 格式候选优先：它调的是「这种写法排第几」，不是词库里的某个词。
        // 标签也相应改写——操作对象是格式，不是这次算出来的那串文本。
        if let Some(q) = quick {
            let has_adjust = {
                let a = self.quick_adjust_of(q.kind);
                !a.is_empty()
            };
            let items = vec![
                // 「同类型内」这个限定不能省：置顶只在本类（日期/数字/计算）内生效，
                // 类与类之间的先后由 `mix_modes.members` 决定，不归本菜单管。
                M::leaf(
                    "置顶（同类型内）",
                    op(CandidateOp::MoveTop),
                    q.index_in_kind > 0,
                    false,
                ),
                M::leaf("上移", op(CandidateOp::MoveUp), q.index_in_kind > 0, false),
                M::leaf("下移", op(CandidateOp::MoveDown), true, false),
                // 面向用户说「隐藏」，存储字段叫 `disabled`——两者刻意不同名：
                // 菜单里它确实是「看不见了」，而设置页的启用开关要让那一行仍可见、能开回来。
                M::leaf("隐藏此格式", op(CandidateOp::Delete), true, false),
                // 整类恢复：被隐藏的格式不出候选、右键点不到，没有这一项就再也开不回来。
                // 与候选调整菜单的同名项**语义不同**（那边恢复一条，这里恢复整类），
                // 但两个菜单互斥出现，用户不会同时看到。
                M::leaf("恢复默认", op(CandidateOp::Reset), has_adjust, false),
                M::separator(),
                M::leaf("复制", MenuKind::Copy, true, false),
            ];
            self.mark_menu_open(page_local, word);
            let _ = self.ui_tx.send(UiCommand::ShowCandidateMenu {
                items,
                anchor: MenuAnchor::at_point(x, y),
            });
            return;
        }
        // 常用/生僻标记：作用域是**全局的那个字**，与词库落点无关，故对上面那两类
        // 「只有复制」的状态同样成立——它只要求 `cand.text` 是**单个可登记的字符**
        // （`is_markable`：空白与控制字符以外全放行）。issue #83 起不再限定汉字：
        // 字根、间架结构符、注音、假名这些非汉字候选正是用户点名要能关掉的。
        // ⚠️ 刻意不搭 `candidate_op_scope` 的便车：那个判据问的是「有没有词库落点」，
        // 用它来管这一项，会让临拼/临英/混输/空码浏览态下的字莫名其妙标不了。
        let common_item = self.common_char_mark(&word).map(|m| {
            // 文案按当前判定二选一。**不加「（全局）」后缀**（2026-08-24 用户要求菜单简洁）：
            // 作用域差异写进设置页的说明，不占右键菜单的宽度。
            let label = if m.common {
                "设为生僻字"
            } else {
                "设为常用字"
            };
            M::leaf(label, op(CandidateOp::ToggleCommon), true, false)
        });

        // 有词库落点才给词条操作。无落点的两类状态——没有独立词库归属的 overlay（临拼/临英/
        // 混输/网址，编码各持独立缓冲且无处落键）与空码浏览态（特殊模式 show_all_on_enter，
        // 读端 apply_shadow_in 对空码直接 return，写了也永不生效）——仅保留复制。
        // 判据与写端 `candidate_op` 同源，见 `candidate_op_scope`。
        let mut items = if let Some(scope) = scope {
            let cand_id = (!cand.id.is_empty()).then_some(cand.id.as_str());
            let has_rule = self.shadow_has_rule(&scope.schema, &scope.code, &word, cand_id);
            // 拼音普通候选**只放行置顶**，前移/后移仍禁（`position=0` 位置语义稳定，
            // `position=N` 在候选集变动后失去意义）；命令候选不受限。
            // 引擎类型来自 scope：特殊模式问的是它引用的方案，照抄主方案会在「主方案拼音 +
            // 快符码表」时整体误禁调位。
            //
            // ⚠️ 判据必须与写端 `candidate_op` 逐字对应：菜单给了入口而写端 return，或反过来，
            // 都是**完全静默**的错配——用户点得动却毫无反应，或明明能用却是灰的。
            let is_pinyin = matches!(scope.engine_type, Some(wind_engine::EngineType::Pinyin));
            let group_member = candidate_is_group_member(&cand);
            let pinyin_locked = is_pinyin && !cand.is_command;
            let can_pin = !group_member;
            let movable = !pinyin_locked && !group_member;
            let (delete_label, delete_enabled) = candidate_delete_menu(&cand);

            vec![
                M::leaf("置顶", op(CandidateOp::MoveTop), can_pin && idx > 0, false),
                M::leaf("前移", op(CandidateOp::MoveUp), movable && idx > 0, false),
                M::leaf(
                    "后移",
                    op(CandidateOp::MoveDown),
                    movable && idx + 1 < total,
                    false,
                ),
                M::leaf(delete_label, op(CandidateOp::Delete), delete_enabled, false),
                M::leaf("恢复默认", op(CandidateOp::Reset), has_rule, false),
                M::separator(),
            ]
        } else {
            Vec::new()
        };
        // 尾段两项对**两个分支都成立**，故统一在这里追加，不在各分支里各写一份：
        // 分支里各写一份正是「加一项时漏改另一处」的经典入口，而漏改的表现是
        // 「同一个字在临拼下右键没有这一项」——用户绝不会想到那是分支写重了。
        items.extend(common_item);
        items.push(M::leaf("复制", MenuKind::Copy, true, false));
        self.mark_menu_open(page_local, word);
        // 候选右键菜单在光标处向下弹出（above=false，y_bottom 不使用）。
        let _ = self.ui_tx.send(UiCommand::ShowCandidateMenu {
            items,
            anchor: MenuAnchor::at_point(x, y),
        });
    }

    /// 把工具栏移到「输入焦点所在显示器」上的记忆位置（该屏没记过则落到它的右下角）。
    ///
    /// 仅在显示器**发生变化**时下发，靠 `current_toolbar_monitor` 去重：notify_toolbar
    /// 在每次模式切换/焦点事件上都跑，无条件下发会把用户拖动过的位置反复重置回记忆值，
    /// 且拖动中途（save 尚未落地）还会把工具栏拽回原处。
    ///
    /// 调用点必须在 `UpdateToolbar` **之前**——反过来会先在旧屏渲染一帧再跳过去，
    /// 表现为切屏闪一下。`Toolbar::set_pos` 内部受 `visible` 门控（隐藏中只记坐标不显形），
    /// 故本路径不会绕过 `toolbar_gate` 的显示迟滞。
    ///
    /// ⚠️ **已知且有意接受的后果：工具栏无法停在非焦点屏。**
    /// 本函数的判据是前台窗口所在屏，而拖动落盘（`save_toolbar_pos`）记的是工具栏落点
    /// 所在屏。用户把工具栏拖到副屏 B 而焦点仍在主屏 A 时，两者不等，下一次任意
    /// `notify_toolbar`（切中英、切方案、焦点事件……）就会把它拽回 A —— 跨屏拖动因此
    /// 是做不到的操作。这不是 bug：工具栏放在用户没在看的那块屏上本就违背它的用途
    /// （要扭头才能看状态）。2026-08-09 与用户确认后维持此行为，**别把它当缺陷"修"成
    /// sticky 或加开关**，那会引入一个不可见的模式态。
    /// 注意跳回只发生在**拖动结束之后**：拖动期间前台窗口未变，key 与缓存相同，
    /// 本函数一律 early-return，不会出现「拖到一半被拽走」。
    fn sync_toolbar_monitor(&self) {
        let Some((key, work_right, work_bottom)) = focus_monitor() else {
            return;
        };
        // 「判定换屏 + 记新屏 + 取该屏坐标」必须在同一临界区里完成，否则中间那道缝会让
        // 刚拖好的位置被顶掉：本线程认定换到 B 屏并释放锁后、尚未读表时，拖动线程
        // （`save_toolbar_pos`）把 B 屏的新坐标写进表——本线程随后读到的是旧值，下发出去
        // 就把屏上刚拖好的工具栏拽回了旧处，且表里是新值、屏上是旧值，要到下次换屏才纠正。
        //
        // 锁序 `current_toolbar_monitor` → `toolbar_positions`，与 `save_toolbar_pos`
        // 的取用先后一致（那边不嵌套）；⚠️ 新增取这两把锁的代码请沿用同一顺序。
        let saved = {
            let mut cur = self
                .current_toolbar_monitor
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cur.as_deref() == Some(key.as_str()) {
                return;
            }
            let saved = self
                .toolbar_positions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .copied();
            *cur = Some(key.clone());
            saved
        };
        let cmd = match saved {
            Some((x, y)) => UiCommand::SetToolbarPos { x, y },
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
    /// 非 Windows 上 `focus_monitor` 恒为 None，故本函数恒为 no-op——位置恢复整体不生效。
    /// 无实际影响：`manager_macos.rs` 的 forwarder 本就把 `SetToolbarPos`/`SetToolbarCorner`
    /// 当留桩丢弃，工具栏在那边由 .app 原生承载。
    ///
    /// 桌面构造路径（`new`）专用；headless/Android 入口不经此，故仅在无 desktop-ui 时放行
    /// dead_code——**不要**整体 feature 门控（同 impl 块的运行期工具栏逻辑配置重载仍要用）。
    #[cfg_attr(not(feature = "desktop-ui"), allow(dead_code))]
    pub(crate) fn init_toolbar_pos(&self) {
        self.sync_toolbar_monitor();
    }

    /// 持久化工具栏位置（按显示器 key 独立存储，best-effort）。
    ///
    /// key 取自**工具栏落点自身**而非光标：拖动结束时光标压在工具栏上，两者碰巧同屏，
    /// 但工具栏坐标才是「这条工具栏属于哪块屏」的直接事实。存取两侧由此共用同一个
    /// 键空间语义——取那侧问的是「焦点屏上记过什么位置」，存那侧答的是「这块屏上
    /// 工具栏在哪」，只有 key 同源才对得上。
    pub(crate) fn save_toolbar_pos(&self, x: i32, y: i32) {
        let Some(key) = monitor_key_from_point(x, y) else {
            // 查不到显示器就别存：这块表的读取侧（`focus_monitor`）在同样的失败下返回
            // None，存进任何兜底 key 都只会是永远读不出来的垃圾。
            tracing::debug!("工具栏位置未保存：查不到 ({},{}) 所在显示器", x, y);
            return;
        };
        // 拖到别的屏 = 用户在那块屏上重新定了位；同步当前屏记录，否则下一次
        // sync_toolbar_monitor 会认为「屏没变」而不再校正。
        {
            let mut cur = self
                .current_toolbar_monitor
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cur = Some(key.clone());
        }
        {
            let mut map = self
                .toolbar_positions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            map.insert(key, (x, y));
        }
        if let Some(state_dir) = Config::state_dir() {
            let map = self
                .toolbar_positions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut rs = wind_config::RuntimeState::load(&state_dir);
            rs.toolbar_positions = map.clone();
            let _ = rs.save(&state_dir);
        }
    }

    /// 工具栏单元格点击：复用菜单命令切换状态（内部已推送 C++），再刷新工具栏显示。
    pub(crate) fn mouse_toolbar(&self, action: ToolbarAction) {
        match action {
            ToolbarAction::OpenSettings => {
                self.open_settings(None);
                return;
            }
            ToolbarAction::ToggleS2t => {
                self.handle_menu_command("toggle_s2t");
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
            // 上面那个 match 已 return 掉的三支。**加 ToolbarAction 变体时必须一并处理
            // 上面那个 match**——漏了就落到这里当场 panic，而不是静默不响应。
            ToolbarAction::ToggleS2t
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
    /// 复用短语动作那条链（`run_command_candidate`），故 `open` / `proc.run` /
    /// `key.tap` / `wind.cli` 等全部可用，且求值失败会弹 toast 而不是哑失败。
    ///
    /// ⚠️ 经 `spawn_command` 起独立线程：`run_command_candidate` 的文档要求「未持
    /// state 锁时调用」，动作链上的控制器会回调自锁的 coordinator 方法。
    ///
    /// 下标取不到就忽略：UI 侧的 spec 与本侧配置之间有一瞬可能错开（配置刚重载、
    /// 新的 SetToolbarLayout 还没到 UI），越界不是异常。
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
        self.spawn_command(wrap_command_source(action), String::new());
    }

    /// 焦点/激活切换路径专用：先用缓存值立即同步通知（无阻塞），
    /// 再后台刷新全屏缓存，若状态变化则再次通知。
    /// 保证 bridge handler 线程立即返回，缓存刷新在独立线程完成。
    /// 非焦点路径（模式切换/菜单操作）直接调 notify_toolbar()，缓存值仍然有效。
    pub(crate) fn notify_toolbar_async(&self) {
        // 立即用缓存值通知，bridge 线程无阻塞
        self.notify_toolbar();
        // hide_in_fullscreen 关闭时缓存永远为 false，无需后台刷新
        if !self.rt().config.ui.toolbar.hide_in_fullscreen {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        // 单飞：已有探测在途就跳过。探的是**同一个**全局前台状态，重复查没有意义，
        // 而焦点变化是成串来的（一次应用切换会连着触发多次），此前每次都 spawn 一个线程。
        //
        // 这里不并入 first-show 那个共享定时器：is_foreground_fullscreen 会阻塞
        // （异步化它正是 1abab9f 的目的），塞进定时器线程会拖垮兜底时限。
        if self
            .fullscreen_probing
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("fullscreen-probe".into())
            .spawn(move || {
                let is_fs = crate::is_foreground_fullscreen();
                if let Some(c) = weak.upgrade() {
                    let prev = c
                        .fullscreen_cached
                        .swap(is_fs, std::sync::atomic::Ordering::Relaxed);
                    c.fullscreen_probing
                        .store(false, std::sync::atomic::Ordering::Release);
                    if prev != is_fs {
                        // 全屏态发生变化，用新值重新通知
                        c.notify_toolbar();
                    }
                }
            });
        if spawned.is_err() {
            // 线程没起来就得把闸放回去，否则此后永远不再探测
            self.fullscreen_probing
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }

    /// 推送当前状态到常驻工具栏（中英/方案/标点/全半角）
    /// 工具栏可见性单点决策 + 内容刷新。对齐 Go toolbar_reducer 的合取公式：
    /// 仅当 `ime_active && toolbar_visible` 时显示（UpdateToolbar 会刷内容+定位+显示），
    /// 否则下发 HideToolbar。所有调用点（启动/切模式/切方案/激活/失活）经此单点决策，
    /// 不再各自直接显示，根治”工具栏总是显示、切走输入法不隐藏”。
    pub(crate) fn notify_toolbar(&self) {
        // 前台应用全屏时隐藏工具栏（读缓存，由 notify_toolbar_async 后台刷新，无阻塞）。
        let hide_fullscreen = self.rt().config.ui.toolbar.hide_in_fullscreen
            && self
                .fullscreen_cached
                .load(std::sync::atomic::Ordering::Relaxed);
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 四项合取：本输入法在服务某宿主（ime_active）、焦点在可编辑控件里
        // （has_edit_context）、用户开着工具栏（toolbar_visible）、且未处于全屏。
        // 前两项正交且缺一不可——只看 ime_active 会让应用内点到非文本框时工具栏不隐藏。
        if !(s.ime_active && s.has_edit_context && s.toolbar_visible) || hide_fullscreen {
            // 记录是哪一项否决了显示：UI 层日志只看得到「HideToolbar」，判不出成因，
            // 而四条路径的排查方向完全不同（激活态乱序 / 焦点离开输入框 / 用户关了开关 /
            // 全屏探测）。
            tracing::debug!(
                "notify_toolbar: 隐藏 ime_active={} has_edit_ctx={} toolbar_visible={} fullscreen={}",
                s.ime_active,
                s.has_edit_context,
                s.toolbar_visible,
                hide_fullscreen
            );
            drop(s);
            // 内容没变就不再推：焦点抖动时这条 Hide 会被连发数次（真机：飞书 200ms 内
            // 5 轮 focus_lost，每轮一条），全挤在 UI 线程上。见 `last_toolbar_push`。
            if self.take_toolbar_push_if_changed(ToolbarPush::Hidden) {
                let _ = self.ui_tx.send(UiCommand::HideToolbar);
            }
            // ⚠ 去重**只挡 UI 推送这一条**：下面两个各有自己的去重与触发条件
            // （HUD 看的是诊断开关、图标看的是 label/角标），跟着一起跳过就会出现
            // 「工具栏没变但图标该变了却没变」。
            self.push_input_diag_hud_if_visible(); // 见函数末尾同一行的说明
            // 语言栏图标同样收口于此，且**两个出口都要**——「不可输入」恰恰走的是本分支，
            // 只在下面那个出口补的话，图标永远等不到变「英」。同 HUD 刷新的理由。
            self.publish_langbar_icon_now();
            return;
        }
        let (chinese_mode, caps_lock) = (s.chinese_mode, s.caps_lock);
        drop(s);
        // ⚠ **必须在取 state 锁之前算**：effective_input_block() 内部要读 state，
        // 而 std::sync::Mutex 不可重入——写在下面的初始化式里就是当场自死锁
        // （工具栏一显示就走到这里，表现为输入法整个卡住）。
        let input_blocked = self.effective_input_block().shows_english();
        // 不可输入（密码框 / 无编辑上下文 / 系统禁用）时模式格显英文标签：此刻键已全部
        // 透传给宿主，与英文半角态干的是同一件事，故**共用同一个标签、不另设配置键**。
        //
        // 在协调器算而不是留给 wind-ui 覆盖：wind-ui 读不到配置，它自己兜底就只能写死
        // 一个字面量——那正是这处硬编码的由来。ToolbarState 不下发 TSF，没有
        // `_inputTypeLabel` 那种持久值顾虑，可以直接改 icon_label 本身；语言栏图标那条
        // 路**不行**，只能在 spec 上覆盖（见 publish_langbar_icon 的 label 一行）。
        let icon_label = if input_blocked {
            self.rt().config.ui.labels.english_label()
        } else {
            self.mode_icon_label(chinese_mode, caps_lock)
        };
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let tb = ToolbarState {
            chinese_mode,
            icon_label,
            caps_lock,
            full_width: s.full_width,
            chinese_punct: s.chinese_punct,
            s2t_enabled: s.s2t_enabled,
            // 简繁格：已启用时才在工具栏显示（默认 false 不显示）
            s2t_shown: s.s2t_enabled,
            soft_keyboard_on: self.softkeyboard_is_open(),
            // 不可输入（密码框 / 无编辑上下文 / 系统禁用）：模式格显 "英" 且不高亮。
            // 与语言栏图标读**同一个** effective_input_block，不会再出现「图标说英文、
            // 工具栏说中文」的错位——那正是把判据分给两个负责者的代价。
            input_blocked,
        };
        drop(s);
        // 焦点换屏则先把工具栏挪到那块屏（内部按显示器 key 去重，未换屏时零下发）。
        // 必须先于 UpdateToolbar：反序会先在旧屏渲染一帧再跳。
        self.sync_toolbar_monitor();
        if self.take_toolbar_push_if_changed(ToolbarPush::Shown(Box::new(tb.clone()))) {
            let _ = self.ui_tx.send(UiCommand::UpdateToolbar(tb));
        }
        // HUD 刷新收口于此（两个出口各一次）。诊断 HUD 展示的 ime_active /
        // has_edit_context 正是上面那道合取的输入，而**凡是改动它们的路径都必须调
        // notify_toolbar 才能生效**，所以这里是唯一不会漏的落点。
        // 反例（2026-07-26 实测）：起初只在 apply_input_diag 里推，于是 focus_gained
        // 之外的路径（CtxLost 等）改了状态却不刷新，HUD 一直显示上一次的快照。
        // 在此调用是安全的：state 锁已 drop，且 HUD 关闭时该函数首行即返回，零开销。
        self.push_input_diag_hud_if_visible();
        // 语言栏图标收口（与上面那个出口成对）。内部对相同位图跳过重渲与刷新推送，
        // 故多调无副作用；漏调则是「状态变了图标不跟」。
        self.publish_langbar_icon_now();
    }
}

/// 是否 $SS/$AA 展开后的组成员候选：顺序/成员由短语定义决定，禁一切调整
/// （改动走编辑短语路径，不允许 shadow 双轨漂移；对齐 Go isGroupMember 规则）。
/// 组导航候选本身（is_group，text 是组名）不算成员：可禁用整组。
pub(crate) fn candidate_is_group_member(cand: &wind_candidate::Candidate) -> bool {
    cand.is_phrase
        && !cand.is_group
        && (cand.phrase_template.starts_with("$SS") || cand.phrase_template.starts_with("$AA"))
}

/// 右键「删除」菜单项的动态文案与可用性（按候选来源，对齐 Go computeDeleteMenuLabel）：
/// 短语→禁用短语（软删可恢复）；用户词/临时词→真删；系统词→shadow 隐藏。
/// 单字同样允许隐藏（旧版的单字保护已取消：shadow 按 code+word 键控，只隐藏该编码下的
/// 该字，其它编码仍可打出，且设置页可恢复，不存在"某字彻底打不出"）。
/// Windows 菜单构建与 macOS 禁用位推送共用，避免两处规则漂移。
pub(crate) fn candidate_delete_menu(cand: &wind_candidate::Candidate) -> (&'static str, bool) {
    if candidate_is_group_member(cand) {
        ("删除词条", false)
    } else if cand.is_phrase {
        // 静态短语前缀命中（is_prefix 且无完整码）定位不到 store 记录 → 暂禁。
        ("禁用短语", !cand.is_prefix || !cand.group_code.is_empty())
    } else if cand.meta.is_user_dict {
        ("删除用户词", true)
    } else if cand.meta.is_temp_dict {
        ("删除临时词", true)
    } else {
        // 系统词（码表/拼音）：shadow 软隐藏。
        ("隐藏候选", true)
    }
}

/// 把一段动作源补成 `run_command_candidate` 能执行的**短语格式**。
///
/// # 为什么需要这一步
///
/// `run_command_candidate` 走 `evaluate_phrase`，那是短语系统的格式——命令必须带顶层
/// `$CC(…)` 标记。裸的 `proc.run("x.exe")` 会被当成**字面文本**，一个动作都不跑，
/// **而且不报错**：症状是「按钮显示正常、日志一条告警都没有、点了什么都不发生」。
/// 2026-08-26 用户真机报到这个 bug。
///
/// 判据：**按钮的 action 本来就只可能是命令**。`$CC` 标记存在的意义是让短语区分
/// 「这条是要上屏的文本」还是「这条是要执行的命令」，而工具栏按钮没有这个歧义
/// ——它不可能是文本。要求用户为一个不存在的歧义写一个标记，是把内部格式当成了 API。
///
/// 已带标记的原样放行：用户从短语那边抄一条过来照样能用，且不会被包成
/// `$CC("", $CC(...))` 这种嵌套。
///
/// 第一个参数是**候选显示文本**，工具栏按钮不进候选列表，故给空串。
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

/// 根据屏幕坐标计算显示器 key（工作区右下角："workRight,workBottom"）。
/// 查不到显示器时返回 None。
///
/// 失败语义要与 `focus_monitor` 对称——它同样在查不到时返回 None。此前这里回落到
/// `"0,0"`，于是保存侧会把坐标写进一个**读取侧永远问不出来的 key**（`focus_monitor`
/// 不可能产出 `"0,0"`），位置静默丢失。存取共用一张表，两侧的失败也得共用一套语义。
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))] // 显示器查询仅 Windows 有
fn monitor_key_from_point(x: i32, y: i32) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use std::mem::{size_of, zeroed};
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT { x, y };
            let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi: MONITORINFO = zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                return Some(format!("{},{}", mi.rcWork.right, mi.rcWork.bottom));
            }
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 输入焦点所在显示器：`(key, 工作区右边界, 工作区下边界)`；查不到返回 None（不动工具栏）。
///
/// 判据取**前台窗口**而非光标：键盘切窗（Alt+Tab、窗口热键）时光标根本不动，用光标
/// 问不出「用户在哪块屏上打字」。前台窗口恒有值、查询不阻塞，`is_foreground_fullscreen`
/// 已用同一套 `GetForegroundWindow` + `MonitorFromWindow`。
///
/// ⚠ 这里刻意**不用** caret 坐标：caret 属于 TSF 层、常处于未就绪态（coords_ready /
/// caret_pending），拿它当窗口层的判据会在首帧把工具栏定到错屏上——两层判据不可互换。
///
/// 前台窗口是桌面/Shell（无应用在前台）时回退到光标所在屏：仍是同一层的「屏幕上某点」，
/// 只是换了个更弱的信号源，不引入跨层耦合。
fn focus_monitor() -> Option<(String, i32, i32)> {
    #[cfg(target_os = "windows")]
    {
        use std::mem::{size_of, zeroed};
        use windows::Win32::Foundation::{HWND, POINT};
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
            MonitorFromWindow,
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
            let mut mi: MONITORINFO = zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                // key 与 monitor_key_from_point 同格式，两者必须一致——存/取共用一张表。
                return Some((format!("{},{}", wa.right, wa.bottom), wa.right, wa.bottom));
            }
        }
        None
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

/// 固定位置落盘前的哨兵规避（候选窗与状态气泡共用）。
///
/// UI 侧用 `(0, 0)` 表示"已开启固定但尚未设定位置"（落到屏幕默认锚点），可主屏工作区
/// 的左上角**往往正是** `(0, 0)`（任务栏在底部时）——用户真把候选窗拖到屏幕最左上角，
/// 落盘值就撞上哨兵，下次显示被判为"没设过"而跳回默认锚点，表现为"位置没被记住"。
///
/// 哨兵值与合法值域重叠是根因；这里在落盘侧下移 1px 避开：视觉不可察觉，语义无歧义。
fn avoid_unset_sentinel(x: i32, y: i32) -> (i32, i32) {
    if (x, y) == (0, 0) { (0, 1) } else { (x, y) }
}

#[cfg(test)]
mod tests {
    use super::avoid_unset_sentinel;

    /// 输入诊断 HUD 整套在 macOS 未实现（`ShowInputDiag` 落在 forwarder 的兜底臂），
    /// 菜单里不该留一个点了没反应的项。
    ///
    /// 这条同时是 `#[cfg]` **确实作用到了 `vec![]` 元素上**的证据——属性写在数组元素前
    /// 是合法的，但写错位置（比如挂到 `M::leaf` 的某个实参上）照样能编过，只是不生效。
    #[test]
    fn input_diag_hud_menu_item_is_platform_gated() {
        use crate::coordinator::Coordinator;
        use wind_config::Config;

        let c = Coordinator::new_headless(Config::default(), None);
        fn contains(items: &[wind_ui_types::MenuItemSpec], label: &str) -> bool {
            items
                .iter()
                .any(|i| i.label == label || contains(&i.children, label))
        }
        assert_eq!(
            contains(&c.build_main_menu_items(), "输入诊断 HUD"),
            !cfg!(target_os = "macos"),
            "HUD 菜单项的平台门控与当前平台不符"
        );
        // 同一子菜单里的邻项必须还在——防止 cfg 把整块 vec 或相邻项一起吞掉。
        assert!(
            contains(&c.build_main_menu_items(), "密码框强制英文"),
            "邻项被误伤"
        );
    }

    /// 工具栏分格右键：**每一格要么有定制、要么明确回落主菜单**，且回落这条路不可断。
    ///
    /// 隐藏了齿轮之后，右键工具栏是主菜单仅剩的鼠标入口（`toolbar-customization.md`
    /// §2.2 判据③）。若哪天给齿轮格也配了定制菜单而忘了在里面留一条通往主菜单的路，
    /// 用户就可能把自己锁在设置之外——这条断言钉的正是那个前提。
    ///
    /// 用穷举而非逐个点名：`ToolbarAction` 加新变体时，新变体没被想过就会红。
    #[test]
    fn every_toolbar_cell_either_customizes_or_falls_back() {
        use crate::coordinator::Coordinator;
        use wind_config::Config;
        use wind_ui_types::ToolbarAction as A;

        let c = Coordinator::new_headless(Config::default(), None);
        // 有定制的格：菜单非空（空表在 `show_toolbar_menu` 里同样回落，但那是兜底，
        // 不该是这几格的常态）。
        for a in [
            A::ToggleMode,
            A::SwitchEngine,
            A::TogglePunct,
            A::ToggleWidth,
            A::ToggleS2t,
            A::ToggleSoftKeyboard,
        ] {
            let items = c
                .build_toolbar_cell_menu(a)
                .unwrap_or_else(|| panic!("{a:?} 应有定制菜单"));
            assert!(!items.is_empty(), "{a:?} 的定制菜单是空的");
        }
        // 没定制的格：必须返回 None 才会回落主菜单。
        for a in [A::OpenSettings, A::Custom(0)] {
            assert!(
                c.build_toolbar_cell_menu(a).is_none(),
                "{a:?} 不该有定制菜单——它要回落完整主菜单"
            );
        }
    }

    /// ⛔ **每份分格菜单都必须留一条通往完整主菜单的路**。
    ///
    /// 判据③（`toolbar-customization.md` §2.2）是「隐藏齿轮不会锁死用户——右键工具栏
    /// 任意位置同样弹主菜单」。分格右键把功能格的右键让给了精简菜单，若不补这一条，
    /// 隐藏了齿轮的用户就只剩 **12dp 的拖动柄**通向主菜单——一个要瞄准的目标。
    ///
    /// ⚠️ 上面那条穷举测试**证不了这件事**：它断言的恰恰是「这些格有自己的菜单」，
    /// 与本条方向相反。两条一起才把判据③钉住。
    #[test]
    fn every_cell_menu_keeps_a_way_back_to_the_main_menu() {
        use crate::coordinator::Coordinator;
        use wind_config::Config;
        use wind_ui_types::{MenuCmd, MenuKind, ToolbarAction as A};

        let c = Coordinator::new_headless(Config::default(), None);
        for a in [
            A::ToggleMode,
            A::SwitchEngine,
            A::TogglePunct,
            A::ToggleWidth,
            A::ToggleS2t,
            A::ToggleSoftKeyboard,
        ] {
            let items = c
                .build_toolbar_cell_menu(a)
                .unwrap_or_else(|| panic!("{a:?}"));
            assert!(
                items
                    .iter()
                    .any(|i| i.kind == MenuKind::Command(MenuCmd::OpenMainMenu)),
                "{a:?} 的分格菜单没有回主菜单的入口，隐藏齿轮后用户只剩 12dp 拖动柄"
            );
        }
    }

    /// 分格菜单里的开关，文案与勾选态必须与主菜单里同一个开关**逐字相同**。
    ///
    /// 同一个开关在两处叫不同的名字，用户会当成两件事；勾选态对不上则更糟——
    /// 两处菜单会互相"打脸"。这条把「复制了一份文案」这种改动挡在合并前。
    #[test]
    fn cell_menu_labels_match_the_main_menu() {
        use crate::coordinator::Coordinator;
        use wind_config::Config;
        use wind_ui_types::ToolbarAction as A;

        let c = Coordinator::new_headless(Config::default(), None);
        let main = c.build_main_menu_items();
        let find = |items: &[wind_ui_types::MenuItemSpec], label: &str| -> Option<bool> {
            items.iter().find(|i| i.label == label).map(|i| i.checked)
        };

        let cell = c.build_toolbar_cell_menu(A::TogglePunct).expect("标点格");
        for label in ["全角", "中文标点", "简入繁出"] {
            assert_eq!(
                find(&cell, label),
                find(&main, label),
                "{label} 在分格菜单与主菜单里对不上（文案或勾选态）"
            );
        }
        // ★ 顺序也要对齐，而按 label 查找对顺序是**盲的**——上面那个循环换成任意排列
        // 都照样绿。同一组开关在两处排得不一样，用户每次都得重新找。
        let order: Vec<&str> = cell
            .iter()
            .map(|i| i.label.as_str())
            .filter(|l| ["全角", "中文标点", "简入繁出"].contains(l))
            .collect();
        let main_order: Vec<&str> = main
            .iter()
            .map(|i| i.label.as_str())
            .filter(|l| ["全角", "中文标点", "简入繁出"].contains(l))
            .collect();
        assert_eq!(order, main_order, "三个开关在两处的排列顺序不一致");

        // 中英格给的是主菜单「输入方案」子菜单的原样内容（后面另挂「更多…」，见
        // `every_cell_menu_keeps_a_way_back_to_the_main_menu`，故是前缀相等而非全等）。
        let cell = c.build_toolbar_cell_menu(A::ToggleMode).expect("中英格");
        let sub = main
            .iter()
            .find(|i| i.label == "输入方案")
            .expect("主菜单缺「输入方案」");
        assert_eq!(
            cell.get(..sub.children.len()),
            Some(&sub.children[..]),
            "中英格右键与「输入方案」子菜单不同源"
        );
    }

    /// 状态图标开关必须同时出现在 macOS 的**两棵**菜单树里，且文案一致。
    ///
    /// 回归背景：IMK 输入源菜单走精简树 `build_menu_items_macos()`、候选框右键与状态指示器
    /// 走完整树 `build_main_menu_items()`，两棵树各自维护 → 精简树当初把这项砍了，同一个
    /// 输入法在两处菜单里表现不一。这条测试同时钉住「都在」和「同名」。
    #[cfg(target_os = "macos")]
    #[test]
    fn toolbar_toggle_present_in_both_macos_menu_trees() {
        use super::TOOLBAR_MENU_LABEL;
        use crate::coordinator::Coordinator;
        use wind_config::Config;

        let c = Coordinator::new_headless(Config::default(), None);
        fn contains(items: &[wind_ui_types::MenuItemSpec], label: &str) -> bool {
            items
                .iter()
                .any(|i| i.label == label || contains(&i.children, label))
        }
        assert!(
            contains(&c.build_menu_items_macos(), TOOLBAR_MENU_LABEL),
            "IMK 精简菜单缺状态图标开关——关掉图标后开关本身也没了（入口自锁）"
        );
        assert!(
            contains(&c.build_main_menu_items(), TOOLBAR_MENU_LABEL),
            "完整菜单缺状态图标开关"
        );
        // 文案必须是 macOS 语义：这里显隐的是 NSStatusItem，不是 Windows 的悬浮工具栏。
        assert_eq!(TOOLBAR_MENU_LABEL, "显示状态图标");
    }

    /// 勾选态必须跟随 `toolbar_visible`，否则菜单上是个永远不打勾的死开关。
    #[cfg(target_os = "macos")]
    #[test]
    fn toolbar_toggle_reflects_visibility_in_imk_menu() {
        use super::TOOLBAR_MENU_LABEL;
        use crate::coordinator::Coordinator;
        use wind_config::Config;

        let c = Coordinator::new_headless(Config::default(), None);
        let checked = |c: &Coordinator| {
            c.build_menu_items_macos()
                .into_iter()
                .find(|i| i.label == TOOLBAR_MENU_LABEL)
                .expect("菜单项不存在")
                .checked
        };
        let before = checked(&c);
        c.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .toolbar_visible = !before;
        assert_eq!(checked(&c), !before, "勾选态没跟随 toolbar_visible");
    }

    /// 只有恰好 (0,0) 被规避，其余坐标（含含 0 分量与负坐标）必须原样落盘。
    #[test]
    fn only_the_exact_sentinel_is_nudged() {
        assert_eq!(avoid_unset_sentinel(0, 0), (0, 1), "撞哨兵 → 下移 1px");
        // 含 0 分量但非哨兵：不能动，否则用户贴左边/贴顶边的位置会被悄悄改掉
        assert_eq!(avoid_unset_sentinel(0, 5), (0, 5));
        assert_eq!(avoid_unset_sentinel(5, 0), (5, 0));
        // 负坐标：副屏位于主屏左侧/上方时屏幕坐标为负，属合法值
        assert_eq!(avoid_unset_sentinel(-1920, -100), (-1920, -100));
        assert_eq!(avoid_unset_sentinel(100, 200), (100, 200));
    }

    /// 规避结果自身绝不能再是哨兵，否则等于没修。
    #[test]
    fn nudged_result_is_never_the_sentinel() {
        assert_ne!(avoid_unset_sentinel(0, 0), (0, 0));
    }

    #[test]
    fn settings_args_skip_empty_and_quote_whitespace() {
        use super::build_settings_args;
        assert_eq!(build_settings_args(&[]), "");
        assert_eq!(build_settings_args(&[("schema", "")]), "", "空值整项跳过");
        assert_eq!(
            build_settings_args(&[("schema", "wubi86"), ("type", "shadow")]),
            "--schema=wubi86 --type=shadow"
        );
        assert_eq!(
            build_settings_args(&[("text", "a b")]),
            "--text=\"a b\"",
            "含空白必须加引号，否则会被 CommandLineToArgvW 拆成两个 argv"
        );
    }

    /// `build_settings_args` 加的引号必须能被 `settings_argv` 原样还原——两者是一对，
    /// 任一侧单独改都会让含空白的值（如加词的 `--text=你 好`）在 macOS 上被拆成两个参数。
    #[test]
    fn settings_argv_round_trips_quoting() {
        use super::{build_settings_args, settings_argv};
        assert_eq!(settings_argv(Some("dict"), ""), vec!["--page=dict"]);
        assert_eq!(
            settings_argv(Some("dict"), &build_settings_args(&[("schema", "wubi86")])),
            vec!["--page=dict", "--schema=wubi86"]
        );
        // 含空白的值：加引号 → 切词后必须仍是**一个** argv，且引号已剥掉。
        assert_eq!(
            settings_argv(
                Some("add-word"),
                &build_settings_args(&[("text", "你 好"), ("code", "nihao")])
            ),
            vec!["--page=add-word", "--text=你 好", "--code=nihao"]
        );
        // 无页 + 无参数 → 空 argv（设置端按默认页处理）。
        assert!(settings_argv(None, "").is_empty());
        // 无页但有参数（`--dark` 这类）：参数不依附于页，须原样带上。
        assert_eq!(settings_argv(None, "--dark"), vec!["--dark"]);
    }

    /// 附加参数不依附于页：没给页也要原样带上（`--dark`/`--soft` 无页也有意义）。
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn settings_cmdline_keeps_extra_without_page() {
        use super::settings_cmdline;
        assert_eq!(settings_cmdline(None, ""), "");
        assert_eq!(settings_cmdline(Some("dict"), ""), "--page dict");
        assert_eq!(
            settings_cmdline(Some("dict"), "--schema=wubi86 --type=shadow"),
            "--page dict --schema=wubi86 --type=shadow"
        );
        assert_eq!(
            settings_cmdline(None, "--dark"),
            "--dark",
            "无页时附加参数不得被丢弃"
        );
    }
}

#[cfg(test)]
mod command_source_tests {
    //! 工具栏按钮 action → 短语格式的补全（`wrap_command_source`）。

    use super::wrap_command_source;

    /// 裸表达式补上标记。**这是本函数存在的全部理由**：不补的话
    /// `evaluate_phrase` 把它当字面文本，一个动作都不跑且不报错。
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

    /// 含中文路径与双反斜杠的真实配置（用户 2026-08-26 报的那条）原样进包装，
    /// 转义留给 cmdbar 的 lexer 处理——包装层**不得**碰字符串内容。
    #[test]
    fn windows_path_with_escapes_passes_through_verbatim() {
        let action = r#"proc.run("D:\\Download\\知符\\知符.exe")"#;
        let got = wrap_command_source(action);
        assert!(got.contains(r#"D:\\Download\\知符\\知符.exe"#), "{got}");
        assert_eq!(got, format!(r#"$CC("", {action})"#));
    }
}

#[cfg(test)]
mod toolbar_push_dedup_tests {
    use crate::coordinator::{Coordinator, ToolbarPush};
    use wind_ui_types::ToolbarState;

    fn state(label: &str) -> ToolbarPush {
        ToolbarPush::Shown(Box::new(ToolbarState {
            chinese_mode: true,
            icon_label: label.to_string(),
            caps_lock: false,
            full_width: false,
            chinese_punct: true,
            s2t_enabled: false,
            s2t_shown: false,
            soft_keyboard_on: false,
            input_blocked: false,
        }))
    }

    /// 去重的两个方向都要成立：挡住重复、放行变化。
    ///
    /// 只测「挡住重复」会让一个恒返回 false 的实现也绿——那种缺陷的表现是工具栏
    /// 彻底不更新，比重复推送严重得多。
    #[test]
    fn dedups_repeats_but_lets_changes_through() {
        let c = Coordinator::new_headless(wind_config::Config::default(), None);
        // 构造过程本身会推一次工具栏，缓存已非空——先归零，否则测的是构造顺序而非去重。
        c.reset_toolbar_push_dedup();

        assert!(c.take_toolbar_push_if_changed(state("五")), "首次必须下发");
        assert!(
            !c.take_toolbar_push_if_changed(state("五")),
            "内容相同应被挡下——焦点抖动时这条会被连推数次"
        );
        assert!(
            c.take_toolbar_push_if_changed(state("英")),
            "label 变了必须下发"
        );
    }

    /// ★ `Hidden` 与 `Shown` 必须是两个可区分的值。
    ///
    /// 若只缓存 `Option<ToolbarState>`（用 None 表示隐藏），Hide→Show→Hide 里的第二个
    /// Hide 会和「上次是 Show」比出「不同」……但反过来 Show→Hide→Show 的第二个 Show
    /// 又会因为中间那次 Hide 把缓存清成 None 而恒被判成变化。真正致命的是前者的对偶：
    /// 用 None 兼表「没推过」和「推过 Hide」，首帧的 Hide 会被误判成重复而跳过，
    /// 工具栏就再也藏不掉。这里把两种状态的交替钉死。
    #[test]
    fn hidden_and_shown_are_distinguishable() {
        let c = Coordinator::new_headless(wind_config::Config::default(), None);
        c.reset_toolbar_push_dedup(); // 同上：构造已推过一次

        assert!(
            c.take_toolbar_push_if_changed(ToolbarPush::Hidden),
            "首帧 Hide 必须下发"
        );
        assert!(
            !c.take_toolbar_push_if_changed(ToolbarPush::Hidden),
            "重复 Hide 挡下"
        );
        assert!(
            c.take_toolbar_push_if_changed(state("五")),
            "Hide→Show 必须下发"
        );
        assert!(
            c.take_toolbar_push_if_changed(ToolbarPush::Hidden),
            "Show→Hide 必须下发，否则工具栏藏不掉"
        );
    }

    /// 配置热重载后必须重推：热重载可能改变工具栏的显隐策略（`ui.toolbar.visible`、
    /// 全屏策略），而那些量**不在** `ToolbarState` 里——光比内容会把该重推的那一次
    /// 判成「没变」。同 `last_status_text` 在 reload 里被清空的理由。
    #[test]
    fn reset_forces_next_push() {
        let c = Coordinator::new_headless(wind_config::Config::default(), None);
        c.reset_toolbar_push_dedup();
        assert!(c.take_toolbar_push_if_changed(state("五")));
        assert!(!c.take_toolbar_push_if_changed(state("五")));
        c.reset_toolbar_push_dedup();
        assert!(
            c.take_toolbar_push_if_changed(state("五")),
            "reset 之后同样的内容也必须下发一次"
        );
    }
}

impl Coordinator {
    /// 主菜单里的「软键盘」项：本身是开关，子菜单直接选面。
    ///
    /// 面多于一个时才给子菜单——只有一面的话，子菜单里孤零零一项，点它和点父项
    /// 效果一样，纯属多一层。
    pub(crate) fn soft_keyboard_menu_item(&self) -> wind_ui_types::MenuItemSpec {
        use wind_ui_types::MenuItemSpec as M;
        let on = self.softkeyboard_is_open();
        let pages = self.softkeyboard.pages();
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
    /// 抽成独立函数供两处用：主菜单的「软键盘」子菜单、右键软键盘格的快捷菜单。
    /// ⚠️ 只有一面时这份列表**仍然有意义**（开关那一项），故这里不做 `pages.len() < 2`
    /// 的退化——那条判断属于「主菜单该不该给子菜单」，是调用方的问题。
    pub(crate) fn soft_keyboard_menu_children(&self) -> Vec<wind_ui_types::MenuItemSpec> {
        use wind_ui_types::MenuItemSpec as M;
        let on = self.softkeyboard_is_open();
        let pages = self.softkeyboard.pages();
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
                // 勾选当前面，但面板关着时不勾——那会让人以为它开着。
                on && i == cur,
            ));
        }
        children
    }

    /// 输入方案的「英文 + 各方案单选」项列表（主菜单的「输入方案」子菜单内容）。
    ///
    /// `chinese` 由调用方传入而不是这里现读：主菜单构建时已在一次加锁里把几个状态
    /// 一并取出，重读一次是第二次加锁，且两次之间状态可能变——勾选态与同一菜单里
    /// 别的项就会互相矛盾。
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

    /// 右键工具栏某一格时给的**精简快捷菜单**；这一格没有定制则返回 `None`。
    ///
    /// # 为什么按格分而不是一律给主菜单
    ///
    /// 主菜单是完整的功能面，而右键一个具体的格，意图几乎总是「就在这一格管的事情里
    /// 换一个」。把方案切换从「右键 → 输入方案 → 展开子菜单 → 点」压成「右键 → 点」，
    /// 省的正是最高频那条路径上的两步。
    ///
    /// # 各格给什么
    ///
    /// - **中英格**：整份方案列表（含「英文」），即主菜单「输入方案」子菜单的内容。
    /// - **软键盘格**：开关 + 各面单选。
    /// - **标点格 / 全半角格**：共用一份「输出形态」——中文标点、全角、简入繁出。
    ///   三者是同一类「打出来长什么样」的量，分给两个格各做一份反而要用户记住
    ///   哪个格管哪个；一次看全三个开关，右键谁都对。
    ///
    /// ⛔ **齿轮格 / 自定义按钮格 / 拖动柄不在此列**，返回 `None` 回落完整主菜单：
    /// 隐藏了齿轮之后，右键工具栏是主菜单**仅剩的鼠标入口**（`toolbar-customization.md`
    /// §2.2 判据③），这条回落断了就等于让用户可以把自己锁在外面。
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
            // 简繁格也走这一份：它本来就是那三项之一，右键给同一张表最省记忆。
            ToolbarAction::TogglePunct | ToolbarAction::ToggleWidth | ToolbarAction::ToggleS2t => {
                let (punct, full, s2t) = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    (s.chinese_punct, s.full_width, s.s2t_enabled)
                };
                // 文案、勾选态**与顺序**都跟主菜单里那三项一致（见 `build_main_menu_items`
                // 的 items 开头）：同一组开关在两处排得不一样，用户每次都要重新找。
                Some(vec![
                    M::leaf("全角", cmd(MenuCmd::ToggleWidth), true, full),
                    M::leaf("中文标点", cmd(MenuCmd::TogglePunct), true, punct),
                    M::leaf("简入繁出", cmd(MenuCmd::ToggleS2t), true, s2t),
                ])
            }
            ToolbarAction::OpenSettings | ToolbarAction::Custom(_) => None,
        }
        .map(|mut items| {
            // 每份分格菜单末尾都挂一条回主菜单的路。
            //
            // ⛔ 不可省：隐藏齿轮后右键工具栏是主菜单**仅剩的鼠标入口**（§2.2 判据③），
            // 而分格右键把功能格的右键让给了精简菜单，只剩 12dp 的拖动柄还通向主菜单
            // ——那是个要瞄准的目标。有了这一条，判据③就不再依赖那 12dp。
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
        // 空列表同样回落：一个弹出来什么都没有的菜单比不弹更让人以为坏了。
        // （软键盘一面都没有时 `soft_keyboard_menu_children` 只剩个禁用的开关项，
        //  不为空，走的仍是定制那条。）
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
