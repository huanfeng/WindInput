//! 设置端主题预览：协调器请渲染端按当前外观画一帧示例候选窗（[`UiCommand::RenderCandidatePreview`]）。
//!
//! [`UiCommand::RenderCandidatePreview`]: crate::UiCommand::RenderCandidatePreview

use crate::CandidateItem;

/// 预览渲染器版本：示例内容、排版或绘制（wind-scene / wind-ui 的候选窗管线）有改动、会让同一份
/// 输入画出不同像素时递增。它进预览缓存键，旧图随之失效。
pub const CANDIDATE_PREVIEW_RENDER_VERSION: u32 = 1;

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
    /// 回执：渲染端处理完回送一次；不支持预览的渲染端直接丢掉请求（发送端随之断开）。
    pub reply: std::sync::mpsc::SyncSender<Result<CandidatePreviewImage, String>>,
    /// 请求方已不再等（超时或 RPC 已返回）时置位：渲染端开画前查它，置位的直接丢掉。
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
