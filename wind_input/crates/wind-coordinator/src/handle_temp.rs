//! 临时拼音 / 临时英文输入模式
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。
//! 触发键判定、进入/退出、候选刷新、按键处理、选词上屏。

use crate::coordinator::{
    Coordinator, ENGINE_MAX_CANDIDATES, State, TEMP_PINYIN_MAX_CANDIDATES, numpad_char, punct_char,
};
use crate::key_convert::printable_char;
use crate::pipeline::{ModeKind, Rewind};
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::{Candidate, CandidateSource};
use wind_engine::manager::ENGLISH_SCHEMA;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;
use wind_transform::fullwidth::to_full_width;

impl Coordinator {
    /// VK → 组合区前缀字符（统一映射，见 `keymap`；缺省回退反引号）。
    ///
    /// 用**带字母**的版本：z 经 `z_key_action` 进临拼时，组合区要显示用户按下的那个 `z`。
    /// 不带字母的版本对 VK_Z 返回 None、兜底成反引号，组合区会凭空显示一个没按过的 `` ` ``，
    /// 且回车归还引导字母的判据（首字符是否字母）也会因此永远不成立。
    pub(crate) fn temp_pinyin_prefix_for(key_code: u32) -> char {
        keymap::vk_to_prefix_char_with_letters(key_code).unwrap_or('`')
    }

    /// 当前按键是否为临时拼音的引导键。
    ///
    /// 数据源是 [`Self::bound_action_for`]（方案表 → 全局 `keys.key_actions` → `z_key_action`），
    /// 不再直接读 `input.temp_pinyin.trigger_keys`——那个字段已由 `normalize` 折算进
    /// `keys.key_actions`（设计文档五c）。
    ///
    /// ★ 顺带修好一处旧缺陷：本函数被「模式内二次按同键退出」「智能符号 press2 门控」
    /// 「设置页冲突检测」共用，而它们原先只看得到**全局** trigger_keys。方案级 `[key_actions]`
    /// 里绑的引导键对它们是不可见的——进了临拼后按那个键不退出、press2 也不武装。
    /// 换成统一来源后这些场景自动跟上。
    pub(crate) fn is_temp_pinyin_trigger(&self, key_code: u32) -> bool {
        matches!(
            self.bound_action_for(key_code),
            Some(wind_config::BoundAction::TempPinyin)
        )
    }

    /// z 三重身份裁决的「活码前缀」判据（对齐 Go `HasPrefix`）：码表引擎候选（含 BFS 前缀扫描）
    /// 或短语层存在以 `code` 开头的条目 → true。用于区分 z 作正常码字母（有前缀，如自定义 `zhang`）
    /// vs 临时拼音触发（死前缀，如标准五笔 86 的 z）。
    ///
    /// # ★★ 这是**存在性**判据，与 `build_candidates` 的显示门槛刻意不同源
    ///
    /// 本函数问的是「`code` 在这个方案里是不是活的编码」——是就不能把该键独占给功能，
    /// 否则那个字母开头的编码在本方案里彻底打不出来。它是**前瞻**的：`zzbd` 存在就说明 z 是
    /// 活码，哪怕此刻只打了一个 z、还没到显示短语的时候。故前缀查询用 `1`（存在即可）。
    ///
    /// `build_candidates` 读 `input.phrase.min_prefix`（默认 2），问的是另一件事：
    /// 「刚打了一个 z，现在该显示什么」——那是显示策略，用户设 2 就是不想打一个字母就弹
    /// 一屏短语（真机上 `zz*` 是 `1 标点 2 数字 3 字母 4 偏旁` 这样的 `$SS` 分组导航，
    /// 提前一级列出来尤其吵）。
    ///
    /// ⚠️ 曾把两者当成同一个问题，试过把门槛对齐到任一边，**两个方向都错**：
    /// 判据降到 2 → z 不再算活码 → 首键进模式，`zz*` 在该方案全部消失；
    /// 产出提到 1 → 按 z 就弹出整屏分组导航，用户设的 `min_prefix` 形同虚设。
    /// （2026-08-08、08-09 各栽一次。）
    ///
    /// 正确的关系是：**让位是对的，但让位那一帧必须有反馈**——反馈由
    /// [`Self::inject_bound_letter_hint`] 提供（重复上屏 / 模式提示），不靠短语候选顶上。
    pub(crate) fn has_code_prefix(&self, code: &str) -> bool {
        if code.is_empty() {
            return false;
        }
        // 码表 / 用户词（convert 内含前缀扫描）。
        if !self.engine_mgr.convert(code, 1).candidates.is_empty() {
            return true;
        }
        // 短语层：精确或前缀命中。
        let phrases = self.phrases.read().unwrap_or_else(|e| e.into_inner());
        if phrases.is_empty() {
            return false;
        }
        let recent = self.recent_commits_snapshot();
        // 空宿主：本判据只问「有没有这个码的短语」，不关心它显示成什么，故不值得为它
        // 读一次剪贴板 / 查一次词库（这是每次按键都会过的路径）。
        //
        // ⚠️ 代价是**依赖瞬时状态的短语在这里判为不存在**：`{dict.rev(clip())}` 这类
        // 纯模板短语在空宿主下渲染为空，会被空串守卫丢掉。当前调用方传进来的都是
        // 单字母或 z 缓冲串，而那些前缀下都还有别的短语撑着，故不影响既有判定；
        // 若将来有「整个前缀只剩一条瞬时短语」的情形，这里会翻转成 false。
        let host = wind_phrase::PhraseHost::empty();
        // 方案级短语作用域取**活跃方案**而不是 `effective_data_schema`：本判据服务于「按键
        // 让位」，只在普通输入态（`state.active == None`）被调用，那时两者恒等。
        // ⚠️ 若日后在 overlay 语境里调用它，要改成传 state 走 `phrase_spec_of`。
        let spec = self
            .engine_mgr
            .behavior_for(&self.engine_mgr.active_schema_id())
            .phrases
            .clone();
        let scope = crate::schema_scope::phrase_scope(&spec);
        // 存在性判据用 `1`：只问「有没有以 code 开头的短语」，与显示门槛无关（见函数文档）。
        !phrases.lookup(code, &recent, &host, &scope).is_empty()
            || !phrases.lookup_prefix(code, &recent, 1, &scope).is_empty()
    }

    /// z-fallback 夺取（对齐 Go `decideEngineDefaultZFallback` + `enterTempPinyinFromZBuffer`）：
    /// **码表引擎** + 缓冲以 z 开头 + 本方案 `z_key_action = "temp_pinyin"`，且缓冲加新键 `ch`
    /// 后 `z…` 不再是活码前缀，则判定首 z 实为拼音触发键——抛弃首 z，`buffer[1:]+ch` 作临时
    /// 拼音编码切入，并武装退格 rewind（首次退格还原到正常码表输入流 `buffer+ch`）。
    /// 返回 `Some` 表示已夺取，`None` 表示不夺取。混输引擎排除（避免 `zhang` 丢首字母，
    /// 对齐 Go 门禁）。
    ///
    /// # 支持哪些目标：临拼 / 临英 / mix，不含快符
    ///
    /// 夺取要求目标模式能**接住一段残余编码**：临拼收拼音、临英收英文原文、mix 收自由输入
    /// （日期/计算/拼音/英文各自试），三者的残余码都有意义。
    ///
    /// `special`（快符类）刻意排除：那类表的编码是作者精心设计的短码，`zab` 抛掉 z 之后的
    /// `ab` 落到快符表里多半什么也查不到；且它常开 `show_all_on_enter`，价值在「进入即浏览
    /// 整表」，而夺取路径永远给不了那一下。
    ///
    /// ★ 为什么非得走夺取：本项目 `system.phrases.toml` 出厂带 37 条 `zz*` 标点短语，
    /// `has_code_prefix("z")` 恒为真，**首键 z 恒被活码判据让位**。不给这些目标补一条夺取
    /// 回路，`z_key_action = "mix:…"` 这类配置就是配了也永远不生效（2026-08-06 真机确认）。
    ///
    /// ⚠️ 让位本身没问题，**但让位那一帧必须产得出候选**——曾因短语枚举门槛与候选构建
    /// 不同源而恒为空帧，用户按 z 看不到任何东西，误以为绑定没生效。见
    /// [`Self::phrase_prefix_min`]。修好后三条路共存：`z` 列 `zz*` 短语、`zzbd` 精确命中、
    /// `z1`/`zri` 由本函数夺取。
    ///
    /// # 与 `z_key_repeat` 的关系（刻意不检查）
    ///
    /// 首键裁决里 repeat 优先、z 让位落普通输入；到了这里**不再查 repeat**，`zh` 破前缀就夺取。
    /// 即「让位」只维持一个按键，靠**用户是否继续打字母**区分意图：继续打就说明不是要重复上屏。
    /// 这是刻意的——两者若改成互斥，开了 `z_key_repeat` 的方案上 z 进临拼会彻底不可用
    /// （首键让位、后续也不夺取，一条进入通路都不剩）。
    /// 目标模式能否**接住**这个残余字符——夺取的前提是残余码在目标里有意义。
    ///
    /// | 目标 | 接受 | why |
    /// |---|---|---|
    /// | `temp_pinyin` | 仅字母 | 拼音里数字/符号没有意义，`z1` 的 1 仍该是选词键 |
    /// | `temp_english` | 字母+数字+符号 | 收英文原文，`z-` → `-` 是合法开头 |
    /// | `mix:<id>` | 字母+数字+符号 | 收自由输入，**算式与日期恰恰要数字和运算符** |
    ///
    /// ★ 这条判据是 2026-08-08 补的。夺取机制原本只为临拼而建（残余码只可能是字母），
    /// 故只挂在字母臂上；推广到 mix 时没跟着放开字符类别，表现是「z 进快捷输入后算不了数」
    /// ——数字在缓冲非空时走数字选词臂、符号走标点流水线，都到不了夺取判定。
    fn z_fallback_accepts(action: &wind_config::BoundAction, ch: char) -> bool {
        use wind_config::BoundAction as BA;
        match action {
            BA::TempPinyin => ch.is_ascii_alphabetic(),
            BA::TempEnglish | BA::Mix(_) => ch.is_ascii_graphic(),
            // special 不走夺取（见本函数文档），其余动词与夺取无关。
            _ => false,
        }
    }

    pub(crate) fn try_z_fallback(&self, state: &mut State, ch: char) -> Option<KeyAction> {
        if !matches!(
            self.engine_mgr.current_engine_type(),
            Some(wind_engine::EngineType::CodeTable)
        ) {
            return None;
        }
        if !state.input_buffer.starts_with('z') {
            return None;
        }
        // 走 `bound_action_for` 而非 `z_key_action()`：方案级 `[key_actions]` 里写的 z 必须
        // 压过全局 `schema.codetable.z_key_action`。用后者的话，方案把 z 改绑到别的目标后，
        // 首键判定按新目标、而这里仍按「临拼」夺取——同一个键在两条路径上是两个身份。
        let action = self.bound_action_for(wind_keys::keymap::VK_Z)?;
        // 残余码在目标模式里有没有意义。放在活码判据之前——它更便宜，且不通过时
        // 本就该让该键走它原本的身份（数字选词 / 标点上屏）。
        if !Self::z_fallback_accepts(&action, ch) {
            return None;
        }
        let combined = format!("{}{}", state.input_buffer, ch);
        // 加新键后仍是活码前缀（如 zhang 存在时的 "zh"，或系统短语 `zzbd` 的 "zz"）→ 不夺取，
        // 继续正常码表。这条同时保住了出厂那 37 条 `zz*` 标点短语。
        if self.has_code_prefix(&combined) {
            return None;
        }
        // residual = 去掉首 z + 新键。
        let residual = format!("{}{}", &state.input_buffer[1..], ch);
        // snapshot = **夺取前**的正常码流，不含触发夺取的这一键（与 `Rewind::snapshot` 的字段
        // 语义、以及 URL 那个调用点一致）。
        //
        // ⚠️ 曾取 `combined`（= buffer + ch），那必然退到一个无候选的死状态——夺取的前提
        // 恰恰是 `has_code_prefix(combined) == false`，判据说「这里没东西」，回退目标却偏要
        // 退到那里。且 `combined` 这一帧用户从没见过（按下该键的同一帧就被夺取了），退过去
        // 看起来就像「退格没生效、只是候选窗闪没了」，得再按一次才回到有候选的那一帧。
        let snapshot = state.input_buffer.clone();

        // 按目标模式装载残余码。门卫没过一律返回 None（不夺取，落正常码表），
        // 与首键进入点同策略——绝不能吞键。
        let mode_name = match &action {
            wind_config::BoundAction::TempPinyin => {
                let target = self.engine_mgr.temp_pinyin_target()?;
                state.active = Some(ModeKind::TempPinyin);
                state.temp_pinyin_schema = target;
                state.temp_pinyin_buffer = residual.clone();
                state.temp_pinyin_cursor = state.temp_pinyin_buffer.len();
                state.temp_pinyin_prefix = "z".to_string();
                "temp pinyin"
            }
            wind_config::BoundAction::TempEnglish => {
                if !self.rt().config.input.temp_english.enabled {
                    return None;
                }
                state.active = Some(ModeKind::TempEnglish);
                state.temp_english_buffer = residual.clone();
                state.temp_english_cursor = state.temp_english_buffer.len();
                state.temp_english_prefix = "z".to_string();
                "temp English"
            }
            wind_config::BoundAction::Mix(id) => {
                let idx = self.mix_mode_idx(id)?;
                if !self.mix_has_quick_input(idx) && self.mix_members(idx).is_empty() {
                    return None;
                }
                state.active = Some(ModeKind::Mix(idx));
                state.mix_id = idx;
                state.mix_buffer = residual.clone();
                state.mix_cursor = state.mix_buffer.len();
                state.mix_prefix = "z".to_string();
                "mix"
            }
            // 快符类不走夺取（残余码在符号表里查不到，且它要的是「进入即浏览」）；
            // None 表示本方案没给 z 绑任何东西。
            _ => return None,
        };

        state.rewind = Some(Rewind {
            snapshot,
            host_text: residual,
        });
        state.input_buffer.clear();
        state.candidates.clear();
        match state.active {
            Some(ModeKind::TempPinyin) => self.update_temp_pinyin_candidates(state),
            Some(ModeKind::TempEnglish) => self.update_temp_english_candidates(state),
            Some(ModeKind::Mix(_)) => self.update_mix_candidates(state),
            _ => {}
        }
        let display = state.preedit.clone();
        self.notify_ui_update(state);
        debug!("z-fallback: hijacked to {mode_name}");
        Some(KeyAction::UpdateComposition {
            text: display.clone(),
            caret_pos: display.chars().count() as u32,
        })
    }

    /// 当前按键是否匹配配置的临时英文触发键
    /// 当前按键是否为临时英文的引导键。来源与理由同 [`Self::is_temp_pinyin_trigger`]。
    pub(crate) fn is_temp_english_trigger(&self, key_code: u32) -> bool {
        matches!(
            self.bound_action_for(key_code),
            Some(wind_config::BoundAction::TempEnglish)
        )
    }

    /// 临拼：回退最后一个已转换段——把它消费的码并回缓冲**前部**并重转，光标落码末尾
    /// （理由同主输入的 `pop_committed_seg`）。Backspace（段优先）与 Delete（删空后）共用。
    fn pop_temp_pinyin_seg(&self, state: &mut State) -> KeyAction {
        let Some((raw_code, _, _, _, _)) = state.committed_segs.pop() else {
            return KeyAction::Consumed;
        };
        state.committed_text = state
            .committed_segs
            .iter()
            .map(|(_, _, t, _, _)| t.as_str())
            .collect();
        state.temp_pinyin_buffer = format!("{}{}", raw_code, state.temp_pinyin_buffer);
        state.temp_pinyin_cursor = state.temp_pinyin_buffer.len();
        self.update_temp_pinyin_candidates(state);
        let display = state.preedit.clone();
        let caret_pos = self.overlay_caret(state);
        self.notify_ui_update(state);
        KeyAction::UpdateComposition {
            caret_pos,
            text: display,
        }
    }

    /// 退出临时拼音模式并清空相关状态（含逐步转换的已转换前缀）
    pub(crate) fn exit_temp_pinyin(&self, state: &mut State) {
        state.active = None;
        state.temp_pinyin_buffer.clear();
        state.temp_pinyin_cursor = 0;
        state.temp_pinyin_schema.clear();
        state.temp_pinyin_prefix.clear();
        state.committed_text.clear();
        state.committed_segs.clear();
        state.candidates.clear();
        state.preedit.clear();
        self.reset_candidate_view(state);
    }

    /// 临拼向引擎取数的上限：目标方案是拼音类才取全量。
    ///
    /// 目标方案来自用户配置 `schema.primary_pinyin`，**该配置没有类型校验**——手改配置文件
    /// 指向码表方案时，取全量会是 34.9MB 峰值 + 39.6ms 的严重劣化（码表单字母候选达 5472 条）。
    /// 故按引擎类型分流：非拼音类退回原有小批量。
    ///
    /// 用 `loaded_engine_type` 而非 `schema_engine_type`：后者每次都读文件 + 解析 TOML，
    /// 本函数在逐键路径上。目标方案此时必然已加载（`temp_pinyin_target` 内部调过 `ensure_loaded`）。
    fn temp_pinyin_limit(&self, schema: &str) -> usize {
        match self.engine_mgr.loaded_engine_type(schema) {
            Some(wind_engine::EngineType::Pinyin) => TEMP_PINYIN_MAX_CANDIDATES,
            _ => ENGINE_MAX_CANDIDATES,
        }
    }

    /// 用临时拼音目标方案转换缓冲，刷新候选与组合区（前缀 + 已转换汉字 + 剩余拼音）
    pub(crate) fn update_temp_pinyin_candidates(&self, state: &mut State) {
        state.candidates.clear();
        self.reset_candidate_view(state);
        let prefix = format!("{}{}", state.temp_pinyin_prefix, state.committed_text);
        if state.temp_pinyin_buffer.is_empty() {
            state.preedit = prefix;
            return;
        }
        let Some(schema) = self.overlay_engine_schema(state) else {
            state.preedit = format!("{}{}", prefix, state.temp_pinyin_buffer);
            state.overlay_body = state.temp_pinyin_buffer.clone();
            return;
        };
        let limit = self.temp_pinyin_limit(&schema);
        let result = self
            .engine_mgr
            .convert_with(&schema, &state.temp_pinyin_buffer, limit);
        let display = if result.preedit_display.is_empty() {
            state.temp_pinyin_buffer.clone()
        } else {
            result.preedit_display
        };
        state.preedit = format!("{}{}", prefix, display);
        state.overlay_body = display; // 供光标换算（含引擎插入的音节分隔符，与缓冲不同形）

        // 临拼候选按**与主路径同一套层级链**排序（`candidate_display_order`：
        // is_fuzzy → is_partial → is_prefix → weight → natural_order）。
        //
        // ⚠️ 曾用纯 weight 排序（`b.weight.cmp(&a.weight).then(natural_order)`），缺了
        // `is_prefix` 那一层 ⇒ **前缀补全压过精确匹配**：实测临拼 `ni` 首选是「年」(nian)、
        // 整页被「你的」「你们」等高频词组占满，而全拼下首选是「你」。用户报障即此。
        //
        // ⚠️ `ignore_weight` 必须按**临拼目标方案**取（`base_sort_ignores_weight_of`），
        // 不能用 `active_base_sort_ignores_weight()`——活跃方案是码表（五笔），拿它的
        // `base_sort` 去排拼音候选就是「被五笔干扰」。
        let ignore_weight = self.engine_mgr.base_sort_ignores_weight_of(&schema);
        // 供跨来源档位判「消费整串」与「码 == 输入」。缓冲恒 ASCII，字节长度与 `consumed_length` 同域。
        let input_str = state.temp_pinyin_buffer.clone();
        let mut candidates = result.candidates;
        // mixed=false：临拼是纯拼音 overlay，不存在码表/拼音跨来源竞争。
        candidates.sort_by(|a, b| {
            crate::handle_candidate::candidate_display_order(a, b, ignore_weight, false, &input_str)
        });
        // 截断值必须跟取数上限同源：这两处曾同用一个常量兼任「取多少」与「留多少」，
        // 只改一处会出现「取了 5000 条又砍回 50」。
        candidates.truncate(limit);
        // 统一展开汇聚点：临时拼音词库候选内 `$` 特殊语法在此展开（见 finalize_candidates）。
        let mut candidates = self.finalize_candidates(candidates, &state.temp_pinyin_buffer);
        // 检索范围过滤，与主路径同序：mark_common（判定，无条件）→ apply_filter（按模式裁剪）。
        // **必须在 finalize 之后**：过滤的保留条件含 `is_command` / `is_group`，而这两个标志
        // 正是 finalize_candidates 展开 `$CC`/`$AA` 时才置位的，提前过滤会把命令/组候选误删。
        //
        // 临拼此前完全不接过滤——「检索范围」设置对它从来无效，且默认 smart 下临拼比主路径
        // 多出数百个生僻字候选（实测 `ying`：临拼 299 条 vs 主路径 76 条）。
        self.mark_common(&mut candidates);
        self.apply_filter(state, &mut candidates);
        state.candidates = candidates;
        // 简繁 1对多变体展开（约束见 expand_s2t_variants 文档）。
        self.expand_s2t_variants(state);
    }

    /// 临时拼音选词 —— 组合区逐步转换（C）。部分匹配并入 committed 前缀留模式内（不上屏）；
    /// 完整匹配整体上屏 committed+候选（前缀触发键不输出）+ 造词，退出。返回最终 KeyAction。
    pub(crate) fn commit_temp_pinyin_selected(
        &self,
        state: &mut State,
        cand: &Candidate,
        candidate_pos: i32,
    ) -> KeyAction {
        // $AA/$SS 组折叠候选：补全编码到完整码并重查展开（二级选择，不上屏组名）。
        if cand.is_group {
            state.temp_pinyin_buffer = cand.group_code.clone();
            state.temp_pinyin_cursor = state.temp_pinyin_buffer.len(); // 补全到完整码：光标落末尾
            self.update_temp_pinyin_candidates(state);
            let display = state.preedit.clone();
            let caret_pos = self.overlay_caret(state);
            self.notify_ui_update(state);
            return KeyAction::UpdateComposition {
                caret_pos,
                text: display,
            };
        }
        // $CC 命令候选：执行动作（退出临拼后异步跑），不走文本/分段上屏。
        let cmd_code = state.temp_pinyin_buffer.clone();
        if let Some(act) =
            self.overlay_commit_command(state, cand, &cmd_code, |s, st| s.exit_temp_pinyin(st))
        {
            return act;
        }
        let total = state.temp_pinyin_buffer.len();
        let consumed = cand.consumed_length;
        let code = Self::cand_code(&state.temp_pinyin_buffer, cand);
        let partial =
            consumed > 0 && consumed < total && state.temp_pinyin_buffer.is_char_boundary(consumed);
        // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
        self.record_selection(
            &self.freq_code(&state.temp_pinyin_buffer, cand),
            &cand.text,
            cand.source,
        );
        // 输入统计：每次临拼选词记一段（来源临时拼音）。
        self.record_commit(
            &cand.text,
            code.len() as u32,
            candidate_pos,
            wind_store::stats::CommitSource::TempPinyin,
        );
        let raw_code = Self::raw_consumed_code(&state.temp_pinyin_buffer, consumed, partial);
        if partial {
            state.committed_segs.push((
                raw_code,
                code,
                cand.text.clone(),
                cand.source,
                cand.boundary,
            ));
            state.committed_text.push_str(&cand.text);
            state.temp_pinyin_buffer = state.temp_pinyin_buffer[consumed..].to_string();
            // 分步确认消费掉前缀码：光标落剩余码末尾
            state.temp_pinyin_cursor = state.temp_pinyin_buffer.len();
            self.update_temp_pinyin_candidates(state);
            let display = state.preedit.clone();
            self.notify_ui_update(state);
            KeyAction::UpdateComposition {
                caret_pos: display.chars().count() as u32,
                text: display,
            }
        } else {
            state.committed_segs.push((
                raw_code,
                code,
                cand.text.clone(),
                cand.source,
                cand.boundary,
            ));
            let final_simplified = format!("{}{}", state.committed_text, cand.text);
            // 单段整句同样要造词（临拼模式下整句一次上屏亦只 push 一段）。
            self.learn_phrase_on_commit(state, cand.is_synthesized);
            // 变体候选末段用覆盖文本；普通候选整体转换（保留 STPhrases 跨段词级消歧）。
            let out = match &cand.s2t_override {
                Some(t) => format!("{}{}", self.maybe_s2t(state, &state.committed_text), t),
                None => self.maybe_s2t(state, &final_simplified),
            };
            self.exit_temp_pinyin(state);
            self.notify_ui_hide();
            Self::commit_action(out, true)
        }
    }

    /// 这个键在临拼下是否应作为**非字母码元**进缓冲；是则返回进缓冲的小写字符。
    ///
    /// 存在的理由：双拼布局把韵母塞进符号键（微软/搜狗/紫光的 `;` = ing），而主输入路的
    /// 码元闸门 [`Coordinator::try_code_char_gate`] 位于 `message_handler.rs` 那句
    /// `Some(ModeKind::TempPinyin) => return handle_temp_pinyin_key(..)` **之后**，临拼
    /// 根本走不到 ⇒ 那些音节在临拼里一个也打不出（`;` 反被兜底臂当成次选键）。
    ///
    /// ★★ 判据按 **overlay 方案**取（`is_code_char_of` / `is_leading_char_of`），与
    /// `manual_separator_key_of` 同一条纪律：主输入路的 `can_enter_buffer` 问的是活跃
    /// 引擎，而临拼在五笔方案下引用的是拼音方案。
    ///
    /// ⚠️ **零回归的依据**：拼音引擎的码元集完全由双拼布局推导，全拼恒 `None` ⇒ 回落
    /// 默认 `a-z` ⇒ 非字母恒 false。故本函数在全拼、以及纯字母布局的双拼（小鹤等）下
    /// **恒返回 None**，调用点那条臂等于不存在。
    ///
    /// 首码/全集之分与主输入路一致：缓冲空时查首码集（数字默认不在其中 ⇒ 空缓冲的数字键
    /// 仍是选词/透传），否则查全集。
    ///
    /// ⚠️ 字母**不走这里**：它们有专门的累积臂，且那条臂不问码元集——拼音码元恒含 a-z，
    /// 多问一次只会在布局解析失败时把整个临拼打死。这与 `try_code_char_gate` 开头
    /// `if ch.is_ascii_alphabetic() { return None }` 是同一个分工。
    fn temp_pinyin_code_char(&self, state: &State, data: &KeyEventData) -> Option<char> {
        let ch = printable_char(data.key_code, data.modifiers & MOD_SHIFT != 0)?;
        if ch.is_ascii_alphabetic() {
            return None;
        }
        let lower = ch.to_ascii_lowercase();
        let schema = self.overlay_engine_schema(state)?;
        let accepted = if state.temp_pinyin_buffer.is_empty() {
            self.engine_mgr.is_leading_char_of(&schema, lower)
        } else {
            self.engine_mgr.is_code_char_of(&schema, lower)
        };
        accepted.then_some(lower)
    }

    /// 临时拼音模式下的按键处理
    pub(crate) fn handle_temp_pinyin_key(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> KeyAction {
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先，否则组合键会落到
        // 下方各臂被当成普通输入。临拼有逐步转换的已转换前缀，故 committed_text 也算待输入。
        if let Some(act) = self.overlay_ctrl_alt_guard(
            state,
            data,
            !state.temp_pinyin_buffer.is_empty() || !state.committed_text.is_empty(),
            |s, st| s.exit_temp_pinyin(st),
        ) {
            return act;
        }
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 目标方案 `[key_actions]` 里绑的辅助码触发键（如出厂 shuangpin 的 `backtick = "aux_code"`）。
        //
        // ★ 辅助码是**编码类**动词——它筛的是眼前这批拼音候选的字形，故按**产出候选的方案**
        // 取，而不是活跃的五笔方案（见 docs/design/key-resolver-unification.md §4.4）。整张表
        // 不随目标方案走：`special:*` 那类仍恒属主方案，本处只认 `aux_code` 这一个动词。
        //
        // ★ 位置在 `handle_candidate_nav` **之后**：全局/主方案 `session_actions` 那条路
        // （`apply_session_action` 认得 `SessionAction::AuxCode`，含共键形态）优先，与主输入路
        // 的裁决顺序一致；本处只补「目标方案自己声明了 aux_code」这一种。两条最终都汇到
        // `enter_aux_code`，由它的门卫统一裁决（分隔符占用 / 未启用 / 无候选一律返回 None
        // 不吞键，键继续落下方各臂）。
        //
        // ⚠️ 字母不参与：辅助码态里字母恒是码元，与 `aux_code_key_role` 的第一道守卫同源
        // （少了它，配过 `z = "aux_code"` 的用户在临拼里打不出 z）。
        if data.modifiers & MOD_SHORTCUT == 0
            && !(keymap::VK_A..=keymap::VK_Z).contains(&data.key_code)
            && let Some(schema) = self.overlay_engine_schema(state)
            && matches!(
                self.bound_action_in_schema(data.key_code, &schema),
                Some(wind_config::BoundAction::AuxCode)
            )
            && let Some(act) = self.enter_aux_code(state, data.key_code)
        {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）；置于候选导航之后，导航键优先。
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        // 进入键二次按下（缓冲空 + 无已转换前缀）：按中英标点配置上屏该符号并退出。
        // 顺带武装智能符号：时限内再按同键即换英文形——否则这个键被模式占着，英文形没有通路
        // （空闲态一按就又进模式）。press2 的拦截在 try_activate_mode 开头，早于模式激活链。
        //
        // ★★ 上屏字符取**当前按键**，不取 `temp_pinyin_prefix`（进入时按的那个键）。
        //
        // 临拼是全局单例、没有 special / mix 那样的实例 id，判据只问得出「这个键是不是**某个**
        // 临拼引导键」。而同绑是常态：全局 `backtick = "temp_pinyin"` 配上方案级
        // `z_key_action = "temp_pinyin"`，`` ` `` 与 z 就互相认领了对方的身份。判据取当前键、
        // 产出取进入键 ⇒ 用 `` ` `` 进临拼后按 z 被判成二次按下，上屏的却是 `·`，
        // z 开头的拼音（zi / zuo / zhang）一个都打不出来。
        //
        // 改取当前键后，字母键被 `punct_char` 自然挡在门外（字母无标点形态），落下方字母臂
        // 正常累积拼音；两个符号键同绑时也各自上屏自己的符号。至此与 special
        // （`handle_special.rs`）、mix（`handle_mode.rs`）的同名分支完全同构——那两处正是靠
        // 这道 `punct_char` 关卡才没暴露同一个缺陷。
        if state.temp_pinyin_buffer.is_empty()
            && state.committed_text.is_empty()
            && self.is_temp_pinyin_trigger(data.key_code)
            && let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
        {
            let out = self.convert_punct_char(state, ch);
            self.arm_smart_symbol_after_commit(state, ch, &out);
            self.record_commit(&out, 0, -1, wind_store::stats::CommitSource::Punctuation);
            self.exit_temp_pinyin(state);
            self.notify_ui_hide();
            return Self::commit_action(out, true);
        }
        match data.key_code {
            // Esc：放弃退出。实现收口在 `cancel_session`（按 `state.active` 分派回
            // `exit_temp_pinyin`），与绑了 `cancel` 的自定义键共用同一条路径。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // Backspace：段回退**优先于光标**（有已转换段先退回最后一段，你→ni，码并回缓冲
                // 前部）；否则删光标前一字符。Delete 只删光标后一字符、删空后才回退段——与主输入
                // 同构的刻意不对称（见 coordinator.rs 的 VK_DELETE 臂）。皆空则退出。
                let backward = data.key_code == keymap::VK_BACK;
                if backward && !state.committed_segs.is_empty() {
                    return self.pop_temp_pinyin_seg(state);
                }
                if state.temp_pinyin_buffer.is_empty() {
                    if backward {
                        self.exit_temp_pinyin(state);
                        self.notify_ui_hide();
                        return KeyAction::ClearComposition;
                    }
                    // Delete 且剩余拼音已空（只剩只读前缀）：吃掉，不改变退出语义。
                    return KeyAction::Consumed;
                }
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.temp_pinyin_buffer,
                        &mut state.temp_pinyin_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if !removed {
                    // 退格时光标已在最左 / Delete 时已在末尾：吃掉不透传。
                    return KeyAction::Consumed;
                }
                if state.temp_pinyin_buffer.is_empty() {
                    if !state.committed_segs.is_empty() {
                        return self.pop_temp_pinyin_seg(state);
                    }
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    caret_pos,
                    text: display,
                }
            }
            keymap::VK_SPACE => {
                // 空格：选当前高亮候选（逐步转换）
                if !state.candidates.is_empty() {
                    let (start, _) = self.page_range(state);
                    let idx = (start + state.selected_index).min(state.candidates.len() - 1);
                    let cand = state.candidates[idx].clone();
                    self.commit_temp_pinyin_selected(state, &cand, (idx - start) as i32)
                } else {
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                }
            }
            keymap::VK_RETURN => {
                // clear 模式：整段放弃，不上屏任何内容（含已选词的 committed_text）。
                // 须先于下方各分支——此前该判断只写在「空缓冲」分支内，导致「打了码再回车」
                // 仍走非空缓冲路径无条件上屏原码，配置形同虚设（与主输入路径行为不一致）。
                if self.enter_clears_composition() {
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 空缓冲（只按了模式键、无已转换前缀）：commit 模式上屏模式键符号本身
                // （原样不转换，如 `）。
                if state.temp_pinyin_buffer.is_empty() && state.committed_text.is_empty() {
                    if !state.temp_pinyin_prefix.is_empty() {
                        let sym = state.temp_pinyin_prefix.clone();
                        self.record_commit(
                            &sym,
                            0,
                            -1,
                            wind_store::stats::CommitSource::Punctuation,
                        );
                        self.exit_temp_pinyin(state);
                        self.notify_ui_hide();
                        return Self::commit_action(sym, true);
                    }
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 非空缓冲：上屏「引导字母 + 已转换前缀 + 剩余拼音原码」
                // （符号引导键行为不变，如 `nihao → nihao）。
                //
                // 引导**字母**要还回去（zhang → zhang，而非 hang）；判据与切中英文、mix 回车
                // 共用，见 `guide_to_return`。z-fallback 进来的尤其要还——那个 z 是从
                // `input_buffer` 里**抢走**的真实击键（zzha 夺取后 prefix="z" + buffer="zha"，
                // 还回去恰好复原成 zzha）。
                let guide = Self::guide_to_return(&state.temp_pinyin_prefix, &state.committed_text);
                // committed 段已在选词时记过，此处只记本次实际上屏的原码避免重复。
                let raw = format!("{}{}", guide, state.temp_pinyin_buffer);
                self.record_commit(
                    &raw,
                    raw.len() as u32,
                    -1,
                    wind_store::stats::CommitSource::TempPinyin,
                );
                let out = self.maybe_s2t(
                    state,
                    &format!(
                        "{}{}{}",
                        guide, state.committed_text, state.temp_pinyin_buffer
                    ),
                );
                self.exit_temp_pinyin(state);
                self.notify_ui_hide();
                if out.is_empty() {
                    KeyAction::ClearComposition
                } else {
                    Self::commit_action(out, true)
                }
            }
            keymap::VK_1..=keymap::VK_9 if data.modifiers & MOD_SHIFT == 0 => {
                // 数字键选当前页第 N 个
                let (start, end) = self.page_range(state);
                let idx = start + (data.key_code - 0x31) as usize;
                if idx < end {
                    let cand = state.candidates[idx].clone();
                    self.commit_temp_pinyin_selected(state, &cand, (data.key_code - 0x31) as i32)
                } else {
                    KeyAction::Consumed
                }
            }
            k if let Some(ch) = numpad_char(k) => {
                // 小键盘 direct 语义（follow_main 时键已在入口归一化为主键盘键，走上面的数字
                // 选词臂）：拼音缓冲是**编码**，数字不是合法拼音 → 顶屏「已转换前缀 + 当前高亮
                // 候选」再接着输出该字符并退出，已打的码不丢。
                if !state.candidates.is_empty() {
                    let idx = self
                        .highlighted_global_index(state)
                        .min(state.candidates.len() - 1);
                    let cand = state.candidates[idx].clone();
                    let code = state.temp_pinyin_buffer.clone();
                    // 命令候选：执行命令，不追加字符（与主路径 direct 一致）。
                    if let Some(act) = self
                        .overlay_commit_command(state, &cand, &code, |s, st| s.exit_temp_pinyin(st))
                    {
                        return act;
                    }
                    // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
                    // 临拼缓冲是击键域（双拼下 `siyr`），与候选码 `siyuan` 不同域。
                    self.record_selection(&self.freq_code(&code, &cand), &cand.text, cand.source);
                    self.record_commit(
                        &cand.text,
                        code.len() as u32,
                        (idx - self.page_range(state).0) as i32,
                        wind_store::stats::CommitSource::TempPinyin,
                    );
                    state.committed_text.push_str(&cand.text);
                }
                let head = self.maybe_s2t(state, &state.committed_text.clone());
                let tail = if state.full_width {
                    to_full_width(&ch.to_string())
                } else {
                    ch.to_string()
                };
                self.record_commit(&tail, 0, -1, wind_store::stats::CommitSource::Punctuation);
                self.exit_temp_pinyin(state);
                self.notify_ui_hide();
                Self::commit_action(format!("{}{}", head, tail), true)
            }
            // 手动音节分隔符：把 `'` 压入临拼缓冲作硬边界，与主输入路完全同构
            // （`message_handler.rs` 的 VK_QUOTE|VK_BACKTICK 臂）。引擎侧零改动——
            // `convert_with` 走的就是主路径同一个入口，全拼路径按 `'` 硬切分、查询前剥除、
            // `preedit_display` 原样回显，`overlay_body` 已按「含引擎插入的分隔符」换算光标。
            //
            // ★★ 判据必须走 `manual_separator_key_of(.., overlay 方案)`：`manual_separator_key`
            // 问的是**活跃**引擎，而临拼的典型场景（五笔方案 + z 引导）下活跃引擎是码表，
            // 恒返回 false。此前本模式**根本没有这条臂**，反引号一路落到下方 `_` 兜底的
            // 「其它键：有候选则上屏高亮候选」——现场表现就是「按分隔符直接把第一个字打出去了」。
            //
            // ★ 必须排在上方「引导键二次按下」之后：反引号出厂即临拼引导键
            // （`[input.temp_pinyin] trigger_keys = ["backtick"]`）。两者语义互补——那条要求
            // 缓冲为空，分隔符只在缓冲非空时有意义，故同键同绑不冲突。
            keymap::VK_QUOTE | keymap::VK_BACKTICK
                if data.modifiers & MOD_SHIFT == 0
                    && !state.temp_pinyin_buffer.is_empty()
                    && self
                        .overlay_engine_schema(state)
                        .is_some_and(|s| self.manual_separator_key_of(data.key_code, &s)) =>
            {
                preedit_cursor::BufEdit::new(
                    &mut state.temp_pinyin_buffer,
                    &mut state.temp_pinyin_cursor,
                )
                .insert('\'');
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            // 非字母码元（双拼布局的符号韵母键，如微软/搜狗/紫光的 `;` = ing）。
            // 判据与产出同源，见 [`Self::temp_pinyin_code_char`]——全拼与纯字母布局下它恒
            // 返回 `None`，这条臂等于不存在。
            //
            // ★ 位置：排在 Esc / 退格 / 空格 / 回车 / 数字选词 / 小键盘 / 分隔符**之后**，
            // 只抢下方 `_` 兜底臂的键。主输入路的闸门比这更靠前（在选词与翻页之前），
            // 这里刻意保守——临拼里那些键各有明确语义，而双拼布局的码元集只会含符号键，
            // 与它们本就不相交；真正需要夺回来的只有兜底臂那条「其它键 → 上屏高亮候选」，
            // `;` 正是在那里被当成次选键、把首选打了出去。
            _ if data.modifiers & MOD_SHORTCUT == 0
                && let Some(ch) = self.temp_pinyin_code_char(state, data) =>
            {
                preedit_cursor::BufEdit::new(
                    &mut state.temp_pinyin_buffer,
                    &mut state.temp_pinyin_cursor,
                )
                .insert(ch);
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            // 守卫条件在入口的 `overlay_ctrl_alt_guard` 之后已恒真（Ctrl/Alt 组合到不了这里），
            // 保留作纵深防御。它曾是**全仓唯一**一处模式内 Ctrl 判定，而且只护住了这一条臂。
            keymap::VK_A..=keymap::VK_Z if data.modifiers & MOD_SHORTCUT == 0 => {
                // 字母累积拼音
                let ch = (b'a' + (data.key_code - 0x41) as u8) as char;
                preedit_cursor::BufEdit::new(
                    &mut state.temp_pinyin_buffer,
                    &mut state.temp_pinyin_cursor,
                )
                .insert(ch);
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            _ => {
                // 二三候选键
                if data.modifiers & MOD_SHIFT == 0
                    && let Some(offset) = self.select_key_offset(data.key_code)
                {
                    let (start, end) = self.page_range(state);
                    let idx = start + offset;
                    if idx < end {
                        let cand = state.candidates[idx].clone();
                        return self.commit_temp_pinyin_selected(state, &cand, offset as i32);
                    }
                }
                // 其它键：有候选则上屏高亮候选（分段则保留剩余拼音）；否则退出清空。
                if !state.candidates.is_empty() {
                    let (start, _) = self.page_range(state);
                    let idx = (start + state.selected_index).min(state.candidates.len() - 1);
                    let cand = state.candidates[idx].clone();
                    self.commit_temp_pinyin_selected(state, &cand, (idx - start) as i32)
                } else {
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                }
            }
        }
    }

    /// 退出临时英文模式并清空状态
    /// 临英缓冲的光标位插入（字母 / 数字 / 空格 / 符号五个入口共用）。
    fn temp_english_insert(state: &mut State, ch: char) {
        preedit_cursor::BufEdit::new(
            &mut state.temp_english_buffer,
            &mut state.temp_english_cursor,
        )
        .insert(ch);
    }

    pub(crate) fn exit_temp_english(&self, state: &mut State) {
        state.active = None;
        state.temp_english_buffer.clear();
        state.temp_english_cursor = 0;
        state.temp_english_prefix.clear();
        state.preedit.clear();
        state.candidates.clear();
    }

    /// 刷新临时英文候选：`原文 → 大小写变形 → 英文词库前缀匹配（保持词库原文）`。
    /// 需 `temp_english.show_candidates` 开启才产出变形与词库候选；词库为固定 id "english" 方案。
    ///
    /// 词库候选**不再按输入形态适配大小写**（旧 `adapt_en_case` 已删）——临英由 Shift+字母进入，
    /// 缓冲首字母恒大写，旧适配便把整列候选强制套成 `Hello`/`Help`，而词库 86% 的词本是小写。
    /// 大小写改由 [`en_case_variants`] 产出的显式变形候选承载，位置紧随原文（1-3 号键即可取到
    /// 三种形态），词库候选顺延其后。
    ///
    /// 去重按**精确文本**（旧实现按小写去重）：变形候选之间恰是同一小写形态的不同大小写，
    /// 小写去重会把它们全部抹掉。精确去重同时仍能挡住与原文/变形重复的词库项
    /// （如缓冲 `Hello` 时词库的 `hello` 被变形候选先占位挡下）。
    pub(crate) fn update_temp_english_candidates(&self, state: &mut State) {
        state.candidates.clear();
        self.reset_candidate_view(state);
        let buf = state.temp_english_buffer.clone();
        state.preedit = format!("{}{}", state.temp_english_prefix, buf);
        if buf.is_empty() {
            return;
        }
        // 头部候选：原文 + 大小写变形，两者各有开关（`input.temp_english.*`）。
        //
        // 与英文方案那一侧**配置各自独立、实现共用**（见 crate::english_candidates）：
        // 分歧只允许出现在「要不要生成」，不允许出现在「生成什么」——否则同一串输入在两个
        // 入口给出的候选不一样，而用户根本不知道自己此刻在哪条路径上。
        //
        // ⚠️ 两个开关都关且词库无命中时，最终候选会是空的。那不是缺陷：下方空格臂的判据是
        // `!state.candidates.is_empty()`（**实际候选**），空候选会正确落到「上屏缓冲原文」。
        //
        // ★ 变形候选**还要求当前真的在出候选**（`show_candidates` 开且英文方案可用）：
        // 关掉候选显示时列表里只应剩原文那一条。这不是洁癖——临英的数字键判据是
        // 「除原文外还有没有别的候选」（有则选词、无则入缓冲），变形候选在候选关闭时冒出来
        // 会让 `Ver2b` 里的 `2` 被当成选词键；次选键越界回落标点的判据同理。
        // 原文那条不受此限：它在候选关闭时仍是「空格上屏什么」的依据（既有语义）。
        let dict_schema = self.overlay_engine_schema(state);
        let (want_raw, want_variants) = {
            let te = &self.rt().config.input.temp_english;
            (te.raw_candidate, te.case_variants && dict_schema.is_some())
        };
        let mut cands =
            crate::english_candidates::english_head_candidates(&buf, want_raw, want_variants);
        let mut seen: std::collections::HashSet<String> =
            cands.iter().map(|c| c.text.clone()).collect();
        for (i, c) in cands.iter_mut().enumerate() {
            c.natural_order = i as i32;
        }
        // 去重按精确文本；入列时补 `natural_order`（= 入列序）。取整条 `Candidate` 而非
        // 只取文本：词库候选的 `source` / `code` 是词频记账与重排的依据，在这里丢掉的话
        // 下游再也拿不回来。
        let mut push_cand = |mut c: Candidate, cands: &mut Vec<Candidate>| {
            if !seen.insert(c.text.clone()) {
                return;
            }
            c.natural_order = cands.len() as i32;
            cands.push(c);
        };
        if let Some(schema) = dict_schema {
            // 词库段起点：下面的词频重排与候选调整**只作用于这一段**。
            let dict_start = cands.len();
            let code = buf.to_lowercase();
            // 取数上限按**词库方案自己的引擎类型**分级，与主输入路同一张表（见
            // `initial_candidate_limit_of`）。此前写死 `ENGINE_MAX_CANDIDATES`（50），
            // 于是同一串输入在英文方案下取 300 条、在临英下只取 50 条，用户刚用过的词
            // 若在 top-k 里排 50 名开外，临英就永远等不到词频把它提上来。
            // `loaded_engine_type` 而非 `schema_engine_type`：后者每次读文件 + 解析 TOML，
            // 本函数在逐键路径上。
            //
            // ⚠️ 但它查的是**已加载引擎表**，而英文方案通常不在 `schema.available` 里、不受
            // 预热覆盖 ⇒ 不先 `ensure_schema` 的话，每次进程启动后的**首次**临英拿到的是
            // `None`。今天 `None` 与 `English` 恰好同落 `_ => 300` 那一分支，看不出问题；
            // 一旦临英改指码表类方案，就是「首键 300、此后 100」的真缺陷。
            // ★ 不可照抄 `temp_pinyin_limit`：那边的 `_` 兜底是**小值**（50），本函数的
            // `_` 兜底是**大值**（300）——同一个「取不到引擎类型」的处置，在两处后果相反。
            // ensure 的代价：已加载时只是一次 `is_loaded` map 查找，而下一行的 `convert_with`
            // 内部本来也要调 `ensure_loaded`，等于只把它提前了一行。
            self.engine_mgr.ensure_schema(&schema);
            let limit = Self::initial_candidate_limit_of(
                self.engine_mgr.loaded_engine_type(&schema),
                &code,
            );
            let result = self.engine_mgr.convert_with(&schema, &code, limit);
            for c in result.candidates {
                // 来源与码原样带上：临英与英文方案共用一个词频桶，记账要的正是这两样
                // （见 `record_temp_english_selection`）。此前只取 `text`，候选身份在这里
                // 就丢了，上屏出口再想记词频已无从记起。
                push_cand(c, &mut cands);
            }
            // 词频重排 + 候选调整（置顶/隐藏），归属方案与写端同源，见 `effective_data_schema`。
            //
            // ★ 只切词库段：原文与大小写变形必须钉在最前——「首候选恒是用户所打原文」是临英
            // 的硬承诺（打词库里没有的词时，它是唯一能上屏的东西）。手法与主路径把「自动补充
            // 候选」排除在重排之外同型。顺带地，置顶也就只在词库候选内部生效，不会把原文挤走。
            //
            // ★ 码取**小写化缓冲**：临英缓冲带大写（Shift+H 进入即 `H`），而英文方案下
            // `input_buffer` 恒为全小写。不归一的话两个入口各存一份键，「临英里学到的、
            // 切到英文方案不生效」，而这种失效是完全静默的。
            let mut dict_part: Vec<Candidate> = cands.split_off(dict_start);
            self.apply_freq_rerank_in(Some(ENGLISH_SCHEMA), &mut dict_part, &code);
            self.apply_shadow_in(Some(ENGLISH_SCHEMA), &mut dict_part, &code);
            cands.extend(dict_part);
        }
        // 统一展开汇聚点：临时英文词库候选内 `$` 特殊语法在此展开（见 finalize_candidates）。
        state.candidates = self.finalize_candidates(cands, &buf);
    }

    /// 临英文本上屏（可选全角）+ 退出模式。临英所有上屏出口的单一真相源。
    ///
    /// `append_space` = `schema.english.commit_space` 的临英落点，**按出口给**而非在这里
    /// 统一判：与英文方案同口径——选词类出口（空格 / 数字键 / 次三选键 / 鼠标）补，
    /// 回车与标点顶屏不补（前者是终结性动作，后者补了会得到 `hello ,`）。
    pub(crate) fn commit_temp_english_text(
        &self,
        state: &mut State,
        t: String,
        append_space: bool,
    ) -> KeyAction {
        let text = if state.full_width {
            to_full_width(&t)
        } else {
            t
        };
        // 临时英文上屏（独占模式，无分段 committed）：来源临英，英文无编码故 code_len=0。
        self.record_commit(&text, 0, -1, wind_store::stats::CommitSource::TempEnglish);
        // 补空格排在**记账之后**：带空格的文本进统计表就是一条对不上的脏键。词频侧同理，
        // 那边更早一步记在 `record_temp_english_selection`（本仓已有三处这样的孤儿键漏网史）。
        // 全角态下补全角空格——此时上屏的英文本就是全角形，跟着转才一致。
        let text = if append_space && !text.is_empty() {
            let sp = if state.full_width {
                to_full_width(" ")
            } else {
                " ".to_string()
            };
            format!("{text}{sp}")
        } else {
            text
        };
        self.exit_temp_english(state);
        self.notify_ui_hide();
        if text.is_empty() {
            KeyAction::ClearComposition
        } else {
            Self::commit_action(text, true)
        }
    }

    /// 临英**选中某条候选**的上屏出口：命令守卫 → 词频记账 → 补空格 → 上屏退出。
    ///
    /// 五个选词出口（空格 / 回车(`space_as_input`) / 数字键 / 次三选键 / 鼠标点选）一律走
    /// 这里。此前它们各自 `candidates[gi].text.clone()` 后直接上屏文本，**候选身份在出口
    /// 处就丢了**——这才是临英一直没有词频的根因，不是漏调了哪一行。
    pub(crate) fn commit_temp_english_selected(&self, state: &mut State, gi: usize) -> KeyAction {
        if let Some(act) = self.temp_english_try_command(state, gi) {
            return act;
        }
        let cand = state.candidates[gi].clone();
        self.record_temp_english_selection(state, &cand);
        let append = self.english_space_enabled_in(state);
        self.commit_temp_english_text(state, cand.text, append)
    }

    /// 临英选词的词频记账——与英文方案**同一个桶**（`ENGLISH_SCHEMA`），故临英里选过的词，
    /// 切到英文方案照样受益，反之亦然。
    ///
    /// ★ **只记词库来的候选**：原文与大小写变形没有词库来源（`code` 空、`source` 为 `None`），
    /// 记进去就是一条读端按候选码永远查不中的孤儿键，只会逐日累积垃圾。判据是来源，与
    /// 「短语有文本无码位、恒不记词频」同一先例。
    ///
    /// ★ 取码前**先小写化缓冲**：`freq_code` 在 `code_scope = "input"` 下拿的就是这个串，
    /// 而英文方案那侧 `input_buffer` 恒为全小写。不归一 ⇒ `Hel` 与 `hel` 是两个键，两个入口
    /// 永远学不到一块去。
    fn record_temp_english_selection(&self, state: &State, cand: &Candidate) {
        if cand.source != CandidateSource::English {
            return;
        }
        let code = state.temp_english_buffer.to_lowercase();
        self.record_selection_in(
            Some(ENGLISH_SCHEMA),
            &self.freq_code(&code, cand),
            &cand.text,
            cand.source,
        );
    }

    /// 临英选中候选（全局下标 `gi`）的命令前置守卫：`$CC` 命令候选 → 执行动作（退出临英后异步跑），
    /// 返回 `Some(action)`；非命令 → `None`，调用方按各自文本上屏语义继续。
    pub(crate) fn temp_english_try_command(
        &self,
        state: &mut State,
        gi: usize,
    ) -> Option<KeyAction> {
        let cand = state.candidates[gi].clone();
        let code = state.temp_english_buffer.clone();
        self.overlay_commit_command(state, &cand, &code, |s, st| s.exit_temp_english(st))
    }

    /// 临英下字符 `ch` 是否被放行「直接入缓冲」——`allow_symbols` 总开关 + `symbol_chars`
    /// 白名单的**单一真相源**。
    ///
    /// ★ 该判据有四个消费点，语义各不相同却必须同批读它，漏一个就是「某个字符设了不生效」：
    /// 1. 标点臂（`_ =>`）：放行 → 入缓冲，否则「上屏高亮候选 + 转换后标点 → 退出」。
    /// 2. 数字臂（`VK_1..=VK_9`）：放行 → 入缓冲，否则按页选词。
    /// 3. 选词键 `;`/`'`：放行 → 让位，落标点臂入缓冲；否则选第 2/3 候选。
    /// 4. 导航门控（`handle_candidate_nav`）：放行 → 让位；否则 `-=[],.` 仍翻页。
    ///
    /// 此前它是一个 bool（`allow_symbols`），四处一开全开——想打 `C++` 就得连带牺牲
    /// 全部选词键与翻页键。改成按**字符**问之后，每个键各自决定，互不牵连。
    ///
    /// 判定按字符而非按键：`@` 是 Shift+2、`+` 是 Shift+=，同一个键的两个 shift 态是
    /// 两个独立字符，白名单也就该分别管辖（`punct_char(vk, shift)` 本就返回具体字符）。
    pub(crate) fn temp_english_char_allowed(&self, ch: char) -> bool {
        let te = &self.rt().config.input.temp_english;
        te.allow_symbols && te.symbol_chars.contains(ch)
    }

    /// 临英缓冲是否已进入「纯文本累积态」——含任何非字母字符（`C++` / `x64` / `e-mail`）。
    ///
    /// 此态下数字键无条件入缓冲，不再当选词键。原判据 `state.candidates.len() > 1` 表达的
    /// 本意是「有候选可选才选词」，但取值口径不对：`update_temp_english_candidates` 恒把
    /// 原文塞进首候选，`case_variants` 又对含符号串照样产出变形（`C++` → `c++`），于是
    /// `len > 1` 恒真，按 `1` 直接上屏 `C++` 并退出——`C++11` 根本打不出来。
    ///
    /// 判据落在缓冲而不是候选上：缓冲一旦含符号，词库必然查不到，剩下的候选全是「原文 +
    /// 大小写变形」这类没有词库来源的条目，选词已无价值。这条规则也让白名单不含数字的配置
    /// （只放行 `+`）仍能打出 `C++11`。
    pub(crate) fn temp_english_buffer_is_literal(state: &State) -> bool {
        state
            .temp_english_buffer
            .chars()
            .any(|c| !c.is_ascii_alphabetic())
    }

    /// 临时英文模式按键处理（首版：缓冲累积 + 空格/回车/标点上屏，暂无词库候选）
    pub(crate) fn handle_temp_english_key(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> KeyAction {
        // 候选感知刷新后返回组合区动作。
        let refresh = |this: &Self, state: &mut State| -> KeyAction {
            this.update_temp_english_candidates(state);
            let d = state.preedit.clone();
            let caret_pos = this.overlay_caret(state);
            this.notify_ui_update(state);
            KeyAction::UpdateComposition { text: d, caret_pos }
        };
        // **原文类**上屏（不经候选：回车、以及 `show_candidates` 关闭时的兜底）。选中候选
        // 一律走 `commit_temp_english_selected`——那条路要记词频，而这里没有候选可记。
        // 补空格按出口给：回车不补（终结性动作），空格兜底补（对应英文方案「空格上屏原码」）。
        let commit_text = |this: &Self, state: &mut State, t: String, sp: bool| -> KeyAction {
            this.commit_temp_english_text(state, t, sp)
        };
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先。临英是独占模式、
        // 无分段 committed，待输入内容只看自己的缓冲。
        // 临英此前**一处 Ctrl/Alt 判定都没有**：`Ctrl+E` 落字母臂当字面 e 入缓冲、
        // `Ctrl+1` 落数字臂当选词键、`Ctrl+,` 落标点臂顶屏候选并退出。
        if let Some(act) = self.overlay_ctrl_alt_guard(
            state,
            data,
            !state.temp_english_buffer.is_empty(),
            |s, st| s.exit_temp_english(st),
        ) {
            return act;
        }
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）；置于候选导航之后，导航键优先。
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        match data.key_code {
            // Esc：放弃退出，实现收口在 `cancel_session`。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // 退格删光标前 / Delete 删光标后；缓冲被删空则退出（本就空缓冲时只有退格退出）。
                let backward = data.key_code == keymap::VK_BACK;
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.temp_english_buffer,
                        &mut state.temp_english_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if state.temp_english_buffer.is_empty() && (removed || backward) {
                    self.exit_temp_english(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else if removed {
                    refresh(self, state)
                } else {
                    KeyAction::Consumed
                }
            }
            keymap::VK_SPACE => {
                // space_as_input：空格作为输入字符入缓冲，仅回车上屏（对齐 Go）。
                // 上屏职责随之转给回车，且回车此时取**高亮候选**而非原文（见下方 VK_RETURN）。
                if self.rt().config.input.temp_english.space_as_input {
                    Self::temp_english_insert(state, ' ');
                    refresh(self, state)
                } else if !state.candidates.is_empty() {
                    // 空格：上屏当前高亮候选（首候选=原始输入）；命令候选执行动作。
                    let idx = self
                        .highlighted_global_index(state)
                        .min(state.candidates.len() - 1);
                    self.commit_temp_english_selected(state, idx)
                } else {
                    // 无候选（`show_candidates` 关闭）：上屏缓冲原文。这正是英文方案
                    // 「空格上屏原码」的对应出口，故同样补空格。
                    let text = state.temp_english_buffer.clone();
                    let sp = self.english_space_enabled_in(state);
                    commit_text(self, state, text, sp)
                }
            }
            keymap::VK_RETURN => {
                // clear 模式在临英**只管空缓冲**：临英缓冲装的是英文原文而非「编码」，
                // 且 `space_as_input` 开启后空格被占作输入字符、上屏职责整个压在回车上——
                // 若 clear 一并管辖非空缓冲，本模式将一个上屏通路都不剩（只余 Esc 放弃整段），
                // 打进去的内容永远出不来。故非空缓冲无条件走下方上屏路径，不读该配置。
                // 空缓冲本就没有内容可上屏，clear 语义照旧：不回显触发键字符。
                if self.enter_clears_composition() && state.temp_english_buffer.is_empty() {
                    self.exit_temp_english(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // space_as_input：空格已被占作输入字符，回车接过「上屏高亮候选」的职责——
                // 否则该配置下一个选词键都不剩（allow_symbols 再开，数字键也让位于输入），
                // 候选窗形同虚设。未导航时高亮就在首候选（=用户原文），故对「回车上屏原文」
                // 的既有直觉向下兼容：只有主动导航过才会上屏别的候选。
                if self.rt().config.input.temp_english.space_as_input
                    && !state.candidates.is_empty()
                {
                    let idx = self
                        .highlighted_global_index(state)
                        .min(state.candidates.len() - 1);
                    // 此配置下回车接过的是**选词**职责，故走选中出口（记词频、补空格），
                    // 与空格键在默认配置下的行为对齐——同一个动作换了个键，不该换语义。
                    return self.commit_temp_english_selected(state, idx);
                }
                // 回车：上屏原始输入文本（不取候选）；缓冲空时上屏触发键字符（触发键透传）。
                // **不补空格**：回车是终结性动作，与英文方案 `VK_RETURN` 空码分支同口径。
                let text = if state.temp_english_buffer.is_empty() {
                    state.temp_english_prefix.clone()
                } else {
                    state.temp_english_buffer.clone()
                };
                commit_text(self, state, text, false)
            }
            keymap::VK_A..=keymap::VK_Z => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                let base = data.key_code - 0x41;
                let ch = if shift {
                    (b'A' + base as u8) as char
                } else {
                    (b'a' + base as u8) as char
                };
                Self::temp_english_insert(state, ch);
                refresh(self, state)
            }
            keymap::VK_1..=keymap::VK_9 if data.modifiers & MOD_SHIFT == 0 => {
                let ch = (b'0' + (data.key_code - 0x30) as u8) as char;
                // 数字入缓冲有两条独立通路，缺一不可：
                // 1. 该数字被显式列入白名单（`symbol_chars` 出厂含 `0-9`）——数字是合法英文
                //    内容（hello2 / mp3 / x64），此时选词改走「方向/翻页键导航 + 空格上屏」。
                // 2. 缓冲已是纯文本态（含非字母字符）——见 `temp_english_buffer_is_literal`。
                //    白名单不含数字时靠这条兜底，否则 `C++11` 打不出来。
                let digits_as_input = self.temp_english_char_allowed(ch)
                    || Self::temp_english_buffer_is_literal(state);
                // 数字：有词库候选（>1，即除原文外还有匹配）时按页选词；否则作输入（英文含数字 v2）
                let (start, end) = self.page_range(state);
                let gi = start + (data.key_code - 0x31) as usize;
                if !digits_as_input && state.candidates.len() > 1 && gi < end {
                    self.commit_temp_english_selected(state, gi)
                } else {
                    Self::temp_english_insert(state, ch);
                    refresh(self, state)
                }
            }
            0x30 if data.modifiers & MOD_SHIFT == 0 => {
                // `0` 不参与选词（选词键是 1-9），没有与之竞争的语义，故**不受白名单管辖**，
                // 与既有行为一致。受管辖只会带来「把 0 从列表里删掉后 0 键静默无反应」的回归。
                Self::temp_english_insert(state, '0');
                refresh(self, state)
            }
            k if let Some(ch) = numpad_char(k) => {
                // 小键盘 direct 语义（follow_main 时键已在入口归一化成主键盘键，不到达这里）：
                // 临英缓冲是**文本**不是编码，数字/运算符都是合法内容 → 直接入缓冲，
                // 「英文数字连输」得以在默认配置下可用。此前小键盘落到下方标点臂被
                // punct_char 判 None 后静默 Consumed，故临英下小键盘数字完全打不出。
                Self::temp_english_insert(state, ch);
                refresh(self, state)
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 二三候选键（默认 `;` `'`）→ 选候选。临英此前是**唯一**没接
                // `select_key_offset` 的模式处理器（主流程 / 临拼 / 特殊 / mix 都接了），
                // 于是次选键一路落到下方标点臂，被判成「上屏高亮候选 + 标点」——用户按 `;`
                // 想选第 2 候选，实得首候选被直接上屏并退出临英。
                // 与数字臂同构地受白名单抑制：列入的字符语义是「入缓冲，而非上屏退出**或
                // 选词**」（见 config.toml `symbol_chars` 说明）。此前问的是 allow_symbols
                // 这个整体开关，于是为了打 `C++` 就得连 `;`/`'` 的选词能力一起赔进去；
                // 现在只有 `;` 自己被列入白名单时它才让位。
                // 越界（页内候选不足）不在此处理，落下方标点臂保持既有语义。
                if !shift
                    && punct_char(data.key_code, shift)
                        .is_none_or(|ch| !self.temp_english_char_allowed(ch))
                    && let Some(offset) = self.select_key_offset(data.key_code)
                {
                    let (start, end) = self.page_range(state);
                    let gi = start + offset;
                    if gi < end {
                        return self.commit_temp_english_selected(state, gi);
                    }
                }
                // 其它（标点等）：上屏当前高亮候选 + 转换后标点，退出
                if let Some(ch) = punct_char(data.key_code, shift) {
                    // 白名单内的可见符号直接入缓冲累积（如 C++），不上屏退出（对齐 Go）。
                    // 列表外的照旧走下方「上屏高亮候选 + 转换后标点 → 退出」——这条通路是
                    // 「打完英文顺手加句号上屏」的唯一实现，不能因为开了总开关就整体消失。
                    if self.temp_english_char_allowed(ch) {
                        Self::temp_english_insert(state, ch);
                        return refresh(self, state);
                    }
                    let base = if !state.candidates.is_empty() {
                        let idx = self
                            .highlighted_global_index(state)
                            .min(state.candidates.len() - 1);
                        if let Some(act) = self.temp_english_try_command(state, idx) {
                            return act;
                        }
                        let cand = state.candidates[idx].clone();
                        // 顶屏也是一次选中 —— 记词频，但**不补空格**（会得到 `hello ,`）。
                        // 与主输入路 `commit_highlight_then_char` 逐条同口径：那里同样是
                        // 记账、不补。本臂自建上屏动作、不走 `commit_temp_english_text`，
                        // 故两件事都得在这里显式做。
                        self.record_temp_english_selection(state, &cand);
                        cand.text
                    } else {
                        state.temp_english_buffer.clone()
                    };
                    let base = if state.full_width {
                        to_full_width(&base)
                    } else {
                        base
                    };
                    let punct = self.convert_punct_char(state, ch);
                    self.record_commit(&base, 0, -1, wind_store::stats::CommitSource::TempEnglish);
                    self.record_commit(&punct, 0, -1, wind_store::stats::CommitSource::Punctuation);
                    self.exit_temp_english(state);
                    self.notify_ui_hide();
                    Self::commit_action(format!("{}{}", base, punct), true)
                } else {
                    KeyAction::Consumed
                }
            }
        }
    }

    /// 顶屏当前高亮候选（若有）并进入临时拼音模式（对齐 Go decideBufferedTrigger 的 actEnterMode）。
    /// 有候选：上屏高亮候选 + 原子开启临时拼音组合；空码：丢弃缓冲后进入。
    pub(crate) fn commit_and_enter_temp_pinyin(
        &self,
        state: &mut State,
        key_code: u32,
        target: String,
    ) -> KeyAction {
        // 命令候选顶屏 → 执行命令（与按空格一致），不进模式、不上屏 display 标签。
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        // 已转换前缀 + 高亮候选一并上屏（含记账与简繁转换）。
        let committed = self.take_committed_with_highlight(state);
        state.input_buffer.clear();
        state.candidates.clear();
        // 进入临时拼音
        state.active = Some(ModeKind::TempPinyin);
        state.temp_pinyin_schema = target;
        state.temp_pinyin_buffer.clear();
        // key_code == 0 是直达热键哨兵：不写引导符（temp_pinyin_prefix_for 对未映射键会兜底
        // 反引号，故此处显式取空，对齐 enter_special_mode 的 key_code=0 语义）。
        state.temp_pinyin_prefix = if key_code == 0 {
            String::new()
        } else {
            Self::temp_pinyin_prefix_for(key_code).to_string()
        };
        self.update_temp_pinyin_candidates(state);
        self.notify_ui_update(state);
        let prefix = state.temp_pinyin_prefix.clone();
        match committed {
            Some(text) => self.commit_then_new_composition(text, prefix),
            None => KeyAction::UpdateComposition {
                text: prefix.clone(),
                caret_pos: prefix.chars().count() as u32,
            },
        }
    }
}
