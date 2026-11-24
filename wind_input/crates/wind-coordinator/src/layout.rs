//! 候选布局（竖排 / 横排）的唯一决策点。设计见 `docs/design/mode-candidate-layout.md`。
//!
//! 「横排时文字怎么排」（旋转 90° / 文字竖排）是方案属性，取值在 [`crate::schema_scope`]；
//! 本模块只在最后 [`Orientation::normalized`] 那一步把两根轴合起来。
//!
//! 刻意不做「进入模式时保存旧布局、退出时回放」（要在 `state.active` 的每个清空点各写恢复，
//! 漏一处就卡在竖排且无日志），而是声明式重算：方向 = f(全局基线, 当前模式意图)。
//! 「恢复」是重算的自然结果，任何退出路径都会在下次显示时自愈。
//!
//! 决策（纯函数 [`intent_for`] / [`orientation_for`]）与取值（`impl Coordinator` 的包装）
//! 分开，前者可直接用 `Config` + `ModeKind` 测出完整矩阵。

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;
use wind_config::{Config, LayoutIntent, Orientation, OverlaySpec};
use wind_ui_types::UiCommand;

/// 「模式 → 布局意图」映射。唯一一处写死这层对应关系的地方——新增模式只加一行。
///
/// 优先级：加词 > 独占模式 > 全局。`add_word` 不在 `state.active` 里（独立的
/// `add_word_active` 标志），故必须在这里一起收进来。
///
/// `overlay` = 当前特殊模式的 `[overlay]` 段快照（`State::overlay_spec`），单独传入以保持纯函数。
pub(crate) fn intent_for(
    cfg: &Config,
    overlay: Option<&OverlaySpec>,
    active: Option<ModeKind>,
    add_word: bool,
) -> LayoutIntent {
    if add_word {
        return cfg.input.add_word.candidate_layout;
    }
    match active {
        Some(ModeKind::Mix(i)) => cfg
            .schema
            .mix_modes
            .get(i as usize)
            .map(|m| m.candidate_layout),
        Some(ModeKind::Special(_)) => overlay.map(|o| o.candidate_layout),
        // 生僻字模式没有 [overlay] 段可读，布局跟随全局（同辅助码：只筛选、不改呈现形态）。
        Some(ModeKind::RareChar) => None,
        Some(ModeKind::Reverse) => Some(cfg.input.reverse.candidate_layout),
        Some(ModeKind::TempPinyin) => Some(cfg.input.temp_pinyin.candidate_layout),
        Some(ModeKind::TempEnglish) => Some(cfg.input.temp_english.candidate_layout),
        Some(ModeKind::Url) => Some(cfg.input.url.candidate_layout),
        Some(ModeKind::Email) => Some(cfg.input.email.candidate_layout),
        Some(ModeKind::Unicode) => Some(cfg.input.unicode.candidate_layout),
        // 辅助码：候选布局沿用主路径（筛选不改呈现形态）。
        Some(ModeKind::AuxCode) => None,
        None => None,
    }
    // 下标越界（热重载删掉了该实例）回落 Follow——跟随全局是安全的默认，不猜方向。
    .unwrap_or_default()
}

/// 意图叠加到基线上得出实际方向。
///
/// 完整优先级链：加词 > 独占模式 > 手动值 > 方案 > 全局基线，前两级已由 [`intent_for`]
/// 折成 `mode`。`Follow` 是「跟随下一层」而非「跟随全局基线」——「模式 `Follow` + 方案
/// `Vertical` + 基线横排」须得竖排，测试钉住这一格。
///
/// `manual` = 用户在本方案期间手动切换的方向（已判过代际，见 [`crate::schema_scope`]）。
/// 排在模式之下：模式是更内层的临时态，退出后手动值自动重新生效。
pub(crate) fn orientation_for(
    mode: LayoutIntent,
    manual: Option<bool>,
    schema: LayoutIntent,
    baseline: Orientation,
) -> Orientation {
    /// 意图 → 竖排位。`Follow` 交给调用方续查下一层，故这里返回 `None`。
    ///
    /// 返回 `bool` 而不是 `Orientation`：这条链只决定竖排位，返回整个结构会让人
    /// 以为它也管文字排列，而各层的 `Orientation` 常量里那一位恒是 `Normal`——
    /// 一路 `or` 下来就把 baseline 带的方案意图冲掉了（本轮正是这么写错的，
    /// 且只有「手动切一下方向」这一条路径能测出来）。
    fn of(intent: LayoutIntent) -> Option<bool> {
        match intent {
            LayoutIntent::Vertical => Some(true),
            LayoutIntent::Horizontal => Some(false),
            LayoutIntent::Follow => None,
        }
    }
    // `manual` 本就是 Option<bool>，与 of() 同型，直接进链。
    let vertical = of(mode)
        .or(manual)
        .or_else(|| of(schema))
        .unwrap_or(baseline.vertical);
    // 文字排列原样带过：它是方案属性，这条链上的四层（模式/手动/方案 layout/基线）
    // 没有一层有资格改它。用户临时切一下方向不该把方案声明的「这套文字要竖着写」丢掉——
    // 这正是把两根轴拆开的主要收益。
    Orientation {
        vertical,
        text: baseline.text,
    }
}

impl Coordinator {
    /// 当前生效的布局意图（[`intent_for`] 的取值包装）。
    pub(crate) fn layout_intent(&self, state: &State) -> LayoutIntent {
        let rt = self.rt();
        intent_for(
            &rt.config,
            state.overlay_spec.as_ref(),
            state.active,
            state.add_word_active,
        )
    }

    /// 期望的候选方向（竖排位 + 旋转位）。
    ///
    /// 基线取运行时镜像 `candidate_orientation`，不读 `config.ui.candidate.layout`：
    /// 命令栏 `ime.toggle("layout")` 改的是镜像，config 要等写盘 + 热重载回灌才跟上，
    /// 期间读 config 会按旧方向恢复。此前的 `force_vertical` 实现读的正是 config，
    /// 这是它的既存缺陷之一。
    pub(crate) fn desired_orientation(&self, state: &State) -> Orientation {
        let baseline = *self
            .candidate_orientation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut want = orientation_for(
            self.layout_intent(state),
            state.layout_manual,
            self.schema_layout_intent(),
            baseline,
        );
        // 第二根轴：方案声明的「横排时文字怎么排」。归属是数据方案，与候选字体同源
        // （见 `candidate_text_orientation_of`），故不经上面那条「模式 > 手动 > 方案 > 基线」
        // 的链——那条链回答的是用户的呈现偏好，这一根回答的是这套文字怎么写。
        want.text = self.candidate_text_orientation_of(state);
        // 竖排时归零：渲染端的 `list_vertical` 同时看两位，不归一化会得到「既按竖排堆叠
        // 又整体转 90°」这种没有定义的状态。归一化只此一处。
        want.normalized()
    }

    /// 当前期望的候选方向（测试/诊断用，对齐 `debug_in_temp_pinyin` 的既有形态）。
    pub fn debug_desired_vertical(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.desired_orientation(&state).vertical
    }

    /// 当前是否为旋转态。与 [`Self::debug_desired_vertical`] 分开两个方法而不是返回元组：
    /// 既有的六处断言只关心竖排位，改成元组会让它们全部变成 `.0`，读起来像在取魔法下标。
    pub fn debug_desired_rotated(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.desired_orientation(&state).rotated()
    }

    /// 当前是否为「文字直立」（对联式）。
    pub fn debug_desired_upright(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.desired_orientation(&state).upright()
    }

    /// 把期望方向下发 UI，仅在与上次真正下发的值不同时发送。
    ///
    /// 去重不是性能优化：每键都发 `SetCandidateLayout` 会触发 UI 重排、在首显路径上抖动。
    /// 测试因此要断言 [`Self::desired_orientation`] 的返回值，别断言「有没有发出 UiCommand」。
    ///
    /// 调用点是 `UpdateCandidates` 的两个发送点之前：`notify_ui_update`（主路径）与
    /// `show_add_word_preview`（加词面板独立绘制）。同 channel 按序处理，不会闪。隐藏路径
    /// 无需调用：恢复发生在下次显示之前。
    pub(crate) fn sync_candidate_layout(&self, state: &State) {
        let want = self.desired_orientation(state);
        let mut last = self
            .candidate_layout_sent
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *last != want {
            *last = want;
            let _ = self.ui_tx.send(UiCommand::SetCandidateLayout {
                vertical: want.vertical,
                rotated: want.rotated(),
                upright: want.upright(),
            });
        }
    }

    /// 把方案级候选字体下发 UI，同样只在变化时发送。
    ///
    /// 必须与 [`Self::sync_candidate_layout`] 在同一批调用点：归属判据 `effective_data_schema`
    /// 随输入语境逐键变化（临英/快符叠加），挂到代际驱动的 `sync_schema_scope` 上会整个失效。
    pub(crate) fn sync_candidate_font(&self, state: &State) {
        let want = self.candidate_font_of(state);
        let mut last = self
            .candidate_font_sent
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *last != want {
            last.clone_from(&want);
            let _ = self.ui_tx.send(UiCommand::SetCandidateTextFamily(want));
        }
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
