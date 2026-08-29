//! 候选布局（竖排 / 横排）的**唯一**决策点。
//!
//! ⚠️ 「横排时文字怎么排」（旋转 90° / 文字竖排）**不在这条链上**：它是方案属性，
//! 归属判据是数据方案而非活跃方案，取值在 [`crate::schema_scope`] 与候选字体同处。
//! 本模块只在最后 [`Orientation::normalized`] 那一步把两根轴合起来。
//!
//! 设计见 `docs/design/mode-candidate-layout.md`。
//!
//! 这里刻意**不**做「进入模式时保存旧布局、退出时回放」——那需要在 `state.active` 的
//! 8 个清空点各写一遍恢复（此前 quick / add_word 两个模式就已各写了三处），漏一处的表现是
//! 候选窗卡在竖排且没有任何日志。改为**声明式重算**：
//!
//! > 任何时刻的方向 = f(全局基线, 当前模式意图)
//!
//! 「恢复」于是不再是一个需要被执行的动作，而是模式退出后重算的自然结果。副作用是**自愈**：
//! 即使某条退出路径什么都没做（失焦、或将来新增一条谁都没想到的退出路径），
//! 下一次候选显示会自动算回基线。
//!
//! 决策（纯函数 [`intent_for`] / [`orientation_for`]）与取值（`impl Coordinator` 的包装）
//! 刻意分开：前者可直接用 `Config` + `ModeKind` 测出完整矩阵，不必构造协调器。

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;
use wind_config::{Config, LayoutIntent, Orientation, OverlaySpec};
use wind_ui_types::UiCommand;

/// 「模式 → 布局意图」映射。**唯一一处**把这层对应关系写死的地方——新增模式只加一行。
///
/// 优先级：加词 > 独占模式 > 全局。加词面板是覆盖在任意输入态之上的临时面板，
/// 其显示需求（逐字确认）与底层模式无关，故优先。
///
/// 注意 `add_word` **不在** `state.active` 里（它是独立的 `add_word_active` 标志），
/// 所以「当前是什么模式」的判定必须把它一起收进来——这正是需要一个集中函数、
/// 而不是各模式内部各判各的理由。
///
/// `overlay` = 当前特殊模式的 `[overlay]` 段快照（`State::overlay_spec`）。特殊模式的
/// 配置住在方案文件而不是 `Config` 里，故它必须单独传入——保持本函数是纯函数，
/// 测试直接造 `OverlaySpec` 即可，不必构造 `EngineManager`。
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
        Some(ModeKind::TempPinyin) => Some(cfg.input.temp_pinyin.candidate_layout),
        Some(ModeKind::TempEnglish) => Some(cfg.input.temp_english.candidate_layout),
        Some(ModeKind::Url) => Some(cfg.input.url.candidate_layout),
        // 辅助码：候选布局沿用主路径（筛选不改呈现形态）。
        Some(ModeKind::AuxCode) => None,
        None => None,
    }
    // 下标越界（热重载删掉了该实例）回落 Follow——跟随全局是安全的默认，不猜方向。
    .unwrap_or_default()
}

/// 意图叠加到基线上得出实际方向。
///
/// 完整优先级链：**加词 > 独占模式 > 手动值 > 方案 > 全局基线**，
/// 其中前两级已由 [`intent_for`] 折成 `mode` 这一个参数。
///
/// ★ `Follow` 的语义是**「跟随下一层」**，不是「跟随全局基线」——加了方案层之后这两者
/// 不再等价。唯一能区分新旧实现的格子是「模式 `Follow` + 方案 `Vertical` + 基线横排」，
/// 旧实现给横排、新实现给竖排，测试必须钉住它。
///
/// `manual` = 用户在本方案期间手动切换的方向（已判过代际，见 [`crate::schema_scope`]）。
/// 它排在模式之下：模式（临英、快符、加词面板）是更内层的临时态，其布局意图是「这段时间
/// 的候选长这样」，本就该压过用户对整个方案的偏好；且模式退出后手动值自动重新生效。
pub(crate) fn orientation_for(
    mode: LayoutIntent,
    manual: Option<bool>,
    schema: LayoutIntent,
    baseline: Orientation,
) -> Orientation {
    /// 意图 → 竖排位。`Follow` 交给调用方续查下一层，故这里返回 `None`。
    ///
    /// 返回 `bool` 而不是 `Orientation`：这条链**只决定竖排位**，返回整个结构会让人
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
    // ★ 文字排列**原样带过**：它是方案属性，这条链上的四层（模式/手动/方案 layout/基线）
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
    /// 基线取运行时镜像 `candidate_orientation`，**不读 `config.ui.candidate.layout`**：
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
        // 第二根轴：方案声明的「横排时文字怎么排」。归属是**数据方案**，与候选字体同源
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

    /// 把期望方向下发 UI，仅在与**上次真正下发的值**不同时发送。
    ///
    /// 去重不是性能优化：没有它每次按键都会发一条 `SetCandidateLayout`，UI 侧
    /// `set_orientation` 触发重排，在首显时序敏感的路径上会引入抖动。
    ///
    /// ⚠️ 它同时是测试的假绿来源——断言要落在 [`Self::desired_orientation`] 的返回值上，
    /// 不要断言「有没有发出 UiCommand」：值没变时本就不发，测试会拿不到信号却看起来通过。
    ///
    /// 调用点是 `UpdateCandidates` 的**两个**发送点之前：`notify_ui_update`（主路径）与
    /// `show_add_word_preview`（加词面板走独立绘制路径，不经 notify_ui_update）。
    /// 同 channel 按序处理，UI 先改方向再填候选，不会闪。隐藏路径无需调用——布局只在
    /// 显示时有意义，退出模式必然伴随「隐藏 + 下次显示」，恢复发生在显示之前。
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
    /// ★ **必须与 [`Self::sync_candidate_layout`] 在同一批调用点**（`UpdateCandidates`
    /// 的两个发送点之前）。它的归属是 `effective_data_schema`，而那个判据随**输入语境**
    /// 逐次按键变化（临英/快符叠加），不是随方案代际变化——挂到 `sync_schema_scope`
    /// 那条代际驱动的路上会整个失效：`state.schema_scope_gen == generation` 直接 return，
    /// 临英进出根本不改代际。
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
mod tests {
    use super::*;
    use wind_config::TextOrientation;
    use wind_config::config::MixModeConfig;

    /// 造一份把各模式意图都设成指定值的配置。
    ///
    /// **不含特殊模式**——它的意图住在方案文件的 `[overlay]` 段，由 [`overlay_with`]
    /// 单独造出来经参数传入（见 `intent_for` 的 `overlay` 参数）。
    fn cfg_with(intent: LayoutIntent) -> Config {
        let mut c = Config::default();
        c.input.temp_pinyin.candidate_layout = intent;
        c.input.temp_english.candidate_layout = intent;
        c.input.url.candidate_layout = intent;
        c.input.add_word.candidate_layout = intent;
        c.schema.mix_modes = vec![MixModeConfig {
            candidate_layout: intent,
            ..Default::default()
        }];
        c
    }

    /// 造一份 `[overlay]` 段快照（= 特殊模式那一路的配置来源）。
    /// 只考察「模式意图 + 全局基线」两层的简写：无手动值、方案无意见。
    ///
    /// 方案层与手动层是后加的（`docs/design/schema-scoped-behavior.md` §3），既有用例
    /// 考察的是模式层语义，用它保持原样可读——**不要**让每个既有断言都去写两个 `None`。
    fn v_only_mode(mode: LayoutIntent, baseline: bool) -> bool {
        orientation_for(mode, None, LayoutIntent::Follow, bl(baseline)).vertical
    }

    /// 布尔基线 → `Orientation`（既有用例只表达竖排/横排两态）。
    fn bl(vertical: bool) -> Orientation {
        if vertical {
            Orientation::VERTICAL
        } else {
            Orientation::HORIZONTAL
        }
    }

    fn overlay_with(intent: LayoutIntent) -> OverlaySpec {
        OverlaySpec {
            candidate_layout: intent,
            ..Default::default()
        }
    }

    const MODES: &[ModeKind] = &[
        ModeKind::Mix(0),
        ModeKind::Special(0),
        ModeKind::TempPinyin,
        ModeKind::TempEnglish,
        ModeKind::Url,
    ];

    /// 全矩阵：每种模式 × 三种意图 × 两种基线。
    ///
    /// **`Follow` + 基线竖排是唯一能区分新旧语义的一格**（其余格三态与旧布尔表现相同）——
    /// 漏了它整个三态改造等于没测，故它在本用例里被显式断言而非顺带覆盖。
    #[test]
    fn every_mode_maps_intent_over_baseline() {
        for &mode in MODES {
            for (intent, baseline, want) in [
                (LayoutIntent::Follow, false, false),
                (LayoutIntent::Follow, true, true), // ← 三态相对布尔的全部增量
                (LayoutIntent::Vertical, false, true),
                (LayoutIntent::Vertical, true, true),
                (LayoutIntent::Horizontal, false, false),
                (LayoutIntent::Horizontal, true, false), // ← 旧布尔表达不了这一格
            ] {
                let cfg = cfg_with(intent);
                let ovs = overlay_with(intent);
                let got = v_only_mode(intent_for(&cfg, Some(&ovs), Some(mode), false), baseline);
                assert_eq!(
                    got, want,
                    "mode={mode:?} intent={intent:?} baseline={baseline} 应得 {want}"
                );
            }
        }
    }

    /// 无模式时一律跟随基线，与任何模式配置无关。
    #[test]
    fn no_active_mode_follows_baseline() {
        let cfg = cfg_with(LayoutIntent::Vertical);
        let ovs = overlay_with(LayoutIntent::Vertical);
        let ov = Some(&ovs);
        assert_eq!(intent_for(&cfg, ov, None, false), LayoutIntent::Follow);
        assert!(!v_only_mode(intent_for(&cfg, ov, None, false), false));
        assert!(v_only_mode(intent_for(&cfg, ov, None, true), true));
    }

    /// 加词优先于底层模式：底层要横排，加词仍按加词的意图。
    #[test]
    fn add_word_outranks_active_mode() {
        let mut cfg = cfg_with(LayoutIntent::Horizontal);
        cfg.input.add_word.candidate_layout = LayoutIntent::Vertical;
        let ovs = overlay_with(LayoutIntent::Horizontal);
        let ov = Some(&ovs);
        for &mode in MODES {
            assert_eq!(
                intent_for(&cfg, ov, Some(mode), true),
                LayoutIntent::Vertical,
                "mode={mode:?} 下加词应优先"
            );
        }
        // 无底层模式时同样生效。
        assert_eq!(intent_for(&cfg, None, None, true), LayoutIntent::Vertical);
    }

    /// mix 下标越界（热重载删掉了该实例）回落 Follow，不猜方向、不 panic。
    /// 特殊模式侧的对应情形是**快照缺失**（该方案没有 `[overlay]` 段），同样回落。
    #[test]
    fn out_of_range_instance_falls_back_to_follow() {
        let cfg = cfg_with(LayoutIntent::Vertical);
        assert_eq!(
            intent_for(&cfg, None, Some(ModeKind::Mix(9)), false),
            LayoutIntent::Follow
        );
        assert_eq!(
            intent_for(&cfg, None, Some(ModeKind::Special(0)), false),
            LayoutIntent::Follow,
            "无 [overlay] 快照时回落跟随全局"
        );
        // 回落后仍跟随基线两个方向。
        assert!(v_only_mode(
            intent_for(&cfg, None, Some(ModeKind::Mix(9)), false),
            true
        ));
        assert!(!v_only_mode(
            intent_for(&cfg, None, Some(ModeKind::Mix(9)), false),
            false
        ));
    }

    /// 特殊模式的意图**只来自 `[overlay]` 快照**，与下标、与 `Config` 都无关。
    ///
    /// 这条钉住的是本次下沉的核心：配置从 config.toml 的数组搬到了方案文件，
    /// 若有人把取值改回读 `cfg`，这里会红。
    #[test]
    fn special_mode_intent_comes_from_overlay_spec() {
        // cfg 里所有模式都是 Horizontal，快照是 Vertical——取值必须听快照的。
        let cfg = cfg_with(LayoutIntent::Horizontal);
        let ovs = overlay_with(LayoutIntent::Vertical);
        for idx in [0u8, 9u8] {
            assert_eq!(
                intent_for(&cfg, Some(&ovs), Some(ModeKind::Special(idx)), false),
                LayoutIntent::Vertical,
                "下标 {idx} 不参与取值"
            );
        }
    }

    /// 每个模式只读自己的配置项，不串味（防止映射表复制粘贴写错字段）。
    #[test]
    fn each_mode_reads_its_own_key() {
        let mut cfg = cfg_with(LayoutIntent::Follow);
        cfg.input.temp_english.candidate_layout = LayoutIntent::Horizontal;
        let ovs = overlay_with(LayoutIntent::Follow);
        let ov = Some(&ovs);
        assert_eq!(
            intent_for(&cfg, ov, Some(ModeKind::TempEnglish), false),
            LayoutIntent::Horizontal
        );
        for &mode in MODES {
            if matches!(mode, ModeKind::TempEnglish) {
                continue;
            }
            assert_eq!(
                intent_for(&cfg, ov, Some(mode), false),
                LayoutIntent::Follow,
                "改临英不应影响 {mode:?}"
            );
        }
    }

    /// 内置 quick_mix 出厂强制竖排（等价于旧 `quick_input.force_vertical = true`）。
    /// 守的是「默认值只能落在 default_mix_modes()、预置文件不写 mix_modes」这条约束——
    /// 若有人把默认改回 Follow，全局横排的用户会突然发现快捷输入变横排了。
    #[test]
    fn builtin_quick_mix_defaults_to_vertical() {
        let cfg = Config::default();
        assert!(
            v_only_mode(intent_for(&cfg, None, Some(ModeKind::Mix(0)), false), false),
            "内置 quick_mix 应出厂竖排"
        );
    }

    /// 加词出厂竖排（此前是硬编码强制竖排，迁成配置项后行为须不变）。
    #[test]
    fn add_word_defaults_to_vertical() {
        let cfg = Config::default();
        assert!(v_only_mode(intent_for(&cfg, None, None, true), false));
    }
    /// ★ 方案层：`Follow` 的语义是**「跟随下一层」**，不是「跟随全局基线」。
    ///
    /// 第一行是**唯一能区分新旧实现的一格**——旧实现里模式 `Follow` 直接取基线，
    /// 方案说了竖排也没用。漏了它，整个方案层等于没测。
    #[test]
    fn schema_layer_sits_between_manual_and_baseline() {
        use LayoutIntent::{Follow, Horizontal, Vertical};
        for (mode, manual, schema, baseline, want, why) in [
            (
                Follow,
                None,
                Vertical,
                false,
                true,
                "唯一区分新旧语义的一格",
            ),
            (
                Follow,
                None,
                Horizontal,
                true,
                false,
                "方案压过基线，另一个方向",
            ),
            (Follow, None, Follow, true, true, "方案无意见时才跟基线"),
            (Follow, None, Follow, false, false, "同上，另一个方向"),
            (Horizontal, None, Vertical, true, false, "模式压过方案"),
            (
                Vertical,
                None,
                Horizontal,
                false,
                true,
                "模式压过方案，另一个方向",
            ),
            (Follow, Some(false), Vertical, true, false, "手动值压过方案"),
            (
                Follow,
                Some(true),
                Horizontal,
                false,
                true,
                "手动值压过方案，另一个方向",
            ),
            (
                Horizontal,
                Some(true),
                Vertical,
                true,
                false,
                "模式压过手动值",
            ),
            (
                Follow,
                Some(true),
                Follow,
                false,
                true,
                "方案无意见时手动值仍压过基线",
            ),
        ] {
            assert_eq!(
                orientation_for(mode, manual, schema, bl(baseline)).vertical,
                want,
                "mode={mode:?} manual={manual:?} schema={schema:?} baseline={baseline}: {why}"
            );
        }
    }

    /// 方案层不得影响 `intent_for` 本身——它只回答「模式怎么想」。
    ///
    /// 分层的意义在于每层各答各的问题。若有人图省事把方案意图折进 `intent_for`，
    /// 「模式 Follow」与「模式没意见但方案有意见」就再也分不开，手动层无处插入。
    #[test]
    fn intent_for_answers_mode_layer_only() {
        let cfg = cfg_with(LayoutIntent::Follow);
        for &mode in MODES {
            assert_eq!(
                intent_for(
                    &cfg,
                    Some(&overlay_with(LayoutIntent::Follow)),
                    Some(mode),
                    false
                ),
                LayoutIntent::Follow,
                "mode={mode:?}：模式层没意见就该是 Follow，与方案层无关"
            );
        }
    }

    /// ★★ 两根轴**真的正交**：切横竖不碰文字排列，改文字排列不碰横竖。
    ///
    /// 这是把旋转从 `LayoutIntent` 里拆出来的**主要收益**，也是唯一能证明它拆干净了的
    /// 断言。曾经旋转是 `LayoutIntent` 的第三个取值，于是「蒙古文用户想临时切一下竖排」
    /// 会把方案声明的旋转一并丢掉——切回来还是竖排，用户完全无法把它和刚才那次切换联系起来。
    ///
    /// ⚠️ 断言必须比**整个** [`Orientation`]。只断言 `text` 没变的话，一个把 `vertical`
    /// 也一起写坏的实现照样通过。
    #[test]
    fn toggling_direction_never_touches_text_orientation() {
        use LayoutIntent::*;
        for text in TextOrientation::ALL.iter().copied() {
            let base = Orientation {
                vertical: false,
                text,
            };
            // 手动切竖排：只翻 vertical。
            let got = orientation_for(Follow, Some(true), Follow, base);
            assert_eq!(
                got,
                Orientation {
                    vertical: true,
                    text
                },
                "手动竖排把文字排列 {text:?} 弄丢了"
            );
            // 模式级强制横排：同样只动 vertical。
            let got = orientation_for(Horizontal, None, Vertical, base);
            assert_eq!(
                got,
                Orientation {
                    vertical: false,
                    text
                },
                "模式级横排把文字排列 {text:?} 弄丢了"
            );
        }
    }

    /// 竖排时文字排列必须归零——渲染端 `list_vertical` 同时看两位，
    /// `vertical && rotated` 是「既按竖排堆叠又整体转 90°」这种没有定义的状态。
    ///
    /// ★ 反向对照不可少：横排时**不得**归零，否则这条断言可以由「无条件清空」满足，
    /// 而那会让旋转功能整个失效。
    #[test]
    fn vertical_normalizes_text_orientation_away() {
        for text in TextOrientation::ALL.iter().copied() {
            let v = Orientation {
                vertical: true,
                text,
            }
            .normalized();
            assert_eq!(v, Orientation::VERTICAL, "竖排下 {text:?} 没被归零");
            assert!(!v.rotated() && !v.upright(), "竖排下两个派生位必须都为假");

            let h = Orientation {
                vertical: false,
                text,
            }
            .normalized();
            assert_eq!(h.text, text, "横排下 {text:?} 不该被归零");
        }
    }

    /// 派生位的真值表。渲染端只看这两位，读侧一律走它们、不要自己判 `text != Normal`。
    ///
    /// ★ `upright` 必须蕴含 `rotated`：只有 `upright` 为真时，`list_vertical` 判的是
    /// `rotated` ⇒ 列表退回横排的 Row，而叶子已经被切成竖着的格，
    /// 候选会变成一行「每个字都躺倒」的乱码。
    #[test]
    fn derived_bits_match_the_axis() {
        let cases = [
            (Orientation::HORIZONTAL, false, false),
            (Orientation::VERTICAL, false, false),
            (Orientation::ROTATED, true, false),
            (Orientation::UPRIGHT, true, true),
        ];
        for (o, rot, upr) in cases {
            assert_eq!(o.rotated(), rot, "{o:?} 的 rotated 位不对");
            assert_eq!(o.upright(), upr, "{o:?} 的 upright 位不对");
            assert!(
                !o.upright() || o.rotated(),
                "{o:?}：upright 必须蕴含 rotated"
            );
        }
    }

    /// 文字排列的字符串 round-trip（方案文件里手写的取值）。
    ///
    /// ★ 三个取值的字符串必须互不相同：只测 round-trip 的话，`as_str` 把直立也写成
    /// "rotated"、`from_str_or_normal` 再把 "rotated" 读成直立，两边一致、全绿，
    /// 而用户写的 rotated 会变成 upright。
    #[test]
    fn text_orientation_string_round_trips() {
        for t in TextOrientation::ALL.iter().copied() {
            assert_eq!(TextOrientation::from_str_or_normal(t.as_str()), t);
        }
        let names: std::collections::BTreeSet<&str> =
            TextOrientation::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(names.len(), TextOrientation::ALL.len(), "取值字符串撞了");
        // 用户手写了错的值：回落默认，不 panic、不猜。
        assert_eq!(
            TextOrientation::from_str_or_normal("nonsense"),
            TextOrientation::Normal
        );
        assert_eq!(
            TextOrientation::from_str_or_normal("UPRIGHT"),
            TextOrientation::Upright,
            "大小写不敏感"
        );
    }

    /// 配置字符串 ↔ `Orientation` 的 round-trip：`ime.toggle` 会把结果写回
    /// `ui.candidate.layout`，两个方向对不上就会「重启后方向变了」。
    #[test]
    fn layout_string_round_trips() {
        // ⚠️ 只有横竖两个：`ui.candidate.layout` 不承载文字排列那根轴
        // （承载了的话，`ime.toggle("layout")` 的写回会把方案意图覆盖掉）。
        const ALL: [Orientation; 2] = [Orientation::HORIZONTAL, Orientation::VERTICAL];
        for o in ALL {
            assert_eq!(Orientation::from_layout_str(o.layout_str()), o);
        }
        // ★ 取值的字符串必须**互不相同**。只测 round-trip 的话，两个取值写成同一个串、
        // 再原样读回来，两边一致、round-trip 全绿——而用户选的方向会在重启后变成另一个。
        let names: std::collections::BTreeSet<&str> = ALL.iter().map(|o| o.layout_str()).collect();
        assert_eq!(names.len(), ALL.len(), "取值字符串撞了：{names:?}");
        // 未知值按横排（出厂行为），不 panic。
        assert_eq!(
            Orientation::from_layout_str("nonsense"),
            Orientation::HORIZONTAL
        );
        assert_eq!(
            Orientation::from_layout_str("VERTICAL"),
            Orientation::VERTICAL,
            "大小写不敏感"
        );
        // ★ 旋转/直立**不从这个键来**：写进去也只当未知值按横排处理。
        assert_eq!(
            Orientation::from_layout_str("rotated"),
            Orientation::HORIZONTAL,
            "文字排列不该由 ui.candidate.layout 承载"
        );
    }
}
