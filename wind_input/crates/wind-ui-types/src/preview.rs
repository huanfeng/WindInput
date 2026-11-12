//! 设置端主题预览：协调器请渲染端按当前外观画一帧（[`UiCommand::RenderCandidatePreview`]），
//! 默认是候选窗（[`PreviewKind::Candidate`]），也可画悬浮工具栏（[`PreviewKind::Toolbar`]）。
//!
//! [`UiCommand::RenderCandidatePreview`]: crate::UiCommand::RenderCandidatePreview

use crate::{CandidateItem, ToolbarItem, ToolbarState};

/// 预览渲染器版本：示例内容、排版或绘制（wind-scene / wind-ui 的候选窗管线）有改动、会让同一份
/// 输入画出不同像素时递增。它进预览缓存键，旧图随之失效。
pub const CANDIDATE_PREVIEW_RENDER_VERSION: u32 = 2;

/// 示例编码：拼音带音节分隔符 `'`，显示时按 `ui.candidate.syllable_separator` 换（见
/// [`preview_preedit`]）。
pub const PREVIEW_PREEDIT: &str = "ni'hao";

/// 按音节分隔符显示的示例编码（`sep` 可为空串）。与实机编码栏同一换法：只换 `'`。
pub fn preview_preedit(sep: &str) -> String {
    PREVIEW_PREEDIT.replace('\'', sep)
}

/// 示例候选（正文, 注释）：按每页候选数取前若干个，第三个带注释。改动要递增
/// [`CANDIDATE_PREVIEW_RENDER_VERSION`]（缓存键不含示例内容本身）。设置端预览（协调器）与
/// 主题编辑器的 wasm 预览共用。
pub const PREVIEW_CANDIDATES: [(&str, &str); 10] = [
    ("你好", ""),
    ("拟好", ""),
    ("你", "wqiy"),
    ("尼", ""),
    ("泥", ""),
    ("妮", ""),
    ("倪", ""),
    ("逆", ""),
    ("腻", ""),
    ("匿", ""),
];

/// 示例候选的前 `n` 个（钳到 1..=10），第 `i` 个的序号标签取 `label(i)`。
pub fn preview_candidates(n: usize, label: impl Fn(usize) -> String) -> Vec<CandidateItem> {
    let n = n.clamp(1, PREVIEW_CANDIDATES.len());
    PREVIEW_CANDIDATES[..n]
        .iter()
        .enumerate()
        .map(|(i, (text, comment))| CandidateItem {
            text: (*text).to_string(),
            text_styled: Default::default(),
            code: String::new(),
            label: label(i),
            tooltip: Default::default(),
            comment: (*comment).to_string().into(),
            comment_above: Default::default(),
            no_index: false,
        })
        .collect()
}

/// 序号标签在没有用户设置时的裁决：主题槽位（非空）优先，否则数字（第 10 个是 `0`）。
pub fn theme_index_label(theme_labels: &[String], i: usize) -> String {
    if let Some(s) = theme_labels.get(i).filter(|s| !s.is_empty()) {
        return s.clone();
    }
    if i == 9 {
        return "0".to_string();
    }
    (i + 1).to_string()
}

/// 预览出图的单边像素上限。预览请求来自 ctrl 管道，参数与草稿外观都不可信：主题尺寸、
/// 字号、scale、fit 叠乘起来能要出几十 GB 的缓冲，分配失败即整个服务进程 abort。
pub const MAX_PREVIEW_EDGE_PX: u32 = 8192;
/// 预览出图的总像素上限（RGBA 约 96 MiB）。
pub const MAX_PREVIEW_PIXELS: u64 = 24 * 1024 * 1024;
/// 草稿工具栏格数上限：实机能配的条目远少于此，再多只会把条拉长。
pub const MAX_PREVIEW_TOOLBAR_ITEMS: usize = 64;

/// `w`×`h` 设备像素是否在预览出图预算内。分配缓冲**之前**调。
pub fn preview_size_ok(w: u32, h: u32) -> bool {
    w > 0
        && h > 0
        && w <= MAX_PREVIEW_EDGE_PX
        && h <= MAX_PREVIEW_EDGE_PX
        && u64::from(w) * u64::from(h) <= MAX_PREVIEW_PIXELS
}

/// 把 fit 倍率压到出图预算内：`base_*` 是 r = 1 时的设备像素尺寸。压不进（r = 1 已超）
/// 时返回 1，由 [`preview_size_ok`] 拒掉。
pub fn cap_fit_ratio(r: f32, base_w: f32, base_h: f32) -> f32 {
    if r <= 1.0 || base_w <= 0.0 || base_h <= 0.0 {
        return r.max(1.0);
    }
    let edge = MAX_PREVIEW_EDGE_PX as f32;
    let by_area = (MAX_PREVIEW_PIXELS as f32 / (base_w * base_h)).sqrt();
    r.min(edge / base_w)
        .min(edge / base_h)
        .min(by_area)
        .max(1.0)
}

/// fit 目标框折算渲染倍率的上限：防异常大的 fit 撑爆 PNG（本意只是「设置端 Contain 后
/// 不发糊」，不是无界放大）。
pub const MAX_FIT_RATIO: f32 = 3.0;

/// 按 fit 目标框折算渲染倍率：`r = clamp(min(fit_w/n_w, fit_h/n_h), 1, [MAX_FIT_RATIO])`，
/// 只给一维就按给的那维算。`n_*` 是该形态的自然逻辑尺寸（渲染端取：candidate = 该请求下
/// 候选窗的自然窗口尺寸，toolbar = toolbar_frame 的逻辑尺寸）。自然尺寸非正或 fit ≤ 0
/// （应被上游校验挡住的异常值）时返回 1，不放大——fit 是出图更锐利的优化，不能让它把图撑爆。
pub fn preview_fit_ratio(
    fit_w: Option<u32>,
    fit_h: Option<u32>,
    natural_w: f32,
    natural_h: f32,
) -> f32 {
    let one = |fit: Option<u32>, n: f32| {
        if n <= 0.0 {
            return None; // 异常自然尺寸：不放大（fit 是优化，不能让它把图撑爆）。
        }
        fit.filter(|f| *f > 0).map(|f| f as f32 / n)
    };
    let r = match (one(fit_w, natural_w), one(fit_h, natural_h)) {
        (Some(a), Some(b)) => a.min(b),
        (Some(r), None) | (None, Some(r)) => r,
        (None, None) => 1.0,
    };
    r.clamp(1.0, MAX_FIT_RATIO)
}

/// `kind = Toolbar` 的固定样本状态（与 [`PREVIEW_CANDIDATES`] 同一条约定：样本值写死，
/// 改动即旧图失效）。中文模式、标签「中」、无大写锁定、标点/全角默认、简繁关、软键盘关、
/// 输入不被阻——工具栏预览不应跟着真机状态变，格式随 `ui.toolbar` 走。
///
/// 不直接用 [`ToolbarState::default`]：默认值今天恰好等于样本，但那不是它的语义
/// （default 描述「协调器还没推送前的初值」），样本要求的是「永不随时间变」。
pub fn preview_toolbar_state() -> ToolbarState {
    ToolbarState {
        chinese_mode: true,
        icon_label: "中".to_string(),
        caps_lock: false,
        full_width: false,
        chinese_punct: true,
        s2t_enabled: false,
        t2s_enabled: false,
        s2t_shown: false,
        soft_keyboard_on: false,
        input_blocked: false,
    }
}

/// 预览画什么：候选窗（默认）或悬浮工具栏。
///
/// 不派生 serde：本 crate 零 serde（见 crate 文档），线上的取值口径是
/// [`PreviewKind::parse`] / [`PreviewKind::as_str`]，RPC 层（wind-webdata）按它校验字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewKind {
    /// 示例候选窗（现状）。
    #[default]
    Candidate,
    /// 悬浮工具栏（`ui.toolbar` 画的那样：中英 / 标点 / 全半角 / 简繁 / 软键盘 / 设置格）。
    Toolbar,
}

impl PreviewKind {
    /// 条目字符串协议：`theme.renderPreview` 的 `kind` 参数按这一份判决。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "candidate" => Some(Self::Candidate),
            "toolbar" => Some(Self::Toolbar),
            _ => None,
        }
    }

    /// [`Self::parse`] 的逆（文档 / 调试用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Toolbar => "toolbar",
        }
    }
}

/// 一次预览请求。主题是**原始**主题（未叠外观覆盖），字体、字号、阴影开关等覆盖由渲染端按
/// 当前状态叠加，与实时候选窗同一处合并。
#[derive(Debug)]
pub struct CandidatePreviewRequest {
    pub theme: wind_theme::Resolved,
    /// 画哪个窗（缺省即候选窗：不带 kind 的旧请求等价 `Candidate`）。
    pub kind: PreviewKind,
    pub preedit: String,
    pub candidates: Vec<CandidateItem>,
    /// 竖排（不旋转、不直立）。`kind = Toolbar` 时语义为工具栏纵向排列
    /// （`ui.toolbar.vertical`）。
    pub vertical: bool,
    /// `kind = Toolbar` 时工具栏的渲染项（顺序即绘制顺序，协调器
    /// `parse_toolbar_items` 的产物；候选格等状态由 [`preview_toolbar_state`] 的样本表达）。
    /// `kind = Candidate` 时忽略。
    pub toolbar_items: Vec<ToolbarItem>,
    /// `kind = Toolbar` 时工具栏的状态（缺省 = [`preview_toolbar_state`] 的固定样本）。
    /// `kind = Candidate` 时忽略。
    pub toolbar_state: Option<ToolbarState>,

    /// DPI 缩放。
    pub scale: f32,
    /// 目标框逻辑尺寸（`fit_w` / `fit_h` 参数，可只给其一；`None` = 不约束，出图与旧请求
    /// 一致）。渲染端按 `min(fit/n)` 的 Contain 比折算渲染倍率（clamp 到 1..=[MAX_FIT_RATIO]）
    /// 重渲染，设置端 Contain 时只缩小或 1:1，不再双线性放大发糊。
    pub fit_w: Option<u32>,
    pub fit_h: Option<u32>,
    /// 设置端草稿外观；`None` = 用渲染端窗口当前的外观状态（与实时候选窗一致）。
    pub appearance: Option<PreviewAppearance>,
    /// 回执：渲染端处理完回送一次；不支持预览的渲染端直接丢掉请求（发送端随之断开）。
    pub reply: std::sync::mpsc::SyncSender<Result<CandidatePreviewImage, String>>,
    /// 请求方已不再等（超时或 RPC 已返回）时置位：渲染端开画前查它，置位的直接丢掉。
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// 设置端草稿配置折算出的外观状态，整组替代渲染端窗口的同名状态（字段与协调器
/// `apply_ui_config` 下发的那几条 `UiCommand` 一一对应）。方案级候选字体不在其中：它随方案走，
/// 草稿改不了，仍取窗口当前值。
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAppearance {
    /// `ui.font.family`（原样，空 = 跟随主题节点字族）。
    pub font_family: String,
    /// `ui.font.fallback`。
    pub font_fallback: Vec<String>,
    /// `ui.font.scripts`（脚本类名 → 字体链）。
    pub font_scripts: Vec<(String, Vec<String>)>,
    /// `ui.font.weight`（0 = 不指定）。
    pub font_weight: i32,
    /// `ui.candidate.font_size`（0 = 跟随主题）。
    pub font_size: f32,
    /// 是否采用主题阴影（`ui.candidate.shadow != "off"`）。
    pub shadow: bool,
    /// 编码嵌入候选行首（`ui.candidate.preedit_display`）。
    pub preedit_embedded: bool,
    /// `ui.candidate.pager_in_preedit`。
    pub pager_in_preedit: bool,
    /// `ui.candidate.pager_bar_display`（横排档, 竖排档）。
    pub pager_display: (String, String),
    /// `ui.candidate.page_number_display`（横排档, 竖排档）。
    pub page_number_display: (String, String),
    /// 尺寸下限：横排宽、竖排宽、横排高、竖排高、最小行数（含方案级覆盖）。
    pub min_size: [u32; 5],
}

impl CandidatePreviewRequest {
    /// 请求方是否已放弃。
    pub fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// 预览图：PNG（直通 alpha 的 RGBA）。
#[derive(Debug)]
pub struct CandidatePreviewImage {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// kind 的取值口径：`parse` / `as_str` 互逆，缺省是候选窗（与不带 kind 的旧请求一致）。
    #[test]
    fn preview_kind_values_are_round_trippable_and_default_to_candidate() {
        assert_eq!(PreviewKind::default(), PreviewKind::Candidate);
        for k in [PreviewKind::Candidate, PreviewKind::Toolbar] {
            assert_eq!(PreviewKind::parse(k.as_str()), Some(k), "{}", k.as_str());
        }
        assert_eq!(
            PreviewKind::parse("candidate"),
            Some(PreviewKind::Candidate)
        );
        assert_eq!(PreviewKind::parse("toolbar"), Some(PreviewKind::Toolbar));
        assert_eq!(
            PreviewKind::parse("Candidate"),
            None,
            "大小写敏感，拼错必须报错而不是静默画候选窗"
        );
        assert_eq!(PreviewKind::parse(""), None);
        assert_eq!(PreviewKind::parse("cand"), None);
    }

    /// fit 折算：缺省 = 1；Contain 比取两维 min、只给一维按该维算；clamp 到 1..=3；
    /// 自然尺寸非正与 fit ≤ 0 的异常值不放大（防撑爆，正常调用到不了这里）。
    #[test]
    fn preview_budget_rejects_oversized_and_caps_fit() {
        assert!(preview_size_ok(MAX_PREVIEW_EDGE_PX, 1));
        assert!(!preview_size_ok(MAX_PREVIEW_EDGE_PX + 1, 1), "单边超限");
        assert!(!preview_size_ok(8000, 8000), "单边没超但总像素超");
        assert!(!preview_size_ok(0, 10), "零尺寸");
        // 预算内的放大原样保留。
        assert_eq!(cap_fit_ratio(2.0, 100.0, 50.0), 2.0);
        // 压到单边上限。
        let r = cap_fit_ratio(3.0, 4000.0, 100.0);
        assert!((r * 4000.0 - MAX_PREVIEW_EDGE_PX as f32).abs() < 1.0);
        // 压到总像素上限。
        let r = cap_fit_ratio(3.0, 4000.0, 4000.0);
        assert!(((4000.0 * r) * (4000.0 * r)) as u64 <= MAX_PREVIEW_PIXELS + 4000 * 8);
        // r = 1 都装不下：不放大，交给 preview_size_ok 拒。
        assert_eq!(cap_fit_ratio(3.0, 9000.0, 10.0), 1.0);
    }

    #[test]
    fn fit_ratio_clamps_contain_ratio() {
        let n = (240.0, 64.0);
        assert_eq!(preview_fit_ratio(None, None, n.0, n.1), 1.0, "缺省不放大");
        assert_eq!(
            preview_fit_ratio(Some(240), Some(64), n.0, n.1),
            1.0,
            "fit = 自然尺寸 → 倍率 1"
        );
        assert_eq!(
            preview_fit_ratio(Some(480), Some(128), n.0, n.1),
            2.0,
            "两维取 min"
        );
        assert_eq!(
            preview_fit_ratio(Some(960), Some(128), n.0, n.1),
            2.0,
            "窄的一维说了算"
        );
        assert_eq!(
            preview_fit_ratio(Some(480), None, n.0, n.1),
            2.0,
            "只给一维按该维算"
        );
        assert_eq!(
            preview_fit_ratio(Some(2400), Some(640), n.0, n.1),
            3.0,
            "封顶 3"
        );
        assert_eq!(
            preview_fit_ratio(Some(120), Some(64), n.0, n.1),
            1.0,
            "fit 小于自然 → 不缩小，仍 1:1"
        );
        assert_eq!(preview_fit_ratio(Some(480), None, 0.0, 64.0), 1.0);
        assert_eq!(preview_fit_ratio(Some(0), None, n.0, n.1), 1.0);
    }

    #[test]
    fn sample_clamps_count_and_labels_fall_back_to_digits() {
        assert_eq!(preview_candidates(0, |_| String::new()).len(), 1);
        let all = preview_candidates(99, |i| theme_index_label(&["a".into(), String::new()], i));
        assert_eq!(all.len(), 10);
        let labels: Vec<_> = all.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["a", "2", "3", "4", "5", "6", "7", "8", "9", "0"]);
        assert_eq!(all[2].comment.as_str(), "wqiy");
        assert_eq!(preview_preedit("'"), "ni'hao");
        assert_eq!(preview_preedit(" "), "ni hao");
        assert_eq!(preview_preedit(""), "nihao");
    }
}
