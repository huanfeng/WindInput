//! 设置端主题预览的协调器一侧：示例内容、序号标签与外观指纹，经 UI 通道请渲染端出图
//! （[`UiCommand::RenderCandidatePreview`]）。缓存与 RPC 形态在 wind-webdata。
//!
//! 本模块不碰渲染：桌面渲染端（wind-ui 的 UI 线程）用实时窗口的文字后端与外观覆盖画；
//! 不支持预览的渲染端（headless / 移动端）丢掉请求，回执通道随之断开，调用方立即得到错误。

use std::sync::mpsc;

use wind_ui_types::{
    CandidatePreviewImage, CandidatePreviewRequest, PreviewAppearance, PreviewKind, ToolbarItem,
    UiCommand,
};

use crate::Coordinator;

/// 示例内容在 wind-ui-types，与主题编辑器的 wasm 预览共用。
pub use wind_ui_types::PREVIEW_PREEDIT;

/// 渲染端的回执。
pub type PreviewReply = mpsc::Receiver<Result<CandidatePreviewImage, String>>;

impl Coordinator {
    /// 当前配置的候选排列是不是竖排（`ui.candidate.layout`，含方案级覆盖）。
    pub fn candidate_preview_vertical(&self) -> bool {
        wind_config::Orientation::from_layout_str(&self.eff_candidate().layout).vertical
    }

    /// 影响预览像素的外观配置指纹：候选窗配置（含方案级覆盖）、字体配置、方案级候选字体
    /// 与工具栏段（`ui.toolbar`，供 `kind = toolbar` 预览的磁盘缓存键——items 在协调器侧
    /// 才解析成渲染项，指纹取原配置整段）。有意放宽（整段都算），宁可多重画也不留旧图。
    pub fn candidate_preview_fingerprint(&self) -> String {
        let cand = self.eff_candidate();
        let font = &self.rt().config.ui.font;
        let tb = &self.rt().config.ui.toolbar;
        let scheme_font = self
            .candidate_font_sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        format!("{:?}\n{:?}\n{:?}\n{:?}", *cand, font, scheme_font, tb)
    }

    /// 设置端草稿（`theme.renderPreview` 的 `ui` 参数，草稿配置的 `ui` 子树）叠到当前生效配置上，
    /// 折算出预览要用的候选配置（含方案级覆盖）与渲染端外观。
    ///
    /// 不可信输入：只收注册表登记过的 `ui.*` 键，类型 / 枚举不符的叶子丢弃（同 `config.setItems`），
    /// 数值夹到设置页的取值范围，字体名与链长封顶。
    pub fn candidate_preview_draft(&self, ui: &toml::Value) -> anyhow::Result<PreviewDraft> {
        let mut leaves = Vec::new();
        if let toml::Value::Table(t) = ui {
            collect_valid_ui("ui", t, &mut leaves);
        } else {
            anyhow::bail!("ui 须是对象");
        }
        // 登记过的键整值替换（不深合并）：草稿删掉 `ui.font.scripts` 的一个子键，预览就不该
        // 再从当前配置里把它合并回来——与设置端保存时整值写回同一口径。
        let mut tree = self.rt().config.to_toml_value()?;
        for (key, v) in leaves {
            set_dotted(&mut tree, &key, v);
        }
        let mut cfg = wind_config::Config::from_toml_value(tree)?;
        clamp_preview_ui(&mut cfg.ui);
        let behavior = self.engine_mgr.active_behavior();
        let candidate = cfg.ui.candidate.folded_with(&behavior.candidate_ui);
        let font = &cfg.ui.font;
        let (pager_h, pager_v) = cfg.ui.candidate.pager_bar_display.both_str();
        let (num_h, num_v) = cfg.ui.candidate.page_number_display.both_str();
        let raw = &cfg.ui.candidate;
        let appearance = PreviewAppearance {
            font_family: font.family.clone(),
            font_fallback: font.fallback.clone(),
            font_scripts: font
                .scripts
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            font_weight: font.weight,
            font_size: raw.font_size,
            shadow: raw.shadow != "off",
            preedit_embedded: raw.preedit().embedded(),
            pager_in_preedit: raw.pager_in_preedit,
            pager_display: (pager_h.to_string(), pager_v.to_string()),
            page_number_display: (num_h.to_string(), num_v.to_string()),
            min_size: [
                candidate.min_window_width_horizontal,
                candidate.min_window_width_vertical,
                candidate.min_window_height_horizontal,
                candidate.min_window_height_vertical,
                candidate.effective_min_rows(),
            ],
        };
        Ok(PreviewDraft {
            vertical: wind_config::Orientation::from_layout_str(&candidate.layout).vertical,
            comments: cfg.ui.comment.enabled,
            theme_name: self.preview_theme_name(&cfg.ui.theme.name),
            theme_style: cfg.ui.theme.style.clone(),
            candidate,
            appearance,
            toolbar_vertical: cfg.ui.toolbar.vertical,
            // 草案里没有 `ui.toolbar` 段的键时，这些就是当前生效值：draft 树以当前配置打底，
            // 叠层只整值替换登记过的键，天然带出「改了什么、没改什么」。
            toolbar_items: crate::coordinator::parse_toolbar_items(
                &cfg.ui.toolbar.items,
                &cfg.ui.toolbar.buttons,
            ),
        })
    }

    /// 草稿主题的实际出图对象：没装 / 被定制版藏掉时同实机一样降级为
    /// [`crate::handle_mode::FALLBACK_THEME`]（不记日志：草稿还没保存）。
    fn preview_theme_name(&self, want: &str) -> String {
        let ok = wind_theme::is_plain_slug(want)
            && !wind_config::Config::custom_hides_theme(want)
            && wind_theme::theme::find_theme_dir(&self.theme_search_dirs(), want).is_some();
        if ok {
            want.to_string()
        } else {
            crate::handle_mode::FALLBACK_THEME.to_string()
        }
    }

    /// 工具栏预览的缺省格子（`kind = toolbar` 且 `draft` 没带发布会上时的格子来源）：
    /// 当前生效配置的 `ui.toolbar`，与实机 `apply_ui_config` 下发的同一份解析。
    fn toolbar_items_effective(&self) -> Vec<ToolbarItem> {
        let tb = &self.rt().config.ui.toolbar;
        crate::coordinator::parse_toolbar_items(&tb.items, &tb.buttons)
    }

    /// 投递一次预览请求，返回回执。`theme` 是原始主题（外观覆盖由渲染端叠加）；`draft` 给了就按
    /// 草稿出图（候选数、序号、注释开关取草稿，外观整组替代渲染端当前状态）；
    /// `kind = Toolbar` 时画的是工具栏（格子取草稿的 `ui.toolbar.items`，没带草稿取当前生效配置）。
    /// 不再等结果时把 `cancel` 置位，渲染端排到它时就直接丢掉。
    #[allow(clippy::too_many_arguments)] // 与 trait 同款：fit 两维直排，见 web_host 上的注
    pub fn request_candidate_preview(
        &self,
        theme: wind_theme::Resolved,
        vertical: bool,
        scale: f32,
        kind: PreviewKind,
        draft: Option<&PreviewDraft>,
        fit_w: Option<u32>,
        fit_h: Option<u32>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> PreviewReply {
        let live = self.eff_candidate();
        let cand: &wind_config::config::UiCandidateConfig = match draft {
            Some(d) => &d.candidate,
            None => &live,
        };
        let mut candidates = wind_ui_types::preview_candidates(cand.per_page, |i| {
            crate::coordinator::pick_index_label(cand, &theme.views.index_labels, i)
        });
        if draft.is_some_and(|d| !d.comments) {
            for c in &mut candidates {
                c.comment = Default::default();
            }
        }
        // 工具栏预览复用候选样本填充 candidates（渲染端在 toolbar 分支不消费它们），
        // 让请求结构不用拆成两个：样本值无关紧要，别读它。
        let (tx, rx) = mpsc::sync_channel(1);
        let toolbar_items = match kind {
            PreviewKind::Toolbar => draft
                .map(|d| d.toolbar_items.clone())
                .unwrap_or_else(|| self.toolbar_items_effective()),
            PreviewKind::Candidate => Vec::new(),
        };
        let req = CandidatePreviewRequest {
            theme,
            kind,
            preedit: wind_ui_types::preview_preedit(&cand.syllable_separator),
            candidates,
            vertical,
            scale,
            fit_w,
            fit_h,
            toolbar_items,
            toolbar_state: None,
            appearance: draft.map(|d| d.appearance.clone()),
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

/// [`Coordinator::candidate_preview_draft`] 的结果。
#[derive(Debug, Clone)]
pub struct PreviewDraft {
    /// 草稿的 `ui.candidate`，已叠方案级覆盖。
    pub candidate: wind_config::config::UiCandidateConfig,
    /// 草稿的候选排列是否竖排（`layout` 参数缺省时用）。
    pub vertical: bool,
    /// 草稿的 `ui.comment.enabled`：关了示例候选不带注释。
    pub comments: bool,
    /// 草稿的 `ui.theme.name` / `ui.theme.style`（`name` / `style` 参数缺省时用）。
    pub theme_name: String,
    pub theme_style: String,
    pub appearance: PreviewAppearance,
    /// 草稿的工具栏纵向排列（`ui.toolbar.vertical`；`kind = toolbar` 预览的缺省朝向）。
    pub toolbar_vertical: bool,
    /// 草稿的工具栏渲染项（`ui.toolbar.items`，`parse_toolbar_items` 的产物；
    /// `kind = toolbar` 预览的格子来源。草案没写这条键 = 当前生效值）。
    pub toolbar_items: Vec<ToolbarItem>,
}

/// 字体名、链长的上限（草稿是不可信输入）。
const MAX_FONT_NAME_CHARS: usize = 256;
const MAX_FONT_CHAIN: usize = 32;
const MAX_SEPARATOR_CHARS: usize = 8;

/// 逐层下钻草稿：静态注册表登记过的键（`field`；动态的注释寻址键不收——`Config` 里没有同名
/// 字段）整值过注册表校验（Map / 结构数组也按整值），通过的收进 `out`；未登记的表继续下钻，
/// 其余丢弃。
fn collect_valid_ui(prefix: &str, t: &toml::Table, out: &mut Vec<(String, toml::Value)>) {
    for (k, v) in t {
        let key = format!("{prefix}.{k}");
        if wind_config::config_schema::field(&key).is_some() {
            if wind_config::config_schema::validate(&key, v).is_ok() {
                out.push((key, v.clone()));
            }
        } else if let toml::Value::Table(sub) = v {
            collect_valid_ui(&key, sub, out);
        }
    }
}

/// 按点分路径整值写进 `tree`（中间缺的表补上；路径上撞到非表就放弃这一项）。
fn set_dotted(tree: &mut toml::Value, key: &str, v: toml::Value) {
    let mut cur = tree;
    let mut parts = key.split('.').peekable();
    while let Some(p) = parts.next() {
        let toml::Value::Table(t) = cur else { return };
        if parts.peek().is_none() {
            t.insert(p.to_string(), v);
            return;
        }
        cur = t
            .entry(p)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    }
}

/// 数值夹到设置页的取值范围；字体名与链长封顶（只影响预览，不写回）。
fn clamp_preview_ui(ui: &mut wind_config::config::UiConfig) {
    let c = &mut ui.candidate;
    c.per_page = c.per_page.clamp(1, 10);
    c.font_size = if c.font_size.is_finite() {
        c.font_size.clamp(0.0, 72.0)
    } else {
        0.0
    };
    for v in [
        &mut c.min_window_width_horizontal,
        &mut c.min_window_width_vertical,
        &mut c.min_window_height_horizontal,
        &mut c.min_window_height_vertical,
    ] {
        *v = (*v).min(1200);
    }
    c.min_rows = c.min_rows.min(9);
    if let Some((i, _)) = c.syllable_separator.char_indices().nth(MAX_SEPARATOR_CHARS) {
        c.syllable_separator.truncate(i);
    }
    let f = &mut ui.font;
    f.weight = f.weight.clamp(0, 950);
    let cut = |s: &mut String| {
        if let Some((i, _)) = s.char_indices().nth(MAX_FONT_NAME_CHARS) {
            s.truncate(i);
        }
    };
    cut(&mut f.family);
    f.fallback.truncate(MAX_FONT_CHAIN);
    f.fallback.iter_mut().for_each(cut);
    for chain in f.scripts.values_mut() {
        chain.truncate(MAX_FONT_CHAIN);
        chain.iter_mut().for_each(cut);
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
        let reply = c.request_candidate_preview(
            theme,
            true,
            1.5,
            PreviewKind::Candidate,
            None,
            None,
            None,
            Default::default(),
        );
        let req = loop {
            match rx.recv().unwrap() {
                UiCommand::RenderCandidatePreview(r) => break r,
                _ => continue,
            }
        };
        assert_eq!(req.preedit, PREVIEW_PREEDIT, "出厂分隔符就是 '");
        assert!(req.vertical);
        assert_eq!(req.scale, 1.5);
        assert_eq!(req.fit_w, None, "缺省不约束");
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
    fn toolbar_request_fills_items_from_effective_config() {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        while rx.try_recv().is_ok() {}
        let reply = c.request_candidate_preview(
            wind_theme::Resolved::default(),
            false,
            1.0,
            PreviewKind::Toolbar,
            None,
            None,
            None,
            Default::default(),
        );
        let req = loop {
            match rx.recv().unwrap() {
                UiCommand::RenderCandidatePreview(r) => break r,
                _ => continue,
            }
        };
        assert!(
            !req.toolbar_items.is_empty(),
            "不带草稿时格子取当前生效 ui.toolbar"
        );
        assert!(req.toolbar_state.is_none(), "状态走渲染端的固定样本");
        assert!(
            !req.candidates.is_empty(),
            "候选样本照填（toolbar 分支不消费）"
        );
        let _ = reply;
    }

    #[test]
    fn draft_carries_toolbar_segment() {
        let c = Coordinator::new_headless(Config::default(), None);
        let ui: toml::Value =
            toml::from_str("[toolbar]\nvertical = true\nitems = [\"mode\", \"settings\"]").unwrap();
        let d = c.candidate_preview_draft(&ui).unwrap();
        assert!(d.toolbar_vertical, "草稿的 ui.toolbar.vertical");
        assert_eq!(
            d.toolbar_items,
            vec![ToolbarItem::Mode, ToolbarItem::Settings],
            "草稿的 ui.toolbar.items"
        );
    }

    #[test]
    fn headless_without_ui_disconnects_immediately() {
        let c = Coordinator::new_headless(Config::default(), None);
        let reply = c.request_candidate_preview(
            wind_theme::Resolved::default(),
            false,
            1.0,
            PreviewKind::Candidate,
            None,
            None,
            None,
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

    /// 草稿叠到当前配置：没给的键保留当前值，Map 整值收、未登记键丢、越界夹回。
    #[test]
    fn draft_overlays_current_config_and_sanitizes() {
        let mut cfg = Config::default();
        cfg.ui.font.family = "Live".into();
        cfg.ui.candidate.per_page = 7;
        let c = Coordinator::new_headless(cfg, None);
        let ui: toml::Value = toml::from_str(
            r#"
            [candidate]
            font_size = -3.0
            min_rows = 99
            per_page = 3
            shadow = "off"
            syllable_separator = " "
            bogus = 1
            [font]
            weight = 5000
            scripts = { latin = ["A", "B"] }
            [comment]
            enabled = false
            "#,
        )
        .unwrap();
        let d = c.candidate_preview_draft(&ui).unwrap();
        assert_eq!(d.appearance.font_family, "Live", "没给的键保留当前值");
        assert_eq!(d.candidate.per_page, 3);
        assert_eq!(d.appearance.font_size, 0.0);
        assert_eq!(d.appearance.font_weight, 950);
        assert_eq!(d.appearance.min_size[4], 3, "最小行数按每页数封顶");
        assert!(!d.appearance.shadow && !d.comments);
        assert_eq!(d.candidate.syllable_separator, " ");
        assert_eq!(
            d.appearance.font_scripts,
            vec![("latin".to_string(), vec!["A".to_string(), "B".to_string()])]
        );
        assert!(c.candidate_preview_draft(&toml::Value::Integer(1)).is_err());
    }

    /// 草稿里删掉一个子键（Map 整值），预览就不再用它：整值替换，不与当前配置深合并。
    #[test]
    fn draft_map_replaces_current_value_whole() {
        let mut cfg = Config::default();
        cfg.ui.font.scripts.insert("latin".into(), vec!["A".into()]);
        cfg.ui.font.scripts.insert("cjk".into(), vec!["B".into()]);
        let c = Coordinator::new_headless(cfg, None);
        let ui: toml::Value = toml::from_str("[font]\nscripts = { cjk = [\"B\"] }\n").unwrap();
        let d = c.candidate_preview_draft(&ui).unwrap();
        assert_eq!(
            d.appearance.font_scripts,
            vec![("cjk".to_string(), vec!["B".to_string()])]
        );
        // 动态注释寻址键不收（Config 里没有同名字段），也不让整份草稿失败。
        let ui: toml::Value = toml::from_str("[comment.template]\nmine = \"${code}\"\n").unwrap();
        assert!(c.candidate_preview_draft(&ui).is_ok());
    }
}
