//! 设置端主题预览：协调器请渲染端按当前外观画一帧示例候选窗（[`UiCommand::RenderCandidatePreview`]）。
//!
//! [`UiCommand::RenderCandidatePreview`]: crate::UiCommand::RenderCandidatePreview

use crate::CandidateItem;

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

/// 一次预览请求。主题是**原始**主题（未叠外观覆盖），字体、字号、阴影开关等覆盖由渲染端按
/// 当前状态叠加，与实时候选窗同一处合并。
#[derive(Debug)]
pub struct CandidatePreviewRequest {
    pub theme: wind_theme::Resolved,
    pub preedit: String,
    pub candidates: Vec<CandidateItem>,
    /// 竖排（不旋转、不直立）。
    pub vertical: bool,
    /// DPI 缩放。
    pub scale: f32,
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
