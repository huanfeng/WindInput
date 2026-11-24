//! 悬停提示（候选气泡）的右键菜单：按段 / 按行复制、上屏，复制全部，截图此窗口。
//! 设计见 `docs/design/candidate-tooltip-sections.md` §7。
//!
//! 气泡显示结构（`TooltipDoc`）下发给 UI，原始行（取值）留在协调器的
//! `Coordinator::tooltip_page`；UI 右键时只回报「哪个候选、点中哪段哪行」，取值在这边做。
//!
//! # 两道一致性闸门
//!
//! 气泡随时可能被重算，段下标一错位「上屏此行」就取到别段内容。故：
//! 1. 弹菜单时核对 UI 所画与缓存的 `TooltipDoc` 指纹，不等就只给「截图此窗口」（「复制全部」
//!    也不给：缓存那份不是用户看见的）。
//! 2. 执行动作一律从弹菜单那一刻的整份快照（`TooltipMenuTarget::entry`）取值，标签与取值
//!    同源。上屏还要核对会话与当前候选，原文已变或会话已结束就放弃。
//!
//! 放弃时发 Toast「候选已变化，未执行」，并记 warn（不含候选原文）。

use crate::coordinator::Coordinator;
use crate::tooltip::RenderedTooltip;
use tracing::{debug, warn};
use unicode_segmentation::UnicodeSegmentation;
use wind_bridge::handler::KeyAction;
use wind_ui_types::{
    MenuAnchor, MenuCmd, MenuItemSpec, MenuKind, ToastKind, ToastPosition, TooltipHit, UiCommand,
};

/// 本页一个候选的气泡缓存。
#[derive(Debug, Clone)]
pub(crate) struct TooltipPageEntry {
    /// 候选完整原文（核对「还是不是那个候选」用）。
    pub(crate) text: String,
    pub(crate) rendered: RenderedTooltip,
}

/// 右键菜单弹出时的目标快照（仅在 UI 所画与缓存一致时才有）。
#[derive(Debug, Clone)]
pub(crate) struct TooltipMenuTarget {
    /// 页内下标。
    pub(crate) candidate: usize,
    /// 弹菜单那一刻的整份缓存条目：菜单标签由它生成，取值也只从它取。
    pub(crate) entry: TooltipPageEntry,
    pub(crate) hit: Option<TooltipHit>,
}

/// 放弃执行时的 Toast 文案（候选已变、会话已结束、没有可信快照）。
const ABANDONED_TOAST: &str = "候选已变化，未执行";

/// 菜单项里段名的长度上限（字素簇数，emoji 序列不被切开），超出截断加 `…`。菜单是一列
/// 窄条，段名是用户写的模板求值结果，可能很长（`编码{(${code_source})}` 求出长方案名）。
const MENU_LABEL_MAX_CHARS: usize = 8;

/// 菜单里指称一段的名字：段名，空则「第 N 段」（N 从 1 起），过长截断。
fn section_label(title: Option<&str>, section: usize) -> String {
    match title.filter(|t| !t.is_empty()) {
        Some(t) => match t.grapheme_indices(true).nth(MENU_LABEL_MAX_CHARS) {
            Some((cut, _)) => format!("{}…", &t[..cut]),
            None => t.to_string(),
        },
        None => format!("第 {} 段", section + 1),
    }
}

/// 按命中位置构建菜单（设计 §7.2）：
/// - 命中某段 ⇒ 复制「段名」· 上屏「段名」；
/// - 命中逐字段的某一行 ⇒ 另加 复制此行 · 上屏此行；
/// - 有可信快照 ⇒ 复制全部；
/// - 恒有 ⇒ 截图此窗口。
///
/// `entry` 为 `None` 表示没有可信快照（缓存里没有这个候选，或与 UI 所画不一致），此时
/// 只剩「截图此窗口」；命中越界时退化为「复制全部 · 截图此窗口」。
pub(crate) fn tooltip_menu_items(
    entry: Option<&TooltipPageEntry>,
    hit: Option<TooltipHit>,
) -> Vec<MenuItemSpec> {
    use MenuItemSpec as M;
    let cmd = |c: MenuCmd| MenuKind::Command(c);
    let mut items = Vec::new();
    if let (Some(e), Some(h)) = (entry, hit) {
        let s = usize::from(h.section);
        if let Some(sec) = e.rendered.doc.sections.get(s) {
            let name = section_label(sec.title.as_ref().map(wind_ui_types::StyledText::as_str), s);
            items.push(M::leaf(
                format!("复制「{name}」"),
                cmd(MenuCmd::TooltipCopySection),
                true,
                false,
            ));
            items.push(M::leaf(
                format!("上屏「{name}」"),
                cmd(MenuCmd::TooltipCommitSection),
                true,
                false,
            ));
            let per_char = e.rendered.per_char.get(s).copied().unwrap_or(false);
            if let (true, Some(l)) = (per_char, h.raw_line)
                && e.rendered.line_text(s, usize::from(l)).is_some()
            {
                items.push(M::leaf(
                    "复制此行",
                    cmd(MenuCmd::TooltipCopyLine),
                    true,
                    false,
                ));
                items.push(M::leaf(
                    "上屏此行",
                    cmd(MenuCmd::TooltipCommitLine),
                    true,
                    false,
                ));
            }
            items.push(M::separator());
        }
    }
    if entry.is_some() {
        items.push(M::leaf("复制全部", cmd(MenuCmd::TooltipCopy), true, false));
    }
    items.push(M::leaf(
        "截图此窗口",
        cmd(MenuCmd::TooltipScreenshot),
        true,
        false,
    ));
    items
}

/// 菜单命令 → 要复制 / 上屏的原文。命中与命令对不上（如命令要「此行」而命中在标题行）
/// 返回 `None`。
fn tooltip_value(
    entry: &TooltipPageEntry,
    hit: Option<TooltipHit>,
    cmd: MenuCmd,
) -> Option<String> {
    let r = &entry.rendered;
    match cmd {
        MenuCmd::TooltipCopy => Some(r.raw_plain_text()),
        MenuCmd::TooltipCopySection | MenuCmd::TooltipCommitSection => {
            r.section_text(usize::from(hit?.section))
        }
        MenuCmd::TooltipCopyLine | MenuCmd::TooltipCommitLine => {
            let h = hit?;
            r.line_text(usize::from(h.section), usize::from(h.raw_line?))
                .map(str::to_string)
        }
        _ => None,
    }
}

impl Coordinator {
    /// 右键悬停提示：核对 UI 所画与缓存一致、记下快照、弹出菜单。
    ///
    /// 先发 SetTooltipMenuOpen(true) 抑制 tooltip 的 WM_MOUSELEAVE 自动隐藏——
    /// 右键弹出菜单后鼠标会移到菜单窗口上，若不抑制 tooltip 会当场消失，菜单就指向一个
    /// 已不存在的窗口，「截图此窗口」会截空。抑制标志在菜单关闭时由 menu_close 统一清除。
    pub(crate) fn show_tooltip_menu(
        &self,
        x: i32,
        y: i32,
        candidate: i32,
        hit: Option<TooltipHit>,
        doc_fingerprint: u64,
    ) {
        let _ = self.ui_tx.send(UiCommand::SetTooltipMenuOpen(true));
        let target = {
            let page = self.tooltip_page.lock().unwrap_or_else(|e| e.into_inner());
            let idx = usize::try_from(candidate).ok();
            let entry = idx.and_then(|i| page.get(i));
            let consistent = entry.filter(|e| e.rendered.doc.fingerprint() == doc_fingerprint);
            if entry.is_some() && consistent.is_none() {
                debug!("悬停提示菜单：候选 #{candidate} 的气泡与 UI 所画不一致，只给截图");
            }
            consistent.zip(idx).map(|(e, i)| TooltipMenuTarget {
                candidate: i,
                entry: e.clone(),
                hit,
            })
        };
        let items = tooltip_menu_items(target.as_ref().map(|t| &t.entry), hit);
        *self
            .tooltip_menu_target
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = target;
        self.mark_menu_open(0, String::new());
        // Linux：气泡依附于候选，候选收起时菜单失去对象，与候选右键菜单同样随之收掉（见
        // `notify_ui_hide`）。Windows 那边 `HideCandidates` 本就连带收掉任何菜单。
        #[cfg(ext_presenter)]
        self.candidate_menu_open
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.ui_tx.send(UiCommand::ShowCandidateMenu {
            items,
            anchor: MenuAnchor::at_point(x, y),
        });
    }

    /// 执行悬停提示菜单的复制 / 上屏类命令。放弃时 Toast 告知。
    pub(crate) fn tooltip_menu_action(&self, cmd: MenuCmd) {
        let target = self
            .tooltip_menu_target
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(target) = target else {
            warn!("悬停提示菜单：没有可信的气泡快照，放弃 {cmd:?}");
            self.tooltip_abandoned();
            return;
        };
        let Some(value) = tooltip_value(&target.entry, target.hit, cmd) else {
            debug!("悬停提示菜单：{cmd:?} 与命中位置 {:?} 对不上", target.hit);
            return;
        };
        match cmd {
            MenuCmd::TooltipCommitSection | MenuCmd::TooltipCommitLine => {
                match self.tooltip_commit_action(&target, &value) {
                    Some(act) => {
                        // 不经按键出口，收尾在这里做（同鼠标点选，见 `crate::session_exit`）。
                        let act = {
                            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            self.finish_session_action(
                                st,
                                crate::session_exit::ActionOrigin::Tooltip,
                                act,
                            )
                        };
                        self.push_no_key_ctx_action(&act, true, false)
                    }
                    None => self.tooltip_abandoned(),
                }
            }
            _ => {
                let _ = self.ui_tx.send(UiCommand::CopyTooltipText(value));
            }
        }
    }

    fn tooltip_abandoned(&self) {
        self.show_toast(ABANDONED_TOAST, ToastPosition::BottomRight, ToastKind::Info);
    }

    /// 上屏悬停提示里的一段文本：走「上屏任意文本并结束会话」同一出口，不记词频、
    /// 不进联想（上屏的不是这个候选）。
    ///
    /// 放弃（返回 `None`）的两种情形：会话已结束（菜单打开期间已上屏 / 取消——组合没了，
    /// 宿主里的光标已不在用户右键时的语境）；目标候选原文已变（上屏会落在另一个候选的
    /// 组合上）。
    pub(crate) fn tooltip_commit_action(
        &self,
        target: &TooltipMenuTarget,
        text: &str,
    ) -> Option<KeyAction> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            warn!("悬停提示菜单：候选会话已结束，放弃上屏");
            return None;
        }
        let same = self
            .tooltip_page
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(target.candidate)
            .is_some_and(|e| e.text == target.entry.text);
        if !same {
            // 不打候选原文：info 以上不记用户输入（见 AGENTS.md 日志规范）。
            warn!(
                "悬停提示菜单：候选 #{} 已在菜单打开期间变化，放弃上屏",
                target.candidate
            );
            return None;
        }
        Some(self.commit_text_ending_session(&mut state, text))
    }
}

/// 一个候选悬停提示的一段（纯文本），给按下标取提示的宿主（移动端长按）用。
///
/// 行取原始行（[`RenderedTooltip::raw`]），不带桌面气泡的单行截断与折行——手机卡片
/// 自己按屏宽折行，截断的 `…` 只会丢信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateTooltipSection {
    /// 段名（已求值，如「编码(五笔)」「拆字」）；`None` = 无标题。
    pub title: Option<String>,
    /// 与桌面同义：标题与第一行写在同一行（`段名: 内容`）。
    pub inline: bool,
    /// 非空。
    pub lines: Vec<String>,
}

impl Coordinator {
    /// 第 `idx`（全量绝对下标）个候选的悬停提示，按 `ui.tooltip.sections` 渲染——
    /// 与桌面候选窗气泡同一份模板、同一份求值上下文（[`crate::coordinator::TooltipEvalCtx`]）。
    ///
    /// 越界返回 `None`；提示总开关关着或各段都为空返回空列表。编码段依赖的反查索引没建好时
    /// 这一次没有编码段（已在后台触发构建，绝不在此等）。
    pub fn candidate_tooltip_at(&self, idx: usize) -> Option<Vec<CandidateTooltipSection>> {
        let rt = self.rt();
        let tip = &rt.tooltip;
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let c = state.candidates.get(idx)?;
        let full = self.cand_display_text(&state, c);
        let disp = rt.config.ui.candidate.truncate_display(&full);
        let ctx = self.tooltip_eval_ctx(&state, tip);
        let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
        let rendered = self.render_candidate_tooltip(&state, c, &disp, &full, tip, &ctx, &reverse);
        Some(
            rendered
                .doc
                .sections
                .iter()
                .zip(rendered.raw)
                .map(|(sec, lines)| CandidateTooltipSection {
                    title: sec.title.as_ref().map(|t| t.as_str().to_string()),
                    inline: sec.inline,
                    lines,
                })
                .collect(),
        )
    }
}

#[cfg(test)]
#[path = "handle_tooltip_tests.rs"]
mod tests;
