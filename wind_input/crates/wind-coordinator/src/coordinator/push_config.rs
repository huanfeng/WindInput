//! push 通道的配置/状态推送：activation status、各配置帧、状态更新。
//!（coordinator 子模块。）

use super::*;

impl Coordinator {
    /// `client_token` = 触发本次 activation 的客户端 token（高 32 位 = PID，
    /// BinaryProtocol.h PushTokenHandshake 约定）。hostRenderAvail 位必须按事件源 PID 查白名单，
    /// 不能用全局焦点槽：开始菜单弹出会连带激活 StartMenuExperienceHost 等兄弟进程，污染后推给
    /// SearchHost 的 avail 位错置 0，触发 DLL「flag missing after reconnect」销毁重建循环。
    pub(super) fn push_activation_status(&self, client_token: u64) {
        // 这里也要发布图标：SHM 只有一张当前图，不变量是「SHM 内容 ≡ 当前前台宿主的状态」，
        // 改变前台宿主的路径（activation）必须重发，否则图标恒落后一步。DLL 侧按钮状态只由本推送
        // 更新，这是能触发 GetIcon 的前提。见 docs/design/langbar-icon-shared-render.md 的
        // 「发布时序：两条不变量」。
        let s = self.status_with_icon_published();
        debug!(
            "push_activation_status: chinese={} key_down={:?} key_up={:?}",
            s.chinese_mode, s.key_down_hotkeys, s.key_up_hotkeys
        );
        #[cfg(windows)]
        let host_render_avail = {
            let pid = (client_token >> 32) as u32;
            pid != 0
                && self
                    .host_render()
                    .map(|m| m.is_process_whitelisted(pid))
                    .unwrap_or(false)
        };
        #[cfg(not(windows))]
        let host_render_avail = {
            let _ = client_token;
            false
        };
        let encoded = wind_ipc::codec::encode_activation_status_push(
            s.chinese_mode,
            s.full_width,
            s.chinese_punct,
            s.toolbar_visible,
            s.caps_lock,
            host_render_avail,
            s.soft_keyboard,
            s.soft_keyboard_keys,
            s.hotkey_session,
            &s.key_down_hotkeys,
            &s.key_up_hotkeys,
            &s.icon_label,
        );
        // 这条也发出了热键会话位，缓存跟着对齐，见 sync_hotkey_session_cache。
        self.sync_hotkey_session_cache();
        // 定向投递给事件源客户端。push_to_active 实为广播，会把按别的进程算的 hostRenderAvail
        // 位污染给无关客户端。事件源无 push 连接时丢弃，绝不兜底转发。
        if client_token != 0 {
            if !self.push_server.push_to_token(client_token, &encoded) {
                debug!("activation push: 事件源 token 无 push 连接，丢弃（防污染不广播）");
            }
        } else {
            // 无 token 的旧路径（不应出现于当前 DLL）：保持原广播行为
            self.push_server.push_to_active(&encoded);
        }
        // tooltip 不含 per-pid 的量，广播安全，非事件源宿主也需要最新文案。内部去重。
        self.push_langbar_tooltip(0);
    }

    /// push 客户端完成 token 握手后的补推（仅 Windows；由 main.rs 注册到 PushServer）。
    ///
    /// 服务重启后，白名单受限宿主（SearchHost 等 locked/transient DocMgr）重连时既不发
    /// focus_gained 也不重发 IME_ACTIVATED，DLL 的 host 窗口会挂着死 SHM。此处对白名单 pid
    /// 定向补推一帧 activation status，触发 C++ `_EnsureHostRenderSetup`（forceRefresh）重新 setup。
    /// DLL 每次重连各配置都从默认值起步，故下面的配置帧握手必推。
    #[cfg(windows)]
    pub fn on_push_client_connected(&self, client_token: u64) {
        let pid = (client_token >> 32) as u32;
        if pid == 0 {
            return;
        }

        self.push_english_pair_config(client_token);
        self.push_jump_out_keys_config(client_token); // 配对跳出键（英文模式跳出 + 中文转发放行）
        self.push_password_suppress_config(client_token); // 密码框抑制策略（DLL 本地吃键门控）
        self.push_composition_placeholder_config(client_token); // 空文本兜底占位字符
        self.push_space_before_digit_config(client_token); // 汉字后的数字吃不吃（中数间补空格）
        self.push_multiline_commit_config(client_token); // 多行上屏是否走粘贴：DLL 重连从 commit 起步
        self.push_compat_title_match_config(client_token); // 采不采窗口标题：DLL 重连从「关」起步
        self.push_extension_key_observe_config(client_token); // 扩展旁路按键：DLL 重连从「关」起步
        self.push_custom_en_punct_config(client_token); // 英半列自定义标点：DLL 据此吃键转发
        self.push_cn_passthrough_punct_config(client_token); // 中文模式该透传的标点：DLL 据此**不**吃
        self.push_en_passthrough_punct_config(client_token); // 同上，英文标点态那份（超集）
        self.push_space_custom_cols_config(client_token); // 空格行自定义列：DLL 重连从 0 起步
        self.push_pair_state_ttl_config(client_token); // 配对状态时效（DLL 侧闸门据此判陈旧）
        self.push_pair_via_comp_config(client_token); // 配对插入提交方式（组合语义 vs 直写）
        self.push_ctrl_space_toggle_config(client_token); // Ctrl+空格 切换开关：DLL 重连从「开」起步
        // 诊断采集开关：不推则 HUD 开着也收不到新连接宿主（如常重连的 SearchHost）的快照。
        self.push_diag_snapshot_config(client_token);
        self.push_langbar_tooltip(client_token); // 握手强推：新连接手里没有任何文本

        let Some(mgr) = self.host_render() else {
            return;
        };
        if !mgr.is_process_whitelisted(pid) {
            return;
        }
        tracing::info!("push 客户端注册补推 activation（host-render 白名单宿主）pid={pid}");
        self.push_activation_status(client_token);
    }

    /// 推给 `token` 那个 DLL 的符号自动配对开关（per-app 规则，未配则跟随全局）。
    ///
    /// 按目标客户端直查规则表，不走 `active_compat` 焦点槽：推送路径的目标客户端未必是焦点进程，
    /// 用焦点槽会把焦点应用的规则套到别人头上。窗口取该 token 自己最近一次获焦的那个。
    pub(super) fn auto_pair_allowed_for_token(&self, token: u64) -> bool {
        self.compat_bool_for_token(token, |r| r.auto_pair)
            .unwrap_or(true)
    }

    /// 按 PID 直查 per-app 规则里的某个 `Option<bool>` 字段；pid / 进程名未知或未配 = `None`。
    ///
    /// 与 [`Self::auto_pair_allowed_for_token`] 同一纪律：不走 `active_compat` 焦点槽。窗口取该
    /// pid 最近一次获焦的那个（[`Self::focus_window_of_pid`]）；没获焦过则不匹配窗口规则。
    fn compat_bool_for_pid(
        &self,
        pid: u32,
        pick: impl Fn(&wind_config::app_compat::AppCompatRule) -> Option<bool>,
    ) -> Option<bool> {
        if pid == 0 {
            return None;
        }
        self.with_pid_compat_rule(pid, |r| r.and_then(pick))
    }

    /// 同 [`Self::compat_bool_for_pid`]，窗口取 `token` 自己最近一次获焦的那个（推给该 token 的
    /// DLL 配置用：同进程的不同 TSF 实例可能各在不同窗口）。
    fn compat_bool_for_token(
        &self,
        token: u64,
        pick: impl Fn(&wind_config::app_compat::AppCompatRule) -> Option<bool>,
    ) -> Option<bool> {
        if (token >> 32) as u32 == 0 {
            return None;
        }
        self.with_token_compat_rule(token, |r| r.and_then(pick))
    }

    /// 指定 PID 的进程密码框是否强制英文：per-app 规则优先，未配则跟随全局
    /// `input.password_force_english`（运行时镜像 `password_suppress_enabled`）。
    ///
    /// 本函数与按 token 版本是这项判定的唯一出处：`apply_input_diag` 调本函数，
    /// [`Self::push_password_suppress_config`] 调 [`Self::password_force_english_for_token`]，
    /// 解析与回落同一份。两边各算会导致 core 抑制而 DLL 照吃、密码框丢键（不变量
    /// core.suppress ⊆ C++.suppress，见 C++ `IsPasswordSuppressActive`）。
    pub(crate) fn password_force_english_for_pid(&self, pid: u32) -> bool {
        self.compat_bool_for_pid(pid, |r| r.password_force_english)
            .unwrap_or_else(|| {
                self.password_suppress_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
    }

    /// 推给 `token` 那个 DLL 的密码框强制英文开关，口径同 [`Self::password_force_english_for_pid`]。
    pub(crate) fn password_force_english_for_token(&self, token: u64) -> bool {
        self.compat_bool_for_token(token, |r| r.password_force_english)
            .unwrap_or_else(|| {
                self.password_suppress_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
    }

    /// 推送英文自动配对配置到指定客户端（或逐个推给所有活跃客户端）。
    ///
    /// 这是 per-app 自动配对开关的第三条消费通路：纯英文模式的配对由 C++ `_englishPairEngine`
    /// 处理，标点键到不了协调器。故 enabled 须按目标进程现算，不能广播同一个值。
    pub fn push_english_pair_config(&self, client_token: u64) {
        let rt = self.rt();
        let make = |token: u64| {
            let enabled =
                rt.config.input.auto_pair.english && self.auto_pair_allowed_for_token(token);
            let value = wind_ipc::codec::encode_english_pairs_value(enabled, &rt.en_pairs);
            wind_ipc::codec::encode_sync_config(
                wind_ipc::protocol::CONFIG_KEY_ENGLISH_PAIRS,
                &value,
            )
        };
        if client_token != 0 {
            self.push_server
                .push_to_token(client_token, &make(client_token));
        } else {
            self.push_server.push_per_client(make);
        }
    }

    /// 下发配对状态时效给 DLL。吃键闸门（`_pairPendingDepth`）在 DLL 侧，须能本地判陈旧，
    /// 否则协调器已过期而 DLL 照吃跳出键会「吃了再吐」丢键。
    pub fn push_pair_state_ttl_config(&self, client_token: u64) {
        let secs = self.rt().config.input.auto_pair.state_ttl_secs;
        let value = wind_ipc::codec::encode_pair_state_ttl_value(secs);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_PAIR_STATE_TTL,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// 推给 `token` 那个 DLL 的配对插入提交方式开关（组合语义 vs 直写）。
    ///
    /// per-app 规则 `pair_via_comp` 优先，未配回落全局 `input.auto_pair.commit_via_composition`
    /// （默认直写）。按目标客户端直查，同 [`Self::auto_pair_allowed_for_token`]。宿主背景见
    /// `docs/architecture/pair-commit-host-compat-notes.md`。
    pub fn push_pair_via_comp_config(&self, client_token: u64) {
        let rt = self.rt();
        let make = |token: u64| {
            let enabled = self
                .compat_bool_for_token(token, |r| r.pair_via_comp)
                .unwrap_or(rt.config.input.auto_pair.commit_via_composition);
            let value = wind_ipc::codec::encode_pair_via_comp_value(enabled);
            wind_ipc::codec::encode_sync_config(
                wind_ipc::protocol::CONFIG_KEY_PAIR_VIA_COMP,
                &value,
            )
        };
        if client_token != 0 {
            self.push_server
                .push_to_token(client_token, &make(client_token));
        } else {
            self.push_server.push_per_client(make);
        }
    }

    /// 推给 `token` 那个 DLL 的兜底占位字符（compat 规则 `composition_placeholder`）。
    ///
    /// 按目标客户端直查，同 [`Self::auto_pair_allowed_for_token`]；出厂的 `Chrome_WidgetWin_*` /
    /// `MozillaWindowClass` 窗口规则靠该 token 的窗口命中。进程名未知 ⇒ 空格。
    pub(crate) fn composition_placeholder_for_token(
        &self,
        token: u64,
    ) -> wind_config::app_compat::PlaceholderChar {
        if (token >> 32) as u32 == 0 {
            return Default::default();
        }
        self.with_token_compat_rule(token, |r| r.and_then(|r| r.composition_placeholder))
            .unwrap_or_default()
    }

    /// 协调器自己发出的占位组合用什么字符：所有发占位的地方都从这里取，别直接写
    /// `COMPOSITION_PLACEHOLDER`。
    ///
    /// 按焦点应用的 compat 规则解析，缓存缺失 ⇒ 空格。在按键路径上，只读焦点变化 / 规则重载时
    /// 预提取好的值（[`super::ActiveLookups`]），不查表。
    pub(crate) fn composition_placeholder(&self) -> &'static str {
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lookups
            .composition_placeholder
            .as_str()
    }

    /// 联想态挂在宿主里的占位字符：`assoc_placeholder`，没配跟随 [`Self::composition_placeholder`]。
    pub(crate) fn assoc_placeholder(&self) -> &'static str {
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lookups
            .assoc_placeholder
            .as_str()
    }

    /// 联想态不挂占位组合（`assoc_placeholder = "none"`）。此时宿主里没有组合，
    /// 按键转发资格靠上屏应答的 `COMMIT_FLAG_ASSOC_SESSION` 与 `STATUS_HOTKEY_SESSION` 告知 DLL。
    pub(crate) fn assoc_placeholder_is_none(&self) -> bool {
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lookups
            .assoc_placeholder
            == wind_config::app_compat::AssocPlaceholder::None
    }

    /// 此刻是「无占位的联想会话」：联想候选活着、且当前应用配了 `none`。
    pub(crate) fn assoc_bare_session_now(&self) -> bool {
        if !self.assoc_placeholder_is_none() {
            return false;
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .assoc_active()
    }

    /// 测试用：把焦点设成 `pid`/`process`，并让该进程的 compat 规则配 `composition_placeholder = "zwsp"`
    /// （模拟出厂的 Edge 规则）。各路径「在浏览器焦点下出 ZWSP」的测试共用。
    #[cfg(test)]
    pub(crate) fn test_focus_zwsp_app(&self, pid: u32, process: &str) {
        use wind_config::app_compat::{AppCompat, AppCompatRule, PlaceholderChar};
        *self.app_compat.lock().unwrap() = AppCompat::from_rules(vec![AppCompatRule {
            process: process.into(),
            composition_placeholder: Some(PlaceholderChar::Zwsp),
            ..Default::default()
        }]);
        self.pid_names
            .lock()
            .unwrap()
            .insert(pid, process.to_lowercase());
        self.active_compat.lock().unwrap().pid = pid;
        self.refresh_active_compat_lookups();
    }

    /// 测试用：整表换掉规则表，并像 `reload_app_compat` 那样刷新按键路径预提取的值
    /// （生产里规则表只经 `reload_app_compat` 替换，直接赋值会让 `ActiveLookups` 停在旧表）。
    #[cfg(test)]
    pub(crate) fn test_set_app_compat(&self, table: wind_config::app_compat::AppCompat) {
        *self.app_compat.lock().unwrap() = table;
        self.refresh_active_compat_lookups();
    }

    /// 下发「空文本兜底占位用哪个字符」给 DLL（CONFIG_KEY_COMPOSITION_PLACEHOLDER）。
    ///
    /// DLL 在组合文本为空时自己补一个占位撑开 range，补哪个字符由这里按**各客户端 pid** 现算
    /// （模板同 [`Self::push_password_suppress_config`]）。握手、pid 校正、compat 重载时推。
    pub fn push_composition_placeholder_config(&self, client_token: u64) {
        let make = |token: u64| {
            let kind = placeholder_wire_kind(self.composition_placeholder_for_token(token));
            let value = wind_ipc::codec::encode_composition_placeholder_value(kind);
            wind_ipc::codec::encode_sync_config(
                wind_ipc::protocol::CONFIG_KEY_COMPOSITION_PLACEHOLDER,
                &value,
            )
        };
        if client_token != 0 {
            self.push_server
                .push_to_token(client_token, &make(client_token));
        } else {
            self.push_server.push_per_client(make);
        }
    }

    /// 下发「汉字后的数字要不要吃下来补空格」给 DLL（CONFIG_KEY_SPACE_BEFORE_DIGIT）。
    ///
    /// 全局 `input.spacing.cjk_digit` 且本客户端所在应用的 compat `auto_spacing` 没关。按各客户端
    /// 现算，口径与时机同 [`Self::push_composition_placeholder_config`]，配置重载时也推。
    /// 这里推慢一拍不会丢键：服务端对 DLL 交来的那种数字恒出字（见 message_handler 那条臂）。
    pub fn push_space_before_digit_config(&self, client_token: u64) {
        let make = |token: u64| {
            let on = self.rt().config.input.spacing.cjk_digit
                && self.with_token_compat_rule(token, |r| r.and_then(|r| r.auto_spacing))
                    != Some(false);
            wind_ipc::codec::encode_sync_config(
                wind_ipc::protocol::CONFIG_KEY_SPACE_BEFORE_DIGIT,
                &[on as u8],
            )
        };
        if client_token != 0 {
            self.push_server
                .push_to_token(client_token, &make(client_token));
        } else {
            self.push_server.push_per_client(make);
        }
    }

    /// 下发「含换行的上屏是否走剪贴板粘贴」给 DLL（CONFIG_KEY_MULTILINE_COMMIT，compat 规则
    /// `multiline_commit`）。按各客户端现算，口径与时机同 [`Self::push_composition_placeholder_config`]。
    pub fn push_multiline_commit_config(&self, client_token: u64) {
        let make = |token: u64| {
            let paste = (token >> 32) as u32 != 0
                && self.with_token_compat_rule(token, |r| r.and_then(|r| r.multiline_commit))
                    == Some(wind_config::app_compat::MultilineCommit::Paste);
            wind_ipc::codec::encode_sync_config(
                wind_ipc::protocol::CONFIG_KEY_MULTILINE_COMMIT,
                &wind_ipc::codec::encode_multiline_commit_value(paste),
            )
        };
        if client_token != 0 {
            self.push_server
                .push_to_token(client_token, &make(client_token));
        } else {
            self.push_server.push_per_client(make);
        }
    }

    /// 下发「有扩展订阅按键」开关给 DLL（`CONFIG_KEY_EXTENSION_KEY_OBSERVE`）：开着时 DLL 把放行给
    /// 宿主的用户按键旁路上报（见 `hook_events`）。所有客户端同一个值；`client_token = 0` 时广播。
    /// DLL 每次重连从关起步，故握手必推；扩展授权变化时由 `hook_changed` 广播。
    pub fn push_extension_key_observe_config(&self, client_token: u64) {
        let encoded = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_EXTENSION_KEY_OBSERVE,
            &wind_ipc::codec::encode_extension_key_observe_value(self.hook.wants_key()),
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &encoded);
        } else {
            self.push_server.push_per_client(|_| encoded.clone());
        }
    }

    /// 下发「本进程可能命中 compat 标题规则」开关给 DLL（`CONFIG_KEY_COMPAT_TITLE_MATCH`）：开着
    /// DLL 才在 FocusGained 里附上顶层窗口标题。用不上标题规则的进程一次标题都不取（用户数据）。
    ///
    /// 按各客户端进程现算（`AppCompat::process_may_match_title`），`client_token = 0` 时逐客户端推。
    /// DLL 每次重连从关起步，故握手必推；compat 重载后逐客户端重推。
    ///
    /// 已接受的竞态：重载与握手并发时最坏某个焦点按旧开关多取 / 少取一次标题；多取的服务端丢弃
    /// （`focus_window_of`），少取的由 DLL 在开关打开时补发焦点自愈。
    pub fn push_compat_title_match_config(&self, client_token: u64) {
        let make = |token: u64| {
            let name = self.proc_name_or_lookup((token >> 32) as u32);
            let enabled = self
                .app_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .process_may_match_title(&name);
            wind_ipc::codec::encode_sync_config(
                wind_ipc::protocol::CONFIG_KEY_COMPAT_TITLE_MATCH,
                &wind_ipc::codec::encode_compat_title_match_value(enabled),
            )
        };
        if client_token != 0 {
            self.push_server
                .push_to_token(client_token, &make(client_token));
        } else {
            self.push_server.push_per_client(make);
        }
    }

    /// 下发密码框抑制策略开关给 DLL，DLL 据此与 InputScope 掩码在 `OnTestKeyDown` 本地判定放行；
    /// 两侧判据漂移即「吃了再吐」丢键（见 C++ `IsPasswordSuppressActive`）。握手、切换与配置重载后
    /// 都要推。取值按目标进程现算，`client_token=0` 时逐客户端推。
    pub fn push_password_suppress_config(&self, client_token: u64) {
        let make = |token: u64| {
            let enabled = self.password_force_english_for_token(token);
            let value = wind_ipc::codec::encode_password_suppress_value(enabled);
            wind_ipc::codec::encode_sync_config(
                wind_ipc::protocol::CONFIG_KEY_PASSWORD_SUPPRESS,
                &value,
            )
        };
        if client_token != 0 {
            self.push_server
                .push_to_token(client_token, &make(client_token));
        } else {
            self.push_server.push_per_client(make);
        }
    }

    /// 下发「英文模式下 DLL 需吃键转发」的源字符集合给 DLL。两个来源合成一份推送：
    ///   - 配了**英半列自定义**的键（`wind_punct::custom_english_punct_chars`）；
    ///   - 开了 `symbol.english_mode` 时的**英文智能符号参与集**（`english_smart_source_chars`）。
    ///
    /// 英文模式（非全角）下 DLL 默认透传标点键，据此集合精确吃下并转发（空集 = 不吃）。
    /// 吃键集必须 ⊆ 出字集：出字方 `handle_english_custom_punct` 与本推送共用
    /// `rt().custom_en_punct_chars`，不一致即「吃了再吐」丢键。集合内没配英半自定义的键出原样
    /// ASCII，与透传等价。
    pub fn push_custom_en_punct_config(&self, client_token: u64) {
        // BTreeSet 迭代天然有序 → 推送字节可复现（与 jump_out_keys 排序同理）。
        let chars: Vec<char> = self.rt().custom_en_punct_chars.iter().copied().collect();
        let value = wind_ipc::codec::encode_custom_en_punct_value(&chars);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_CUSTOM_EN_PUNCT,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// 下发「中文模式下该让 DLL 透传不吃」的标点集合。与
    /// [`Self::push_custom_en_punct_config`] 方向相反：那个是「本该透传的请多吃几个」，
    /// 这个是「本该吃的请别吃」。
    ///
    /// 吃了再吐原样 ASCII 在非 TSF-aware 宿主上有害：CUAS 送达的字符码会被当虚拟键码解释
    /// （Tkinter 上 `#`→VK_END），字不上屏反倒移了光标。见
    /// `docs/architecture/smart-symbol-compat-notes.md`。
    ///
    /// 判据与出字侧同源（`wind_punct::chinese_passthrough_punct_chars`）。DLL 侧再叠「无输入会话」
    /// 「非全角」两道只有它知道的动态闸门。
    pub fn push_cn_passthrough_punct_config(&self, client_token: u64) {
        let chars: Vec<char> = self
            .rt()
            .cn_passthrough_punct_chars
            .iter()
            .copied()
            .collect();
        // 与 CUSTOM_EN_PUNCT 同格式（count(u8) + UTF-16LE），复用同一个编码器。
        let value = wind_ipc::codec::encode_custom_en_punct_value(&chars);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_CN_PASSTHROUGH_PUNCT,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// 下发空格行自定义映射的列掩码（`CONFIG_KEY_SPACE_CUSTOM_COLS`）。中文模式空缓冲时
    /// DLL 据此吃下空格转发；出字侧见 VK_SPACE 分支的 `idle_space_eaten`。
    pub fn push_space_custom_cols_config(&self, client_token: u64) {
        let value = wind_ipc::codec::encode_space_custom_cols_value(self.rt().space_custom_cols);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_SPACE_CUSTOM_COLS,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// 方案 override（`schema.saveConfig` / `schema.resetConfig`）落盘后，重建 `ConfigBundle`
    /// 里的跨方案派生集合并下发 DLL。
    ///
    /// `SchemaKeyUnion` 只在 bundle 构建时算，而只改方案设置不会触发 `reload_user_config`；不重建
    /// 则 DLL 仍按旧透传集合放行方案刚配的键，方案表那一格静默失效。
    ///
    /// 推送集合与 `reload_user_config` 里配置派生的那几条逐条对应；热键表经 activation 定向推给
    /// 活跃客户端（广播会污染 hostRenderAvail 位，见 [`Self::push_activation_status`]）。
    pub(crate) fn refresh_schema_derived_config(&self) {
        self.refresh_config_in_memory(|_| {});
        // 方案级注释模板可能变了（`${chaizi}` 加上 / 去掉）：按需数据跟着收放。
        self.apply_data_needs();
        // 方案设置可能改了 `[engine.aux_code].files`（换了引用的码表方案），新来源的反查索引
        // 要提前在后台建好，否则下一次按辅助码键静默不进。
        self.warm_aux_code_sources();
        self.push_custom_en_punct_config(0);
        self.push_cn_passthrough_punct_config(0);
        self.push_en_passthrough_punct_config(0);
        self.push_space_custom_cols_config(0);
        let active_token = self.push_server.active_token();
        if active_token != 0 {
            self.push_activation_status(active_token);
        }
    }

    /// 下发**英文标点态**那份透传集合（[`Self::push_cn_passthrough_punct_config`] 的姊妹）。
    ///
    /// 两份都要推：标点态是运行时状态，DLL 按当下态二选一。
    pub fn push_en_passthrough_punct_config(&self, client_token: u64) {
        let chars: Vec<char> = self
            .rt()
            .en_passthrough_punct_chars
            .iter()
            .copied()
            .collect();
        let value = wind_ipc::codec::encode_custom_en_punct_value(&chars);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_EN_PASSTHROUGH_PUNCT,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// 推送配对跳出键（VK 码集合）到 TSF 客户端。TSF 英文模式配对直接据此跳出；
    /// 中文模式据此在「有待跳出配对」时放行转发（真正裁决仍在协调器）。
    pub fn push_jump_out_keys_config(&self, client_token: u64) {
        let rt = self.rt();
        // HashSet 迭代序不稳定，排序保证推送字节可复现。
        let mut vks: Vec<u32> = rt.jump_out_keys.iter().copied().collect();
        vks.sort_unstable();
        let value = wind_ipc::codec::encode_jump_out_keys_value(rt.jump_out_on_right_symbol, &vks);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_JUMP_OUT_KEYS,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// macOS：把命令直通车按键合成帧（CmdKeyTap/Seq/Hold/Release/Type）推给活跃 `.app`。
    /// 服务进程（LaunchAgent）无辅助功能授权无法 post CGEvent，改由 `.app` 侧 KeySynthesizer
    /// 合成（`.app` 有授权）。只投活跃前台客户端，与 commit 同队列保证与 type() 上屏文本的顺序。
    #[cfg(ext_presenter)]
    pub(crate) fn push_cmdbar_key_frame(&self, encoded: &[u8]) {
        self.push_server.push_commit_to_active(encoded);
    }

    /// macOS 的 open/proc.run/设置均改为进程内执行或 CmdOpenSettings，不再经此 IPC，故仅非 macOS。
    ///
    /// `dir` = 被启动进程的工作目录（空串 = 不指定，由 TSF 侧沿用调用进程当前目录）；
    /// `verb` / `show` = ShellExecute 的动词与初始窗口状态（空串 = open / normal）。
    #[cfg(not(ext_presenter))]
    pub(crate) fn push_shell_exec(
        &self,
        target: &str,
        params: &str,
        dir: &str,
        verb: &str,
        show: &str,
    ) {
        let encoded = wind_ipc::codec::encode_shell_exec(target, params, dir, verb, show);
        // 带副作用操作只投给前台客户端：广播会让后台 TSF 进程竞相 ShellExecuteW，非前台进程
        // 启动的窗口拿不到前台权限、停在后台。
        self.push_server.push_commit_to_active(&encoded);
    }

    /// 让语言栏重取一次图标（无载荷，见 [`CMD_REFRESH_ICON`]）。
    ///
    /// 只投前台宿主，token 为 0 时不投：语言指示器只显示前台窗口的状态，而本命令可能很频繁
    /// （演示动画每帧一次）。丢一次刷新的代价只是图标晚一步更新。
    ///
    /// [`CMD_REFRESH_ICON`]: wind_ipc::protocol::CMD_REFRESH_ICON
    #[cfg(windows)]
    pub(crate) fn push_refresh_icon(&self) {
        let token = self.push_server.active_token();
        if token == 0 {
            return;
        }
        let encoded = wind_ipc::codec::encode_refresh_icon();
        self.push_server.push_to_token(token, &encoded);
    }

    /// 让宿主收掉当前挂着的组合：无按键上下文的收口出口。唯一调用方是联想的自动隐藏超时
    /// （`handle_assoc::fire_assoc_hide`），它在定时器线程上，没有按键应答可搭载。
    ///
    /// 走既有的 `CMD_CLEAR_COMPOSITION`：它既是按键应答，也能经 push 管道主动推给 DLL
    /// （`push_switch_commit` 也这么用），别另造 `*_PUSH` 命令。
    ///
    /// 不保证成功（宿主可能拒绝非按键上下文的 edit session，管道也可能没连上），按键侧的
    /// `adopt_orphaned_placeholder` 仍是必需兜底；本条覆盖用户不按键直接点鼠标或切窗的情形。
    pub(crate) fn push_end_composition(&self) {
        let encoded = wind_ipc::codec::encode_clear_composition();
        // `push_to_active` 实为广播；组合只在前台宿主里，广播会去收别的进程的组合。
        self.push_server.push_commit_to_active(&encoded);
    }

    /// 语言栏按钮的悬停提示文本，文案与选择逻辑的唯一产地（DLL 原样返回）。DLL 本地判不出
    /// 「密码框」「被系统禁用」等成因，成因在服务端。
    ///
    /// `effective_input_block()` 必须在取 state 锁之前调：它内部要取 state 与 gate 两把锁，
    /// `std::sync::Mutex` 不可重入。
    pub(crate) fn langbar_tooltip(&self) -> String {
        let block = self.effective_input_block();
        let (chinese, caps) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.chinese_mode, s.caps_lock)
        };
        const NAME: &str = "清风输入法";
        match block {
            crate::coordinator::InputBlock::KeyboardDisabled => format!("{NAME} - 已禁用"),
            crate::coordinator::InputBlock::Password => format!("{NAME} - 密码框，已切英文"),
            crate::coordinator::InputBlock::ContextDisabled => {
                format!("{NAME} - 当前输入区被应用禁用输入法，已切英文")
            }
            // NoEditContext 不让图标显「英」（见 shows_english），tooltip 也不提。
            _ if chinese && !caps => format!("{NAME} - 中文模式"),
            _ if chinese => format!("{NAME} - 英文大写 (中文模式, Caps Lock)"),
            _ if caps => format!("{NAME} - 英文模式 (Caps Lock 开)"),
            _ => format!("{NAME} - 英文模式 (Caps Lock 关)"),
        }
    }

    /// 下发语言栏悬停提示。
    ///
    /// `client_token != 0`：握手场景，定向且强推（绕过去重），新连接的 DLL 手里没有任何文本。
    ///
    /// `client_token == 0`：状态变化场景，广播且去重。tooltip 不含 per-pid 的量，广播安全；
    /// 不广播则非事件源宿主悬停出陈旧文案。
    pub(crate) fn push_langbar_tooltip(&self, client_token: u64) {
        let text = self.langbar_tooltip();
        // UTF-16LE：C++ 侧 wchar_t 即 UTF-16，照此可直接构造 wstring。
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_LANGBAR_TOOLTIP,
            &utf16,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
            return;
        }
        {
            let mut last = self
                .last_langbar_tooltip
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == text {
                return;
            }
            *last = text;
        }
        self.push_server.push_to_active(&msg);
    }

    /// 「热键模式会话」位变了才推一次状态：这一位的发送边沿。
    ///
    /// `STATUS_HOTKEY_SESSION` 的载荷是 level-triggered 的，但发送仍需要边沿：各热键模式的进出
    /// 路径只调 `notify_ui_*` / `notify_toolbar`，都不推状态。漏推会让 DLL 侧的位卡在 TRUE，
    /// 此后 Backspace/Enter/Escape 被吃下转发而协调器无会话，「吃了再吐」丢键。
    ///
    /// 比较前后值而非让各调用点置 dirty：置位点分散，还有「热键置位了却没进模式」这种没有修改点
    /// 的情形。返回这一次是否真的推了，供测试观察（headless 下没有真实 push 连接）。
    pub(crate) fn push_hotkey_session_if_changed(&self) -> bool {
        let now = self.hotkey_session_now();
        let prev = self
            .last_pushed_hotkey_session
            .swap(now, std::sync::atomic::Ordering::Relaxed);
        // 命中过 key_down 热键则无条件推：C++ 在 WM_HOTKEY 上乐观置位且不看动作，这是协调器
        // 比较法观察不到的 DLL 侧状态（如 `open_add_word_dialog`：置了位而协调器前后都是 false）。
        let forced = self
            .hotkey_session_force_push
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        if forced || prev != now {
            self.push_state_update();
            return true;
        }
        false
    }

    /// 别处已经推过一次完整状态时，把本位的缓存对齐过去（只记账，不推送）。
    ///
    /// 缓存的语义是「对端收到了什么」，每条发出这一位的路径都必须对齐它。不对齐的风险是漏推：
    /// 缓存停在 A 而 DLL 已收到 B，之后状态从 B 变回 A 会被判成「没变」，DLL 永远停在 B。
    pub(crate) fn sync_hotkey_session_cache(&self) {
        let now = self.hotkey_session_now();
        self.last_pushed_hotkey_session
            .store(now, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn push_state_update(&self) {
        // 推的是标点态等状态位，先让方案级覆盖落地，否则切方案后工具栏要等下一次按键才更新。
        self.sync_schema_scope_locked();
        // 图标发布必须先于推送，见 status_with_icon_published。
        let s = self.status_with_icon_published();
        let encoded = wind_ipc::codec::encode_state_push(
            s.chinese_mode,
            s.full_width,
            s.chinese_punct,
            s.toolbar_visible,
            s.caps_lock,
            s.soft_keyboard,
            s.soft_keyboard_keys,
            s.hotkey_session,
            &s.icon_label,
        );
        self.push_server.push_to_active(&encoded);
        // 状态变化可能改变 tooltip（切中英、CapsLock）；内部去重。
        self.push_langbar_tooltip(0);
    }
}

/// compat 规则的占位字符 → `CONFIG_KEY_COMPOSITION_PLACEHOLDER` 的下发值。
///
/// 穷举 match：给 `PlaceholderChar` 加档位时这里编译不过，逼着同步协议与 DLL
/// （`wind_tsf/include/CompositionPlaceholder.h` 的 `PlaceholderForKind`）。
fn placeholder_wire_kind(p: wind_config::app_compat::PlaceholderChar) -> u8 {
    use wind_config::app_compat::PlaceholderChar;
    use wind_ipc::protocol::{
        COMPOSITION_PLACEHOLDER_BLANK, COMPOSITION_PLACEHOLDER_SPACE, COMPOSITION_PLACEHOLDER_ZWSP,
    };
    match p {
        PlaceholderChar::Space => COMPOSITION_PLACEHOLDER_SPACE,
        PlaceholderChar::Zwsp => COMPOSITION_PLACEHOLDER_ZWSP,
        PlaceholderChar::Blank => COMPOSITION_PLACEHOLDER_BLANK,
    }
}
