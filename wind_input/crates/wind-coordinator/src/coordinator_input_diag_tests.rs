//! last_input_diag 存储 + 密码框强制英文抑制。
use super::*;
use crate::input_diag::InputDiagReason;

fn test_coordinator() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
}

#[test]
fn password_scope_sets_suppress_and_state() {
    let c = test_coordinator();
    c.apply_input_diag(1234, false, /*reason*/ 2, 1 << 31);
    assert!(
        c.password_suppress
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let d = c.last_input_diag.lock().unwrap();
    assert_eq!(d.reason, InputDiagReason::InputScopePassword);
    assert_eq!(d.pid, 1234);
}

#[test]
fn suppress_cleared_when_mask_clears() {
    let c = test_coordinator();
    c.apply_input_diag(1, false, 2, 1 << 31);
    c.apply_input_diag(1, false, 0, 0);
    assert!(
        !c.password_suppress
            .load(std::sync::atomic::Ordering::Relaxed)
    );
}

#[test]
fn disabled_policy_no_suppress_when_off() {
    let c = test_coordinator();
    c.password_suppress_enabled
        .store(false, std::sync::atomic::Ordering::Relaxed);
    c.apply_input_diag(1, false, 2, 1 << 31);
    assert!(
        !c.password_suppress
            .load(std::sync::atomic::Ordering::Relaxed)
    );
}

/// 构造最简按键事件（对齐 capslock_tests::kev 的写法）。
fn kev(key_code: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 真实输入路径验证：密码框抑制期间字母键必须透传（强制英文），
/// 解除抑制后同一按键应回到中文组词流——防止「只改图标不拦输入」的回归。
#[test]
fn password_suppress_forces_english_passthrough() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    let c = Coordinator::new_headless(cfg, None);
    assert!(
        c.state.lock().unwrap().chinese_mode,
        "前置条件：应处于中文模式"
    );

    let pid = 4321u32;
    c.apply_input_diag(pid, false, 2, 1 << 31);
    assert!(
        c.password_suppress
            .load(std::sync::atomic::Ordering::Relaxed),
        "前置条件：密码框抑制应已置位"
    );
    let action = c.handle_key_event(&kev(0x41 /* VK_A */, EVENT_KEY_DOWN));
    assert!(
        matches!(action, KeyAction::PassThrough),
        "密码框抑制期间字母键应强制透传（英文），实际: {:?}",
        action
    );
    assert!(
        c.state.lock().unwrap().chinese_mode,
        "抑制不应改动 chinese_mode 持久值（图标保持不变）"
    );

    // 解除抑制：mask 清零。
    c.apply_input_diag(pid, false, 0, 0);
    assert!(
        !c.password_suppress
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let action = c.handle_key_event(&kev(0x41 /* VK_A */, EVENT_KEY_DOWN));
    assert!(
        !matches!(action, KeyAction::PassThrough),
        "解除抑制后字母键应进入中文组词流，不应透传，实际: {:?}",
        action
    );
}

#[test]
fn toggle_hud_flips_visibility() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    assert!(!c.input_diag_hud_visible.load(Relaxed));
    c.toggle_input_diag_hud();
    assert!(c.input_diag_hud_visible.load(Relaxed));
    c.toggle_input_diag_hud();
    assert!(!c.input_diag_hud_visible.load(Relaxed));
}

/// 开关的初值取自配置（`input.password_force_english`）：配置关掉时，构造出的协调器遇到
/// 密码位也不抑制，否则菜单关掉后重启服务又勾回。
///
/// 菜单切换写盘 / 热重载保持见 `tests/password_force_english_persist.rs`（要重定向用户目录，
/// 只能单开测试二进制；本模块不走写盘路径）。
#[test]
fn config_off_constructs_without_suppress() {
    use std::sync::atomic::Ordering::Relaxed;
    let mut cfg = Config::default();
    assert!(cfg.input.password_force_english, "前置条件：出厂开");
    cfg.input.password_force_english = false;
    let c = Coordinator::new_headless(cfg, None);
    assert!(!c.password_suppress_enabled.load(Relaxed));

    c.apply_input_diag(1, false, 2, 0x8000_0001); // IS_DEFAULT + IS_PASSWORD（t197 的掩码）
    assert!(
        !c.password_suppress.load(Relaxed),
        "配置关闭时密码位不应强制英文"
    );
}

/// 关掉开关须立即解除已生效的抑制，不能等下一次焦点上报——用户正对着那个
/// 被误判的框点菜单，点完就要能打中文。
#[test]
fn switching_off_clears_active_suppress() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    c.apply_input_diag(1, false, 2, 0x8000_0001);
    assert!(c.password_suppress.load(Relaxed), "前置条件：抑制已生效");
    c.set_password_suppress_enabled(false);
    assert!(!c.password_suppress_enabled.load(Relaxed));
    assert!(!c.password_suppress.load(Relaxed), "关掉开关应立即解除抑制");
}

#[test]
fn focus_lost_clears_password_suppress() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    c.apply_input_diag(1234, false, 2, 1 << 31);
    assert!(
        c.password_suppress.load(Relaxed),
        "前置条件：密码框抑制应已置位"
    );
    c.handle_focus_lost(0, FocusLostReason::Thread);
    assert!(
        !c.password_suppress.load(Relaxed),
        "失焦后应清除密码框抑制态，避免残留到下次 focus_gained 之前"
    );
}

/// Chromium 网页密码框必须强制英文，即便上报的 disabled=true。
///
/// DLL 放行看的是线程级 KEYBOARD_DISABLED，而 `disabled` 字段可能是 context 级判定（网页
/// 密码框恒为 true），判据里加 `&& !disabled` 会让抑制自我否决。本用例的要害就是
/// `disabled=true`，改动时必须保住这个取值。
#[test]
fn password_scope_suppresses_even_when_disabled_flag_set() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    let c = Coordinator::new_headless(cfg, None);

    // disabled=true + 密码位：Chromium 网页密码框的上报组合。
    c.apply_input_diag(4321, true, 1, 1 << 31);
    assert!(
        c.password_suppress
            .load(std::sync::atomic::Ordering::Relaxed),
        "context 级密码框（disabled=true）必须触发强制英文抑制"
    );

    let action = c.handle_key_event(&kev(0x41 /* VK_A */, EVENT_KEY_DOWN));
    assert!(
        matches!(action, KeyAction::PassThrough),
        "密码框里字母键应强制透传为英文，实际: {:?}",
        action
    );
    assert!(
        c.state.lock().unwrap().chinese_mode,
        "抑制不应改动 chinese_mode 持久值（图标保持不变）"
    );
}

/// Gecko 系（Zen）页面无输入焦点时置 context 级 KEYBOARD_DISABLED，宿主原始 scope 只有
/// IS_DEFAULT。DLL 折位后 mask=0x80000001、reason 字节=4。展示与提示语必须说「被禁用」而不是
/// 「密码框」，抑制行为则与密码框一致（Chromium 密码框靠同一信号）。
#[test]
fn context_disabled_is_shown_apart_from_host_password() {
    use crate::input_diag::InputDiagReason;
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    c.state.lock().unwrap().ime_active = true;

    c.apply_input_diag(1, false, 4, 0x8000_0001);
    assert_eq!(
        c.last_input_diag.lock().unwrap().reason,
        InputDiagReason::ContextDisabled
    );
    assert!(
        c.password_suppress.load(Relaxed),
        "抑制行为不因成因分档而变"
    );
    assert_eq!(c.input_block(), InputBlock::ContextDisabled);

    // 宿主真报了 IS_PASSWORD：reason 字节 2，仍是密码框。
    c.apply_input_diag(1, false, 2, 0x8000_0001);
    assert_eq!(c.input_block(), InputBlock::Password);

    // Chromium 网页密码框：context 禁用 + IS_PRIVATE（rawScope=0x2000000000000000），
    // 宿主虽没明说密码，两个信号同时出现按密码框展示——图标提示不能说「被应用禁用」。
    c.apply_input_diag(1, false, 4, 0x8000_0000 | (1 << 61));
    assert_eq!(
        c.last_input_diag.lock().unwrap().reason,
        InputDiagReason::ContextPassword
    );
    assert_eq!(c.input_block(), InputBlock::Password);
}

#[test]
fn context_disabled_tooltip_does_not_claim_password() {
    let c = test_coordinator();
    c.state.lock().unwrap().ime_active = true;
    c.apply_input_diag(1, false, 4, 0x8000_0001);
    let _ = c.langbar_tooltip(); // 起计时
    std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
    let tip = c.langbar_tooltip();
    assert!(
        !tip.contains("密码"),
        "context 级禁用不该被说成密码框: {tip}"
    );
    assert!(tip.contains("禁用"), "应说明是被应用禁用: {tip}");
}

/// disabled 只参与 `reason_from` 的展示推导，不参与 suppress 决策——单一来源。
///
/// 「compartment 禁用 ⇒ DLL 已放行所有键」只对线程级 KEYBOARD_DISABLED 成立，
/// 不能拿它免掉 suppress。
#[test]
fn disabled_flag_drives_reason_display_not_suppression() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();

    // 线程级禁用 + 密码位：reason 展示为 compartment（优先级最高），suppress 仍置位。
    // suppress=true 在此场景无害：DLL 已全放行，引擎收不到键，取值无从被观测。
    c.apply_input_diag(1, true, 1, 1 << 31);
    assert_eq!(
        c.last_input_diag.lock().unwrap().reason,
        crate::input_diag::InputDiagReason::CompartmentDisabled,
        "disabled=true 时 reason 展示应为 compartment"
    );
    assert!(
        c.password_suppress.load(Relaxed),
        "reason 的展示优先级不应反过来否决抑制决策"
    );

    // 无密码位：无论 disabled 与否都不抑制。
    c.apply_input_diag(1, true, 1, 0);
    assert!(
        !c.password_suppress.load(Relaxed),
        "mask 无密码位时不应抑制"
    );
}

/// 策略开关关闭后，即便命中密码位也不抑制（高级菜单的逃生阀必须真的管用）。
#[test]
fn disabled_switch_defeats_password_scope() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    c.set_password_suppress_enabled(false);
    assert!(
        !c.password_suppress_enabled.load(Relaxed),
        "前置条件：开关已关"
    );

    c.apply_input_diag(1, true, 1, 1 << 31);
    assert!(
        !c.password_suppress.load(Relaxed),
        "开关关闭时密码框不应强制英文"
    );
    c.apply_input_diag(1, false, 2, 1 << 63);
    assert!(
        !c.password_suppress.load(Relaxed),
        "数字密码位同样受开关约束"
    );
}

// ── 按应用覆盖（compat.toml 的 password_force_english）──

/// 给 pid 登记进程名并装上该进程的 `password_force_english` 规则。
fn pfe_rule(c: &Coordinator, pid: u32, name: &str, v: Option<bool>) {
    c.pid_names.lock().unwrap().insert(pid, name.to_string());
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: name.into(),
        password_force_english: v,
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);
}

/// 规则 false：该进程的密码框不再强制英文；别的进程照旧跟随全局（开）。
#[test]
fn per_app_rule_false_exempts_only_that_process() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    pfe_rule(&c, 700, "misreport.exe", Some(false));

    c.apply_input_diag(700, false, 2, 0x8000_0001);
    assert!(!c.password_suppress.load(Relaxed), "规则关掉的进程不应抑制");

    c.pid_names.lock().unwrap().insert(701, "other.exe".into());
    c.apply_input_diag(701, false, 2, 0x8000_0001);
    assert!(
        c.password_suppress.load(Relaxed),
        "无规则的进程跟随全局（开）"
    );
}

/// 规则 true 压过全局关。
#[test]
fn per_app_rule_true_overrides_global_off() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    c.set_password_suppress_enabled(false);
    pfe_rule(&c, 800, "bank.exe", Some(true));

    c.apply_input_diag(800, false, 2, 1 << 31);
    assert!(c.password_suppress.load(Relaxed), "规则开应压过全局关");

    c.pid_names.lock().unwrap().insert(801, "other.exe".into());
    c.apply_input_diag(801, false, 2, 1 << 31);
    assert!(
        !c.password_suppress.load(Relaxed),
        "无规则的进程跟随全局（关）"
    );
}

/// 无规则（含规则存在但本字段未配）一律跟随全局，两个方向都验。
#[test]
fn per_app_no_rule_follows_global() {
    let c = test_coordinator();
    pfe_rule(&c, 900, "plain.exe", None);
    assert!(c.password_force_english_for_pid(900), "全局开 → 开");
    assert!(c.password_force_english_for_pid(0), "pid 未知 → 全局");
    assert!(c.password_force_english_for_pid(12345), "名字未知 → 全局");
    c.set_password_suppress_enabled(false);
    assert!(!c.password_force_english_for_pid(900), "全局关 → 关");
    assert!(!c.password_force_english_for_pid(0));
}

/// 不变量 core.suppress ⊆ C++.suppress：逐客户端推给 DLL 的值与服务端判定出自
/// 同一个函数。这里对每个客户端比对「推送值」与「同 pid 下服务端算出的 suppress」。
#[test]
fn per_client_push_matches_server_judgement() {
    use std::sync::atomic::Ordering::Relaxed;
    let c = test_coordinator();
    pfe_rule(&c, 700, "misreport.exe", Some(false));
    c.pid_names.lock().unwrap().insert(701, "other.exe".into());
    let tok_a = (700u64 << 32) | 1;
    let tok_b = (701u64 << 32) | 1;
    let cap_a = c.push_server.attach_capture_client(tok_a);
    let cap_b = c.push_server.attach_capture_client(tok_b);

    let msg = |v: bool| {
        wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_PASSWORD_SUPPRESS,
            &wind_ipc::codec::encode_password_suppress_value(v),
        )
    };
    for global in [true, false] {
        c.set_password_suppress_enabled(global); // 内部广播一轮
        for (pid, cap) in [(700u32, &cap_a), (701u32, &cap_b)] {
            let got: Vec<Vec<u8>> = cap.try_iter().collect();
            let pushed = if got.contains(&msg(true)) {
                assert!(!got.contains(&msg(false)), "pid={pid} 同一轮收到两种值");
                true
            } else {
                assert!(got.contains(&msg(false)), "pid={pid} 没收到密码框配置");
                false
            };
            c.apply_input_diag(pid, false, 2, 1 << 31);
            assert_eq!(
                c.password_suppress.load(Relaxed),
                pushed,
                "global={global} pid={pid}：服务端 suppress 必须等于推给该 DLL 的值"
            );
        }
    }
    // 规则进程恒不开，与全局无关。
    assert!(!c.password_force_english_for_pid(700));
}

/// 定向推送（握手）同样按目标 pid 取值，不拿全局值。
#[test]
fn handshake_push_uses_target_pid_rule() {
    let c = test_coordinator();
    pfe_rule(&c, 700, "misreport.exe", Some(false));
    let tok = (700u64 << 32) | 3;
    let cap = c.push_server.attach_capture_client(tok);
    c.push_password_suppress_config(tok);
    let got: Vec<Vec<u8>> = cap.try_iter().collect();
    assert_eq!(
        got,
        vec![wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_PASSWORD_SUPPRESS,
            &wind_ipc::codec::encode_password_suppress_value(false),
        )]
    );
}

fn pfe_msg(v: bool) -> Vec<u8> {
    wind_ipc::codec::encode_sync_config(
        wind_ipc::protocol::CONFIG_KEY_PASSWORD_SUPPRESS,
        &wind_ipc::codec::encode_password_suppress_value(v),
    )
}

/// 从一个客户端这一轮收到的推送里取出密码框门控的值（没收到 = `None`）。
fn pushed_pfe(cap: &std::sync::mpsc::Receiver<Vec<u8>>) -> Option<bool> {
    let got: Vec<Vec<u8>> = cap.try_iter().collect();
    match (got.contains(&pfe_msg(true)), got.contains(&pfe_msg(false))) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
        (true, true) => panic!("同一轮收到两种值"),
    }
}

/// PID 复用：推送通道握手早于主管道校正进程名时，握手按旧名算了门控。校正真的
/// 改写了缓存 ⇒ 必须给该进程的客户端补推按新名算的值；别的进程不受打扰。
#[test]
fn revalidated_pid_name_repushes_password_gate_to_that_process() {
    let c = test_coordinator();
    // 新进程 misreport.exe 配了 false，但 pid 700 在缓存里还是上一任 plain.exe（无规则）。
    pfe_rule(&c, 700, "misreport.exe", Some(false));
    c.pid_names.lock().unwrap().insert(700, "plain.exe".into());
    c.pid_names.lock().unwrap().insert(701, "other.exe".into());
    let tok = (700u64 << 32) | 1;
    let bystander = (701u64 << 32) | 1;
    let cap = c.push_server.attach_capture_client(tok);
    let cap_other = c.push_server.attach_capture_client(bystander);

    // 握手（按旧名算 ⇒ 跟随全局 = 开）。
    c.push_password_suppress_config(tok);
    assert_eq!(pushed_pfe(&cap), Some(true), "前置：握手按旧名算");

    c.revalidate_pid_name(700, "MisReport.exe");
    assert_eq!(
        pushed_pfe(&cap),
        Some(c.password_force_english_for_pid(700)),
        "校正了进程名就得按新名补推，DLL 与服务端判定必须一致"
    );
    assert_eq!(pushed_pfe(&cap), None, "补推一次即可");
    assert_eq!(pushed_pfe(&cap_other), None, "别的进程的客户端不该收到补推");

    // 名字没变（同进程重连）不补推。
    c.revalidate_pid_name(700, "misreport.exe");
    assert_eq!(pushed_pfe(&cap), None, "名字没变不补推");
}

/// 任何一次整表重载（不只是密码框那一项菜单）都要重推门控：用户手改配置后点了别的
/// 菜单项，推给客户端的值也必须跟上，否则打破 core.suppress ⊆ C++.suppress。
#[test]
fn any_compat_reload_repushes_password_gate_for_hand_written_rule() {
    let dir = std::env::temp_dir().join(format!("wind_pfe_reload_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ui_tx, _rx) = std::sync::mpsc::channel();
    let push_server = Arc::new(PushServer::new(wind_bridge::push::PushConfig {
        suffix: String::new(),
        write_timeout_ms: 30_000,
    }));
    // 用户目录只给 compat 用；store = None ⇒ state_writer 不写本机状态。
    let c = Coordinator::build(
        Config::default(),
        None,
        push_server,
        crate::UiSender::without_wake(ui_tx),
        Some(dir.clone()),
        None,
        None,
    );
    c.pid_names.lock().unwrap().insert(700, "bank.exe".into());
    c.active_compat.lock().unwrap().pid = 700;
    let tok = (700u64 << 32) | 1;
    let cap = c.push_server.attach_capture_client(tok);

    // 每一轮：先「手写」翻转规则（只改文件，内存表不知道），再触发一个非密码类的重载。
    type Reload<'a> = (&'a str, &'a dyn Fn(&Coordinator));
    let reloads: [Reload; 7] = [
        ("set_first_show_mode", &|c| c.set_first_show_mode(1)),
        ("set_candidate_position_rule", &|c| {
            c.set_candidate_position_rule(1)
        }),
        ("set_ignore_host_ime_switch_rule", &|c| {
            c.set_ignore_host_ime_switch_rule(1)
        }),
        ("set_auto_pair_rule", &|c| c.set_auto_pair_rule(2)),
        ("set_status_fallback_rule", &|c| {
            c.set_status_fallback_rule(2)
        }),
        ("set_status_position_rule", &|c| {
            c.set_status_position_rule(1)
        }),
        ("set_initial_state_rule", &|c| {
            c.set_initial_state_rule(false, 2)
        }),
    ];
    for (i, (what, reload)) in reloads.iter().enumerate() {
        let hand_written = i % 2 == 0; // 在 false / true 间来回翻，两个方向都验
        wind_config::app_compat::set_user_password_force_english(
            &dir,
            "bank.exe",
            Some(hand_written),
        )
        .unwrap();
        let _ = pushed_pfe(&cap);
        reload(&c);
        assert_eq!(
            c.password_force_english_for_pid(700),
            hand_written,
            "{what}: 前置——重载后服务端按手写规则判定"
        );
        assert_eq!(
            pushed_pfe(&cap),
            Some(hand_written),
            "{what}: 推给该客户端的门控必须等于服务端判定"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
