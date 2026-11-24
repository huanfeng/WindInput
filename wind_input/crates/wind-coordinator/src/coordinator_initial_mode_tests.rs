//! 初始状态语义矩阵验证：激活重置 / 全局记忆 / per-app 独立（均纯内存，无词典/UI 依赖）。
use super::*;

fn coord_with(f: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    f(&mut cfg);
    Coordinator::new_headless(cfg, None)
}

/// 注入焦点进程（headless 下 OpenProcess 取不到真实进程名，手动填缓存）。
/// 夹具语义＝焦点与模式归属都已落在这个进程上（focus_gained 跑完后的稳态）。
/// `active_compat.pid` 与 `mode_scope` 都要设，只设前者 `crossed` 会读到陈旧的 `mode_scope`。
fn set_focus_proc(c: &Arc<Coordinator>, pid: u32, name: &str) {
    c.active_compat.lock().unwrap().pid = pid;
    c.pid_names.lock().unwrap().insert(pid, name.to_string());
    let none = FocusWindow::default();
    let has_rule =
        c.rule_initial_mode(name, &none).is_some() || c.rule_initial_punct(name, &none).is_some();
    *c.mode_scope.lock().unwrap() = ModeScope {
        pid,
        has_rule,
        ..Default::default()
    };
}

fn token(pid: u32) -> u64 {
    ((pid as u64) << 32) | 1
}

/// global + remember=false（默认）：状态被污染成英文后，激活时重置回配置默认（中文），
/// 全半角/标点一并重置——本 bug 的核心修复。
#[test]
fn activation_resets_to_default_when_not_remembering() {
    let c = coord_with(|cfg| {
        cfg.input.default.remember_last_state = false;
        cfg.input.default.chinese_mode = true;
        cfg.input.default.full_width = false;
        cfg.input.default.chinese_punct = true;
    });
    {
        let mut s = c.state.lock().unwrap();
        s.chinese_mode = false; // 模拟 compartment 脏事件污染
        s.full_width = true;
        s.chinese_punct = false;
    }
    c.apply_initial_mode(token(100), true);
    let s = c.state.lock().unwrap();
    assert!(s.chinese_mode);
    assert!(!s.full_width);
    assert!(s.chinese_punct);
}

/// global + remember=true：激活时保持用户最后一次主动切换的状态，不重置。
#[test]
fn activation_keeps_last_state_when_remembering() {
    let c = coord_with(|cfg| {
        cfg.input.default.remember_last_state = true;
        cfg.input.default.chinese_mode = true;
    });
    {
        let mut s = c.state.lock().unwrap();
        s.chinese_mode = false; // 用户切到英文
        s.full_width = true;
    }
    // 直接注入"最后状态"内存镜像（不调 record_last_state，避免测试写真实 state.toml）。
    *c.runtime_last.lock().unwrap() = (false, true, true);
    c.apply_initial_mode(token(100), true);
    let s = c.state.lock().unwrap();
    assert!(!s.chinese_mode, "remember=true 激活不得重置回默认");
    assert!(s.full_width);
}

/// scope=app：首见进程用配置默认；record_app_mode 写表后按进程恢复各自状态。
#[test]
fn per_app_scope_remembers_mode_per_process() {
    let c = coord_with(|cfg| {
        cfg.input.default.state_scope = "app".into();
        cfg.input.default.chinese_mode = true;
    });
    // 游戏进程：首见 → 默认中文；用户切英文 → 写表。
    set_focus_proc(&c, 100, "game.exe");
    assert!(
        c.initial_chinese_mode_for("game.exe", &FocusWindow::default()),
        "首见进程应为配置默认"
    );
    c.state.lock().unwrap().chinese_mode = false;
    c.record_app_mode(false);
    // 切到聊天进程：首见 → 默认中文。
    set_focus_proc(&c, 200, "chat.exe");
    c.apply_initial_mode(token(200), false);
    assert!(c.state.lock().unwrap().chinese_mode);
    // 切回游戏进程：恢复英文记忆。
    set_focus_proc(&c, 100, "game.exe");
    c.apply_initial_mode(token(100), false);
    assert!(!c.state.lock().unwrap().chinese_mode);
}

/// scope=app + 记忆前次状态：没手动切过的进程也要记住进入时的状态。在 B 里切英文，
/// 不得把从没切过的 A 一起带成英文。
#[test]
fn per_app_scope_records_untoggled_process_on_entry() {
    let c = coord_with(|cfg| {
        cfg.input.default.state_scope = "app".into();
        cfg.input.default.remember_last_state = true;
        cfg.input.default.chinese_mode = true;
    });
    *c.runtime_last.lock().unwrap() = (true, false, true);
    set_focus_proc(&c, 100, "notepad.exe");
    c.apply_initial_mode(token(100), false);
    assert!(c.state.lock().unwrap().chinese_mode);
    set_focus_proc(&c, 200, "wt.exe");
    c.apply_initial_mode(token(200), false);
    // 在 wt 里切英文：手动切换会同时改「前次状态」镜像与本进程的表项。
    c.state.lock().unwrap().chinese_mode = false;
    c.record_app_mode(false);
    *c.runtime_last.lock().unwrap() = (false, false, true);
    set_focus_proc(&c, 100, "notepad.exe");
    c.apply_initial_mode(token(100), false);
    assert!(
        c.state.lock().unwrap().chinese_mode,
        "记事本进入时是中文、从没切过，回来仍应是中文"
    );
}

/// scope=app：FOCUS_GAINED 同步路径（get_current_mode）命中记忆表时先切换再回传；
/// 未入缓存的进程保持现状（由重型段修正）。
#[test]
fn get_current_mode_switches_per_app_on_cache_hit() {
    let c = coord_with(|cfg| {
        cfg.input.default.state_scope = "app".into();
        cfg.input.default.chinese_mode = true;
    });
    // 同步段先于重型段的 update_active_compat 执行，此刻 active_compat.pid 仍是上一个
    // 进程，`crossed` 靠这点识别跨进程切入；故夹具只把新进程名喂进 pid_names。
    set_focus_proc(&c, 1, "other.exe");
    c.pid_names
        .lock()
        .unwrap()
        .insert(100, "game.exe".to_string());
    c.mode_states
        .lock()
        .unwrap()
        .insert("game.exe".to_string(), false);
    // 当前全局是中文，焦点到 game.exe → 同步切英文并回传。
    let (chinese, _, _) = c.get_current_mode(token(100), "", "");
    assert!(!chinese);
    assert!(!c.state.lock().unwrap().chinese_mode);
    // 未缓存的 pid（首次聚焦）：保持现状不误切。
    let (chinese, _, _) = c.get_current_mode(token(999), "", "");
    assert!(!chinese, "未知进程应回传当前状态");
}

/// global（默认作用域）：get_current_mode 不做 per-app 切换，直接回权威状态。
#[test]
fn get_current_mode_global_scope_passthrough() {
    let c = coord_with(|_| {});
    c.state.lock().unwrap().chinese_mode = false;
    let (chinese, _, _) = c.get_current_mode(token(100), "", "");
    assert!(!chinese);
}

/// 注入 compat.toml 的应用规则（纯内存，不碰文件系统）。
fn set_rule(
    c: &Arc<Coordinator>,
    process: &str,
    mode: Option<wind_config::app_compat::InitialMode>,
    punct: Option<wind_config::app_compat::InitialMode>,
) {
    use wind_config::app_compat::{AppCompat, AppCompatRule};
    *c.app_compat.lock().unwrap() = AppCompat::from_rules(vec![AppCompatRule {
        process: process.to_string(),
        initial_mode: mode,
        initial_punct: punct,
        ..Default::default()
    }]);
}

/// 应用规则压过 per-app 记忆表。
///
/// 顺序反了，Everything / Listary 这类常驻隐藏式进程的规则只在开机后第一次唤出时生效
/// （进程不退出，会话级记忆表里「首次」只有一次）。
#[test]
fn app_rule_beats_per_app_memory() {
    use wind_config::app_compat::InitialMode as IM;
    let c = coord_with(|cfg| {
        cfg.input.default.state_scope = "app".into();
        cfg.input.default.chinese_mode = true;
    });
    set_rule(&c, "everything.exe", Some(IM::English), None);
    c.mode_states
        .lock()
        .unwrap()
        .insert("everything.exe".into(), true); // 记忆表说中文
    assert!(
        !c.initial_chinese_mode_for("everything.exe", &FocusWindow::default()),
        "规则必须压过记忆表，否则对常驻进程只生效一次"
    );
    // 没有规则的进程仍旧走记忆表，既有语义不变。
    c.mode_states
        .lock()
        .unwrap()
        .insert("game.exe".into(), true);
    assert!(c.initial_chinese_mode_for("game.exe", &FocusWindow::default()));
}

/// 重算门控的完整矩阵。这是本功能唯一容易写错又最难从现象反推的地方，
/// 逐条锁死；每条注释即该组合对应的真实场景。
#[test]
fn reapply_gate_matrix() {
    // 同应用内焦点跳转（Everything 搜索框 ↔ 结果列表）：一律不重算，保住用户手切。
    assert!(!should_reapply_initial(false, true, true, true, false));
    // 跨进程、无 per_app、两边都没规则（Word → Chrome）：不动。放宽成「规则表非空」
    // 就会在这里重算，把用户在 Word 手切的英文冲成配置默认。
    assert!(!should_reapply_initial(true, false, false, false, false));
    // 进入规则应用（Word → Everything）。
    assert!(should_reapply_initial(true, false, false, true, false));
    // 离开规则应用（Everything → Word）：只看 new_has_rule 会漏掉这条，
    // 表现为 Everything 的英文残留给之后的每一个应用。
    assert!(should_reapply_initial(true, false, true, false, false));
    // per_app 既有语义不受规则影响。
    assert!(should_reapply_initial(true, true, false, false, false));

    // ── 壳过渡窗口一票否决 ──
    // 点任务栏 / Alt+Tab 切入 explorer.exe：即便配了规则、确是跨进程切入也不重算。
    // 上面每一条为真的组合都要被否掉。
    assert!(!should_reapply_initial(true, false, false, true, true));
    assert!(!should_reapply_initial(true, false, true, false, true));
    assert!(!should_reapply_initial(true, true, false, false, true));
    assert!(!should_reapply_initial(true, true, true, true, true));
}

/// 端到端：作用域外窗口的 focus_gained 不得改动中英状态，桌面窗口必须照改。
///
/// `reapply_gate_matrix` 只锁纯判据；这条锁接线——取错窗口类或没传 `window_class`，
/// 矩阵测试照样全绿。
#[test]
fn out_of_scope_window_skips_initial_mode_but_desktop_does_not() {
    use wind_config::app_compat::{
        AppCompat, AppCompatRule, InitialMode as IM, InitialModeScopeRule,
    };

    let build = || {
        let c = coord_with(|cfg| cfg.input.default.chinese_mode = true);
        *c.app_compat.lock().unwrap() = AppCompat::from_parts(
            vec![AppCompatRule {
                process: "explorer.exe".into(),
                initial_mode: Some(IM::English),
                ..Default::default()
            }],
            vec![InitialModeScopeRule {
                process: "explorer.exe".into(),
                comment: String::new(),
                disabled: false,
                classes: vec!["Progman".into()],
            }],
        );
        // 先停在别的进程上、中文态，下面才构成「跨进程切入 explorer」。
        set_focus_proc(&c, 100, "notepad.exe");
        c.state.lock().unwrap().chinese_mode = true;
        c.pid_names
            .lock()
            .unwrap()
            .insert(200, "explorer.exe".into());
        c
    };

    let focus = |class: &str| FocusData {
        x: 0,
        y: 0,
        height: 0,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: token(200),
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: 0,
        bundle_id: String::new(),
        window_class: class.into(),
        window_title: String::new(),
    };

    // 任务栏（作用域外）：规则不套用，保持切入前的中文。
    let c = build();
    c.handle_focus_gained(&focus("Shell_TrayWnd"));
    assert!(
        c.state.lock().unwrap().chinese_mode,
        "作用域外的窗口不该触发 initial_mode 重算"
    );

    // 桌面（作用域内）：规则照常生效，切成英文。用户配 explorer.exe=english 就是为了它。
    let c = build();
    c.handle_focus_gained(&focus("Progman"));
    assert!(
        !c.state.lock().unwrap().chinese_mode,
        "桌面必须照常套用 initial_mode=english"
    );

    // 拿不到窗口类：保持现状（explorer 新起 TSF 连接的头一个 focus_gained 就是这样，见
    // docs/architecture/app-compat-host-notes.md 的「初始模式作用域」）。
    let c = build();
    c.handle_focus_gained(&focus(""));
    assert!(
        c.state.lock().unwrap().chinese_mode,
        "窗口类为空 = 不知道焦点在哪，必须保持现状而不是按规则重算"
    );

    // 按生产顺序再走一遍：同步段 get_current_mode 先跑（DLL 阻塞等它），重型段
    // handle_focus_gained 后跑。初始模式有这两个落点，只测重型段会漏掉同步段没挡。
    let c = build();
    let (chinese, _, _) = c.get_current_mode(token(200), "Shell_TrayWnd", "");
    assert!(chinese, "同步段也必须跳过作用域外的窗口");
    c.handle_focus_gained(&focus("Shell_TrayWnd"));
    assert!(
        c.state.lock().unwrap().chinese_mode,
        "两个落点都跳过后，状态才真的不变"
    );

    // 对照：桌面走同一条顺序，规则必须照常生效（防过度修复）。
    let c = build();
    let (chinese, _, _) = c.get_current_mode(token(200), "Progman", "");
    assert!(!chinese, "桌面的 initial_mode=english 必须在同步段就生效");
    c.handle_focus_gained(&focus("Progman"));
    assert!(!c.state.lock().unwrap().chinese_mode);
}

/// 作用域外的窗口不得消费掉「跨进程切入」这个一次性事件。
///
/// 记事本 → 点任务栏（作用域外，跳过）→ 回到桌面：两次焦点同属 explorer 进程。若用
/// `active_compat.pid` 判 `crossed`，第二次成了同进程，桌面的 initial_mode 永不生效。
#[test]
fn out_of_scope_window_does_not_consume_the_cross_process_transition() {
    use wind_config::app_compat::{
        AppCompat, AppCompatRule, InitialMode as IM, InitialModeScopeRule,
    };

    let c = coord_with(|cfg| cfg.input.default.chinese_mode = true);
    *c.app_compat.lock().unwrap() = AppCompat::from_parts(
        vec![AppCompatRule {
            process: "explorer.exe".into(),
            initial_mode: Some(IM::English),
            ..Default::default()
        }],
        vec![InitialModeScopeRule {
            process: "explorer.exe".into(),
            comment: String::new(),
            disabled: false,
            classes: vec!["Progman".into()],
        }],
    );
    set_focus_proc(&c, 100, "notepad.exe");
    c.state.lock().unwrap().chinese_mode = true;
    c.pid_names
        .lock()
        .unwrap()
        .insert(200, "explorer.exe".into());

    let focus = |class: &str| FocusData {
        x: 0,
        y: 0,
        height: 0,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: token(200),
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: 0,
        bundle_id: String::new(),
        window_class: class.into(),
        window_title: String::new(),
    };

    // ① 点任务栏（作用域外）：跳过，状态不变。
    c.get_current_mode(token(200), "Shell_TrayWnd", "");
    c.handle_focus_gained(&focus("Shell_TrayWnd"));
    assert!(c.state.lock().unwrap().chinese_mode, "任务栏不该改模式");

    // ② 真正回到桌面：同一个 explorer pid，仍必须算作跨进程切入并套用英文。
    let (chinese, _, _) = c.get_current_mode(token(200), "Progman", "");
    assert!(
        !chinese,
        "桌面必须仍被判为跨进程切入——作用域外的窗口不能提前消费掉这次切换"
    );
    c.handle_focus_gained(&focus("Progman"));
    assert!(!c.state.lock().unwrap().chinese_mode);
}

/// 显式 `initial_punct` 压过 `follow_mode` 的推导。
/// 顺序反了的话，用户配了标点规则却恰好开着 follow_mode 时它会被静默覆盖。
#[test]
fn initial_punct_rule_beats_follow_mode() {
    use wind_config::app_compat::InitialMode as IM;
    let c = coord_with(|cfg| {
        cfg.input.punct.follow_mode = true;
        cfg.input.default.chinese_mode = true;
    });
    set_rule(&c, "everything.exe", Some(IM::English), Some(IM::Chinese));
    set_focus_proc(&c, 100, "everything.exe");
    c.apply_initial_mode(token(100), false);
    let s = c.state.lock().unwrap();
    assert!(!s.chinese_mode, "规则要求初始英文");
    assert!(
        s.chinese_punct,
        "initial_punct=chinese 必须压过 follow_mode 推出的英文标点"
    );
}

/// 同步路径：跨进程切入规则应用时当场回传英文（消除首键竞态），
/// 而同应用内的再次 focus_gained 不得把用户手切的模式拉回规则值。
#[test]
fn get_current_mode_rule_applies_only_on_cross_process_switch() {
    use wind_config::app_compat::InitialMode as IM;
    let c = coord_with(|cfg| cfg.input.default.chinese_mode = true);
    set_rule(&c, "everything.exe", Some(IM::English), None);
    // 焦点原本在别的进程（active_compat.pid=1），现在切入 everything.exe。
    set_focus_proc(&c, 1, "other.exe");
    c.pid_names
        .lock()
        .unwrap()
        .insert(100, "everything.exe".to_string());
    let (chinese, _, _) = c.get_current_mode(token(100), "", "");
    assert!(!chinese, "跨进程切入规则应用 → 同步段即回传英文");

    // 重型段已把焦点与模式归属都更新为 100；用户随后手切回中文。
    set_focus_proc(&c, 100, "everything.exe");
    c.state.lock().unwrap().chinese_mode = true;
    let (chinese, _, _) = c.get_current_mode(token(100), "", "");
    assert!(
        chinese,
        "同应用内跳转不得把手切的中文拉回规则的英文——规则是初始值不是锁定"
    );
}

/// 同步段与重型段对「初始模式」必须给出同一个答案。
///
/// 场景：桌面配 `initial_mode = "english"`，切到无规则的记事本。若同步段两表都没命中就
/// 保持桌面留下的英文，DLL 先拿到「英」、几毫秒后才被重型段改成「中」：语言指示器闪一下，
/// 首键按英文处理。断言写成「两者相等」，钉的是一致性而非具体值。
#[test]
fn sync_and_heavy_paths_agree_on_initial_mode() {
    use wind_config::app_compat::InitialMode as IM;

    let c = coord_with(|cfg| {
        cfg.input.default.chinese_mode = true;
        cfg.input.default.remember_last_state = false;
    });
    // 上一个应用：桌面，规则强制英文，且已经把全局状态带成英文。
    set_rule(&c, "explorer.exe", Some(IM::English), None);
    set_focus_proc(&c, 200, "explorer.exe");
    c.apply_initial_mode(token(200), false);
    assert!(!c.state.lock().unwrap().chinese_mode, "前提：现状是英文");

    // 切到无任何规则、无记忆的记事本。
    c.pid_names
        .lock()
        .unwrap()
        .insert(300, "notepad.exe".into());
    let (sync_chinese, _, _) = c.get_current_mode(token(300), "Notepad", "");

    // 重型段随后落地的值。
    c.handle_focus_gained(&FocusData {
        x: 0,
        y: 0,
        height: 0,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: token(300),
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: 0,
        bundle_id: String::new(),
        window_class: "Notepad".into(),
        window_title: String::new(),
    });
    let heavy_chinese = c.state.lock().unwrap().chinese_mode;

    assert_eq!(
        sync_chinese, heavy_chinese,
        "同步段回传值必须等于重型段落地值，否则 DLL 会先按前者写 compartment 再被改回"
    );
    assert!(
        sync_chinese,
        "记事本无规则应回落到配置默认（中文），而不是沿用桌面留下的英文"
    );
}

/// 三档的优先级与「未激活时不表态」。
///
/// 线程级禁用时引擎收不到键，密码框次之，最后才是「焦点不在可编辑控件里」。
/// 排错了只会让 tooltip / 变淡呈现错档，没有别的信号。
#[test]
fn input_block_priority_and_inactive_short_circuit() {
    let c = coord_with(|_| {});

    // 未激活：一律不表态。否则 has_edit_context 恒假会把图标永久钉成「英」。
    {
        let mut s = c.state.lock().unwrap();
        s.ime_active = false;
        // 用权威信号，不用 has_edit_context——后者被噪声层驱动，不是图标的判据。
        s.focus_no_edit_ctx = true;
    }
    c.password_suppress
        .store(true, std::sync::atomic::Ordering::Relaxed);
    c.last_input_diag.lock().unwrap().disabled = true;
    assert_eq!(c.input_block(), InputBlock::None, "未激活时不该表态");

    c.state.lock().unwrap().ime_active = true;
    assert_eq!(
        c.input_block(),
        InputBlock::KeyboardDisabled,
        "线程级禁用最高"
    );

    c.last_input_diag.lock().unwrap().disabled = false;
    assert_eq!(c.input_block(), InputBlock::Password, "其次密码框");

    c.password_suppress
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        c.input_block(),
        InputBlock::NoEditContext,
        "最后无编辑上下文"
    );

    c.state.lock().unwrap().focus_no_edit_ctx = false;
    assert_eq!(c.input_block(), InputBlock::None);
}

/// `pid_names` 是一次写入、永不失效的缓存，而 Windows 会复用已退出进程的 PID。
/// 连接时校正是唯一的自愈时机，否则 B 复用 A 的 pid 后会被永久当成 A，
/// 整条 per-app 链（compat 规则 + 中英记忆）一起错。
#[test]
fn pid_name_cache_is_corrected_on_reconnect() {
    let c = coord_with(|_| {});
    c.pid_names
        .lock()
        .unwrap()
        .insert(4242, "everedit.exe".into());

    // 同一个 pid 换了进程：必须以现查为准。
    c.revalidate_pid_name(4242, "WindowsTerminal.exe");
    assert_eq!(
        c.pid_names.lock().unwrap().get(&4242).map(String::as_str),
        Some("windowsterminal.exe"),
        "PID 复用后必须纠正，否则新进程会一直套用旧进程的 per-app 配置"
    );

    // 现查为空时保留缓存：macOS 的 process_name 恒为空串，名字由 `.app`
    // 随焦点事件送进来。清掉会让 compat 规则在下一次 focus_gained 之前全部失配。
    c.revalidate_pid_name(4242, "");
    assert_eq!(
        c.pid_names.lock().unwrap().get(&4242).map(String::as_str),
        Some("windowsterminal.exe"),
        "查不到名字不等于名字变了——不许把已有的清掉"
    );

    // 首次见到该 pid 时照常落缓存。
    c.revalidate_pid_name(777, "Notepad.exe");
    assert_eq!(
        c.pid_names.lock().unwrap().get(&777).map(String::as_str),
        Some("notepad.exe"),
        "缓存键与查询都按小写，与 update_active_compat 同口径"
    );
}

/// 噪声层的 `CtxLost` 不得让图标显「英」，只有权威的 `NoEditCtx` 才可以。
///
/// `has_edit_context` 会被 CtxLost 置假：用于工具栏可以（翻错由防抖吸收），用于持续可见的
/// 图标就是误报。同时钉住工具栏仍要跟着 CtxLost 隐藏，防止两者又合并。
#[test]
fn ctx_lost_is_noise_for_icon_but_still_hides_toolbar() {
    use wind_bridge::handler::FocusLostReason;

    let c = coord_with(|_| {});
    {
        let mut s = c.state.lock().unwrap();
        s.ime_active = true;
        s.has_edit_context = true;
        s.focus_no_edit_ctx = false;
    }

    // 噪声层：DocMgr 级失焦。
    c.handle_focus_lost(0, FocusLostReason::CtxLost);
    {
        let s = c.state.lock().unwrap();
        assert!(!s.has_edit_context, "工具栏仍应隐藏——这一档的既有语义不变");
        assert!(
            !s.focus_no_edit_ctx,
            "但图标不许据此表态：CtxLost 回答的是「DocMgr 走了」而非「进了不可输入的地方」"
        );
    }
    assert_eq!(
        c.input_block(),
        InputBlock::None,
        "CtxLost 之后图标必须仍显方案标签"
    );

    // 权威层：新文档确实没有可编辑上下文。
    c.handle_focus_lost(0, FocusLostReason::NoEditCtx);
    assert_eq!(
        c.input_block(),
        InputBlock::NoEditContext,
        "NoEditCtx 才是「确实打不进去」的权威信号"
    );
}

/// 三个档位里只有罕见的两档该把图标覆盖成「英」。
///
/// `NoEditContext` 是日常状态，翻图标就是图标自己在抖，依据见
/// docs/design/mode-icon-label-config.md 的「不可输入态跟随英文标签」。
/// `ctx_lost_is_noise_for_icon_but_still_hides_toolbar` 钉档位判定，这条钉呈现映射，两层都要。
#[test]
fn only_rare_blocks_show_english_on_icon() {
    assert!(!InputBlock::None.shows_english());
    assert!(
        !InputBlock::NoEditContext.shows_english(),
        "无可编辑上下文是日常状态，不配翻图标（2026-08-18 实测：VS Code 里每点一下就翻一次）"
    );
    assert!(
        InputBlock::Password.shows_english(),
        "密码框必须显英——用户要能一眼看出这里敲进去的不是中文"
    );
    assert!(
        InputBlock::KeyboardDisabled.shows_english(),
        "输入法被系统整个禁用同理"
    );
    // 变淡仍然只留给线程级禁用，不因本次改动而放宽。
    assert!(InputBlock::KeyboardDisabled.dims_icon());
    assert!(!InputBlock::Password.dims_icon());
    assert!(!InputBlock::NoEditContext.dims_icon());
}

/// 呈现档位的两个方向不对称：进入要稳够 INPUT_BLOCK_DELAY，恢复立即。
///
/// 取舍：误显英很刺眼、晚显英无感。对称迟滞会让 QQ 密码框的焦点反复把图标打得一闪一闪。
#[test]
fn effective_input_block_is_asymmetric() {
    let c = coord_with(|_| {});
    {
        let mut s = c.state.lock().unwrap();
        s.ime_active = true;
        s.focus_no_edit_ctx = false;
    }
    assert_eq!(c.effective_input_block(), InputBlock::None);

    // 进入方向：真值已变，但没稳够 ⇒ 呈现仍是旧值。
    c.state.lock().unwrap().focus_no_edit_ctx = true;
    assert_eq!(c.input_block(), InputBlock::NoEditContext, "真值立刻就变了");
    assert_eq!(
        c.effective_input_block(),
        InputBlock::None,
        "呈现要等稳定，否则一次抖动就闪一下"
    );

    // churn：中途变回去，应撤销待定，之后再变也要重新计时。
    c.state.lock().unwrap().focus_no_edit_ctx = false;
    assert_eq!(c.effective_input_block(), InputBlock::None);

    // 稳够之后落地。
    c.state.lock().unwrap().focus_no_edit_ctx = true;
    let _ = c.effective_input_block(); // 起计时
    std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
    assert_eq!(c.effective_input_block(), InputBlock::NoEditContext);

    // 恢复方向：立即，不等迟滞。
    c.state.lock().unwrap().focus_no_edit_ctx = false;
    assert_eq!(
        c.effective_input_block(),
        InputBlock::None,
        "恢复必须立即——迟滞只该拖慢「变英」，不该拖慢「变回来」"
    );
}

/// 语言栏悬停提示的六个分支。文案是 DLL 唯一的信息来源（它那边已改成原样返回），
/// 写错不会有任何编译或运行期信号——只有用户悬停时看到胡话。
#[test]
fn langbar_tooltip_covers_every_branch() {
    let c = coord_with(|_| {});
    {
        let mut s = c.state.lock().unwrap();
        s.ime_active = true;
        s.focus_no_edit_ctx = false;
    }
    let set = |chinese: bool, caps: bool| {
        let mut s = c.state.lock().unwrap();
        s.chinese_mode = chinese;
        s.caps_lock = caps;
    };

    set(true, false);
    assert_eq!(c.langbar_tooltip(), "清风输入法 - 中文模式");
    set(true, true);
    assert_eq!(
        c.langbar_tooltip(),
        "清风输入法 - 英文大写 (中文模式, Caps Lock)"
    );
    set(false, true);
    assert_eq!(c.langbar_tooltip(), "清风输入法 - 英文模式 (Caps Lock 开)");
    set(false, false);
    assert_eq!(c.langbar_tooltip(), "清风输入法 - 英文模式 (Caps Lock 关)");

    // tooltip 与图标读同一个 effective_input_block，文案也跟着迟滞，二者才不会错位。
    // 第一次调用起计时，等到期后再断言。
    c.password_suppress
        .store(true, std::sync::atomic::Ordering::Relaxed);
    set(true, false);
    assert_eq!(
        c.langbar_tooltip(),
        "清风输入法 - 中文模式",
        "迟滞期内仍说旧文案，与图标同步"
    );
    std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
    assert_eq!(c.langbar_tooltip(), "清风输入法 - 密码框，已切英文");
    c.password_suppress
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        c.langbar_tooltip(),
        "清风输入法 - 中文模式",
        "恢复方向不迟滞，立即回到常态文案"
    );

    c.last_input_diag.lock().unwrap().disabled = true;
    let _ = c.langbar_tooltip(); // 起计时
    std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
    assert_eq!(c.langbar_tooltip(), "清风输入法 - 已禁用");
    c.last_input_diag.lock().unwrap().disabled = false;
    // 先让闸门回到 None 再测下一档：从「已禁用」直接转向另一个非 None 档走的是
    // 进入方向，仍会呈现旧值——那是闸门的正确行为，不是本条要测的东西。
    assert_eq!(c.langbar_tooltip(), "清风输入法 - 中文模式");

    // NoEditContext 刻意不单独成档（图标不显「英」，tooltip 也不提）。必须等稳够再断言，
    // 迟滞期内不变证明不了这一档没被单独处理。
    c.state.lock().unwrap().focus_no_edit_ctx = true;
    set(true, false);
    std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
    assert_eq!(
        c.input_block(),
        InputBlock::NoEditContext,
        "档位确实已落到 NoEditContext"
    );
    assert_eq!(
        c.langbar_tooltip(),
        "清风输入法 - 中文模式",
        "无可编辑上下文不该改变文案——图标此时也没变"
    );
}

/// tooltip 推送要去重：状态推送远比 tooltip 变化频繁（全半角、标点、方案切换都推状态
/// 却不改文案），不去重就是每次状态变化都白发一条 IPC 给所有宿主。
#[test]
fn langbar_tooltip_push_is_deduped_but_handshake_is_forced() {
    let c = coord_with(|_| {});
    {
        let mut s = c.state.lock().unwrap();
        s.ime_active = true;
        s.chinese_mode = true;
    }
    // 首次广播：缓存为空 ⇒ 必发
    c.push_langbar_tooltip(0);
    let first = c.last_langbar_tooltip.lock().unwrap().clone();
    assert_eq!(first, "清风输入法 - 中文模式");

    // 文案没变 ⇒ 缓存不动（下游是否真的发送由 push_server 决定，这里钉的是判据）
    c.push_langbar_tooltip(0);
    assert_eq!(*c.last_langbar_tooltip.lock().unwrap(), first);

    // 文案变了 ⇒ 缓存更新
    c.state.lock().unwrap().chinese_mode = false;
    c.push_langbar_tooltip(0);
    assert_eq!(
        *c.last_langbar_tooltip.lock().unwrap(),
        "清风输入法 - 英文模式 (Caps Lock 关)"
    );

    // 握手（token != 0）绕过去重、也不写缓存：新连接手里没有任何文本，被全局去重
    // 挡掉就会一直显示本地回落值。push_connect_fix 与 diag_snapshot 都栽过这形状。
    let before = c.last_langbar_tooltip.lock().unwrap().clone();
    c.push_langbar_tooltip(token(100));
    assert_eq!(
        *c.last_langbar_tooltip.lock().unwrap(),
        before,
        "定向推送不该污染广播用的去重缓存"
    );
}

/// cancel_on_mode_switch=false（默认）：CapsLock 开着按切换键，保持翻转语义、不动 CapsLock。
/// （注入路径涉及真实 SendInput，不在单测覆盖，真机验证。）
#[test]
fn toggle_mode_keeps_caps_when_cancel_disabled() {
    let c = coord_with(|_| {});
    {
        let mut s = c.state.lock().unwrap();
        s.caps_lock = true;
        s.chinese_mode = true;
    }
    c.handle_toggle_mode();
    let s = c.state.lock().unwrap();
    assert!(s.caps_lock, "配置关不得动 CapsLock");
    assert!(!s.chinese_mode, "配置关保持原翻转语义");
}

/// 取消大写之后落到哪个模式（`input.capslock.mode_after_cancel`）。
///
/// 注入 CapsLock 走真实 SendInput，单测里取消恒失败，故判定收成纯函数单独钉。
/// 两条硬约束：出厂值 `chinese` 下 Shift 归位中文、Ctrl+空格 按它自己请求的方向；
/// `english` 两条路都进英文。
#[test]
fn mode_after_caps_cancel_matrix() {
    use super::CapsCancelSwitch::{Directed, System, Toggle};
    let f = Coordinator::mode_after_caps_cancel;
    for cur in [true, false] {
        // 出厂：Shift 归位中文（不看当前模式），Ctrl+空格 按请求。
        assert!(f("chinese", Toggle, cur), "chinese/Shift 应归位中文");
        for req in [true, false] {
            assert_eq!(
                f("chinese", System(req), cur),
                req,
                "chinese/Ctrl+空格 应按请求"
            );
            assert_eq!(
                f("toggle", System(req), cur),
                req,
                "toggle/Ctrl+空格 应按请求"
            );
            assert!(
                !f("english", System(req), cur),
                "english/Ctrl+空格 应进英文"
            );
        }
        assert!(!f("english", Toggle, cur), "english/Shift 应进英文");
        assert_eq!(f("toggle", Toggle, cur), !cur, "toggle/Shift 应照常翻转");
        // 定向切换（ime.set）：带明确目标，归位配置怎么配都直达目标。
        assert!(
            f("chinese", Directed(true), cur),
            "Directed(true) 应直达中文"
        );
        assert!(
            !f("chinese", Directed(false), cur),
            "Directed(false) 应直达英文"
        );
        assert!(
            f("english", Directed(true), cur),
            "Directed(true) 压过 english 归位"
        );
        assert!(!f("bogus", Directed(false), cur), "Directed 不吃归位改写");
        // 认不出的值回落出厂语义（设置端 options 与 core 值域脱节时不至于乱跳）。
        assert!(f("bogus", Toggle, cur));
        assert!(f("", Toggle, cur));
    }
}

/// `CMD_SYSTEM_MODE_SWITCH` 的来源分派：只有用户按出来的切换才吃
/// `mode_after_cancel` 的改写，宿主写 compartment / 菜单 / 旧版 DLL 无 Ctrl 的一律
/// 按请求落地——否则 CapsLock 恰好开着时，宿主请求的「中文」会被 `english` 落成英文。
#[test]
fn caps_cancel_switch_from_system_source() {
    use super::CapsCancelSwitch;
    use wind_ipc::protocol::ModeSwitchSource as Src;
    let f = CapsCancelSwitch::from_system;
    for req in [true, false] {
        // 用户发起：按键侧兜底恒算；系统热键翻 compartment 时 Ctrl 还按着。
        assert_eq!(
            f(req, Src::CtrlSpaceKey, false),
            CapsCancelSwitch::System(req)
        );
        assert_eq!(
            f(req, Src::CtrlSpaceKey, true),
            CapsCancelSwitch::System(req)
        );
        for src in [
            Src::CompartmentOpenClose,
            Src::CompartmentConversion,
            Src::Unknown,
        ] {
            assert_eq!(
                f(req, src, true),
                CapsCancelSwitch::System(req),
                "{src:?}+Ctrl"
            );
            assert_eq!(
                f(req, src, false),
                CapsCancelSwitch::Host(req),
                "{src:?} 无 Ctrl"
            );
        }
        // 菜单：用户明确点了目标，不改写。
        assert_eq!(f(req, Src::Menu, false), CapsCancelSwitch::Host(req));
    }
    // 宿主来源在任何取值下都按请求，`english` 也不例外。
    for setting in ["chinese", "english", "toggle"] {
        for req in [true, false] {
            for cur in [true, false] {
                assert_eq!(
                    Coordinator::mode_after_caps_cancel(setting, CapsCancelSwitch::Host(req), cur),
                    req,
                    "{setting} 宿主来源应按请求"
                );
            }
        }
    }
}

/// 出厂值：新字段缺省时必须是 `chinese`，老配置（只写了 cancel_on_mode_switch）行为不变。
#[test]
fn mode_after_cancel_defaults_to_chinese() {
    assert_eq!(
        Config::default().input.capslock.mode_after_cancel,
        "chinese"
    );
}

/// cancel_on_mode_switch=true 但 CapsLock 未开：不注入、正常翻转。
#[test]
fn toggle_mode_normal_flip_when_caps_off() {
    let c = coord_with(|cfg| cfg.input.capslock.cancel_on_mode_switch = true);
    c.state.lock().unwrap().chinese_mode = false;
    c.handle_toggle_mode();
    let s = c.state.lock().unwrap();
    assert!(s.chinese_mode, "caps 未开时应正常翻转");
    assert!(!s.caps_lock);
}

/// 决策顺序：per-app 表命中优先于全局默认。
#[test]
fn initial_mode_decision_order() {
    let c = coord_with(|cfg| {
        cfg.input.default.state_scope = "app".into();
        cfg.input.default.chinese_mode = true;
    });
    c.mode_states
        .lock()
        .unwrap()
        .insert("x.exe".to_string(), false);
    assert!(
        !c.initial_chinese_mode_for("x.exe", &FocusWindow::default()),
        "表命中优先"
    );
    assert!(
        c.initial_chinese_mode_for("y.exe", &FocusWindow::default()),
        "未命中落默认"
    );
    assert!(
        c.initial_chinese_mode_for("", &FocusWindow::default()),
        "空进程名落默认"
    );
}
