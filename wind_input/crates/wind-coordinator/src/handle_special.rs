//! 特殊方案输入模式
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。

use crate::coordinator::{Coordinator, PunctEmptyCodePolicy, State, numpad_char, punct_char};
use crate::pipeline::ModeKind;
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::Candidate;
use wind_engine::AdmitFn;
use wind_ipc::protocol::MOD_SHIFT;
use wind_keys::keymap;

/// 特殊模式取候选的常规上限（本值是从 special 模式沿用的既有行为，未改）。
///
/// ⚠️ 它施加在**过滤之前**。对 special 模式无碍（快符表按码精确匹配，同码没几条），
/// 但生僻字模式要在这批候选里再滤掉常用字 ⇒ 见 [`Coordinator::refill_rare_if_short`]，
/// 那里会在不足一页时加大重取。⛔ 别为了省掉重取而直接调大本值：它会传进拼音引擎
/// 里一个 O(n²) 的查重循环，单字母输入实测 3.5ms → 29ms，而那是每次按键都要付的。
const SPECIAL_CONVERT_LIMIT: usize = 100;

/// 生僻字模式过滤后不足一页时的重取上限。取值理由见
/// [`Coordinator::refill_rare_if_short`]——与拼音引擎的 `MAX_COMPLETION_CANDIDATES`
/// 对齐，再大会被那边 clamp 掉。
const RARE_REFILL_LIMIT: usize = 1000;

impl Coordinator {
    /// 找出 key_code 绑定的特殊模式下标。
    ///
    /// 数据源是 [`Self::bound_action_for`]，不再遍历 `special_modes[].trigger_keys`——
    /// 那些字段已由 `normalize` 折算进 `keys.key_actions`（设计文档五c）。
    ///
    /// ★ 「先到先得」的歧义随之消失：老实现按配置顺序 `.find()`，两个实例配同一个键时
    /// 后者**静默失效**且无从察觉；新表是 Map，一个键只能有一个动词，冲突在迁移期就
    /// 被 warn 出来了。
    pub(crate) fn match_special_trigger(&self, key_code: u32) -> Option<u8> {
        match self.bound_action_for(key_code) {
            Some(wind_config::BoundAction::Special(id)) => self.special_mode_idx(&id),
            _ => None,
        }
    }

    /// 按**方案 id** 在 overlay 注册表中定位下标（与 `match_special_trigger` 的 u8 下标语义
    /// 一致）。供直达热键 `enter_special:<id>` 分发定位；未找到返回 None。
    ///
    /// 身份即方案 id：`special:<id>` 里的 `<id>` 现在就是方案文件名，不再是实例别名。
    pub(crate) fn special_mode_idx(&self, id: &str) -> Option<u8> {
        self.engine_mgr.overlay_index_of(id)
    }

    /// 顶屏当前普通输入的半成品（复用 `take_committed` + 高亮候选）并进入特殊模式，
    /// 供直达热键与「缓冲非空/有候选时按引导键」两处共用（对齐 mix/临拼的 commit_and_enter）。
    /// key_code=0 是热键哨兵：`vk_to_prefix_char(0)` 返回 None → `special_prefix` 为空，满足
    /// 「热键进入不写引导符」；引导键进入传真实 VK，组合区写引导符（与空缓冲进入一致）。
    /// 方案须可加载（调用方 `ensure_schema` 保证）。
    pub(crate) fn commit_and_enter_special_mode(
        &self,
        state: &mut State,
        idx: u8,
        key_code: u32,
    ) -> KeyAction {
        // 命令候选顶屏 → 执行命令（与按空格一致），不进模式。
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        // 已转换前缀 + 高亮候选一并上屏（含记账与简繁转换）。
        let committed = self.take_committed_with_highlight(state);
        // enter_special_mode 内部清空 input_buffer/candidates、建组合区（key_code=0 → 前缀空）、刷 UI。
        let enter = self.enter_special_mode(state, idx, key_code);
        match committed {
            Some(text) => {
                let new_comp = match &enter {
                    KeyAction::UpdateComposition { text, .. } => text.clone(),
                    _ => state.preedit.clone(),
                };
                self.commit_then_new_composition(text, new_comp)
            }
            None => enter,
        }
    }

    /// 当前特殊模式是否开启「进入即展示候选」（`[overlay] show_all_on_enter`）。
    /// 取进入时的快照，不查注册表——理由见 `State::overlay_spec`。
    fn special_mode_show_all(&self, state: &State) -> bool {
        state
            .overlay_spec
            .as_ref()
            .map(|o| o.show_all_on_enter)
            .unwrap_or(false)
    }

    /// 特殊模式对应的方案 id（= overlay 注册表该下标的方案自身）。
    ///
    /// 原先是 `special_modes[idx].schema`——一个指向别处的引用字段；现在实例即方案，
    /// 这个「引用」退化成自指，故直接取注册表条目的 `schema_id`。
    pub(crate) fn special_schema(&self, idx: u8) -> Option<String> {
        self.engine_mgr
            .overlay_modes()
            .get(idx as usize)
            .map(|e| e.schema_id.clone())
            .filter(|s| !s.is_empty())
    }

    /// 进入特殊模式（其方案须可加载，由激活点 ensure_schema 保证）。清空普通输入，初始化空编码缓冲。
    pub(crate) fn enter_special_mode(
        &self,
        state: &mut State,
        idx: u8,
        key_code: u32,
    ) -> KeyAction {
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::Special(idx));
        state.special_id = idx;
        // `[overlay]` 段快照：布局与进入即展示都取它，见 `State::overlay_spec`。
        state.overlay_spec = self
            .engine_mgr
            .overlay_modes()
            .get(idx as usize)
            .map(|e| e.spec.clone());
        state.special_buffer.clear();
        state.special_cursor = 0;
        // 显示态前缀（进入键符号，如 "\"；经 z_key_action 进入时为 "z"）：只显示不消费。
        state.special_prefix = keymap::vk_to_prefix_char_with_letters(key_code)
            .map(|c| c.to_string())
            .unwrap_or_default();
        self.update_special_candidates(state);
        self.notify_ui_update(state);
        let display = state.preedit.clone();
        debug!("Entered special mode idx={}", idx);
        // 直达热键进入时 display 为空（无引导符），此时是否建占位 composition 取坐标由
        // `[input.caret]` 决定；引导键进入时 display 非空，开关不参与。
        self.special_entry_composition(key_code, display)
    }

    /// 进入生僻字模式：用**当前活跃方案**的编码输入，候选只留生僻字。
    ///
    /// 与 [`Self::enter_special_mode`] 的差别只有「不设 `special_id` / `overlay_spec`」
    /// ——本模式没有宿主方案，那两项恒为默认值，凡从它们取值的地方都会落到默认档
    /// （`show_all_on_enter=false`、布局与注释模板跟随全局），这是刻意的。
    ///
    /// 缓冲复用 `special_buffer`：按键处理走的是同一个 `handle_special_key`。
    pub(crate) fn enter_rare_char_mode(&self, state: &mut State, key_code: u32) -> KeyAction {
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::RareChar);
        state.special_id = 0;
        state.overlay_spec = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        state.special_prefix = keymap::vk_to_prefix_char_with_letters(key_code)
            .map(|c| c.to_string())
            .unwrap_or_default();
        self.update_special_candidates(state);
        self.notify_ui_update(state);
        let display = state.preedit.clone();
        debug!("Entered rare-char mode");
        // 同 `enter_special_mode`：只有直达热键（display 为空）那条看开关。
        self.rare_char_entry_composition(key_code, display)
    }

    /// 顶掉当前半成品并进入生僻字模式（引导键 / 直达热键共用）。
    ///
    /// 「顶字重开」是用户 2026-08-31 选定的进入方式，与临拼 / 快符 / mix 一致。
    /// ⚠️ 另一种形态（保留编码、原地把候选换成生僻字，同辅助码）当时也被认可，将来若要
    /// 加，改的是**这里**：准入判据 `wind_candidate::rare_admits` 不依赖进入方式，
    /// 原样复用即可，不必也不该去动过滤那一侧。
    pub(crate) fn commit_and_enter_rare_char_mode(
        &self,
        state: &mut State,
        key_code: u32,
    ) -> KeyAction {
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        let committed = self.take_committed_with_highlight(state);
        let enter = self.enter_rare_char_mode(state, key_code);
        match committed {
            Some(text) => {
                let new_comp = match &enter {
                    KeyAction::UpdateComposition { text, .. } => text.clone(),
                    _ => state.preedit.clone(),
                };
                self.commit_then_new_composition(text, new_comp)
            }
            None => enter,
        }
    }

    /// 交给引擎的生僻字准入闭包（仅生僻字模式；其余模式返回 `None` = 不筛）。
    ///
    /// 与 [`Self::retain_rare_admitted`] **同一个判据**（都走 `wind_candidate::rare_admits`），
    /// 差别只在施加的位置：这个在引擎产出候选时逐条判，那个在拿到结果后兜底。
    /// ⚠️ 两者必须同源——引擎侧放行、调用方侧却滤掉的候选，会白白占掉引擎的配额；
    /// 反过来则是「引擎筛掉了调用方本想要的」，表现为候选莫名其妙地少。
    ///
    /// 常用字表**未加载**时返回 `None`（不筛），与 `retain_rare_admitted` 的同款保护
    /// 一致：那时全体候选都会被判成「非常用」，筛了等于没筛，还白付一次判定。
    fn rare_admit_fn(&self, state: &State) -> Option<AdmitFn> {
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            return None;
        }
        if self
            .common_chars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return None;
        }
        // Arc 克隆而非借用：`ConvertOptions::admit` 要求 `'static`（它可能被引擎存进
        // 局部闭包里传递）。克隆的是 Arc 本身，表数据不复制。
        let cc = self.common_chars.clone();
        // registry 同样按 Arc 克隆进闭包。这是**进入模式那一刻的快照**——与原先从
        // `rt()` 取 `BlockMask` 的语义一致（本函数每次进入生僻模式调一次）。
        let extra = self.engine_mgr.charsets();
        Some(std::sync::Arc::new(move |text: &str| {
            let g = cc.read().unwrap_or_else(|e| e.into_inner());
            wind_candidate::rare_admits(text, &g, &extra)
        }))
    }

    /// 生僻字模式下的取数选项。
    ///
    /// ★★★ **两条取数路径必须共用本函数**：首次取数（[`Self::update_special_candidates`]）
    /// 与不足一页时的重取（[`Self::refill_rare_if_short`]）。此前重取走的是裸的
    /// `convert_with`，于是 1000 条预算有相当一部分花在会被 `retain_rare_admitted` 当场
    /// 丢弃的常用字上——「截断 → 过滤」这个本该由重取解决的缺陷，在更大的上限上又犯了
    /// 一次；同时白付了 `push_unique` 的全量查重成本，而重取恰恰只在最高频的那些码上触发。
    ///
    /// 收成一个函数是为了让「两处同源」由构造保证：想让它们分叉，得先绕开本函数。
    /// 其余模式不下推准入，`rare_admit_fn` 自己会返回 `None`，故本函数对它们是空选项。
    fn rare_convert_opts(&self, state: &State) -> wind_engine::ConvertOptions {
        wind_engine::ConvertOptions {
            admit: self.rare_admit_fn(state),
            // 本路径**不重排**：`finalize_candidates` 只做 `$` 语法展开，之后只有生僻字
            // 准入与 shadow，没有 `candidate_display_order`。简拼保底配额会把候选补在尾部
            // 并腾位挤掉等量的既有候选 —— 而引擎按常用度排序、尾部正是生僻字所在，
            // 补进来的简拼词又会被「只出单字」全数删掉，净效果是白丢几个生僻字。
            // 详见 `ConvertOptions::no_abbrev_quota`。
            no_abbrev_quota: true,
            ..Default::default()
        }
    }

    /// 对给定候选施加生僻字准入（仅生僻字模式；其余模式为空操作）。
    ///
    /// ⚠️ 凡在 `RareChar` 守卫**之外**的调用点，一律经本函数过一手，不要就地调
    /// `retain_rare_admitted` 再自带一份 `matches!`。（`refill_rare_if_short` 是例外而非
    /// 反例：它首行就按 `RareChar` 早退，整个函数体都已在守卫之内，里面再判一次是重复
    /// 判定；`handle_mode.rs` 里 mix 的生僻字成员同理，由 `member ==` 分支天然圈定。）
    /// 曾经的形态正是那样：
    /// 空码补全那条旁路（`update_special_candidates` 里取 `completion_hints` 的收口）
    /// 直接调了裸过滤器，守卫漏在了那一处 —— 快符等 overlay 模式的补全候选于是被生僻
    /// 准入当场滤空（`rare_admits` 对符号与多字条目判 false —— 除非该符号被
    /// `input.rare_char.include_blocks` 点名，见 `data_dir` 夹具那处的同款说明），表现为「精确匹配下
    /// 空码永远不显示后续编码」（论坛 t125）。
    ///
    /// 那条缺陷在自动化测试里是**不可见**的：`retain_rare_admitted` 对未加载的常用字表
    /// 整条早退，而 headless 夹具恰好就是那个状态（`common_chars` 由
    /// `CommonChars::from_registry` 装配，夹具没有 `common_han` 类）⇒ 过滤根本没跑。
    /// 凡要验这道准入的用例，都得先照 `set_common` 那样把字表与 registry 一并装上。
    ///
    /// 取 `active` 而非 `&State`：调用方常在可变借出候选表的同时需要这个判据。
    fn apply_rare_admission(
        &self,
        active: Option<ModeKind>,
        candidates: &mut Vec<wind_candidate::Candidate>,
    ) {
        if !matches!(active, Some(ModeKind::RareChar)) {
            return;
        }
        self.retain_rare_admitted(candidates);
    }

    /// 生僻字模式：**过滤后不足一页**时加大取数上限重取一次。
    ///
    /// # 为什么必须有这一步：「截断 → 过滤」会砍掉几乎全部结果
    ///
    /// 取数上限施加在过滤**之前**，而引擎按常用度排序 ⇒ 生僻字全排在后面，恰好是被
    /// 上限切掉的那一段。拼音方案下实测（`limit=100` → 全量）：
    ///
    /// | 音节 | 上限 100 | 全部 | 漏掉 |
    /// |---|---|---|---|
    /// | `yi` | **4** | 1183 | 99.7% |
    /// | `ji` | **6** | 1047 | 99.4% |
    /// | `shi` | 23 | 316 | 93% |
    ///
    /// ⚠️ **码表方案同样中招**，只是程度轻：五笔 `ii` 由 1 条变 15 条（沝渻濷尛渁…）。
    /// 我起初以为码表不受影响、还照此写了一条「码表不变」的测试，被它当场证伪。
    /// 真正的规律是**编码越短、同码候选越多，被切掉的越多**：四码全码 `sivg` 只有
    /// 「桜」一个解，一点不受影响；一级简码与二级简码则会被切掉大半。
    /// ⇒ 判据不是「哪种方案」，是「这个码有多少同码候选」。
    ///
    /// ⚠️ 与本文件浏览态那段注释是同一个教训的两面：那里写着「反过来（引擎取数时就截到
    /// 1 条）的后果是……屏幕整个空白」，说的是 shadow；这里的过滤器叫 `rare_admits`。
    ///
    /// # 为什么不直接调大上限
    ///
    /// `max_candidates` 不是「最后 truncate」——它会传进拼音引擎的补全路径，那里的
    /// `push_unique` 按 `iter().any()` 线性查重、整体 O(n²)（见 pinyin/mod.rs 的
    /// `completion_limit` 注释）。本机实测单次 `convert_with`：
    ///
    /// | 输入 | 上限 100 | 上限 1000 |
    /// |---|---|---|
    /// | `b`（单字母，最坏） | 3.5ms | **29.2ms** |
    /// | `yi` | 5.8ms | 18.4ms |
    /// | `shi` | 7.3ms | 7.5ms（总数 416，够不着上限）|
    ///
    /// 那是**每次按键**都要付的。故只在真的不够时才付：候选少的码（绝大多数）一次到位，
    /// 高频音节多付一次重取。⛔ 别改成无条件的大上限。
    ///
    /// ⚠️ **准入已下推给引擎**（`ConvertOptions::admit`）之后本函数仍然必需：那个下推
    /// 判定在 `push_unique`，而词库层已按上限取过 top-N，滤掉的名额不补——实测下推后
    /// `limit=100` 时 `y`/`sh` 的合格候选仍是 **0**。下推买到的是查重成本：`y` 的端到端
    /// 耗时 148.7ms → 50.0ms。两者是「够不够」与「快不快」的分工，缺一不可。
    ///
    /// 重取上限取 [`RARE_REFILL_LIMIT`]＝1000，与拼音引擎的 `MAX_COMPLETION_CANDIDATES`
    /// 对齐：再大也被那里 clamp 掉，实测 2000 与 1000 的条数、耗时完全相同，写 2000 只会
    /// 让读者以为真能取到 2000。
    fn refill_rare_if_short(&self, state: &mut State, schema: &str) {
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            return;
        }
        // 够一页就不重取：本模式的候选恒为词库原序、用户本就要翻页找字，多取的那些
        // 他这一屏也看不到。判据用**实际生效的每页条数**而不是写死的数字。
        let need = self.per_page(state.active);
        if state.candidates.len() >= need {
            return;
        }
        // ★ 重取**同样要带准入下推**，与首次取数共用 `rare_convert_opts`（理由见那里）。
        // 这条路的触发条件是「候选不够一页」，恰恰是单字母、单音节这些最高频输入，
        // 也就是那笔 `push_unique` 全量查重成本（`y` 端到端 148.7ms → 50.0ms）最该省下
        // 的地方——偏偏此前只有首次取数省到了。
        let opts = self.rare_convert_opts(state);
        let result = self.engine_mgr.convert_with_opts(
            schema,
            &state.special_buffer,
            RARE_REFILL_LIMIT,
            opts,
        );
        // 重取的结果**必须走同一条加工链**（finalize → 过滤），否则两次取数的候选形态
        // 不一致：`$CC` 那类词条在重取路径上就不会被展开。
        let mut refilled = self.finalize_candidates(result.candidates, &state.special_buffer);
        self.retain_rare_admitted(&mut refilled);
        // 重取只可能是超集（同一个码、更大的上限），但仍以「谁更多」为准而不是无条件替换
        // ——引擎若有任何不单调的行为，取多的那份至少不会比现在更差。
        if refilled.len() > state.candidates.len() {
            state.candidates = refilled;
        }
    }

    /// 按生僻字准入就地保留候选。
    ///
    /// ⚠️ 常用字表**未加载**时整条跳过，不做任何过滤。判据与 `apply_filter` 那道
    /// `common_chars.is_empty()` 同源：表没加载时全体候选都会被判成「非常用」，
    /// 于是这里会**放行全部**候选——那不是「生僻字模式」，是「没有过滤的普通输入」，
    /// 而用户完全看不出区别。宁可什么都不滤，也不要给出一个假装过滤过的列表。
    ///
    /// 「严格过滤，空了就空着」是用户拍板的取舍（2026-08-24）：滤空不回补、不降级。
    pub(crate) fn retain_rare_admitted(&self, candidates: &mut Vec<wind_candidate::Candidate>) {
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        if cc.is_empty() {
            return;
        }
        // 字符类 registry：`input.rare_char.include_blocks` 点名的类在装配期就打好了
        // `in_rare` 标记（`charset_assembly::ExternalRefs`），这里只是取那一份。
        //
        // ★ 在 `retain` **之外**取一次：本函数在每次按键的候选刷新路径上，逐候选去拿
        // 就是逐候选一次加锁。同 `freq_settings` 里 `english_code_by_input` 那条。
        let extra = self.engine_mgr.charsets();
        candidates.retain(|c| wind_candidate::rare_admits(&c.text, &cc, &extra));
    }

    /// 退出特殊模式并清空相关状态（码表缓存保留供复用）。
    pub(crate) fn exit_special_mode(&self, state: &mut State) {
        state.active = None;
        // 夺取回退登记随模式一起作废（防御性）：z 夺取进生僻字后，登记若留着，下次进生僻字
        // 打到与残余码相同的缓冲再退格，会被误判成「退回夺取边界」而跳回 `z`。端到端没能
        // 构造出这条路径（空格 / Esc / 首键进入各有别处先清掉），但这里清掉不依赖那些巧合。
        // `rewind_hijack` 是先 `take` 再调本函数，不受影响。
        state.rewind = None;
        state.overlay_spec = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        state.special_prefix.clear();
        state.candidates.clear();
        state.preedit.clear();
        // 反查模式会写这三位（翻页扩充 / 末页放宽），不复位的话退出后主路径
        // 翻页会拿残留的 `has_more` 去扩充、下一次组码直接处于放宽态。Special / RareChar
        // 从不写它们，对二者是空操作。
        state.has_more = false;
        state.candidate_limit = 0;
        state.scope_relaxed = false;
    }

    /// [`Self::update_special_candidates`] 请求的全码自动上屏：命令候选走命令路径，
    /// 普通候选记统计、退出模式后上屏。字母键臂与 z 夺取进生僻字（`try_z_fallback`）共用。
    pub(crate) fn special_auto_commit(&self, state: &mut State, cand: Candidate) -> KeyAction {
        // $CC 命令候选自动命中：与手动选中同路（退出模式 + 异步执行动作）。
        let code = state.special_buffer.clone();
        if let Some(act) =
            self.overlay_commit_command(state, &cand, &code, |s, st| s.exit_special_mode(st))
        {
            return act;
        }
        // 自动上屏也是一次选中：记账同选词出口（`record_special_selection`）。
        self.record_special_selection(state, &cand);
        self.record_commit(
            &cand.text,
            state.special_buffer.len() as u32,
            -1,
            wind_store::stats::CommitSource::SpecialMode,
        );
        self.exit_special_mode(state);
        self.notify_ui_hide();
        Self::commit_action(cand.text, true)
    }

    /// 按当前编码缓冲刷新特殊模式候选（经其引用方案的引擎查询，复用方案 CodeTableSpec 全码策略）。
    /// 返回 Some(候选) 表示该方案的全码策略请求自动上屏该候选（`$CC` 命令候选由调用方
    /// 走命令执行路径，普通候选上屏其文本）。
    pub(crate) fn update_special_candidates(&self, state: &mut State) -> Option<Candidate> {
        // 反查模式：缓冲 / 按键处理与 special 族共用，候选另建（convert_reverse + 检索范围
        // 过滤），且从不请求自动上屏（spec reverse-mode §3.2）。
        if matches!(state.active, Some(ModeKind::Reverse)) {
            self.update_reverse_candidates(state);
            return None;
        }
        state.candidates.clear();
        self.reset_candidate_view(state);
        // 组合区 = 显示态前缀 + 编码缓冲（前缀只显示不参与查询）。
        state.preedit = format!("{}{}", state.special_prefix, state.special_buffer);
        if state.special_buffer.is_empty() {
            // 进入即展示：该模式开启 show_all_on_enter 时，空码枚举方案码表首页候选（按 weight
            // 降序）供浏览，UI 按 per_page 分页；经 finalize 展开词条内特殊语法（浏览态无输入
            // 上下文，input 传空）。未开则维持空白（原行为，敲码才出候选）。
            if self.special_mode_show_all(state)
                && let Some(schema) = self.overlay_engine_schema(state)
            {
                let raw = self.engine_mgr.enumerate_with(&schema, 100);
                state.candidates = self.finalize_candidates(raw, "");
                // 浏览态**同样吃候选调整**（空码作为 shadow 键位，见 `apply_shadow_in`）。
                // 这批候选往往是用户唯一能右键的对象：`max_code_length=1` +
                // `auto_commit_at_full` 的快符方案敲一码就上屏，没有非空码的候选态。
                // 不接这一句的话，右键改了顺序、重进模式照旧——规则写进了 store 却没人读。
                let owner = self.effective_data_schema(state);
                let mut list = std::mem::take(&mut state.candidates);
                self.run_passes(
                    state,
                    &mut list,
                    crate::postprocess::profile::SPECIAL_SHADOW,
                    &crate::postprocess::PassCtx {
                        owner: owner.as_deref(),
                        freq_code: None,
                        shadow_code: Some(""),
                    },
                );
                state.candidates = list;
                // 呈现上限（精确匹配模式只展示一条）**必须在 shadow 之后**施加。
                // 反过来（引擎取数时就截到 1 条）的后果：用户隐藏掉那一条，池子里明明还有
                // 下一条，屏幕却整个空白——「截断 → 过滤」把候选调整变成了删掉整个列表。
                if let Some(n) = self.engine_mgr.browse_display_limit_of(&schema) {
                    state.candidates.truncate(n);
                }
            }
            return None;
        }
        let schema = self.overlay_engine_schema(state)?;
        // 生僻字模式把准入**下推给引擎**：上限施加在过滤之前，事后 retain 只能在被截断过
        // 的那一段里筛（拼音 `yi` 事后过滤只剩 4 条，而该音实际有 1183 个非常用字）。
        // 详见 `ConvertOptions::admit`。其余模式传 None，行为与本改动前逐条一致。
        let opts = self.rare_convert_opts(state);
        let result = self.engine_mgr.convert_with_opts(
            &schema,
            &state.special_buffer,
            SPECIAL_CONVERT_LIMIT,
            opts,
        );
        // 统一展开汇聚点：快符表内 `$AA/$SS/$CC` 等特殊语法在此炸开/标命令（见 finalize_candidates）。
        state.candidates = self.finalize_candidates(result.candidates, &state.special_buffer);
        // 生僻字模式：候选只留生僻字。放在 finalize **之后**——判据要看候选的最终文本，
        // 而 `$AA`/`$CC` 那类词条是在 finalize 里才展开成真正文本的。
        // 放在 shadow **之前**，与主路径 `apply_filter` → `apply_shadow` 同序：先决定
        // 哪些候选存在，再应用用户的置顶/隐藏。
        self.apply_rare_admission(state.active, &mut state.candidates);
        self.refill_rare_if_short(state, &schema);
        // 空码补全对齐主码表方案（`single_code_input` + `single_code_complete`）：精确匹配模式下
        // 当前编码无精确候选、但更长前缀有候选时，引擎「备货不 push」把首个更长编码候选放进
        // `completion_hint`（见 codetable/engine.rs），交由掌握最终列表的调用方判空后取一条。
        // 特殊模式此前只消费 `result.candidates`、丢弃了这条旁路 → 屏幕全空；此处补上收口，
        // 与主路径 `update_candidates` 一致（见 handle_candidate.rs 的补全收口）。引擎已在
        // `show_code_hint` 循环里给它标好「剩余编码」注释，直接采纳即可。
        // 词频重排与候选调整：归属**特殊方案自身**，与写端 `record_selection_in` 同一个 id。
        // 取自同一处（`effective_data_schema`）是硬要求——读写分别取自不同的地方，会得到
        // 「写进 qsym、读的是 wubi86」：记账看着成功，候选顺序永远不动。
        let owner = self.effective_data_schema(state);
        // 生僻字模式**完全不参与词频**（用户拍板）：不只是不记账，重排也跳过。
        // 只跳写端的话，这里读到的仍是正常输入时积累的词频，模式内的顺序会被它牵着走，
        // 而用户以为自己关掉了这件事。两端一起跳，语义才是「这个模式不碰词频」。
        let mut list = std::mem::take(&mut state.candidates);
        let ctx = crate::postprocess::PassCtx {
            owner: owner.as_deref(),
            freq_code: Some(&state.special_buffer),
            shadow_code: Some(&state.special_buffer),
        };
        self.run_passes(
            state,
            &mut list,
            if matches!(state.active, Some(ModeKind::RareChar)) {
                crate::postprocess::profile::RARE_CHAR_PRE
            } else {
                crate::postprocess::profile::SPECIAL_PRE
            },
            &ctx,
        );
        // 纠错候选不占首选（D6）：本路不重排，与主路 / 临拼同一个 helper、同一位置
        // （全部重排之后、shadow 之前——用户置顶仍有最终话语权）。
        wind_candidate::drop_corrected_duplicates(&mut list);
        wind_candidate::protect_top_from_corrected(&mut list, state.special_buffer.len());
        self.run_passes(
            state,
            &mut list,
            crate::postprocess::profile::SPECIAL_SHADOW,
            &ctx,
        );
        state.candidates = list;
        // ⚠️ 补全收口在过滤**之后**（与主路径 `update_candidates` 同次序）：判空要落在真正的
        // 最终列表上，否则「该码下的候选被用户全部隐藏」时看到的是「还有候选」⇒ 不补 ⇒ 空屏。
        // ⚠️ 补全候选**必须同样过 `finalize_candidates`**：它是 `$CC`/`$AA`/`{..}` 的统一展开
        // 汇聚点，而这条旁路直接取自引擎、绕过了上面那一行。漏掉的表现是补出来的直通命令
        // 候选原样显示成 `$CC(...)` 源码（`result.candidates` 走了汇聚点、它没走）。
        // ⚠️ 也要过 shadow：补进来的候选同样显示在当前码的候选窗里，用户右键隐藏的往往正是
        // 它——不过滤的话隐藏完当场又被补回来。
        if state.candidates.is_empty() {
            let mut hint = self.finalize_candidates(result.completion_hints, &state.special_buffer);
            // 补全候选**同样要过生僻准入**：这条旁路直接取自引擎、绕过了上面那次过滤，
            // 不补这一句的表现是「本码没有生僻字时，补出来的却是个常用字」。
            // ⚠️ 必须走 `apply_rare_admission`（带模式守卫）而**不是**裸的
            // `retain_rare_admitted`：生僻准入只对生僻字模式成立，对快符这类 overlay
            // 模式施加等于把补全候选整批滤空。见 `apply_rare_admission` 的说明。
            self.apply_rare_admission(state.active, &mut hint);
            self.run_passes(
                state,
                &mut hint,
                crate::postprocess::profile::SPECIAL_SHADOW,
                &crate::postprocess::PassCtx {
                    owner: owner.as_deref(),
                    freq_code: None,
                    shadow_code: Some(&state.special_buffer),
                },
            );
            // 仍只采纳一条（`$AA`/`$SS` 在前缀情形折叠为单个组名候选，正常也只有一条）。
            state.candidates.extend(hint.into_iter().next());
        }
        // 自动上屏由方案码表引擎的 should_auto_commit 决定（prefix_free≈全码唯一、fixed_length 等
        // 映射到该方案的 [engine.codetable] 配置）；复核上屏目标仍在候选中。`$CC` 命令词条经
        // finalize_candidates 展开后 text 已改写为 display 标签，而引擎意向 commit_text 是原始
        // `$CC` 源 → 按 phrase_template 补匹配，返回命中候选整条供调用方按命令/文本分流。
        //
        // 引擎首轮意向（上方 convert_with_opts）判在 shadow **生效之前**的候选上：设置页把某
        // 码位的内置词条替换成自定义词条（新增用户词 + shadow 隐藏旧词条）时，改码瞬间该码位
        // 下同时存在新旧两条精确候选，引擎判「不唯一」拒绝自动上屏；shadow 生效后其实只剩一
        // 条，若不回头复评，这个否决会一直生效（论坛 #211）。与主路径 `handle_candidate.rs`
        // 的 `recheck_auto_commit` 对称：那里读的是主方案 `active_engine()`，这里必须按
        // **快符引用的方案**取引擎（`recheck_auto_commit_for`），否则会拿主方案的引擎去判
        // 快符方案的候选——同类坑见 `engine_for` 的注释。
        let auto_commit = if result.should_commit && !result.commit_text.is_empty() {
            Some(result.commit_text)
        } else {
            self.engine_mgr.recheck_auto_commit_for(
                &schema,
                &state.special_buffer,
                &state.candidates,
            )
        };
        if let Some(t) = auto_commit {
            // 引擎意向是词条原文：`$CC` 命令比 phrase_template、`$` 模板比 template_source
            // （展开后 text 已变；`{..}` 插值不放行。判据同主路复核，GH#177）。
            return state
                .candidates
                .iter()
                .find(|c| {
                    (c.is_command && c.phrase_template == t)
                        || Self::auto_commit_target_matches(c, &t)
                })
                .cloned();
        }
        None
    }

    /// 特殊模式各上屏出口（选词 / 标点顶屏 / 全码自动上屏）共用的记账：词频 + 上屏历史。
    /// 输入统计（`record_commit`）由各出口自己记（码长、位次口径各不同）。
    ///
    /// 词频记账**归属特殊方案自身**（与主方案同层级，只是用特殊按键进入）。
    /// 记账码用输入码：特殊方案是码表语义，`a`/`ab`/`abc` 是三个独立码位，
    /// 与 `freq_code` 对 CodeTable 来源的口径一致。
    ///
    /// 生僻字模式不记词频（用户拍板）：模式是一次性的逃生口，用完不留痕，正常输入的
    /// 候选顺序纹丝不动。⚠️ `record_commit`（统计）与上屏历史不在此列，它们是另外
    /// 两条通路——一并跳过会让「重复上屏」取不到刚打出来的那个生僻字。
    ///
    /// 抽出来是因为三个出口此前只有选词这一处记了：标点顶屏与自动上屏只记统计，
    /// 顶屏 / 自动上屏出去的字既不调频、`;` 也重复不出来。
    fn record_special_selection(&self, state: &State, cand: &Candidate) {
        // 反查模式：缓冲是查询串（含通配）而非码位，记账码取候选**全码**（
        // 同主路通配的 `main_freq_code`）；归属 active（`effective_data_schema` 给 None）。
        if matches!(state.active, Some(ModeKind::Reverse)) {
            let code = if cand.code.is_empty() {
                state.special_buffer.clone()
            } else {
                cand.code.clone()
            };
            self.record_selection_cased_in(None, &code, cand.freq_text(), &cand.text, cand.source);
            return;
        }
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            let code = state.special_buffer.clone();
            // 词频记 `freq_text`（模板词为源文本，与 `update_special_candidates` 的重排读端
            // 同键，GH#177），历史记实际上屏的 `text`。
            self.record_selection_cased_in(
                self.effective_data_schema(state).as_deref(),
                &code,
                cand.freq_text(),
                &cand.text,
                cand.source,
            );
        } else {
            // 上屏历史是 `record_selection_in` 的一部分，跳过词频时须单独补上。
            self.push_commit_history(&cand.text);
        }
    }

    /// 特殊模式选中某候选（全局下标 `gi`）：`$AA`/`$SS` 组折叠候选 → 补全编码到完整码重查展开（二级选择）；
    /// `$CC` 命令候选 → 执行动作（退出后异步跑，触发键码不上屏）；否则文本上屏。
    /// 统一空格 / 数字键 / 二三候选键的选中入口，保证组/命令候选选中行为一致。
    pub(crate) fn commit_special_candidate(&self, state: &mut State, gi: usize) -> KeyAction {
        let cand = state.candidates[gi].clone();
        // $AA/$SS 组折叠候选：补全编码到完整码并重查展开（不上屏组名）。
        if cand.is_group {
            state.special_buffer = cand.group_code.clone();
            state.special_cursor = state.special_buffer.len(); // 补全到完整码：光标落末尾
            self.update_special_candidates(state);
            let display = state.preedit.clone();
            let caret_pos = self.overlay_caret(state);
            self.notify_ui_update(state);
            return KeyAction::UpdateComposition {
                text: display,
                caret_pos,
            };
        }
        let code = state.special_buffer.clone();
        if let Some(act) =
            self.overlay_commit_command(state, &cand, &code, |s, st| s.exit_special_mode(st))
        {
            return act;
        }
        self.record_special_selection(state, &cand);
        self.record_commit(
            &cand.text,
            state.special_buffer.len() as u32,
            -1,
            wind_store::stats::CommitSource::SpecialMode,
        );
        self.exit_special_mode(state);
        self.notify_ui_hide();
        Self::commit_action(cand.text, true)
    }

    /// 特殊模式按键处理：编码累积 + 候选选择 + 三档自动上屏；空格选高亮、回车上屏编码原文。
    pub(crate) fn handle_special_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先，否则组合键会落到
        // 下方各臂被当成编码输入。
        if let Some(act) =
            self.overlay_ctrl_alt_guard(state, data, !state.special_buffer.is_empty(), |s, st| {
                s.exit_special_mode(st)
            })
        {
            return act;
        }
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）；置于候选导航之后，导航键优先。
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        // 进入键二次按下（缓冲空）：按中英标点配置上屏该符号并退出。
        // 顺带武装智能符号：时限内再按同键即换英文形，否则这个键被模式占着、英文形没有通路。
        // press2 的拦截在 try_activate_mode 开头，早于模式激活链。
        if state.special_buffer.is_empty()
            && self.match_special_trigger(data.key_code) == Some(state.special_id)
            && let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
        {
            let out = self.convert_punct_char(state, ch);
            self.arm_smart_symbol_after_commit(state, ch, &out);
            self.record_commit(&out, 0, -1, wind_store::stats::CommitSource::Punctuation);
            self.exit_special_mode(state);
            self.notify_ui_hide();
            return Self::commit_action(out, true);
        }
        match data.key_code {
            // Esc：放弃退出，实现收口在 `cancel_session`。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // 退格删光标前 / Delete 删光标后；缓冲被删空则退出（本就空缓冲时只有退格退出，
                // 保持原语义）。删除时不触发自动上屏。
                let backward = data.key_code == keymap::VK_BACK;
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.special_buffer,
                        &mut state.special_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if state.special_buffer.is_empty() && (removed || backward) {
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else if removed {
                    self.update_special_candidates(state);
                    let display = state.preedit.clone();
                    let caret_pos = self.overlay_caret(state);
                    self.notify_ui_update(state);
                    KeyAction::UpdateComposition {
                        text: display,
                        caret_pos,
                    }
                } else {
                    // 退格时光标已在最左 / Delete 时已在末尾：吃掉不透传。
                    KeyAction::Consumed
                }
            }
            keymap::VK_SPACE => {
                // 空格：有候选选高亮上屏（命令候选执行动作，与 `commit_highlighted` 动词同一
                // 出口）；无候选退出
                if let Some(act) = self.commit_highlighted(state) {
                    act
                } else {
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                }
            }
            keymap::VK_RETURN => {
                // clear 模式：整段放弃，不上屏任何内容。须先于下方各分支——此前该判断只写在
                // 「空缓冲」分支内，导致「打了码再回车」仍走非空缓冲路径无条件上屏编码原文，
                // 配置形同虚设（与主输入路径行为不一致）。
                if self.enter_clears_composition() {
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 空缓冲（只按了模式键、还没敲编码）：commit 模式上屏模式键符号本身
                // （原样不转换，补输被占用的符号，如 \）。
                if state.special_buffer.is_empty() {
                    if !state.special_prefix.is_empty() {
                        let sym = state.special_prefix.clone();
                        self.record_commit(
                            &sym,
                            0,
                            -1,
                            wind_store::stats::CommitSource::Punctuation,
                        );
                        self.exit_special_mode(state);
                        self.notify_ui_hide();
                        return Self::commit_action(sym, true);
                    }
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 非空缓冲：上屏编码原文（原行为不变）
                let text = state.special_buffer.clone();
                self.record_commit(
                    &text,
                    text.len() as u32,
                    -1,
                    wind_store::stats::CommitSource::SpecialMode,
                );
                self.exit_special_mode(state);
                self.notify_ui_hide();
                Self::commit_action(text, true)
            }
            keymap::VK_1..=keymap::VK_9 => {
                // 数字 1-9 选当前页候选（命令候选执行动作）
                let (start, end) = self.page_range(state);
                let gi = start + (data.key_code - 0x31) as usize;
                if gi < end {
                    self.commit_special_candidate(state, gi)
                } else {
                    KeyAction::Consumed
                }
            }
            keymap::VK_A..=keymap::VK_Z => {
                // 字母：小写归一，在光标处插入
                let ch = (b'a' + (data.key_code - 0x41) as u8) as char;
                preedit_cursor::BufEdit::new(&mut state.special_buffer, &mut state.special_cursor)
                    .insert(ch);
                if let Some(cand) = self.update_special_candidates(state) {
                    return self.special_auto_commit(state, cand);
                }
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 反查模式的符号通配键（方案 `wildcard_key` 配成 `?` 之类）：进缓冲作通配，
                // **先于**选词键与标点顶屏——否则它在模式里要么选了词、要么把候选顶上屏。
                // 字母通配键走上面的字母臂，不经这里。
                if matches!(state.active, Some(ModeKind::Reverse))
                    && let Some(ch) = punct_char(data.key_code, shift)
                    && self.engine_mgr.active_reverse_key() == Some(ch)
                {
                    preedit_cursor::BufEdit::new(
                        &mut state.special_buffer,
                        &mut state.special_cursor,
                    )
                    .insert(ch);
                    self.update_special_candidates(state);
                    let display = state.preedit.clone();
                    let caret_pos = self.overlay_caret(state);
                    self.notify_ui_update(state);
                    return KeyAction::UpdateComposition {
                        text: display,
                        caret_pos,
                    };
                }
                // 二三候选键 → 选候选（命令候选执行动作）
                if !shift && let Some(offset) = self.select_key_offset(data.key_code) {
                    let (start, end) = self.page_range(state);
                    let gi = start + offset;
                    if gi < end {
                        return self.commit_special_candidate(state, gi);
                    }
                }
                // 其它可打印标点：顶屏当前高亮候选 + 转换后标点，退出。
                // 小键盘键（direct 语义）回退 numpad_char 复用此路：特殊模式缓冲是编码，
                // 数字非法 → 顶屏候选再输出该字符，与主路径 direct 同构。follow_main 时键已在
                // 入口归一化为主键盘键，走上面的数字选词臂。
                if let Some(ch) =
                    punct_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                {
                    let hi = if state.candidates.is_empty() {
                        None
                    } else {
                        Some(
                            self.highlighted_global_index(state)
                                .min(state.candidates.len() - 1),
                        )
                    };
                    // 高亮候选为组/命令：走统一选中（组→展开重查，命令→执行动作），
                    // 触发标点不单独上屏（语义同 top_commit_command_guard）。
                    if let Some(idx) = hi
                        && (state.candidates[idx].is_group || state.candidates[idx].is_command)
                    {
                        return self.commit_special_candidate(state, idx);
                    }
                    let committed = match hi {
                        Some(idx) => {
                            // 顶屏也是一次选中：记账同选词出口（`record_special_selection`）。
                            let cand = state.candidates[idx].clone();
                            self.record_special_selection(state, &cand);
                            self.record_commit(
                                &cand.text,
                                state.special_buffer.len() as u32,
                                -1,
                                wind_store::stats::CommitSource::SpecialMode,
                            );
                            cand.text
                        }
                        // 无候选：与主路空码标点同一判据（`punct_on_empty_behavior`）。字母引导符
                        // 算码，理由同临拼（见 `temp_pinyin_punct_without_candidate`）。
                        // 小键盘（direct）维持既有口径（同主路 `commit_highlight_then_char`）。
                        None if punct_char(data.key_code, shift).is_none() => String::new(),
                        None => {
                            let guide = Self::guide_to_return(&state.special_prefix, "");
                            let raw = format!("{guide}{}", state.special_buffer);
                            match self.punct_empty_code_policy_for(state, &raw) {
                                PunctEmptyCodePolicy::ClearNoInput => {
                                    self.exit_special_mode(state);
                                    self.notify_ui_hide();
                                    return KeyAction::ClearComposition;
                                }
                                PunctEmptyCodePolicy::Clear => String::new(),
                                PunctEmptyCodePolicy::Commit => {
                                    if !raw.is_empty() {
                                        self.record_commit(
                                            &raw,
                                            raw.len() as u32,
                                            -1,
                                            wind_store::stats::CommitSource::SpecialMode,
                                        );
                                        // 原码类上屏进上屏历史（不含标点，同主路）；原码不记词频。
                                        self.push_commit_history(&raw);
                                    }
                                    raw
                                }
                            }
                        }
                    };
                    let punct = self.convert_punct_char(state, ch);
                    self.record_commit(&punct, 0, -1, wind_store::stats::CommitSource::Punctuation);
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    Self::commit_action(format!("{}{}", committed, punct), true)
                } else {
                    KeyAction::Consumed
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "handle_special_tests.rs"]
mod tests;
