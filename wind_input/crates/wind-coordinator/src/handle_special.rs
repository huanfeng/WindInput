//! 特殊方案输入模式
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。

use crate::coordinator::{Coordinator, State, numpad_char, punct_char};
use crate::pipeline::ModeKind;
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::Candidate;
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
        // `[overlay]` 段快照：布局/注释/进入即展示三处都取它，见 `State::overlay_spec`。
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
        KeyAction::UpdateComposition {
            text: display.clone(),
            caret_pos: display.chars().count() as u32,
        }
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
        KeyAction::UpdateComposition {
            text: display.clone(),
            caret_pos: display.chars().count() as u32,
        }
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

    /// 生僻字模式额外纳入的区块（`input.rare_char.include_blocks` 的解析结果）。
    fn rare_char_blocks(&self) -> wind_candidate::BlockMask {
        self.rt().rare_char_blocks
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
    fn rare_admit_fn(
        &self,
        state: &State,
    ) -> Option<std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>> {
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
        let extra = self.rare_char_blocks();
        Some(std::sync::Arc::new(move |text: &str| {
            let g = cc.read().unwrap_or_else(|e| e.into_inner());
            wind_candidate::rare_admits(text, &g, extra)
        }))
    }

    /// 对当前候选施加生僻字准入（仅生僻字模式；其余模式为空操作）。
    fn apply_rare_admission(&self, state: &mut State) {
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            return;
        }
        self.retain_rare_admitted(&mut state.candidates);
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
        let result = self
            .engine_mgr
            .convert_with(schema, &state.special_buffer, RARE_REFILL_LIMIT);
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
        // 额外纳入的区块（`input.rare_char.include_blocks`）。解析结果取自 `rt()` 的镜像，
        // 与词频那份一样在装载期算好——本函数在每次按键的候选刷新路径上。
        let extra = self.rare_char_blocks();
        candidates.retain(|c| wind_candidate::rare_admits(&c.text, &cc, extra));
    }

    /// 退出特殊模式并清空相关状态（码表缓存保留供复用）。
    pub(crate) fn exit_special_mode(&self, state: &mut State) {
        state.active = None;
        state.overlay_spec = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        state.special_prefix.clear();
        state.candidates.clear();
        state.preedit.clear();
    }

    /// 按当前编码缓冲刷新特殊模式候选（经其引用方案的引擎查询，复用方案 CodeTableSpec 全码策略）。
    /// 返回 Some(候选) 表示该方案的全码策略请求自动上屏该候选（`$CC` 命令候选由调用方
    /// 走命令执行路径，普通候选上屏其文本）。
    pub(crate) fn update_special_candidates(&self, state: &mut State) -> Option<Candidate> {
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
                self.apply_shadow_in(owner.as_deref(), &mut state.candidates, "");
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
        let opts = wind_engine::ConvertOptions {
            admit: self.rare_admit_fn(state),
            ..Default::default()
        };
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
        self.apply_rare_admission(state);
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
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            self.apply_freq_rerank_in(
                owner.as_deref(),
                &mut state.candidates,
                &state.special_buffer,
            );
        }
        self.apply_shadow_in(
            owner.as_deref(),
            &mut state.candidates,
            &state.special_buffer,
        );
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
            self.retain_rare_admitted(&mut hint);
            self.apply_shadow_in(owner.as_deref(), &mut hint, &state.special_buffer);
            // 仍只采纳一条（`$AA`/`$SS` 在前缀情形折叠为单个组名候选，正常也只有一条）。
            state.candidates.extend(hint.into_iter().next());
        }
        // 自动上屏由方案码表引擎的 should_auto_commit 决定（prefix_free≈全码唯一、fixed_length 等
        // 映射到该方案的 [engine.codetable] 配置）；复核上屏目标仍在候选中。`$CC` 命令词条经
        // finalize_candidates 展开后 text 已改写为 display 标签，而引擎意向 commit_text 是原始
        // `$CC` 源 → 按 phrase_template 补匹配，返回命中候选整条供调用方按命令/文本分流。
        if result.should_commit && !result.commit_text.is_empty() {
            let t = &result.commit_text;
            return state
                .candidates
                .iter()
                .find(|c| &c.text == t || (c.is_command && &c.phrase_template == t))
                .cloned();
        }
        None
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
        // 词频记账**归属特殊方案自身**（与主方案同层级，只是用特殊按键进入）。
        // 记账码用输入码：特殊方案是码表语义，`a`/`ab`/`abc` 是三个独立码位，
        // 与 `freq_code` 对 CodeTable 来源的口径一致。
        //
        // 此前这里只有 record_commit（统计），完全不记词频——特殊模式的候选顺序
        // 因此永远是词库原序，用户选过多少次都不会往前走。
        // 生僻字模式不记词频（用户拍板）：模式是一次性的逃生口，用完不留痕，正常输入的
        // 候选顺序纹丝不动。⚠️ `record_commit`（统计）与上屏历史不在此列，它们是另外
        // 两条通路——一并跳过会让「重复上屏」取不到刚打出来的那个生僻字。
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            self.record_selection_in(
                self.effective_data_schema(state).as_deref(),
                &code,
                &cand.text,
                cand.source,
            );
        }
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
                // 空格：有候选选高亮上屏（命令候选执行动作）；无候选退出
                if !state.candidates.is_empty() {
                    let idx = self
                        .highlighted_global_index(state)
                        .min(state.candidates.len() - 1);
                    self.commit_special_candidate(state, idx)
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
                    // $CC 命令候选自动命中：与手动选中同路（退出模式 + 异步执行动作）。
                    let code = state.special_buffer.clone();
                    if let Some(act) = self.overlay_commit_command(state, &cand, &code, |s, st| {
                        s.exit_special_mode(st)
                    }) {
                        return act;
                    }
                    self.record_commit(
                        &cand.text,
                        state.special_buffer.len() as u32,
                        -1,
                        wind_store::stats::CommitSource::SpecialMode,
                    );
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    return Self::commit_action(cand.text, true);
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
                    let committed = hi
                        .map(|idx| state.candidates[idx].text.clone())
                        .unwrap_or_default();
                    let punct = self.convert_punct_char(state, ch);
                    self.record_commit(
                        &committed,
                        state.special_buffer.len() as u32,
                        -1,
                        wind_store::stats::CommitSource::SpecialMode,
                    );
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
mod tests {
    //! 直达热键进入特殊模式/临拼的单元测试（无头 Coordinator + 临时 store）。
    //! headless 下无引擎，故只覆盖不依赖引擎查询的行为：id→idx 定位、空前缀进入、半成品上屏。
    use super::*;
    use crate::coordinator::Coordinator;
    use std::sync::Arc;
    use wind_candidate::Candidate;
    use wind_config::Config;
    use wind_store::Store;

    /// 造一个含若干 overlay 方案的 data_dir。
    ///
    /// 实例集合的真相源已是**方案文件目录**（带 `[overlay]` 段者入表），不再是
    /// `config.schema.special_modes` 数组——故装置从「造 Config」改成「造方案文件」。
    fn data_dir_with_overlays(tag: &str, ids: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wind_special_hk_data_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let schemas = dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        for id in ids {
            std::fs::write(
                schemas.join(format!("{id}.schema.toml")),
                format!(
                    "[schema]\nid = \"{id}\"\nname = \"{id}\"\nhidden = true\n\
                     [engine]\ntype = \"codetable\"\n\
                     [overlay]\nkind = \"special\"\n"
                ),
            )
            .unwrap();
        }
        dir
    }

    fn coord_with(tag: &str, cfg: Config) -> Arc<Coordinator> {
        coord_with_overlays(tag, cfg, &[])
    }

    fn coord_with_overlays(tag: &str, cfg: Config, ids: &[&str]) -> Arc<Coordinator> {
        let path = std::env::temp_dir().join(format!("wind_special_hk_{tag}.redb"));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let dir = (!ids.is_empty()).then(|| data_dir_with_overlays(tag, ids));
        Coordinator::new_headless_with_store(cfg, dir.as_deref(), store)
    }

    /// id → 下标定位走 overlay 注册表（按方案 id 字典序），未知 id 返回 None。
    ///
    /// ⚠️ **只断言相对顺序，不断言绝对下标**：`installed_schemas` 会一并扫描
    /// `Config::user_config_dir()/schemas`，开发机上那里可能装着真实的快符方案，
    /// 绝对下标会随之平移。这不是测试将就，而是下标语义本身就是「注册表内的位次」。
    #[test]
    fn special_mode_idx_locates_by_registry_order() {
        let c = coord_with_overlays("idx", Config::default(), &["zz_c", "zz_a", "zz_b"]);
        let (a, b, cc) = (
            c.special_mode_idx("zz_a").expect("zz_a 应在表内"),
            c.special_mode_idx("zz_b").expect("zz_b 应在表内"),
            c.special_mode_idx("zz_c").expect("zz_c 应在表内"),
        );
        // 文件写入序是 c/a/b，注册表按 id 字典序 ⇒ a < b < c。这条同时证明顺序
        // 不来自任何「配置顺序」——那正是本次改造要消除的东西。
        assert!(a < b && b < cc, "应按 id 字典序：a={a} b={b} c={cc}");
        // 未知 id → None（分发点据此安全吞键，不 panic）
        assert_eq!(c.special_mode_idx("nope"), None);
    }

    /// 进入模式时把 `[overlay]` 段快照进 `State`，退出时清掉。
    ///
    /// 快照是布局/注释/进入即展示三处的取值来源，见 `State::overlay_spec`。
    #[test]
    fn entering_snapshots_overlay_spec_and_exit_clears_it() {
        let c = coord_with_overlays("snap", Config::default(), &["zz_snap"]);
        let idx = c.special_mode_idx("zz_snap").expect("应在表内");
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        assert!(st.overlay_spec.is_none(), "进入前无快照");

        c.enter_special_mode(&mut st, idx, 0);
        let spec = st.overlay_spec.as_ref().expect("进入后应有快照");
        assert_eq!(spec.kind, "special", "快照应取自方案文件的 [overlay] 段");

        c.exit_special_mode(&mut st);
        assert!(st.overlay_spec.is_none(), "退出后快照必须清掉");
    }

    #[test]
    fn commit_and_enter_special_writes_no_guide_prefix() {
        let c = coord_with_overlays("enter_empty", Config::default(), &["zz_rare"]);
        // 下标必须按 id 现取, 不能硬编码 0——见 `special_mode_idx_locates_by_registry_order`
        // 的说明: 注册表会一并收进用户目录里真实安装的 overlay 方案, 绝对下标随之平移。
        let idx = c.special_mode_idx("zz_rare").expect("zz_rare 应在表内");
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        // 空缓冲进入：无半成品可上屏 → 返回 UpdateComposition，组合区无引导符。
        let act = c.commit_and_enter_special_mode(&mut st, idx, 0);
        assert_eq!(st.active, Some(ModeKind::Special(idx)));
        assert!(
            st.special_prefix.is_empty(),
            "热键进入不应写引导符（special_prefix 应空）"
        );
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    #[test]
    fn commit_and_enter_special_commits_pending_candidate() {
        let c = coord_with_overlays("enter_commit", Config::default(), &["zz_rare"]);
        // 同上：硬编码 0 会在装了真实 overlay 方案的开发机上落到别人的方案头上。
        // 本用例尤其致命——真实快符方案 `kf` 配了 `show_all_on_enter = true`，进入即装填
        // 候选，末尾的「候选应清空」断言必挂，而 CI 因无用户方案目录照常全绿。
        let idx = c.special_mode_idx("zz_rare").expect("zz_rare 应在表内");
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        // 模拟普通输入半成品：编码 + 高亮候选。
        st.input_buffer = "aa".to_string();
        st.candidates = vec![Candidate {
            text: "工".to_string(),
            ..Default::default()
        }];
        st.selected_index = 0;
        st.current_page = 0;
        let act = c.commit_and_enter_special_mode(&mut st, idx, 0);
        // 进入前的高亮候选应作为 InsertText 上屏，随后进入目标模式、组合区无引导符。
        match act {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "工"),
            other => panic!("应上屏半成品并进入模式，实际 {other:?}"),
        }
        assert_eq!(st.active, Some(ModeKind::Special(idx)));
        assert!(st.special_prefix.is_empty());
        assert!(st.candidates.is_empty());
    }

    #[test]
    fn commit_and_enter_temp_pinyin_zero_keycode_has_no_prefix() {
        let c = coord_with("temp_zero", Config::default());
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        // key_code=0 哨兵：进入临拼但组合区无引导符（对齐特殊模式）。
        let _ = c.commit_and_enter_temp_pinyin(&mut st, 0, "pinyin".to_string());
        assert_eq!(st.active, Some(ModeKind::TempPinyin));
        assert!(
            st.temp_pinyin_prefix.is_empty(),
            "直达热键（key_code=0）进入临拼不应写引导符"
        );
    }

    /// 「顶屏 + 进模式」收尾按 top_commit_mode 分流（与顶码上屏统一）：
    /// direct_commit（默认）+ 引导符新组合 → 真提交 + 延迟组合；新组合为空 → 直接真提交；
    /// pre_confirm → InsertText 聚合。
    #[test]
    fn commit_then_new_composition_follows_top_commit_mode() {
        let c = coord_with("ctnc_direct", Config::default());
        match c.commit_then_new_composition("可能".to_string(), "`".to_string()) {
            KeyAction::CommitThenDeferComposition {
                commit_text,
                deferred_composition,
                ..
            } => {
                assert_eq!(commit_text, "可能");
                assert_eq!(deferred_composition, "`");
            }
            other => panic!("direct_commit 有新组合应走真提交+延迟组合，实际 {other:?}"),
        }
        match c.commit_then_new_composition("可能".to_string(), String::new()) {
            KeyAction::InsertText {
                text,
                new_composition,
                has_new_composition,
                ..
            } => {
                assert_eq!(text, "可能");
                assert!(new_composition.is_none() && !has_new_composition);
            }
            other => panic!("新组合为空应直接真提交，实际 {other:?}"),
        }

        let mut cfg = Config::default();
        cfg.input.top_commit_mode = wind_config::TopCommitMode::PreConfirm;
        let c = coord_with("ctnc_pre", cfg);
        match c.commit_then_new_composition("可能".to_string(), "`".to_string()) {
            KeyAction::InsertText {
                text,
                new_composition,
                has_new_composition,
                ..
            } => {
                assert_eq!(text, "可能");
                assert_eq!(new_composition.as_deref(), Some("`"));
                assert!(has_new_composition);
            }
            other => panic!("pre_confirm 应走 InsertText 聚合，实际 {other:?}"),
        }
    }

    /// ── 生僻字模式 ────────────────────────────────────────────────────────────
    ///
    /// 这几条**刻意不依赖真实词库**：本 worktree 没有 `build_dev/data` 时，依赖词库的
    /// 端到端用例会整族静默跳过而计数照绿（判据是耗时 0.00s）。模式的生命周期与准入
    /// 判据是这轮的核心性质，不能挂在一个可能没跑的测试上。
    mod rare_char {
        use crate::coordinator::Coordinator;
        use crate::pipeline::ModeKind;
        use wind_config::Config;

        fn coord() -> std::sync::Arc<Coordinator> {
            Coordinator::new_headless(Config::default(), None)
        }

        /// 进入后模式标识为 rare_char，且缓冲被清空（顶字重开的语义）。
        #[test]
        fn enters_and_reports_its_own_mode() {
            let c = coord();
            let mut st = c.state.lock().unwrap();
            c.enter_rare_char_mode(&mut st, 0);
            assert_eq!(st.active, Some(ModeKind::RareChar));
            assert!(st.special_buffer.is_empty(), "顶字重开：进入时缓冲为空");
            assert!(st.input_buffer.is_empty(), "主输入缓冲一并清空");
            // ⚠️ special 专属状态必须留在默认值：凡从 overlay_spec 取值的地方都会因此
            // 落到默认档（布局/注释跟随全局、show_all_on_enter=false），这是刻意的。
            assert!(st.overlay_spec.is_none(), "生僻字模式没有 [overlay] 段可读");
        }

        /// 退出复用 `exit_special_mode`，模式与缓冲一并清干净。
        ///
        /// 留下一个没清的 `active` 会让后续按键继续走 overlay 分派，表现为「Esc 之后
        /// 打字没反应」——而 `exit_special_mode` 是两个模式共用的那一份，这里钉住它对
        /// 生僻字模式同样有效。
        #[test]
        fn exits_cleanly() {
            let c = coord();
            let mut st = c.state.lock().unwrap();
            c.enter_rare_char_mode(&mut st, 0);
            c.exit_special_mode(&mut st);
            assert_eq!(st.active, None);
            assert!(st.special_buffer.is_empty());
            assert!(st.preedit.is_empty());
        }

        /// ★ 配置真的走到了判据。
        ///
        /// 本仓的经典失效形态是「配置四层就位、消费点却在不可达的调用点上」——开关配了
        /// 毫无反应，且没有任何报错。这条测试从**配置**出发一路走到**候选列表**，
        /// 中间任何一环断掉都会红：
        /// `input.rare_char.include_blocks` → ConfigBundle 预解析 → `rare_char_blocks()`
        /// → `rare_admits` 的 extra 参数。
        #[test]
        fn include_blocks_config_reaches_the_verdict() {
            let mut cfg = Config::default();
            cfg.input.rare_char.include_blocks = vec!["emoji".to_string()];
            let c = Coordinator::new_headless(cfg, None);
            // 常用字表必须非空，否则 `retain_rare_admitted` 走「表未加载」那条早退。
            set_common(&c, ['我', '你', '好']);

            let mut cands = vec![
                cand("我"),   // 常用汉字 → 滤掉
                cand("龘"),   // 生僻汉字 → 留下
                cand("😀"),   // 域外字符，靠 include_blocks 纳入 → 留下
                cand("ㄅ"),   // 域外字符，没配它那一档 → 滤掉
                cand("你好"), // 多字词 → 滤掉
            ];
            c.retain_rare_admitted(&mut cands);
            let got: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
            assert_eq!(got, vec!["龘", "😀"], "配置未生效或判据接错");
        }

        /// 不配 include_blocks 时 emoji 进不来（与上一条互为对照，锁住「是配置起的作用」）。
        #[test]
        fn without_include_blocks_emoji_stays_out() {
            let c = coord();
            set_common(&c, ['我']);
            let mut cands = vec![cand("龘"), cand("😀")];
            c.retain_rare_admitted(&mut cands);
            let got: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
            assert_eq!(got, vec!["龘"], "没配区块时域外字符不该进来");
        }

        /// `$rare_char` 必须被进入门卫认作有效成员。
        ///
        /// 它不是真实方案，`ensure_schema` 对它必然失败 ⇒ 会被 `mix_members`（真实方案
        /// 列表）滤掉。若门卫只看那份列表，一个只配了 `$rare_char` 的 mix 就**进都进不去**
        /// ——按引导键毫无反应，且没有任何报错。
        #[test]
        fn rare_char_member_is_recognized_by_the_entry_gate() {
            let mut cfg = Config::default();
            let m = cfg
                .schema
                .mix_modes
                .get_mut(0)
                .expect("出厂应有内置 quick_mix");
            m.members = vec![wind_config::config::MIX_MEMBER_RARE_CHAR.to_string()];
            let c = Coordinator::new_headless(cfg, None);

            assert!(c.mix_has_rare_char(0), "门卫须认得生僻字成员");
            assert!(
                c.mix_members(0).is_empty(),
                "它不是真实方案，不该出现在真实方案列表里"
            );
            // 两者合起来才是完整判据：真实方案列表为空，但门卫仍应放行。
            assert!(
                !c.mix_has_quick_input(0),
                "本用例已把 members 换成只有生僻字，不应再含 quick 来源"
            );
        }

        /// 生僻字成员**不该**触发强制竖排。
        ///
        /// 竖排是 `mix_has_quick_input` 的语义（内置来源的候选是长文本，横排放不下），
        /// 而生僻字成员产出的是普通单字。两个谓词合并的话，只配生僻字的 mix 会平白变竖排。
        #[test]
        fn rare_char_member_does_not_force_vertical() {
            let mut cfg = Config::default();
            let m = cfg.schema.mix_modes.get_mut(0).unwrap();
            m.members = vec![wind_config::config::MIX_MEMBER_RARE_CHAR.to_string()];
            let c = Coordinator::new_headless(cfg, None);
            assert!(c.mix_has_rare_char(0));
            assert!(
                !c.mix_has_quick_input(0),
                "生僻字成员不属于 quick 来源族，不应带来竖排"
            );
        }

        /// ── 真实词库端到端 ──────────────────────────────────────────────────
        ///
        /// ⚠️ 上面那些用例走的都是自造夹具，验的是判据与生命周期；**真实词库下这个模式
        /// 到底还剩几个候选**是另一回事，只有这里能答。缺数据时整族静默跳过而计数照绿
        /// （本仓的老坑），故守卫里带 eprintln，别只看绿灯。
        /// 补数据：worktree 根建 `build_dev` 符号链接指向主仓，或跑 `dev.ps1 gd`。
        fn wubi_coord(tag: &str) -> Option<std::sync::Arc<Coordinator>> {
            let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../build_dev/data");
            if !d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
                eprintln!("跳过 {tag}：五笔词库不存在（build_dev/data 缺失）");
                return None;
            }
            let mut cfg = Config::default();
            cfg.schema.active = "wubi86".into();
            cfg.schema.available = vec!["wubi86".into()];
            Some(Coordinator::new_headless(cfg, Some(&d)))
        }

        /// ★ 下推给引擎的准入与调用方兜底的过滤**必须同源**。
        ///
        /// 两者现在都走 `wind_candidate::rare_admits`，但它们是两处独立的调用点，
        /// 将来任何一侧「顺手改一下判据」都不会有编译错误。失效是静默且方向相反的：
        /// 引擎侧更严 ⇒ 调用方想要的候选压根没产出；引擎侧更松 ⇒ 白占配额还是被滤掉。
        #[test]
        fn engine_side_admit_matches_caller_side_filter() {
            let c = coord();
            set_common(&c, ['工', '水']);
            let mut st = c.state.lock().unwrap();
            st.chinese_mode = true;
            c.enter_rare_char_mode(&mut st, 0);
            let f = c.rare_admit_fn(&st).expect("生僻字模式应给出准入闭包");
            c.exit_special_mode(&mut st);
            drop(st);
            // 逐条比对两侧结论：常用字、生僻字、多字词、空串、空白。
            for t in ["工", "水", "沝", "龘", "工作", "", " "] {
                let engine_side = f(t);
                let mut v = vec![cand(t)];
                c.retain_rare_admitted(&mut v);
                let caller_side = !v.is_empty();
                assert_eq!(
                    engine_side, caller_side,
                    "{t:?} 两侧结论不一致：引擎侧={engine_side} 调用方={caller_side}"
                );
            }
        }

        /// 常用字表未加载时不下推准入——那时全体候选都会被判成「非常用」，
        /// 筛了等于没筛，还白付一次判定。与 `retain_rare_admitted` 的同款保护成对。
        #[test]
        fn no_admit_pushdown_when_common_table_is_missing() {
            let c = coord();
            let mut st = c.state.lock().unwrap();
            st.chinese_mode = true;
            c.enter_rare_char_mode(&mut st, 0);
            assert!(c.rare_admit_fn(&st).is_none(), "常用字表为空时不该下推准入");
            c.exit_special_mode(&mut st);
        }

        /// 非生僻字模式不下推——普通 special 模式的候选不该被筛掉。
        #[test]
        fn no_admit_pushdown_outside_rare_mode() {
            let c = coord();
            set_common(&c, ['工']);
            let st = c.state.lock().unwrap();
            assert!(c.rare_admit_fn(&st).is_none(), "非生僻字模式不该下推准入");
        }

        /// 拼音方案下的生僻字模式。与 `wubi_coord` 分开是因为两者暴露的问题不同：
        /// 码表方案的候选总数够不着取数上限，「截断 → 过滤」那个缺陷在五笔端到端测试里
        /// **完全看不出来**。
        fn pinyin_coord(tag: &str) -> Option<std::sync::Arc<Coordinator>> {
            let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../build_dev/data");
            if !d
                .join("schemas/pinyin/rime_frost.dict.merged.wdat")
                .exists()
            {
                eprintln!("跳过 {tag}：拼音词库不存在（build_dev/data 缺失）");
                return None;
            }
            let mut cfg = Config::default();
            cfg.schema.active = "pinyin".into();
            cfg.schema.available = vec!["pinyin".into()];
            Some(Coordinator::new_headless(cfg, Some(&d)))
        }

        /// ★★★ 高频音节在修复前只能出个位数候选。
        ///
        /// `yi` 这个音有 1183 个非常用字，而取数上限 100 施加在过滤**之前**、引擎又按
        /// 常用度排序 ⇒ 修复前只漏出 4 个（漏掉 99.7%）。用户会认为功能坏了。
        ///
        /// 断言取「远多于修复前」而不是某个精确条数：条数随词库版本变，而 4 → 上百这个
        /// 数量级差异不会因换词库而消失。
        #[test]
        fn pinyin_high_frequency_syllable_is_not_starved_by_the_limit() {
            let Some(c) = pinyin_coord("yi") else { return };
            let got = rare_cands(&c, "yi");
            assert!(
                got.len() > 50,
                "高频音节 yi 应能出大量生僻字，实得 {} 条：{:?}",
                got.len(),
                got.iter().take(10).collect::<Vec<_>>()
            );
            assert!(got.iter().all(|t| t.chars().count() == 1), "仍须只出单字");
        }

        /// 候选总数**够不着**上限的码，不受重取影响（也不该白付一次重取）。
        ///
        /// `zhui` 全部候选 72 条 < 100，第一次就取全了；过滤后 57 条也够一页，
        /// 于是 `refill_rare_if_short` 直接返回。这条钉住「常见情况不劣化」。
        #[test]
        fn pinyin_short_candidate_list_needs_no_refill() {
            let Some(c) = pinyin_coord("zhui") else {
                return;
            };
            let got = rare_cands(&c, "zhui");
            assert!(got.len() >= 10, "zhui 应有足量生僻字，实得 {}", got.len());
            assert!(
                got.contains(&"沝".to_string()),
                "沝 是 zhui 的生僻字，应在列"
            );
        }

        /// ★ 码表方案**也**受益于重取——这条测试写下时我以为它会证明「码表不变」，
        /// 结果反过来证伪了那个假设：`ii` 从 1 条变 15 条。
        ///
        /// 留着它是为了钉住这个认知：截断的影响取决于**同码候选有多少**，与方案类型
        /// 无关。二级简码 `ii` 同码字多，切掉大半；四码全码 `sivg` 只有一个解，不受影响
        /// （由 `real_dict_keeps_only_the_rare_homograph` 钉住那一侧）。
        #[test]
        fn codetable_short_code_also_gains_from_refill() {
            let Some(c) = wubi_coord("ii-refill") else {
                return;
            };
            let got = rare_cands(&c, "ii");
            assert!(
                got.len() > 5,
                "二级简码 ii 的同码生僻字远不止一个，实得 {got:?}"
            );
            assert!(got.contains(&"沝".to_string()), "原有的沝仍须在列");
            assert!(got.iter().all(|t| t.chars().count() == 1), "仍须只出单字");
        }

        /// 取某个码在生僻字模式下的候选文本。
        fn rare_cands(c: &Coordinator, code: &str) -> Vec<String> {
            let mut st = c.state.lock().unwrap();
            st.chinese_mode = true;
            c.enter_rare_char_mode(&mut st, 0);
            st.special_buffer = code.to_string();
            st.special_cursor = code.len();
            c.update_special_candidates(&mut st);
            let out = st.candidates.iter().map(|x| x.text.clone()).collect();
            c.exit_special_mode(&mut st);
            out
        }

        /// 取某个码在**普通输入路**的候选文本（对照组）。
        fn normal_cands(c: &Coordinator, code: &str) -> Vec<String> {
            let mut st = c.state.lock().unwrap();
            st.chinese_mode = true;
            st.input_buffer = code.to_string();
            c.update_candidates(&mut st);
            let out = st.candidates.iter().map(|x| x.text.clone()).collect();
            st.input_buffer.clear();
            st.candidates.clear();
            out
        }

        /// ★ 真实五笔词库下，同码位的常用字被滤掉、生僻字留下。
        ///
        /// `sivg` 是本仓反复出现的样本：常用「档」与生僻「桜」同码，检索范围放宽那一轮
        /// 也用的它。这里精确断言而非只验性质——它稳定到足以当回归锁，词库升级真把它改了
        /// 也该有人来看一眼。
        #[test]
        fn real_dict_keeps_only_the_rare_homograph() {
            let Some(c) = wubi_coord("sivg") else { return };
            assert_eq!(
                normal_cands(&c, "sivg"),
                vec!["档"],
                "对照组：普通输入只出常用字"
            );
            assert_eq!(
                rare_cands(&c, "sivg"),
                vec!["桜"],
                "生僻字模式应只留同码的生僻字"
            );
        }

        /// ★ 真实词库下的性质断言：候选**非空**、**全是单字**、**全部判非常用**。
        ///
        /// 非空这一条最要紧——「严格过滤空了就空着」是设计取舍，但如果连 `a` 这种一级简码
        /// 都滤空了，这个模式就是个摆设，而所有自造夹具的测试都不会红。
        #[test]
        fn real_dict_yields_single_uncommon_chars() {
            let Some(c) = wubi_coord("a") else { return };
            let got = rare_cands(&c, "a");
            assert!(!got.is_empty(), "一级简码下不该一个候选都没有");

            let cc = c.common_chars.read().unwrap();
            for t in &got {
                assert!(
                    wind_candidate::single_markable_char(t).is_some(),
                    "「{t}」不是单字——严格只出单字这条没生效"
                );
                assert!(
                    !cc.is_string_common(t),
                    "「{t}」是常用字，不该出现在生僻字模式"
                );
            }
            // 普通输入路里那些常用字，一个都不该漏进来。
            let normal = normal_cands(&c, "a");
            assert!(normal.len() > got.len(), "普通候选应远多于生僻候选");
            for t in ["工", "戈", "式"] {
                assert!(normal.contains(&t.to_string()), "对照组应含常用字「{t}」");
                assert!(
                    !got.contains(&t.to_string()),
                    "常用字「{t}」漏进了生僻字模式"
                );
            }
        }

        /// 多字词一个都不该进来（`ii` 的普通候选里有「洋洋洒洒」这类）。
        #[test]
        fn real_dict_drops_multi_char_words() {
            let Some(c) = wubi_coord("ii") else { return };
            let normal = normal_cands(&c, "ii");
            assert!(
                normal.iter().any(|t| t.chars().count() > 1),
                "对照组里应当有多字词，否则这条测试什么都没验到"
            );
            for t in rare_cands(&c, "ii") {
                assert_eq!(
                    wind_candidate::semantic_units(&t),
                    1,
                    "「{t}」是多字词，不该进生僻字模式"
                );
            }
        }

        /// ★ 生僻字模式不污染普通输入路：进出一趟之后，同一个码的普通候选逐字节不变。
        ///
        /// 「完全不记词频」那条取舍的端到端体现——模式内选过什么都不该改变正常输入的顺序。
        #[test]
        fn real_dict_normal_path_unaffected_by_the_mode() {
            let Some(c) = wubi_coord("roundtrip") else {
                return;
            };
            let before = normal_cands(&c, "ii");
            let _ = rare_cands(&c, "ii");
            let after = normal_cands(&c, "ii");
            assert_eq!(before, after, "进出生僻字模式不应改变普通输入的候选");
        }

        /// 装一份最小常用字表。`retain_rare_admitted` 对空表整条早退（见那条测试），
        /// 故凡要验过滤结果的用例都得先装表。
        fn set_common(c: &Coordinator, chars: impl IntoIterator<Item = char>) {
            *c.common_chars.write().unwrap() = wind_candidate::CommonChars::from_base(chars);
        }

        fn cand(text: &str) -> wind_candidate::Candidate {
            wind_candidate::Candidate {
                text: text.to_string(),
                ..Default::default()
            }
        }

        /// ★ 常用字表未加载时**整条不过滤**，而不是放行全部。
        ///
        /// 判据与 `apply_filter` 那道 `common_chars.is_empty()` 同源：表没加载时全体候选
        /// 都会被判「非常用」，准入于是放行**全部**候选——那不是生僻字模式，是一个没有
        /// 过滤的普通列表，而用户完全看不出区别（他会以为这个码下所有字都是生僻字）。
        #[test]
        fn no_filtering_when_common_table_is_missing() {
            let c = coord();
            let mut cands = vec![
                wind_candidate::Candidate {
                    text: "我".to_string(),
                    ..Default::default()
                },
                wind_candidate::Candidate {
                    text: "你好".to_string(),
                    ..Default::default()
                },
            ];
            c.retain_rare_admitted(&mut cands);
            assert_eq!(cands.len(), 2, "表未加载时不得过滤（宁可不滤，不给假列表）");
        }
    }
}
