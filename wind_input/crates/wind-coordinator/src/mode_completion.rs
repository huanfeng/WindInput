//! 网址与邮箱模式**共用**的补全候选源与上屏收尾。
//!
//! # 为什么两个模式共用一份，而不是各写各的
//!
//! `docs/design/prefix-hijack-modes.md` §5.2 末尾留了这条判据：两者的候选来源形态
//! 几乎一样（用过的记录 + 一份预置表，按使用频次优先），各写一份的话「最近使用优先」
//! 这类规则会分叉——分叉的表现是「邮箱后缀按频次排了、网址历史没排」，而两处代码看上去
//! 都对，没人会去比。故排序、去重、上限、上屏收尾统统收在本文件，两个模式只提供
//! 「候选从哪来」和「上屏时学什么」这两处差异。
//!
//! # 空格键语义（两个模式同时改）
//!
//! 网址模式此前恒无候选，空格 = 上屏缓冲原文。加了补全之后改为：
//!
//! - **有候选** → 上屏当前高亮候选；
//! - **无候选** → 上屏缓冲原文（与改动前逐字相同）。
//!
//! 这条是 2026-09-19 与用户拍的板。对既有网址模式用户的影响被「无候选走原路」这半条
//! 兜住：出厂 `input.url.history_enabled = false` ⇒ 永远没有候选 ⇒ 行为与从前一模一样。
//! 只有主动开了历史的人才会看到新语义，而那正是他要的东西。

use crate::coordinator::{Coordinator, State};
use wind_bridge::handler::KeyAction;
use wind_candidate::Candidate;
use wind_store::completion::CompletionKind;
use wind_store::stats::CommitSource;

/// 一个模式最多取多少条补全候选。
///
/// 候选窗自己会分页，这里的上限是**查询侧**的：历史表可以有几百条，全查出来排完序再
/// 交给只显示一页的窗口纯属浪费。取 30 是「翻几页也够用」与「别把整张表拖进内存」之间
/// 的折中。
const MAX_COMPLETION_CANDIDATES: usize = 30;

impl Coordinator {
    /// 刷新邮箱候选：用已打的后缀片段去匹配「学过的后缀 ∪ 预置后缀」，拼成完整邮箱。
    ///
    /// 排序 = 学习数据在前（它本身已按次数/最近排好），预置表在后按配置顺序。**先学过
    /// 的排前面**是这个功能的全部意义所在：用户的公司邮箱打过几次就该顶到 `qq.com`
    /// 前面去，而不是每次都要翻页找。
    pub(crate) fn update_email_candidates(&self, state: &mut State) {
        state.candidates.clear();
        self.reset_candidate_view(state);
        state.preedit = state.email_buffer.clone();

        let buffer = state.email_buffer.clone();
        let user = Self::email_user_part(&buffer).to_string();
        let typed = Self::email_suffix_part(&buffer).to_string();

        let suffixes = self.completion_suffixes(&typed);
        state.candidates = suffixes
            .into_iter()
            .map(|suffix| Candidate {
                text: format!("{user}@{suffix}"),
                ..Default::default()
            })
            .collect();
    }

    /// 候选用的后缀清单：学过的在前、预置的在后，按 `typed` 前缀过滤并去重。
    ///
    /// 拆成独立函数是为了让排序与去重这条规则**可单测**，不必绕过 `State` 与候选窗。
    fn completion_suffixes(&self, typed: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // ① 学过的后缀（已按 count/last_used 排好序）。
        if let Some(store) = self.store.as_ref() {
            match store.list_completions(
                CompletionKind::EmailSuffix,
                typed,
                0,
                MAX_COMPLETION_CANDIDATES,
            ) {
                Ok((rows, _)) => {
                    for (suffix, _) in rows {
                        if seen.insert(suffix.clone()) {
                            out.push(suffix);
                        }
                    }
                }
                // 学习数据读不出来不该让补全整个失效：预置表还在，退化成「没学过」即可。
                Err(e) => tracing::debug!("邮箱后缀学习数据读取失败，退化为仅预置表: {e}"),
            }
        }

        // ② 预置表，按配置顺序。已在 ① 出现过的跳过——同一个后缀出两条，用户怎么用都
        // 消不掉其中一条，这正是预置表与学习数据必须同域（都不带 `@`）的理由。
        for suffix in &self.rt().config.input.email.suffixes {
            if out.len() >= MAX_COMPLETION_CANDIDATES {
                break;
            }
            if suffix.is_empty() || !suffix.starts_with(typed) {
                continue;
            }
            if seen.insert(suffix.clone()) {
                out.push(suffix.clone());
            }
        }
        out.truncate(MAX_COMPLETION_CANDIDATES);
        out
    }

    /// 刷新网址候选：从历史里取以当前缓冲为前缀的记录。
    ///
    /// 历史开关关闭时**不查库也不出候选**——不是「查出来再丢掉」：关着的时候库里本就
    /// 没有数据，查询纯属每键一次无谓的读事务。
    pub(crate) fn update_url_candidates(&self, state: &mut State) {
        state.candidates.clear();
        self.reset_candidate_view(state);
        state.preedit = state.url_buffer.clone();

        if !self.rt().config.input.url.history_enabled {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let buffer = state.url_buffer.clone();
        match store.list_completions(
            CompletionKind::UrlHistory,
            &buffer,
            0,
            MAX_COMPLETION_CANDIDATES,
        ) {
            Ok((rows, _)) => {
                state.candidates = rows
                    .into_iter()
                    // 与缓冲逐字相同的那条不出：它上屏的结果与「无候选时上屏原文」完全
                    // 一样，留着只是占掉首选位置，让用户以为自己在选什么东西。
                    .filter(|(text, _)| text != &buffer)
                    .map(|(text, _)| Candidate {
                        text,
                        ..Default::default()
                    })
                    .collect();
            }
            Err(e) => tracing::debug!("网址历史读取失败，本次不出补全候选: {e}"),
        }
    }

    /// 当前高亮候选的文本。无候选（或下标越界）时 `None`。
    fn highlighted_completion(&self, state: &State) -> Option<String> {
        let gi = self.highlighted_global_index(state);
        state.candidates.get(gi).map(|c| c.text.clone())
    }

    /// 邮箱模式上屏：有候选上屏高亮候选，无候选上屏缓冲原文；随后学下后缀。
    pub(crate) fn commit_email(&self, state: &mut State) -> KeyAction {
        let text = self
            .highlighted_completion(state)
            .unwrap_or_else(|| state.email_buffer.clone());
        self.learn_email_suffix(&text);
        self.record_commit(&text, 0, -1, CommitSource::Email);
        self.exit_email_mode(state);
        self.notify_ui_hide();
        if text.is_empty() {
            KeyAction::ClearComposition
        } else {
            Self::commit_action(text, true)
        }
    }

    /// 网址模式上屏：有候选上屏高亮候选，无候选上屏缓冲原文；随后按开关记历史。
    pub(crate) fn commit_url(&self, state: &mut State) -> KeyAction {
        let text = self
            .highlighted_completion(state)
            .unwrap_or_else(|| state.url_buffer.clone());
        self.learn_url_history(&text);
        self.record_commit(&text, 0, -1, CommitSource::Url);
        self.exit_url_mode(state);
        self.notify_ui_hide();
        if text.is_empty() {
            KeyAction::ClearComposition
        } else {
            Self::commit_action(text, true)
        }
    }

    /// 学下这次用的邮箱后缀（`@` 之后的部分）。
    ///
    /// 跟随 `input.email.enabled`，没有第二道开关：记的是域名（`qq.com` 这类），**不含
    /// 用户名**，与「把打过的网址原文落盘」不是一个隐私量级。
    fn learn_email_suffix(&self, text: &str) {
        if !self.rt().config.input.email.enabled {
            return;
        }
        let suffix = Self::email_suffix_part(text);
        // 没打后缀就上屏（`abc@` 直接按空格）时无可学。`@` 也不该被当成后缀的一部分。
        if suffix.is_empty() {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if let Err(e) = store.record_completion(CompletionKind::EmailSuffix, suffix) {
            tracing::debug!("邮箱后缀学习写入失败: {e}");
        }
    }

    /// 按 `input.url.history_enabled` 记一条网址历史，并裁剪到上限。
    ///
    /// 裁剪跟在写入后面同步做，而不是另起定时任务：这条路径每次上屏才走一次，代价是
    /// 一次列举 + 至多一次写事务；而定时任务要多一个生命周期、多一处「关掉历史后残留
    /// 数据谁来清」的分叉。
    fn learn_url_history(&self, text: &str) {
        let (enabled, max) = {
            let rt = self.rt();
            (
                rt.config.input.url.history_enabled,
                rt.config.input.url.history_max as usize,
            )
        };
        if !enabled || text.is_empty() {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if let Err(e) = store.record_completion(CompletionKind::UrlHistory, text) {
            tracing::debug!("网址历史写入失败: {e}");
            return;
        }
        if let Err(e) = store.prune_completions(CompletionKind::UrlHistory, max) {
            tracing::debug!("网址历史裁剪失败: {e}");
        }
    }
}
