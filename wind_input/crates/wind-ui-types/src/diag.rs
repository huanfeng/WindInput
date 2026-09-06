//! 输入诊断视图数据与纯格式化（渲染窗口本体在 wind-ui 的 input_diag_hud）。

/// 候选窗定位调试浮窗的一帧数据（渲染在 wind-ui 的 `caret_overlay`）。
///
/// ★ 为什么要画出来：定位缺陷本质是**空间**问题——「宿主说组合在这块区域」与「候选窗
/// 画在了那个点」是否对得上。日志只能给出离散数值，人得在脑子里把它们还原成屏幕位置
/// 关系；宿主一多、操作序列一长（打字/换行/逐字删除交替），这个还原就不可靠了。
/// 直接叠在屏幕上画，错位一眼可见。
///
/// 所有坐标都是**屏幕物理坐标**，与 IPC 上报口径一致。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaretOverlayView {
    /// 宿主上报的插入点 `(x, y_bottom, height)`。
    pub caret: (i32, i32, i32),
    /// 宿主上报的组合起点（折叠到起点的 rect 左下角）；`None` = 本帧没有。
    pub comp_start: Option<(i32, i32)>,
    /// 宿主上报的整个组合范围矩形 `(left, top, right, bottom)`；`None` = 本帧没取到。
    pub comp_rect: Option<(i32, i32, i32, i32)>,
    /// 候选窗**实际**使用的锚点，以及它取自哪里（`composition_start` / `caret` /
    /// `shown_anchor`）。这是把「判据算出了什么」与「宿主说了什么」对上的关键一行。
    pub anchor: (i32, i32),
    pub anchor_source: &'static str,
    /// 本帧 caret 坐标来源（`wind_ipc::protocol::caret_source::*` 的名字）。
    pub caret_source: &'static str,
    /// 组合矩形判据走了哪条分支，直接显示在浮窗上——不必回头翻日志。
    pub rect_verdict: RectVerdict,
    /// 焦点进程名，用于确认 per-app 规则作用在谁身上。
    pub process: String,
}

/// 组合矩形判据的分支，与 `message_handler` 里的判断一一对应。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RectVerdict {
    /// 本帧宿主没给矩形（旧 DLL / macOS / TS_E_NOLAYOUT）。
    #[default]
    Absent,
    /// 采信：锚点取自矩形左下角。
    Trusted,
    /// 退化（宽或高为 0）——整帧不参与锚点决策。
    Degenerate,
    /// 是插入点不是范围（`left >= caret.x`）——回退既有锚点逻辑。
    InsertionPoint,
    /// 不覆盖当前插入行（`caret.y > bottom`）——疑该宿主只返回首行。
    NotCoveringCaretLine,
}

impl RectVerdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Absent => "无矩形",
            Self::Trusted => "采信矩形",
            Self::Degenerate => "退化(w或h=0)",
            Self::InsertionPoint => "插入点非范围",
            Self::NotCoveringCaretLine => "不覆盖插入行",
        }
    }
}

/// 分区显示开关（右键菜单「显示分类」）。
///
/// 默认**全开**——诊断工具的默认形态应该是「什么都看得见」，隐藏是用户为了省地方
/// 主动做的选择。故手写 `Default` 而非 `derive`（derive 会给出全 false = 空窗口）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagSections {
    /// 进程 / 禁用态 / InputScope / 激活位。
    pub input: bool,
    /// 焦点·顶层·前台窗口链。
    pub window: bool,
    /// DocMgr / Context / 会话号。
    pub tsf: bool,
    /// 白名单 / 活跃 / band。
    pub host: bool,
}

impl Default for DiagSections {
    fn default() -> Self {
        Self {
            input: true,
            window: true,
            tsf: true,
            host: true,
        }
    }
}

impl DiagSections {
    /// 按分区序号取值（0=输入态 1=窗口 2=TSF 3=HostRender）。序号是菜单 id 的载荷，
    /// 越界返回 true（不隐藏）——未知序号让内容照常显示，比让它凭空消失安全。
    pub fn get(&self, idx: u8) -> bool {
        match idx {
            0 => self.input,
            1 => self.window,
            2 => self.tsf,
            3 => self.host,
            _ => true,
        }
    }

    /// 翻转指定分区；越界为 no-op。
    pub fn toggle(&mut self, idx: u8) {
        match idx {
            0 => self.input = !self.input,
            1 => self.window = !self.window,
            2 => self.tsf = !self.tsf,
            3 => self.host = !self.host,
            _ => {}
        }
    }

    /// 分区序号 → 菜单标签。与 [`Self::get`]/[`Self::toggle`] 的序号是同一套。
    pub fn label(idx: u8) -> &'static str {
        match idx {
            0 => "输入态",
            1 => "窗口",
            2 => "TSF",
            3 => "HostRender",
            _ => "?",
        }
    }

    /// 全部分区序号（菜单构建用，保证不漏项）。
    pub const ALL: [u8; 4] = [0, 1, 2, 3];
}

#[derive(Clone, Debug)]
pub struct InputDiagView {
    pub process_name: String,
    pub pid: u32,
    pub disabled: bool,
    pub reason_text: String,
    pub mask: u64,
    /// 本输入法是否在为某宿主服务（协调器 `State::ime_active`）。
    pub ime_active: bool,
    /// 焦点是否落在可编辑控件里（协调器 `State::has_edit_context`）。
    ///
    /// 与 `ime_active` 正交，两者都为真工具栏才显示。把它们摆进 HUD 是因为
    /// 「工具栏该显示却没显示 / 该隐藏却不隐藏」只能靠这两位区分，此前只能翻服务端日志。
    pub has_edit_context: bool,
    /// 窗口链 / TSF 上下文 / host-render 运行态。
    pub window: WindowDiagView,
    /// 分区显示开关（右键菜单）。
    pub sections: DiagSections,
    /// 窗口置顶（右键菜单可关）。关掉是为了让 HUD 沉到被观察窗口之下——
    /// 它自己挡住要看的东西是排查时的常见困扰。
    pub topmost: bool,
    /// 冻结中（右键菜单「停止刷新」）。**仅用于在 HUD 上打标**：真正的冻结在协调器侧
    /// （不再推送新快照），这里只负责让用户看得见"现在显示的不是实时值"。
    ///
    /// ⚠ 冻结而不标注，是诊断工具最坏的失败方式之一——用户会拿着一份旧快照当现状读。
    pub frozen: bool,
}

impl Default for InputDiagView {
    /// 分区全开、置顶、不冻结。**不能 derive**：那会给出全 false，等于默认空窗口 + 不置顶。
    fn default() -> Self {
        Self {
            process_name: String::new(),
            pid: 0,
            disabled: false,
            reason_text: String::new(),
            mask: 0,
            ime_active: false,
            has_edit_context: false,
            window: WindowDiagView::default(),
            sections: DiagSections::default(),
            topmost: true,
            frozen: false,
        }
    }
}

/// 窗口 / TSF 上下文诊断快照（DLL 上报 + 服务端补齐的 host-render 运行态）。
///
/// 与 [`InputDiagView`] 的输入态字段**分开存**：两者上报时机不同（禁用态随 compartment
/// 变更走，窗口快照随焦点走），合成一个就得回答「只到了一半时另一半算什么」。
#[derive(Clone, Debug, Default)]
pub struct WindowDiagView {
    /// **本快照的来源进程**（上报它的那个 DLL 实例的 `GetCurrentProcessId`）。
    ///
    /// ⚠ 与 [`InputDiagView::pid`] **不保证是同一个进程**：输入态随 focus_gained /
    /// compartment 变更走，窗口快照随焦点走，两个槽由不同进程的上报各自覆盖。多进程宿主
    /// （Win10 任务栏搜索：explorer + searchapp 各有一个 DLL 实例）下并排显示两份数据，
    /// 就隐含承诺了它们同源——不同源时必须显式说破，否则读者会脑补出一个不存在的完整画面。
    pub pid: u32,
    /// 来源进程名（服务端按 `pid` 补齐）。
    pub process_name: String,
    /// 焦点窗口句柄（仅展示与同一性比较——跨进程句柄在服务进程里调用无效）。
    pub focus_hwnd: u64,
    /// 焦点窗口类名。
    pub focus_class: String,
    /// 焦点窗口句柄的来源域标签（"TSF" / "GUI" / "前台" / "无"）。
    ///
    /// ⚠ 没有这一项，三条来源完全不同的句柄就混成一个字段了——尤其"前台"域的窗口
    /// 可能根本不属于本进程，拿它当"焦点窗口"去推 per-app 判据必然推错。
    ///
    /// 存**已翻译的字符串**而非协议里的 u8：`wind-ipc` 只在 windows/macos target 下才是
    /// 渲染端的依赖（CI 的 test 跑 Linux 宿主），视图层引不到那个枚举。与 `reason_text`
    /// 同一惯例——映射只发生在协调器一处。
    pub focus_source_label: String,
    /// 顶层窗口句柄（`GetAncestor(GA_ROOT)`）。
    pub root_hwnd: u64,
    /// 顶层窗口类名——**per-app 窗口级判据取这个**（控件自身类名跨版本不稳定）。
    pub root_class: String,
    /// 顶层窗口 z-band（0 = 取不到）。
    pub root_band: u32,
    /// 前台窗口句柄。
    pub fg_hwnd: u64,
    /// 前台窗口类名。
    pub fg_class: String,
    /// 前台窗口所属进程 id。
    pub fg_pid: u32,
    /// 前台窗口所属进程名（服务端按 `fg_pid` 补齐；DLL 不上报）。
    pub fg_process_name: String,
    /// 焦点 DocMgr 指针值（实例同一性标识）。
    pub docmgr_id: u64,
    /// 焦点 Context 指针值（实例同一性标识）。
    pub context_id: u64,
    /// DLL 焦点会话序号（与服务端日志对齐用）。
    pub focus_session_id: u32,
    /// 本次焦点是否换了 DocMgr。
    pub docmgr_changed: bool,
    /// DLL 侧 host-render band 窗口当前 band（0 = 未建）。
    pub host_band: u32,
    /// 进程是否命中 host-render 白名单（服务端现算）。
    pub host_whitelisted: bool,
    /// host-render 是否有活跃目标且就是本进程（服务端现算）。
    pub host_active: bool,
    /// 已收到过至少一次快照。
    ///
    /// **「没数据」和「数据是 0」必须能分辨**：采集开关刚推给 DLL 时还没有任何快照，
    /// 若此时照常渲染一排 0，用户会把"尚未采集"读成"band 确实是 0"，进而据此得出
    /// 完全错误的结论。这一位就是为了让 HUD 能说出"还没采到"。
    pub received: bool,
}

impl WindowDiagView {
    /// 前台窗口是否属于**别的**进程。
    ///
    /// ⚠ 比较基准必须是**本快照自己的** `pid`，不能是 [`InputDiagView::pid`]——后者是
    /// 「最后一次输入态上报」的进程，多进程宿主下与本快照可能根本不是一个进程，用它比
    /// 会得到一条与事实无关的告警（真机实测：快照与前台同属 searchapp，却因输入态那半
    /// 停在 explorer 而报出「前台属于其他进程」）。
    ///
    /// 这条信号本身仍是关键的：焦点进程与前台进程分家时，per-app 配置按进程名匹配就
    /// 描述不了当前场景。
    pub fn foreground_is_other_process(&self) -> bool {
        self.fg_pid != 0 && self.pid != 0 && self.fg_pid != self.pid
    }

    /// 本快照与输入态分区是否来自**不同进程**（`input_pid` = [`InputDiagView::pid`]）。
    /// 两者都非 0 且不等时为真——此时 HUD 上下两半描述的是两个进程。
    pub fn differs_from_input_process(&self, input_pid: u32) -> bool {
        self.pid != 0 && input_pid != 0 && self.pid != input_pid
    }
}

/// 句柄的短展示形式。`0` 显示为 `-`——**空句柄和小地址必须一眼可分**，
/// 否则 `0x0` 会被读成"拿到了一个句柄"。
fn hwnd_str(h: u64) -> String {
    if h == 0 {
        "-".to_string()
    } else {
        format!("0x{h:X}")
    }
}

/// COM 指针的实例标识：取低 32 位。完整 64 位指针（`0x7FF6…`）会把行撑得很宽，
/// 而这里只需要回答"还是不是刚才那个实例"，低 32 位足够区分同进程内的不同对象。
fn inst_str(p: u64) -> String {
    if p == 0 {
        "-".to_string()
    } else {
        format!("0x{:08X}", p as u32)
    }
}

/// 类名的展示形式：空串显示为 `?`，与"类名就是空"区分不开的风险由采集端消除
/// （取不到时本就是空串，这里统一渲染成 `?` 表示未知）。
fn class_str(s: &str) -> &str {
    if s.is_empty() { "?" } else { s }
}

/// 纯格式化：诊断文本行（可单测）。
///
/// 分区渲染：输入态 → 窗口 → TSF → HostRender。窗口/TSF/HostRender 三段依赖 DLL 的
/// 诊断快照，未收到时**只出一行"未采集"**而不是一排占位 0（见 [`WindowDiagView::received`]）。
pub fn format_diag_lines(v: &InputDiagView) -> Vec<String> {
    let name = if v.process_name.is_empty() {
        "(未知)"
    } else {
        &v.process_name
    };
    let yn = |b: bool| if b { "是" } else { "否" };
    let s = &v.sections;
    let mut lines: Vec<String> = Vec::new();

    // 冻结标注置顶行：显示的已不是实时值，这件事必须先说，否则用户会拿旧快照当现状。
    if v.frozen {
        lines.push("⏸ 已停止刷新".to_string());
    }

    if s.input {
        lines.push(format!("{} ({})", name, v.pid));
        lines.push(format!(
            "禁用态: {}  原因: {}",
            yn(v.disabled),
            v.reason_text
        ));
        lines.push(format!("InputScope: 0x{:X}", v.mask));
        lines.push(format!(
            "激活: {} 可编辑上下文: {}",
            yn(v.ime_active),
            yn(v.has_edit_context)
        ));
    }

    let w = &v.window;
    // 「未采集」提示挂在窗口区：三个依赖快照的分区里它排第一，用户最可能先打开它。
    // 三个分区都关掉时不出这条——那时用户是主动不看，不需要被提醒去切焦点。
    let need_snapshot = s.window || s.tsf || s.host;

    if s.window {
        lines.push("── 窗口 ──".to_string());
        if w.received {
            // 快照来源进程。与首行进程不同时**必须点破**：那说明 HUD 上下两半描述的是
            // 两个进程，而并排显示天然让人以为是同一个。Win10 任务栏搜索就是这个形态
            // （explorer 与 searchapp 各有一个 DLL 实例，各自覆盖不同的槽）。
            let src_name = if w.process_name.is_empty() {
                "?".to_string()
            } else {
                w.process_name.clone()
            };
            if w.differs_from_input_process(v.pid) {
                lines.push(format!("⚠ 本节来自 {}({})，非上方进程", src_name, w.pid));
            } else {
                lines.push(format!("来源: {}({})", src_name, w.pid));
            }
            lines.push(format!(
                "焦点: {} [{}] {}",
                hwnd_str(w.focus_hwnd),
                class_str(&w.focus_source_label),
                class_str(&w.focus_class)
            ));
            lines.push(format!(
                "顶层: {} {} band={}",
                hwnd_str(w.root_hwnd),
                class_str(&w.root_class),
                w.root_band
            ));
            let fg_name = if w.fg_process_name.is_empty() {
                "?".to_string()
            } else {
                w.fg_process_name.clone()
            };
            lines.push(format!(
                "前台: {} {}({}) {}",
                hwnd_str(w.fg_hwnd),
                fg_name,
                w.fg_pid,
                class_str(&w.fg_class)
            ));
            // 只在"前台属于别的进程"时加这一行——它是异常信号，常态下不该占地方。
            // per-app 配置按进程名匹配，而这一行正是"进程名不足以描述当前场景"的证据。
            if w.foreground_is_other_process() {
                lines.push("⚠ 前台窗口属于其他进程".to_string());
            }
        }
    }

    if s.tsf {
        lines.push("── TSF ──".to_string());
        if w.received {
            lines.push(format!(
                "DocMgr: {}{}",
                inst_str(w.docmgr_id),
                if w.docmgr_changed {
                    " (本次已换)"
                } else {
                    ""
                }
            ));
            lines.push(format!(
                "Context: {}  会话#{}",
                inst_str(w.context_id),
                w.focus_session_id
            ));
        }
    }

    if s.host {
        lines.push("── HostRender ──".to_string());
        if w.received {
            lines.push(format!(
                "白名单: {}  活跃: {}  band={}",
                yn(w.host_whitelisted),
                yn(w.host_active),
                w.host_band
            ));
        }
    }

    // 采集开关是随 HUD 打开才推给 DLL 的，此后要等下一次焦点事件才有数据。
    // 明说"切一下焦点"，否则用户会以为功能坏了。放在末尾出一次，而不是每个分区各出一次。
    if need_snapshot && !w.received {
        lines.push("(未采集：切换一次输入焦点)".to_string());
    }

    // 分区全关会渲染出一个空窗口——那看起来和"HUD 坏了"一模一样。给一行可操作的提示。
    if lines.is_empty() {
        lines.push("(所有分区已隐藏：右键 →「显示分类」)".to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_view() -> InputDiagView {
        InputDiagView {
            process_name: "chrome.exe".into(),
            pid: 4242,
            disabled: true,
            reason_text: "compartment".into(),
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
        assert!(lines[1].contains("禁用态: 是"));
        assert!(lines[1].contains("compartment"));
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
}
