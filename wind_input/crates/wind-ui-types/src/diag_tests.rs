use super::*;

fn base_view() -> InputDiagView {
    InputDiagView {
        process_name: "chrome.exe".into(),
        pid: 4242,
        disabled: true,
        reason_text: "线程级禁用".into(),
        mask: 1 << 31,
        ime_active: true,
        has_edit_context: false,
        window: WindowDiagView::default(),
        ..Default::default()
    }
}

#[test]
fn format_lines_shape() {
    let lines = format_diag_lines(&base_view());
    let text = lines.join("\n");
    assert!(lines[0].contains("chrome.exe"));
    assert!(lines[0].contains("4242"));
    assert!(lines[1].contains("线程禁用: 是"));
    assert!(lines[1].contains("线程级禁用"));
    assert!(lines[2].contains("0x")); // mask 十六进制
    // 两个状态位取值不同，确保没有把同一个变量渲染两遍
    assert!(lines[3].contains("激活: 是"));
    assert!(lines[3].contains("可编辑上下文: 否"));
    assert!(text.contains("── 窗口 ──"));
}

/// 未收到快照时**只出一行提示**，不得渲染一排占位 0。
/// 「没数据」被读成「数据是 0」正是诊断工具最坏的失败方式——它不会报错，
/// 只会让人拿着 band=0 去下结论。
#[test]
fn unreceived_window_section_says_so_instead_of_showing_zeros() {
    let lines = format_diag_lines(&base_view());
    let text = lines.join("\n");
    assert!(text.contains("未采集"), "应明确说明尚未采集: {text}");
    assert!(
        !text.contains("band="),
        "未采集时不得渲染 band 占位值: {text}"
    );
    assert!(
        !text.contains("DocMgr"),
        "未采集时不得渲染 TSF 分区: {text}"
    );
}

fn received_window() -> WindowDiagView {
    WindowDiagView {
        pid: 4242, // 与 base_view().pid 同源（常态）
        process_name: "chrome.exe".into(),
        focus_hwnd: 0xA1B2C3,
        focus_class: "Edit".into(),
        focus_source_label: "TSF".into(),
        root_hwnd: 0x11223344,
        root_class: "Shell_TrayWnd".into(),
        root_band: 1,
        fg_hwnd: 0x556677,
        fg_class: "Windows.UI.Core.CoreWindow".into(),
        fg_pid: 777,
        fg_process_name: "SearchApp.exe".into(),
        docmgr_id: 0x7FF6_1234_5678_9ABC,
        context_id: 0x7FF6_1234_5678_0000,
        focus_session_id: 19,
        docmgr_changed: true,
        host_band: 6,
        host_whitelisted: true,
        host_active: true,
        received: true,
    }
}

#[test]
fn received_window_section_renders_all_groups() {
    let mut v = base_view();
    v.window = received_window();
    let text = format_diag_lines(&v).join("\n");
    assert!(text.contains("0xA1B2C3"), "焦点句柄: {text}");
    assert!(text.contains("[TSF]"), "来源标记必须可见: {text}");
    assert!(text.contains("Shell_TrayWnd"), "顶层类名: {text}");
    assert!(text.contains("band=1"));
    assert!(text.contains("SearchApp.exe(777)"));
    assert!(text.contains("── TSF ──"));
    // 指针只取低 32 位（完整 64 位会把行撑得过宽）。
    assert!(text.contains("0x56789ABC"), "DocMgr 实例 id: {text}");
    assert!(text.contains("(本次已换)"));
    assert!(text.contains("会话#19"));
    assert!(text.contains("── HostRender ──"));
    assert!(text.contains("白名单: 是"));
    assert!(text.contains("band=6"));
}

/// 跨进程前台是异常信号，只在成立时出现；常态下不占地方。
/// 这一行正是「进程名不足以描述当前场景」的证据——per-app 配置只按进程名匹配。
#[test]
fn foreground_other_process_warning_is_conditional() {
    let mut v = base_view();
    v.window = received_window(); // 快照 pid=4242，fg_pid=777
    assert!(
        format_diag_lines(&v)
            .join("\n")
            .contains("⚠ 前台窗口属于其他进程")
    );

    v.window.fg_pid = v.window.pid; // 与快照同进程 → 不该出现
    assert!(!format_diag_lines(&v).join("\n").contains("⚠ 前台窗口"));

    // fg_pid 未知（0）只是采集失败，不得误报成"跨进程"。
    v.window.fg_pid = 0;
    assert!(!format_diag_lines(&v).join("\n").contains("⚠ 前台窗口"));
}

/// ★ 跨进程告警的基准必须是**快照自己的 pid**，不是首行那个。
///
/// 真机形态（Win10 任务栏搜索）：快照与前台同属 searchapp，输入态那半却停在 explorer。
/// 拿首行 pid 作基准就会报出一条与事实无关的「前台属于其他进程」。
#[test]
fn foreground_warning_uses_snapshot_pid_not_input_pid() {
    let mut v = base_view();
    v.pid = 7172; // 输入态来自 explorer
    v.window = received_window();
    v.window.pid = 8704; // 快照来自 searchapp
    v.window.process_name = "searchapp.exe".into();
    v.window.fg_pid = 8704; // 前台也是 searchapp —— 与快照同进程
    v.window.fg_process_name = "searchapp.exe".into();

    let text = format_diag_lines(&v).join("\n");
    assert!(
        !text.contains("⚠ 前台窗口属于其他进程"),
        "快照与前台同进程，不得因首行 pid 不同而误报: {text}"
    );
}

/// 上下两半来自不同进程时必须点破——并排显示天然让人以为它们同源。
#[test]
fn cross_process_snapshot_is_called_out() {
    let mut v = base_view();
    v.pid = 7172;
    v.window = received_window();
    v.window.pid = 8704;
    v.window.process_name = "searchapp.exe".into();
    let text = format_diag_lines(&v).join("\n");
    assert!(text.contains("本节来自 searchapp.exe(8704)"), "{text}");
    assert!(text.contains("非上方进程"), "{text}");

    // 同进程（常态）→ 平铺直叙地报来源，不加警示。
    v.window.pid = v.pid;
    v.window.process_name = "explorer.exe".into();
    let text = format_diag_lines(&v).join("\n");
    assert!(text.contains("来源: explorer.exe(7172)"), "{text}");
    assert!(!text.contains("非上方进程"), "{text}");
}

/// pid 未知（0）不算「不同进程」——那只是采集失败。
#[test]
fn unknown_pid_is_not_a_cross_process_signal() {
    let w = WindowDiagView {
        pid: 0,
        ..received_window()
    };
    assert!(!w.differs_from_input_process(7172));
    assert!(!w.foreground_is_other_process());
    assert!(!received_window().differs_from_input_process(0));
}

/// 默认全开：诊断工具的默认形态是"什么都看得见"。
/// `derive(Default)` 会给出全 false（空窗口），故此处钉死手写实现的语义。
#[test]
fn sections_default_all_on() {
    let s = DiagSections::default();
    assert!(s.input && s.window && s.tsf && s.host);
    assert!(InputDiagView::default().topmost, "默认置顶");
    assert!(!InputDiagView::default().frozen, "默认不冻结");
    for i in DiagSections::ALL {
        assert!(s.get(i), "分区 {i} 默认应开");
    }
    // 越界序号不隐藏内容——未知序号让内容照常显示比让它凭空消失安全。
    assert!(s.get(9));
}

#[test]
fn sections_toggle_only_target() {
    let mut s = DiagSections::default();
    s.toggle(2); // TSF
    assert!(!s.tsf);
    assert!(s.input && s.window && s.host, "其余分区不受牵连");
    s.toggle(2);
    assert!(s.tsf, "再切回来");
    s.toggle(9); // 越界 = no-op
    assert_eq!(s, DiagSections::default());
}

#[test]
fn hidden_sections_are_omitted() {
    let mut v = base_view();
    v.window = received_window();
    v.sections = DiagSections {
        input: false,
        window: true,
        tsf: false,
        host: false,
    };
    let text = format_diag_lines(&v).join("\n");
    assert!(text.contains("── 窗口 ──"));
    assert!(text.contains("Shell_TrayWnd"));
    assert!(!text.contains("InputScope"), "输入态已隐藏: {text}");
    assert!(!text.contains("── TSF ──"), "TSF 已隐藏: {text}");
    assert!(!text.contains("── HostRender ──"), "Host 已隐藏: {text}");
}

/// 分区全关会渲染空窗口——那和"HUD 坏了"在屏幕上一模一样，必须给一行能自救的提示。
#[test]
fn all_sections_hidden_shows_recovery_hint() {
    let mut v = base_view();
    v.sections = DiagSections {
        input: false,
        window: false,
        tsf: false,
        host: false,
    };
    let lines = format_diag_lines(&v);
    assert_eq!(lines.len(), 1, "只出提示行: {lines:?}");
    assert!(
        lines[0].contains("显示分类"),
        "提示要指出怎么恢复: {lines:?}"
    );
}

/// 冻结必须在 HUD 上有标注：显示的已不是实时值，不说清楚用户会拿旧快照当现状读。
#[test]
fn frozen_is_labeled_at_top() {
    let mut v = base_view();
    v.frozen = true;
    let lines = format_diag_lines(&v);
    assert!(
        lines[0].contains("已停止刷新"),
        "冻结标注须在首行: {lines:?}"
    );
    v.frozen = false;
    assert!(!format_diag_lines(&v).join("\n").contains("已停止刷新"));
}

/// 「未采集」提示只在**依赖快照的分区至少开一个**时出现。
/// 三个分区都关掉的人是主动不看，再提醒他"切一次焦点"就成了噪音。
#[test]
fn unreceived_hint_follows_snapshot_sections() {
    let mut v = base_view(); // received=false
    assert!(format_diag_lines(&v).join("\n").contains("未采集"));

    v.sections = DiagSections {
        input: true,
        window: false,
        tsf: false,
        host: false,
    };
    assert!(
        !format_diag_lines(&v).join("\n").contains("未采集"),
        "快照分区全关时不该再提示"
    );
}

/// 空句柄必须与"拿到了一个小地址句柄"可分辨。
#[test]
fn zero_handle_renders_as_dash_not_0x0() {
    let mut v = base_view();
    v.window = WindowDiagView {
        received: true,
        ..Default::default()
    };
    let text = format_diag_lines(&v).join("\n");
    assert!(text.contains("焦点: -"), "空句柄应显示为 -: {text}");
    assert!(!text.contains("0x0 "), "不得渲染成 0x0: {text}");
}
