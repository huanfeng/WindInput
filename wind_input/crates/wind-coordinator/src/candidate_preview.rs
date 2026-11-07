//! 设置端主题预览的协调器一侧：示例内容、序号标签与外观指纹，经 UI 通道请渲染端出图
//! （[`UiCommand::RenderCandidatePreview`]）。缓存与 RPC 形态在 wind-webdata。
//!
//! 本模块不碰渲染：桌面渲染端（wind-ui 的 UI 线程）用实时窗口的文字后端与外观覆盖画；
//! 不支持预览的渲染端（headless / 移动端）丢掉请求，回执通道随之断开，调用方立即得到错误。

use std::sync::mpsc;

use wind_ui_types::{CandidateItem, CandidatePreviewImage, CandidatePreviewRequest, UiCommand};

use crate::Coordinator;

/// 示例编码。
pub const PREVIEW_PREEDIT: &str = "nihao";

/// 示例候选（正文, 注释）：按每页候选数取前若干个，第三个带注释。改动要递增
/// [`wind_ui_types::CANDIDATE_PREVIEW_RENDER_VERSION`]（缓存键不含示例内容本身）。
const PREVIEW_CANDIDATES: [(&str, &str); 10] = [
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

/// 渲染端的回执。
pub type PreviewReply = mpsc::Receiver<Result<CandidatePreviewImage, String>>;

impl Coordinator {
    /// 当前配置的候选排列是不是竖排（`ui.candidate.layout`，含方案级覆盖）。
    pub fn candidate_preview_vertical(&self) -> bool {
        wind_config::Orientation::from_layout_str(&self.eff_candidate().layout).vertical
    }

    /// 影响预览像素的外观配置指纹：候选窗配置（含方案级覆盖）、字体配置、方案级候选字体。
    /// 有意放宽（候选窗配置整段都算），宁可多重画也不留旧图。
    pub fn candidate_preview_fingerprint(&self) -> String {
        let cand = self.eff_candidate();
        let font = &self.rt().config.ui.font;
        let scheme_font = self
            .candidate_font_sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        format!("{:?}\n{:?}\n{:?}", *cand, font, scheme_font)
    }

    /// 投递一次预览请求，返回回执。`theme` 是原始主题（外观覆盖由渲染端叠加）。
    /// 不再等结果时把 `cancel` 置位，渲染端排到它时就直接丢掉。
    pub fn request_candidate_preview(
        &self,
        theme: wind_theme::Resolved,
        vertical: bool,
        scale: f32,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> PreviewReply {
        let cand = self.eff_candidate();
        let n = cand.per_page.clamp(1, PREVIEW_CANDIDATES.len());
        let candidates = PREVIEW_CANDIDATES[..n]
            .iter()
            .enumerate()
            .map(|(i, (text, comment))| CandidateItem {
                text: (*text).to_string(),
                text_styled: Default::default(),
                code: String::new(),
                label: crate::coordinator::pick_index_label(&cand, &theme.views.index_labels, i),
                tooltip: Default::default(),
                comment: (*comment).to_string().into(),
                comment_above: Default::default(),
                no_index: false,
            })
            .collect();
        let (tx, rx) = mpsc::sync_channel(1);
        let req = CandidatePreviewRequest {
            theme,
            preedit: PREVIEW_PREEDIT.to_string(),
            candidates,
            vertical,
            scale,
            reply: tx,
            cancel,
        };
        // 投递失败（UI 线程已退出）时请求连同回执发送端一起丢掉，接收端立即断开。
        let _ = self
            .ui_tx
            .send(UiCommand::RenderCandidatePreview(Box::new(req)));
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_config::Config;

    #[test]
    fn request_carries_sample_with_labels_and_per_page() {
        let mut cfg = Config::default();
        cfg.ui.candidate.per_page = 3;
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        while rx.try_recv().is_ok() {}
        let mut theme = wind_theme::Resolved::default();
        theme.views.index_labels = vec!["a".into(), String::new()];
        let reply = c.request_candidate_preview(theme, true, 1.5, Default::default());
        let req = loop {
            match rx.recv().unwrap() {
                UiCommand::RenderCandidatePreview(r) => break r,
                _ => continue,
            }
        };
        assert_eq!(req.preedit, PREVIEW_PREEDIT);
        assert!(req.vertical);
        assert_eq!(req.scale, 1.5);
        let labels: Vec<_> = req.candidates.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["a", "2", "3"], "主题槽位优先、空槽回落数字");
        assert!(req.candidates.iter().any(|c| !c.comment.is_empty()));
        req.reply
            .try_send(Ok(CandidatePreviewImage {
                width: 1,
                height: 2,
                png: vec![],
            }))
            .unwrap();
        assert_eq!(reply.recv().unwrap().unwrap().height, 2);
    }

    #[test]
    fn headless_without_ui_disconnects_immediately() {
        let c = Coordinator::new_headless(Config::default(), None);
        let reply = c.request_candidate_preview(
            wind_theme::Resolved::default(),
            false,
            1.0,
            Default::default(),
        );
        assert!(matches!(
            reply.recv_timeout(std::time::Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn fingerprint_follows_candidate_and_font_config() {
        let base =
            Coordinator::new_headless(Config::default(), None).candidate_preview_fingerprint();
        let mut cfg = Config::default();
        cfg.ui.candidate.font_size = 23.0;
        let sized = Coordinator::new_headless(cfg, None).candidate_preview_fingerprint();
        let mut cfg = Config::default();
        cfg.ui.font.family = "Noto Sans".into();
        let font = Coordinator::new_headless(cfg, None).candidate_preview_fingerprint();
        assert_ne!(base, sized);
        assert_ne!(base, font);
        assert_eq!(
            base,
            Coordinator::new_headless(Config::default(), None).candidate_preview_fingerprint()
        );
    }
}
