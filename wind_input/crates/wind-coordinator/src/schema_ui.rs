//! 方案级通用 UI 覆盖的**消费端**：把「全局 `ui.candidate` ⊕ 活跃方案 `[candidate]` 补丁」
//! 折叠成生效配置，读取点统一走 [`Coordinator::eff_candidate`]。
//!
//! 声明与校验在 `wind-config`（`SCHEMA_OVERRIDES` 里 `fold = true` 的登记、
//! `Schema::behavior().candidate_ui`），折叠在 `UiCandidateConfig::folded_with`；
//! 本模块只负责「拿活跃方案的补丁、折叠、缓存」。设计见
//! `docs/design/schema-scoped-behavior.md` 的「通用折叠」一节。
//!
//! # 归属：活跃方案
//!
//! 与 `[candidate] layout` 同源（呈现偏好，跟用户当前选的方案走），**不是**
//! `effective_data_schema`——那是 `font_family` 一类「这些字怎么渲染」的数据属性。
//! 因此切方案才会变化，不随临英等语境逐键变化，可以按「切方案推一次」的节奏下发。
//!
//! # ⛔ 读取点不要绕过
//!
//! 登记为 `fold` 的键（如 `per_page`），读取时不得再写 `rt().config.ui.candidate.per_page`，
//! 否则方案里配了不生效——本仓最高频的报障形态。`wind-config/tests/scheme_fold_read_gates.rs`
//! 会扫源码拦这种写法。

use std::ops::Deref;
use std::sync::Arc;

use wind_config::SchemaBehavior;
use wind_config::config::UiCandidateConfig;
use wind_ui_types::UiCommand;

use crate::config_bundle::ConfigBundle;
use crate::coordinator::Coordinator;

/// 折叠结果的单项缓存。持有两个来源的 `Arc` 本身，`ptr_eq` 比对才不会被地址复用骗到。
pub(crate) struct EffCandidateCache {
    bundle: Arc<ConfigBundle>,
    behavior: Arc<SchemaBehavior>,
    folded: Arc<UiCandidateConfig>,
}

/// 当前生效的 `ui.candidate`。`Deref` 到 [`UiCandidateConfig`]，读法与原先一致。
///
/// 方案没有补丁（绝大多数情况）时不分配、不折叠，直接借全局那份。
pub(crate) struct EffCandidate {
    bundle: Arc<ConfigBundle>,
    folded: Option<Arc<UiCandidateConfig>>,
}

impl Deref for EffCandidate {
    type Target = UiCandidateConfig;
    fn deref(&self) -> &UiCandidateConfig {
        self.folded
            .as_deref()
            .unwrap_or(&self.bundle.config.ui.candidate)
    }
}

impl Coordinator {
    /// 活跃方案下生效的 `ui.candidate`（全局 ⊕ 方案 `[candidate]` 补丁）。
    ///
    /// 热路径安全：无补丁时只多一次 `active_behavior()`（带缓存的 `Arc` 读取）；
    /// 有补丁时折叠结果按「配置包 + 方案行为」的 `Arc` 身份缓存，两者任一被重建即自动失效
    /// （热重载换配置包、`invalidate_schema` 换方案行为）。
    pub(crate) fn eff_candidate(&self) -> EffCandidate {
        let bundle = self.rt();
        let behavior = self.engine_mgr.active_behavior();
        if behavior.candidate_ui.is_empty() {
            return EffCandidate {
                bundle,
                folded: None,
            };
        }
        let mut cache = self
            .eff_candidate_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.as_ref()
            && Arc::ptr_eq(&c.bundle, &bundle)
            && Arc::ptr_eq(&c.behavior, &behavior)
        {
            return EffCandidate {
                folded: Some(Arc::clone(&c.folded)),
                bundle,
            };
        }
        let folded = Arc::new(
            bundle
                .config
                .ui
                .candidate
                .folded_with(&behavior.candidate_ui),
        );
        *cache = Some(EffCandidateCache {
            bundle: Arc::clone(&bundle),
            behavior,
            folded: Arc::clone(&folded),
        });
        EffCandidate {
            bundle,
            folded: Some(folded),
        }
    }

    /// 下发候选窗尺寸下限。`rows` 按**生效的**每页数封顶（`effective_min_rows`），
    /// 所以方案改了 `per_page` 之后要重发——启动、热重载、切方案三处共用。
    pub(crate) fn send_candidate_min_size(&self) {
        let cand = self.eff_candidate();
        let _ = self.ui_tx.send(UiCommand::SetCandidateMinSize {
            width_horizontal: cand.min_window_width_horizontal,
            width_vertical: cand.min_window_width_vertical,
            height_horizontal: cand.min_window_height_horizontal,
            height_vertical: cand.min_window_height_vertical,
            rows: cand.effective_min_rows(),
        });
    }
}
