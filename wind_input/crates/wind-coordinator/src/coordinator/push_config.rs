//! push 通道的配置/状态推送：activation status、各配置帧、状态更新。
//!（coordinator 子模块，自 coordinator.rs 平移，纯搬运。）

use super::*;

impl Coordinator {
    /// `client_token` = 触发本次 activation 的客户端 token（高 32 位 = PID，
    /// BinaryProtocol.h PushTokenHandshake 约定）。hostRenderAvail 位**必须**按
    /// 事件源 PID 查白名单（对齐 Go PushActivationStatusToActiveClient(status, processID)）——
    /// 不能用全局焦点槽：开始菜单弹出会连带激活 StartMenuExperienceHost 等兄弟进程，
    /// 其激活事件若污染全局槽，推给 SearchHost 的 avail 位会错置 0，触发 DLL
    /// 「flag missing after reconnect」销毁重建循环（真机踩坑）。
    pub(super) fn push_activation_status(&self, client_token: u64) {
        // ★ 这里同样要发布图标，理由与 push_state_update 不同、且更要命：**焦点切入正是
        // 中英态最可能变化的时刻**——handle_focus_gained 会按 compat.toml 规则与 per-app
        // 记忆重算模式（apply_initial_mode），「某应用默认英文」就是在那里生效的。而这条
        // 路径此前不发布图标，于是服务端状态、推给 DLL 的 status、DLL 本地的 _bChineseMode
        // 全都正确变成了「英」，唯独 SHM 里还是上一个应用留下的「中」；GetIcon 在非本地态
        // 时无条件信任 SHM，本地 label 根本不参与选择，结果就是**图标恒落后真实状态一步**
        // （用户表现：切进配了默认英文的应用显示「中」，切换一次状态仍显示「中」）。
        //
        // SHM 的变体表只按 (尺寸, 明暗) 索引、不含状态，全系统共用唯一一张当前图，故整套
        // 设计依赖一条不变量：**SHM 内容 ≡ 当前前台宿主的状态**。任何改变前台状态、或改变
        // 前台宿主是谁的路径，都必须重发一次——activation 恰好是后一种。
        //
        // 补一条让本修复得以成立的前提：focus_gained 的**同步**回传（CMD_MODE_PUSH）虽然
        // 也带权威模式，但 DLL 侧只把它 InterlockedExchange 进 `CTextService::_bChineseMode`，
        // 不碰语言栏按钮——`CLangBarItemButton` 有**自己那一份**同名字段，只由本推送经
        // `_SyncStateFromResponse` 更新。两份分开是关键：若同步段就把按钮那份改掉，
        // `UpdateFullStatus` 的 needUpdate 去重会判定"状态没变"而不发 OnUpdate，本推送
        // 便再也触发不了 GetIcon，图标将永远停在旧图——发布得再及时也没用。
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
            &s.key_down_hotkeys,
            &s.key_up_hotkeys,
            &s.icon_label,
        );
        // 定向投递给事件源客户端（精确 token 匹配）。push_to_active 实为广播——广播会把
        // 按别的进程计算的 hostRenderAvail 位污染给无关客户端（真机踩坑：开始菜单弹出时
        // StartMenuExperienceHost 等兄弟实例的激活推送被 SearchHost 收到，avail=0 触发
        // Band 窗口销毁重建循环）。事件源无 push 连接时丢弃，绝不兜底转发。
        if client_token != 0 {
            if !self.push_server.push_to_token(client_token, &encoded) {
                debug!("activation push: 事件源 token 无 push 连接，丢弃（防污染不广播）");
            }
        } else {
            // 无 token 的旧路径（不应出现于当前 DLL）：保持原广播行为
            self.push_server.push_to_active(&encoded);
        }
        // tooltip 走**广播**而非跟着上面定向发：它不含 per-pid 的量，广播安全，且非
        // 事件源的宿主同样需要最新文案（悬停到别的窗口上时不该看到陈旧的）。内部去重。
        self.push_langbar_tooltip(0);
    }

    /// push 客户端完成 token 握手后的补推握手（仅 Windows；由 main.rs 注册到 PushServer）。
    /// 场景：服务重启后，白名单受限宿主（SearchHost 等 locked/transient DocMgr）重连时
    /// 既不发 focus_gained（被 DLL OnSetFocus 跳过）也不重发 IME_ACTIVATED——没有任何
    /// activation push 会到达，DLL 的 host 窗口挂着死 SHM 永不重新 setup（真机踩坑：
    /// 服务重启后概率性停留普通渲染）。此处对白名单 pid 定向补推一帧 activation status
    /// （avail=1），触发 C++ ApplyActivationStatusResponse → _EnsureHostRenderSetup
    /// （forceRefresh）→ 重新握手 setup。非白名单进程不推，零影响。
    #[cfg(windows)]
    pub fn on_push_client_connected(&self, client_token: u64) {
        let pid = (client_token >> 32) as u32;
        if pid == 0 {
            return;
        }

        // 推送英文自动配对配置到新连接的客户端（不受 host-render 白名单限制，
        // 所有 TSF 实例都需要收到此配置才能在英文模式下正确处理标点配对）。
        self.push_english_pair_config(client_token);
        self.push_jump_out_keys_config(client_token); // 配对跳出键（英文模式跳出 + 中文转发放行）
        self.push_password_suppress_config(client_token); // 密码框抑制策略（DLL 本地吃键门控）
        self.push_custom_en_punct_config(client_token); // 英半列自定义标点：DLL 据此吃键转发
        self.push_pair_state_ttl_config(client_token); // 配对状态时效（DLL 侧闸门据此判陈旧）
        // 诊断采集开关：DLL 每次重连都从默认值（关）起步，握手不推则 HUD 开着也收不到
        // 新连接宿主的快照——而最需要它的 SearchHost 恰恰是最常重连的那类。
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

    /// 指定 PID 的进程是否启用符号自动配对（per-app 规则，未配则跟随全局）。
    ///
    /// ⚠ **按 PID 直查规则表，绝不走 `active_compat` 焦点槽**：本函数的调用方是推送路径，
    /// 目标客户端未必是当前焦点进程（新客户端握手、配置变更广播都会推给后台进程）。
    /// 拿焦点槽的值会把焦点应用的规则套到别人头上——同 `host_render` 的既有纪律。
    pub(super) fn auto_pair_allowed_for_pid(&self, pid: u32) -> bool {
        if pid == 0 {
            return true;
        }
        let name = {
            let cached = self
                .pid_names
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&pid)
                .cloned();
            cached.unwrap_or_else(|| process_name(pid))
        };
        if name.is_empty() {
            return true;
        }
        self.app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_rule(&name)
            .and_then(|r| r.auto_pair)
            .unwrap_or(true)
    }

    /// 推送英文自动配对配置到指定客户端（或逐个推给所有活跃客户端）。
    ///
    /// 这是 per-app 自动配对开关的**第三条**消费通路：纯英文模式的配对完全由 C++ 侧
    /// `_englishPairEngine` 处理，那些标点键根本到不了协调器，只关另两条的话「切到英文
    /// 模式又配上了」。故 enabled 必须按**目标进程**现算，不能全局广播同一个值。
    pub fn push_english_pair_config(&self, client_token: u64) {
        let rt = self.rt();
        let make = |token: u64| {
            let pid = (token >> 32) as u32;
            let enabled = rt.config.input.auto_pair.english && self.auto_pair_allowed_for_pid(pid);
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

    /// 下发配对状态时效给 DLL。吃键闸门（`_pairPendingDepth`）在 DLL 侧，它必须能本地判定
    /// 状态是否陈旧——只有协调器过期而 DLL 照吃跳出键的话，协调器回 PassThrough 已太晚
    /// （「吃了再吐」丢键）。故 TTL 以 DLL 侧判据为准，此处只推阈值。
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

    /// 下发密码框抑制策略开关给 DLL。DLL 据此 + 自身持有的 InputScope 掩码在
    /// `OnTestKeyDown` 本地判定是否放行；判据两侧必须一致（见 `apply_input_diag` 与
    /// C++ `IsPasswordSuppressActive`），漂移即「吃了再吐」丢键。
    /// 开关是会话级运行时态（右键菜单「高级」可切），故握手时与每次切换后都要推。
    pub fn push_password_suppress_config(&self, client_token: u64) {
        let enabled = self
            .password_suppress_enabled
            .load(std::sync::atomic::Ordering::Relaxed);
        let value = wind_ipc::codec::encode_password_suppress_value(enabled);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_PASSWORD_SUPPRESS,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// 下发「英文模式下 DLL 需吃键转发」的源字符集合给 DLL。两个来源合成一份推送：
    ///   - 配了**英半列自定义**的键（`wind_punct::custom_english_punct_chars`）；
    ///   - 开了 `symbol.english_mode` 时的**英文智能符号参与集**（`english_smart_source_chars`）。
    ///
    /// 英文模式（非全角）下 DLL 默认直接透传标点键、引擎收不到，上面两件事因此都无从发生；
    /// DLL 据此集合精确吃下这些键并转发（集合为空 = 完全保持历史行为）。**吃键集必须 ⊆ 出字集**：
    /// 出字方 `handle_english_custom_punct` 与本推送共用 `rt().custom_en_punct_chars` 作判据，
    /// 同源即不会漂移；两侧一旦不一致就是「吃了再吐」丢键（Chrome/Electron 不回退合成 WM_CHAR）。
    /// 集合内没配英半自定义的键会出原样 ASCII（与透传等价），故并入是安全的。
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
    #[cfg(target_os = "macos")]
    pub(crate) fn push_cmdbar_key_frame(&self, encoded: &[u8]) {
        self.push_server.push_commit_to_active(encoded);
    }

    /// macOS 的 open/proc.run/设置均改为进程内执行或 CmdOpenSettings，不再经此 IPC，故仅非 macOS。
    ///
    /// `dir` = 被启动进程的工作目录（空串 = 不指定，由 TSF 侧沿用调用进程当前目录）；
    /// `verb` / `show` = ShellExecute 的动词与初始窗口状态（空串 = open / normal）。
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn push_shell_exec(
        &self,
        target: &str,
        params: &str,
        dir: &str,
        verb: &str,
        show: &str,
    ) {
        let encoded = wind_ipc::codec::encode_shell_exec(target, params, dir, verb, show);
        // 带副作用操作（启动/激活外部程序）只投给活跃（前台）客户端，与 push_commit 语义一致。
        // 若广播全部客户端，多个后台 TSF 进程会竞相 ShellExecuteW，非前台进程启动的 wind_setting
        // 第二实例无前台权限，其 SetForegroundWindow 失败，导致窗口有较大概率停在后台。
        self.push_server.push_commit_to_active(&encoded);
    }

    /// 让语言栏重取一次图标（无载荷，见 [`CMD_REFRESH_ICON`]）。
    ///
    /// **只投前台宿主，token 为 0 时干脆不投。** 任务栏的语言指示器只显示前台窗口的输入法
    /// 状态，别的宿主收到也只是白重绘一次；而本命令的调用频率可以很高（演示动画每帧一次），
    /// 广播出去就是几十个进程 × 每秒十几次的无谓唤醒。丢掉一次刷新的代价则很小——图标晚
    /// 一步更新，且下一次状态推送或焦点切换必然把它带上。
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

    /// 让宿主收掉当前挂着的那个组合。**无按键上下文**的收口出口。
    ///
    /// 唯一调用方是联想的自动隐藏超时（`handle_assoc::fire_assoc_hide`）：它跑在定时器
    /// 线程上，没有待应答的按键可以搭载收口动作，占位组合会留在宿主里成为孤儿。
    ///
    /// ★ 走的是**既有的** `CMD_CLEAR_COMPOSITION`(0x0103)——它是**双用途**的：既是
    /// `KeyAction::ClearComposition` 的按键应答，也能经 push 管道主动推给 DLL
    /// （`IPCClient.cpp` 的 AsyncReader 有对应分支 → `PostClearComposition` →
    /// TSF 线程上 `EndComposition` + `ResetComposingState`）。`push_switch_commit` 与
    /// `restart_service` 早就在这么用。**别为此另造一条 `*_PUSH` 命令**：按命令号后缀
    /// 找收口通道会找不到它，那正是本轮差点多写一条协议 + 六处 C++ 的由来。
    ///
    /// ⚠️ 这条**不保证成功**：宿主可能拒绝非按键上下文的 edit session
    /// （`EndComposition` 用 `TF_ES_ASYNCDONTCARE`），push 管道也可能没连上。按键侧的
    /// 改判（`adopt_orphaned_placeholder`）因此仍是必需的兜底，两者不是二选一——
    /// 本条覆盖「用户一个键都不按、直接点鼠标或切窗」，那一半按键侧永远够不着。
    pub(crate) fn push_end_composition(&self) {
        let encoded = wind_ipc::codec::encode_clear_composition();
        // `push_commit_to_active` 而非 `push_to_active`：后者名字有误导，实为**广播**。
        // 组合只可能在前台那个宿主里，广播给别的进程要么是空操作、要么去收一个不该它收的组合。
        self.push_server.push_commit_to_active(&encoded);
    }

    /// 语言栏按钮的悬停提示文本。**文案与选择逻辑的唯一产地**（DLL 只负责原样返回）。
    ///
    /// 收归的理由与 `InputBlock` 同一条：DLL 本地只有中英态与 CapsLock 两个量，判不出
    /// 「密码框」「输入法被系统禁用」这些成因——图标只能表达「不可用」，说清是哪一种
    /// 全靠 tooltip，而成因在服务端。密码框那一档正是本轮从 DLL 删掉后又在这里补回来的。
    ///
    /// ⚠ `effective_input_block()` 必须在取 state 锁**之前**调：它内部要取 state 与 gate
    /// 两把锁，`std::sync::Mutex` 不可重入。同类事故在 notify_toolbar 上真机卡死过一次。
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
            // NoEditContext 不进这里：它已不再让图标显「英」（是日常状态，见 shows_english），
            // tooltip 自然也不该提，否则悬停时说的与看到的对不上。
            _ if chinese && !caps => format!("{NAME} - 中文模式"),
            _ if chinese => format!("{NAME} - 英文大写 (中文模式, Caps Lock)"),
            _ if caps => format!("{NAME} - 英文模式 (Caps Lock 开)"),
            _ => format!("{NAME} - 英文模式 (Caps Lock 关)"),
        }
    }

    /// 下发语言栏悬停提示。
    ///
    /// `client_token != 0`：握手场景，**定向且强推**（绕过去重）。新连接的 DLL 手里没有
    /// 任何文本，若被全局去重挡掉就会一直显示本地回落值——`push_connect_fix` 与
    /// `diag_snapshot` 都栽过这个形状。
    ///
    /// `client_token == 0`：状态变化场景，**广播且去重**。广播是安全的：tooltip 不含
    /// 任何 per-pid 的量（这正是 activation status 不能广播的原因——那里的
    /// `hostRenderAvail` 是按事件源 pid 算的）。不广播的话，非事件源的宿主会一直悬停
    /// 出陈旧文案，与 compartment 陈旧是同一个形状。
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

    pub(crate) fn push_state_update(&self) {
        // 推的是标点态等状态位，先让方案级覆盖落地，否则切方案后工具栏要等下一次按键才更新。
        self.sync_schema_scope_locked();
        // 图标位图与状态推送同源同时机，且**发布必须先于推送**——顺序的理由与保证方式
        // 见 status_with_icon_published。
        let s = self.status_with_icon_published();
        let encoded = wind_ipc::codec::encode_state_push(
            s.chinese_mode,
            s.full_width,
            s.chinese_punct,
            s.toolbar_visible,
            s.caps_lock,
            s.soft_keyboard,
            s.soft_keyboard_keys,
            &s.icon_label,
        );
        self.push_server.push_to_active(&encoded);
        // 状态变化同样可能改变 tooltip（切中英、CapsLock）。内部去重，绝大多数
        // 状态推送（全半角、标点、方案切换）不会真的发出去。
        self.push_langbar_tooltip(0);
    }
}
