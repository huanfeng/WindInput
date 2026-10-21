//! 快捷输入格式表的**用户调整**（右键调序 / 停用 / 恢复默认）。
//!
//! 三个落点各司其职，缺一个就出问题：
//!
//! | 落点 | 内容 | 谁写 |
//! |---|---|---|
//! | `data/system.quick.toml` | 格式模板与出厂顺序 | 出厂 / 高级用户手写 |
//! | `<userdata>/data/quick_format.toml` | 用户的调序与停用 | 本模块（右键） |
//! | [`Coordinator::quick_adjust`] | 上一行的运行时镜像 | 本模块（写入时同步） |
//!
//! **GUI 调整绝不回写 `system.quick.toml`**：那会抢走高级用户手写文件的所有权
//! （重写丢注释与排版），更糟的是让普通用户点两下右键就永久脱离出厂更新——
//! 整份覆盖的代价必须是知情选择，不能是右键的副作用。
//!
//! ## 与候选调整（shadow）的分界
//!
//! shadow 的键是 `(方案, 输入码)`；快捷输入的「输入码」是 `2026.6.19` 这种具体值，
//! 把格式调整存进去，用户调完次日换个日期就失效。故本模块另用一张按**类别**索引的表，
//! 且**不复用 `candidate_op_scope`**——那个判据回答的是「有没有词库落点」，
//! 混输确实没有，它返回 `None` 是对的。

use crate::coordinator::{Coordinator, State};
use wind_quick_input::{FormatAdjust, FormatKind};

/// 右键菜单能对一条格式做的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickFormatOp {
    /// 移到本类首位。
    MoveTop,
    /// 上移一位。
    MoveUp,
    /// 下移一位。
    MoveDown,
    /// 不再显示这种格式。
    Disable,
    /// 恢复本类的全部默认（顺序 + 显示）。
    ///
    /// 粒度是**整类**而非单条：被停用的格式不出现在候选里，右键点不到，
    /// 没有整类重置就再也开不回来了。单条恢复要等设置页。
    ResetKind,
}

impl QuickFormatOp {
    /// 复用候选菜单的动作枚举——语义一一对应，省掉一套跨 crate 的新枚举与菜单 id。
    ///
    /// 两处语义有偏移，菜单标签必须相应改写（在 `show_candidate_menu` 里）：
    /// - `Delete` 对候选是「从词库屏蔽这个词」，对格式是「不再显示这种写法」；
    /// - `Reset` 对候选是「恢复这一条」，对格式是「恢复**整类**」（停用后点不到单条）。
    ///
    /// `None` = 这个动作对格式候选没有对应语义，调用方应原样忽略。
    pub fn from_candidate_op(op: wind_ui_types::CandidateOp) -> Option<Self> {
        use wind_ui_types::CandidateOp as C;
        match op {
            C::MoveTop => Some(Self::MoveTop),
            C::MoveUp => Some(Self::MoveUp),
            C::MoveDown => Some(Self::MoveDown),
            C::Delete => Some(Self::Disable),
            C::Reset => Some(Self::ResetKind),
            // 常用/生僻标记是**字**的属性，而格式候选是求值结果（`2026年8月24日`）：
            // 既不是单字，那串文本次日还会变。菜单侧的 quick 分支本就不给这一项，
            // 这里返回 None 是第二道闸——返回类型逼着「新加一个 op」时必须来这儿表态，
            // 而不是默默落进某个语义不搭的分支。
            C::ToggleCommon => None,
        }
    }
}

/// 右键作用域：这条候选属于哪一类格式、id 是什么、在本类里排第几。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickFormatScope {
    pub kind: FormatKind,
    pub format_id: String,
    /// 该条在**本类候选**中的下标（上移/下移的基准）。
    ///
    /// 不是页内下标，也不是全列表下标：候选列表可能混着 calc / date / number 三类
    /// （由 `mix_modes.members` 决定），而 `position` 是**组内**语义。拿全列表下标去写
    /// position，用户会看到「上移一位」跳过好几条。
    pub index_in_kind: usize,
}

/// 候选 id 的前缀。与短语的 `phrase:` 同域不同前缀——两者都放在 `Candidate::id` 里，
/// 靠前缀分辨归属。
const ID_PREFIX: &str = "quick:";

/// 生成快捷输入候选的稳定 id：`quick:{kind}:{格式 id}`。
///
/// 候选文本逐次输入都不同（`2026年6月19日` / `2026年6月20日`），右键要认的是**格式**，
/// 按文本认人必然失配——与短语 `date` 候选需要 `cand_id` 是同一个理由。
pub fn quick_cand_id(kind: FormatKind, format_id: &str) -> String {
    format!("{ID_PREFIX}{}:{format_id}", kind.as_str())
}

/// 从候选 id 解析回 (类别, 格式 id)；不是快捷输入候选则 `None`。
pub fn parse_quick_cand_id(id: &str) -> Option<(FormatKind, String)> {
    let rest = id.strip_prefix(ID_PREFIX)?;
    let (kind, format_id) = rest.split_once(':')?;
    if format_id.is_empty() {
        return None;
    }
    Some((FormatKind::parse(kind)?, format_id.to_string()))
}

impl Coordinator {
    /// 从 store 装载用户调整到运行时镜像。启动时调用一次；写库后也走它保持一致。
    pub(crate) fn reload_quick_adjust(&self) {
        let Some(store) = self.store.as_ref() else {
            return; // headless：无 store = 无调整 = 出厂顺序
        };
        let rows = match store.list_quick_format() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("快捷输入格式调整: 读取失败，本次按出厂顺序: {e}");
                return;
            }
        };
        let mut map = std::collections::HashMap::new();
        for (kind, rec) in rows {
            // 用户条目要带上类别才能进 `FormatAdjust`（store 那侧只按字符串键分表）。
            // 未知类别（存量数据来自更高版本）连同它的条目一起丢弃：渲染是按 kind 查表的，
            // 留着也永远匹配不上，而 warn 让「我的自定义条目不见了」有迹可循。
            let added = match FormatKind::parse(&kind) {
                Some(k) => rec
                    .added
                    .iter()
                    .enumerate()
                    .map(|(i, a)| wind_quick_input::FormatEntry {
                        id: a.id.clone(),
                        kind: k,
                        text: a.text.clone(),
                        // 组内序号 = 存储序：新条目追加在后，故「加的顺序」就是初始顺序。
                        position: i as i32,
                    })
                    .collect(),
                None => {
                    if !rec.added.is_empty() {
                        tracing::warn!(
                            "快捷输入格式调整: 类别 {kind} 无法识别，其 {} 条自定义条目本次不生效",
                            rec.added.len()
                        );
                    }
                    Vec::new()
                }
            };
            map.insert(
                kind,
                FormatAdjust {
                    moved: rec.moved.into_iter().map(|m| (m.id, m.position)).collect(),
                    disabled: rec.disabled,
                    added,
                },
            );
        }
        if let Ok(mut w) = self.quick_adjust.write() {
            *w = map;
        }
    }

    /// 取整张调整表的快照（候选生成用）。
    ///
    /// 返回副本而不是持锁引用：候选生成期间会调用 cmdbar 求值等外部逻辑，
    /// 持读锁跨越那段是自找死锁。表极小（至多 4 个类别），clone 成本可忽略。
    pub(crate) fn quick_adjust_snapshot(&self) -> wind_quick_input::FormatAdjustMap {
        self.quick_adjust
            .read()
            .map(|m| m.clone())
            .unwrap_or_default()
    }

    /// 取某类的用户调整副本（无则空调整 = 出厂顺序）。
    pub(crate) fn quick_adjust_of(&self, kind: FormatKind) -> FormatAdjust {
        self.quick_adjust
            .read()
            .ok()
            .and_then(|m| m.get(kind.as_str()).cloned())
            .unwrap_or_default()
    }

    /// 当前高亮候选是否可做格式调整，返回它的类别与格式 id。
    ///
    /// 判据**独立于 `candidate_op_scope`**：后者问的是「有没有词库落点」（混输没有，
    /// 故对它返回 `None`），这里问的是「这条候选是不是某条格式渲染出来的」。
    /// 两个判据混用会让格式调整要么整个不可用、要么误落到主方案的词库上。
    pub(crate) fn quick_format_scope(
        &self,
        state: &State,
        page_local: usize,
    ) -> Option<QuickFormatScope> {
        let (start, end) = self.page_range(state);
        let idx = start + page_local;
        if idx >= end {
            return None;
        }
        self.quick_format_scope_at(state, idx)
    }

    /// 同 [`Self::quick_format_scope`]，但按**全量绝对下标**判（移动端候选栏可横滑、展开面板
    /// 看全部，手里只有绝对下标）。页内版只是在此之上多一道「落在当页」的边界。
    pub(crate) fn quick_format_scope_at(
        &self,
        state: &State,
        idx: usize,
    ) -> Option<QuickFormatScope> {
        let (kind, format_id) = parse_quick_cand_id(&state.candidates.get(idx)?.id)?;
        // 组内下标：只数同类的快捷候选。列表里混着别的来源时，全列表下标会让
        // 「上移一位」跳过好几条。
        let index_in_kind = state
            .candidates
            .iter()
            .take(idx)
            .filter(|c| parse_quick_cand_id(&c.id).is_some_and(|(k, _)| k == kind))
            .count();
        Some(QuickFormatScope {
            kind,
            format_id,
            index_in_kind,
        })
    }

    /// 菜单动作分发：快捷输入的格式候选走格式调整，其余走词库 shadow。
    ///
    /// 两条路径共用同一组 [`CandidateOp`]（语义一一对应，见 [`QuickFormatOp::from_candidate_op`]），
    /// 只是落点不同。**判据必须与菜单构造侧同源**（都是 [`Coordinator::quick_format_scope`]）——
    /// 菜单给了入口而这里落到另一条路径，用户会看到「点了没反应」且日志干净。
    pub(crate) fn candidate_or_quick_format_op(
        &self,
        op: wind_ui_types::CandidateOp,
        page_local: usize,
    ) {
        let (scope, history) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                self.quick_format_scope(&state, page_local),
                self.quick_history_at(&state, page_local),
            )
        };
        if !self.quick_format_or_history_op(op, scope, history) {
            self.candidate_op(op, page_local);
        }
    }

    /// 同 [`Self::candidate_or_quick_format_op`]，但按**全量绝对下标**定位（移动端候选长按菜单）。
    ///
    /// 不借道翻页：页内版的下标以 `current_page` 为基准，先把页翻到目标所在页再调它，
    /// 会改掉用户可见的页状态（桌面候选窗、空格上屏的目标都跟着变）。这里三条分支各取
    /// 绝对下标版的判据，与页内版逐条同源，只是不设「落在当页」那道边界。
    pub fn candidate_or_quick_format_op_at(&self, op: wind_ui_types::CandidateOp, idx: usize) {
        let (scope, history) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                self.quick_format_scope_at(&state, idx),
                self.quick_history_of(&state, idx),
            )
        };
        if !self.quick_format_or_history_op(op, scope, history) {
            self.candidate_op_at(op, idx);
        }
    }

    /// 历史候选 / 格式候选两条分支的执行体；返回 `false` = 两者都不是，交回调用方走词库 shadow。
    ///
    /// 页内版与绝对下标版共用这一份：分支写两遍，加一种动作时漏改一边，表现是
    /// 「桌面右键点得动、手机长按没反应」（或反过来），而且日志干净。
    fn quick_format_or_history_op(
        &self,
        op: wind_ui_types::CandidateOp,
        scope: Option<QuickFormatScope>,
        history: Option<String>,
    ) -> bool {
        // 快捷输入历史候选：只有「删除」有语义（它没有位置可调，也不在任何词库里）。
        // 判据与菜单构造侧同源（`quick_history_at`），其余动作原样忽略。
        if let Some(text) = history {
            if op == wind_ui_types::CandidateOp::Delete {
                self.delete_quick_history(&text);
            }
            return true;
        }
        let Some(scope) = scope else {
            return false;
        };
        // 对格式候选无语义的动作（常用/生僻标记）原样忽略：菜单侧的 quick 分支本就不给，
        // 走到这里只可能是热键或协议回传的越界 id。
        let Some(qop) = QuickFormatOp::from_candidate_op(op) else {
            return true;
        };
        self.apply_quick_format_op(&scope, qop);
        // 立即重排：不刷新的话，用户得退出重进才看得到新顺序。
        // 走 mix 路径——快捷输入的候选在 `mix_buffer` 上，主路径的 `update_candidates`
        // 读 `input_buffer`（此处恒空），用错的后果不是「不刷新」而是候选窗被清空。
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.update_mix_candidates(&mut state);
        self.notify_ui_update(&state);
        true
    }

    /// 执行一次格式调整：写库 → 回灌镜像。
    ///
    /// ⚠️ 两步都要做。只写库不回灌，用户会看到「调了没反应，重启才生效」。
    pub(crate) fn apply_quick_format_op(&self, scope: &QuickFormatScope, op: QuickFormatOp) {
        let current_index = scope.index_in_kind;
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let kind = scope.kind.as_str();
        let id = scope.format_id.as_str();
        let r = match op {
            QuickFormatOp::MoveTop => store.move_quick_format(kind, id, 0),
            // 首位再上移 = 原地不动（菜单侧应已灰显，这里兜住手滑与并发）
            QuickFormatOp::MoveUp => {
                store.move_quick_format(kind, id, current_index.saturating_sub(1))
            }
            // 下移不设上界：越界由渲染期 clamp 到末尾（条目数会因停用而变）
            QuickFormatOp::MoveDown => store.move_quick_format(kind, id, current_index + 1),
            QuickFormatOp::Disable => store.set_quick_format_enabled(kind, id, false),
            QuickFormatOp::ResetKind => store.reset_quick_format_kind(kind),
        };
        if let Err(e) = r {
            tracing::warn!("快捷输入格式调整失败 kind={kind} id={id} op={op:?}: {e}");
            return;
        }
        self.reload_quick_adjust();
    }
}

// ───────── 设置页（词库管理 → 快捷输入）─────────

/// 设置页列表的一行。
///
/// 与右键菜单的 [`QuickFormatScope`] 不在同一层：那个回答「当前高亮的这条候选属于谁」，
/// 本结构是**格式表全貌**——含被停用的条目。右键菜单点不到停用项（它们不在候选里），
/// 所以在设置页出现之前，停用之后的唯一出口是「整类重置」。
///
/// ⚠️ 两个下标字段刻意分开，**别拿一个当另一个用**：
/// - [`Self::display_pos`] 是列表行号（含停用项占位），只给人看；
/// - [`Self::move_index`] 是这条在**候选**里的下标，是写操作唯一认的口径。
///
/// 停用项占着行号却不在候选里，拿行号去写移动规则，用户会看到「上移一位跳过好几条」
/// ——与 [`QuickFormatScope::index_in_kind`] 防的是同一个错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickFormatRow {
    /// 类别（`date` / `month_day` / `year_month` / `number` / `calc`）。
    pub kind: &'static str,
    pub id: String,
    /// 模板原文（`农历$LMD`）。
    pub text: String,
    /// 列表行号，1-based，**含停用项**。
    pub display_pos: usize,
    /// 这条在候选中的 0-based 下标；停用项为 `None`。
    ///
    /// `None` 同时表达了「不能移动」：条目不在候选里，移它没有意义，得先启用。
    pub move_index: Option<usize>,
    pub enabled: bool,
    /// 用户是否调整过（移动或停用）。设置页据此决定「恢复此条」能不能点。
    pub adjusted: bool,
    /// 是不是用户自己加的条目。
    ///
    /// 设置页按它分流三件事，**别拿 [`Self::adjusted`] 当它用**：出厂条目被调序过也是
    /// `adjusted`，但它只能停用、能「恢复默认」；用户条目反过来——能删、能改模板，
    /// 而它没有「默认」可回到。
    pub user: bool,
    /// 示例效果。渲染不出时为空串（见 [`Coordinator::quick_format_samples`]）。
    pub sample: String,
}

/// 设置页对一条格式的直接编辑。
///
/// 与右键菜单的 [`QuickFormatOp`] 分层不同，**刻意不合并**：菜单是「相对当前候选位置」
/// 的动作（「上移一位」得先知道它现在排第几），设置页是「对格式表状态的直接编辑」
/// （移到第 N 位、启用/停用双向、单条恢复）。合并成一套的话，设置页得先算出一个
/// 它并不需要的 `index_in_kind`，而菜单侧得凭空造出一个绝对位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickFormatEdit {
    /// 移到候选内的 0-based 下标（越界由渲染期 clamp）。
    MoveTo(usize),
    /// 启用 / 停用。**双向**——菜单只能停用，恢复得靠整类重置。
    SetEnabled(bool),
    /// 恢复单条的默认（位置 + 启用状态）。
    ///
    /// store 侧的 `reset_quick_format_entry` 此前无人调用，注释写着「单条恢复要等设置页」
    /// ——就是这里。
    ResetEntry,
    /// 恢复整类默认。**忽略 `id` 参数**（整类操作没有单条归属）。
    ResetKind,
}

impl Coordinator {
    /// 设置页要的全部行（按类别分组，组内按显示序）。
    pub fn quick_format_rows(&self) -> Vec<QuickFormatRow> {
        let samples = self.quick_format_samples();
        let mut out = Vec::new();
        for &kind in FormatKind::ALL {
            let adjust = self.quick_adjust_of(kind);
            // 启用项的下标口径必须与候选同源，故这里不自己数——直接看它在
            // `entries_of_view` 里的启用序（视图的启用段就是候选顺序，有测试钉住）。
            let mut enabled_seen = 0usize;
            for (i, v) in self
                .quick_formats
                .entries_of_view(kind, &adjust)
                .iter()
                .enumerate()
            {
                let move_index = if v.enabled {
                    let n = enabled_seen;
                    enabled_seen += 1;
                    Some(n)
                } else {
                    None
                };
                out.push(QuickFormatRow {
                    kind: kind.as_str(),
                    id: v.entry.id.clone(),
                    text: v.entry.text.clone(),
                    display_pos: i + 1,
                    move_index,
                    enabled: v.enabled,
                    adjusted: v.adjusted,
                    user: v.user,
                    sample: samples.get(&v.entry.id).cloned().unwrap_or_default(),
                });
            }
        }
        out
    }

    /// 每条格式的示例效果：`格式 id → 渲染文本`。
    ///
    /// ★ **不新增渲染路径**：用示例输入串跑一遍真实的候选生成，所以设置页显示的示例与
    /// 用户实际打出来的东西必然一致。另写一套「设置页专用渲染」迟早与候选漂移，
    /// 而那种漂移没人会发现——两边看着都对，只是不一样。
    ///
    /// 调整**只清 `moved`/`disabled`、保留 `added`**：停用条目也要有示例（设置页要显示
    /// 它们），而候选路径会把停用项剔掉；顺序在这里无意义，只取 id → 文本的映射，
    /// 行序由 `entries_of_view` 决定。
    ///
    /// ⚠️ P1 这里用的是**空调整**，那时是对的（只有出厂条目）。用户条目住在 `adjust.added`
    /// 里，继续传空 map 就等于「渲染一张没有用户条目的表」——症状是自己加的那几行示例列
    /// 恒为空，而其它行都好，看着像模板写错了。
    ///
    /// 取不到示例的条目（农历超出 1900–2100、表达式写错）为空串。⚠️ 「示例为空」
    /// **不等于**「这条永远不出」——它只说明这一组样本值渲染不出来。
    fn quick_format_samples(&self) -> std::collections::HashMap<String, String> {
        use chrono::Datelike;
        use wind_quick_input::QuickSource;
        let dp = self.rt().config.schema.quick_input.decimal_places;
        let mut samples_adjust = self.quick_adjust_snapshot();
        for a in samples_adjust.values_mut() {
            a.moved.clear();
            a.disabled.clear();
        }
        let eval = |text: &str, values: &wind_quick_input::QuickValues| {
            crate::quick_eval::eval_expr(text, values)
        };
        let now = chrono::Local::now();
        let (y, m, d) = (now.year(), now.month(), now.day());
        // `QuickSource::Date` 按输入形态分派到 date / month_day / year_month 三个类别，
        // 一个样本只能覆盖其中一个，故三种形态各跑一次。
        //
        // 日期样本用**今天**：用户脑子里知道今天几号，一眼就能把 `$YC年$MC月$DC日`
        // 这类模板对上号；固定日期反而要他先换算。
        //
        // ⚠️ 后两条的尾点不是笔误：`QuickSource::Date` 要求缓冲里已有第二个小数点才
        // 归日期（一个点归数字，见 `wind_quick_input::has_second_dot` 的判据）。少了它，
        // month_day 与 year_month 两类的示例列会**整列空白**，而其余类别都正常——
        // 看着像这两类的模板全写错了，实际是样本压根没走到渲染。
        let samples: [(QuickSource, String); 5] = [
            (QuickSource::Date, format!("{y}.{m}.{d}")),
            (QuickSource::Date, format!("{m}.{d}.")),
            (QuickSource::Date, format!("{y}.{m}.")),
            (QuickSource::Number, "1234.5".to_string()),
            (QuickSource::Calc, "1+2*3".to_string()),
        ];
        let mut out = std::collections::HashMap::new();
        for (src, buffer) in samples {
            for r in wind_quick_input::generate_adjusted(
                src,
                &buffer,
                dp,
                &self.quick_formats,
                &samples_adjust,
                Some(&eval),
            ) {
                out.insert(r.id, r.text);
            }
        }
        out
    }

    /// 设置页的一次编辑：**写库 + 回灌运行时镜像**。
    ///
    /// 两件事合成一个方法而不是让调用方各调一次，是因为「只写库不回灌」的症状是
    /// 「设置页改了不生效、重启后才生效」——本仓这个坑踩过不止一次，且没有任何报错。
    /// 合成一个方法后，调用方结构上无法只做一半。
    pub fn edit_quick_format(
        &self,
        kind: FormatKind,
        id: &str,
        edit: QuickFormatEdit,
    ) -> anyhow::Result<()> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let k = kind.as_str();
        match edit {
            QuickFormatEdit::MoveTo(index) => store.move_quick_format(k, id, index)?,
            QuickFormatEdit::SetEnabled(enabled) => {
                store.set_quick_format_enabled(k, id, enabled)?
            }
            QuickFormatEdit::ResetEntry => store.reset_quick_format_entry(k, id)?,
            QuickFormatEdit::ResetKind => store.reset_quick_format_kind(k)?,
        }
        self.reload_quick_adjust();
        Ok(())
    }

    /// 新增一条用户自定义格式，返回分配到的 id。
    ///
    /// ## 为什么没有「编辑出厂条目」的对应方法
    ///
    /// 出厂条目的模板**不可改**（设计决策，见 `docs/design/quick-input-format-table.md`
    /// §11.1）：这张表的顺序由用户直接指定，「停用出厂那条 + 自己加一条 + 摆到想要的位置」
    /// 已完整覆盖「想改出厂那条」，再开一条 `overrides` 路径就得额外处理出厂表升级时的
    /// 三方合并（留用户旧值 = 他永远看不到新出厂模板；采用新值 = 丢他的改动）。
    ///
    /// 约束由数据结构兜住：模板只存在 `added` 里，出厂条目根本没有可写的字段。
    pub fn add_quick_format(&self, kind: FormatKind, text: &str) -> anyhow::Result<String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 校验在**保存前**拒绝并给出原因，与文件加载「剔除 + warn」的策略刻意相反：
        // 那边用户看不到日志、只能发现候选少了一条，这边他正盯着输入框等回话。
        wind_quick_input::validate_format_text(kind, text).map_err(|e| anyhow::anyhow!(e))?;
        let adjust = self.quick_adjust_of(kind);
        if let Some(dup) = self
            .quick_formats
            .duplicate_text_of(kind, text, &adjust, None)
        {
            anyhow::bail!("已有一条相同的格式（{dup}），不必重复添加");
        }
        let id = wind_quick_input::next_user_format_id(kind, &adjust);
        store.add_quick_format(kind.as_str(), &id, text)?;
        self.reload_quick_adjust();
        Ok(id)
    }

    /// 改写用户条目的模板。出厂条目会被拒绝（它们不在 `added` 里）。
    pub fn set_quick_format_text(
        &self,
        kind: FormatKind,
        id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        wind_quick_input::validate_format_text(kind, text).map_err(|e| anyhow::anyhow!(e))?;
        let adjust = self.quick_adjust_of(kind);
        // 排除自己：只改了个错别字、模板本身没动时，不该被判成「与自己重复」。
        if let Some(dup) = self
            .quick_formats
            .duplicate_text_of(kind, text, &adjust, Some(id))
        {
            anyhow::bail!("已有一条相同的格式（{dup}）");
        }
        if !store.set_quick_format_text(kind.as_str(), id, text)? {
            anyhow::bail!("{id} 不是自定义条目，出厂条目的模板不可修改");
        }
        self.reload_quick_adjust();
        Ok(())
    }

    /// 删除用户条目（连带它的调序/停用规则）。出厂条目会被拒绝——它们只能停用。
    pub fn delete_quick_format(&self, kind: FormatKind, id: &str) -> anyhow::Result<()> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if !store.delete_quick_format(kind.as_str(), id)? {
            anyhow::bail!("{id} 不是自定义条目，出厂条目只能停用");
        }
        self.reload_quick_adjust();
        Ok(())
    }
}

/// 导入的结果（RPC 回报给设置页）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuickImportOutcome {
    /// 实际写入的移动规则数。
    pub moved: usize,
    /// 实际写入的停用数。
    pub disabled: usize,
    /// 实际写入的自定义条目数。
    ///
    /// P1 时叫 `ignored_formats`（那时没有存储落点，如实报「已忽略 N 条」）。P2 起真的
    /// 导入，故连名字一起改——留着旧名字会让 UI 文案与行为对不上，而那种偏差没人会发现。
    ///
    /// 被跳过的（模板非法、与已有条目模板相同）不计在这里，进 [`Self::skipped`]。
    pub formats: usize,
    /// 解析期与写入期跳过的条目及原因（未知类别、模板非法、模板重复……）。
    pub skipped: Vec<String>,
}

/// 导入预览（只读，不写库）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuickImportPreview {
    pub moved: usize,
    pub disabled: usize,
    /// 文件里的自定义条目数。
    ///
    /// 预览**不判重复**：那要模拟一遍写入过程（同一份文件里两条相同模板，第二条才算撞车），
    /// 而预览的职责是「让用户看清影响范围」，不是精确到条。实际导入时被跳过的会进 `skipped`。
    pub formats: usize,
    pub skipped: Vec<String>,
    /// 会被改动的类别（`date` / `number` …），给用户一个「影响范围」的直观交代。
    pub kinds: Vec<&'static str>,
}

impl Coordinator {
    /// 导出用户改动为 TOML 文本。
    ///
    /// 数据取自 **store**（真相源）而不是运行时镜像：镜像是热路径读缓存，万一某次回灌
    /// 漏了，从它导出就会写出一份与实际不符的文件——而这种错误在导入到另一台机器之前
    /// 完全看不出来。
    pub fn export_quick_format(&self) -> anyhow::Result<String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let mut settings = wind_quick_input::user_file::UserSettings::default();
        for (kind, rec) in store.list_quick_format()? {
            // 无法解析的类别：用户在新版本停用过某类条目、又回退到旧版本时会遇到。
            // 跳过是对的（旧版本导出的文件不该带它），但不能不响——静默跳过会让
            // 「导出→导入」悄悄丢掉一段。
            let Some(k) = FormatKind::parse(&kind) else {
                tracing::warn!("快捷输入导出: 跳过未知类别 {kind}（可能来自更高版本）");
                continue;
            };
            for (i, a) in rec.added.iter().enumerate() {
                settings.formats.push(wind_quick_input::FormatEntry {
                    id: a.id.clone(),
                    kind: k,
                    text: a.text.clone(),
                    position: i as i32,
                });
            }
            settings.adjust.push((
                k,
                FormatAdjust {
                    moved: rec.moved.into_iter().map(|m| (m.id, m.position)).collect(),
                    disabled: rec.disabled,
                    // 自定义条目走上面的 `formats` 段（文件里两段分开，与 `system.quick.toml`
                    // 的结构一致），不在 adjust 里重复一份。
                    added: Vec::new(),
                },
            ));
        }
        Ok(wind_quick_input::user_file::serialize_user_settings(
            &settings,
        ))
    }

    /// 导入预览：解析并计数，**不写任何东西**。
    pub fn preview_quick_format_import(&self, content: &str) -> anyhow::Result<QuickImportPreview> {
        let out = wind_quick_input::user_file::parse_user_settings(content)
            .map_err(|e| anyhow::anyhow!("不是一份有效的快捷输入设置文件: {e}"))?;
        Ok(QuickImportPreview {
            moved: out.settings.adjust.iter().map(|(_, a)| a.moved.len()).sum(),
            disabled: out
                .settings
                .adjust
                .iter()
                .map(|(_, a)| a.disabled.len())
                .sum(),
            formats: out.settings.formats.len(),
            skipped: out.skipped,
            kinds: out
                .settings
                .adjust
                .iter()
                .filter(|(_, a)| !a.is_empty())
                .map(|(k, _)| k.as_str())
                .collect(),
        })
    }

    /// 导入用户改动。`replace` 为真时先清空现有全部调整。
    ///
    /// 应用方式是**逐条重放既有原语**（`move_quick_format` / `set_quick_format_enabled`），
    /// 不另写一套合并逻辑：这样导入的结果与用户手动操作出来的结果必然一致，
    /// 同 id 撞车时的顶替语义也由那些原语自带。
    pub fn import_quick_format(
        &self,
        content: &str,
        replace: bool,
    ) -> anyhow::Result<QuickImportOutcome> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let parsed = wind_quick_input::user_file::parse_user_settings(content)
            .map_err(|e| anyhow::anyhow!("不是一份有效的快捷输入设置文件: {e}"))?;

        if replace {
            // 逐类清空。非单事务，但这不是热路径，且中途失败的后果是「清了一部分」——
            // 与「导入了一部分」同量级，不值得为它新造一个跨类别的清空原语。
            //
            // ⚠️ 用 `clear_`（含自定义条目）而不是 `reset_`（保留自定义条目）：replace 的
            // 语义是「用文件里的状态覆盖现状」，留着旧条目会得到「文件里的 + 我原有的」。
            // 面向用户的「恢复默认」才是 reset。
            for &k in FormatKind::ALL {
                store.clear_quick_format_kind(k.as_str())?;
            }
            self.reload_quick_adjust(); // 下面生成 id 要看清空后的状态
        }

        let mut outcome = QuickImportOutcome {
            skipped: parsed.skipped,
            ..Default::default()
        };
        let remap = self.import_quick_user_entries(&parsed.settings.formats, &mut outcome)?;
        for (kind, adjust) in &parsed.settings.adjust {
            let k = kind.as_str();
            // ★★ **逆序**重放。`moved` 是 LIFO 列表（index 0 = 最新 = 优先级最高），而
            // `move_quick_format` 每次调用都把规则插到队首。顺着遍历的话，文件里最老的
            // 规则最后写、反而占了队首，用户会看到「导入后顺序和导出前不一样」。
            //
            // 与 shadow 的 pin 规则导入是同一个陷阱（那次是靠 `.rev()` 修的）。
            for (id, position) in adjust.moved.iter().rev() {
                store.move_quick_format(
                    k,
                    remap.get(id).map_or(id.as_str(), |s| s.as_str()),
                    *position,
                )?;
                outcome.moved += 1;
            }
            for id in &adjust.disabled {
                store.set_quick_format_enabled(
                    k,
                    remap.get(id).map_or(id.as_str(), |s| s.as_str()),
                    false,
                )?;
                outcome.disabled += 1;
            }
        }
        // 一次回灌即可（不必每条一次）：镜像是整表替换。
        self.reload_quick_adjust();
        Ok(outcome)
    }

    /// 写入文件里的自定义条目，返回**被改过 id 的映射**（旧 id → 新 id）。
    ///
    /// ## ★★ 为什么必须返回映射
    ///
    /// 文件里 `[[formats]]` 带 `date.u1`，`[[adjust]]` 的 `moved` 也引用 `date.u1`。
    /// 导入到一台**已有 `date.u1`**（内容不同）的机器上时，那条得改叫 `date.u3`；
    /// 若只改 `[[formats]]` 侧、不改 `moved` 侧，那条移动规则就悄悄指向了本机原有的
    /// `date.u1`——用户看到的症状是「导入后顺序不对」，而两条条目都在、都没报错。
    ///
    /// 与漏 `.rev()` 是同一类错（导入重放的引用完整性），只是这次跨了两个段。
    fn import_quick_user_entries(
        &self,
        formats: &[wind_quick_input::FormatEntry],
        outcome: &mut QuickImportOutcome,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let mut remap = std::collections::HashMap::new();
        // 每类的当前状态在内存里推进，不是每写一条就整表回灌：生成不冲突的 id 需要看到
        // 「连同刚导入的那几条」的全貌，而回灌一次要读整张表。
        let mut live: std::collections::HashMap<&'static str, FormatAdjust> =
            std::collections::HashMap::new();
        for e in formats {
            let cur = live
                .entry(e.kind.as_str())
                .or_insert_with(|| self.quick_adjust_of(e.kind));
            if let Err(err) = wind_quick_input::validate_format_text(e.kind, &e.text) {
                outcome.skipped.push(format!("条目 {}：{}", e.id, err));
                continue;
            }
            // 模板撞车（与出厂或已有用户条目逐字相同）：跳过并如实报告。两条一样的候选
            // 除了占位没有作用，而静默导入会让用户在列表里看到两行长得完全一样的东西。
            if let Some(dup) = self
                .quick_formats
                .duplicate_text_of(e.kind, &e.text, cur, None)
            {
                outcome
                    .skipped
                    .push(format!("条目 {}：与已有的 {dup} 模板相同", e.id));
                continue;
            }
            // id 冲突：本机已有同 id 的用户条目（内容必然不同——相同的话上一步已按模板
            // 撞车跳掉了），故换一个没被占用的 id，并记下映射供 moved/disabled 改写。
            let id = if cur.is_user(&e.id) {
                let fresh = wind_quick_input::next_user_format_id(e.kind, cur);
                remap.insert(e.id.clone(), fresh.clone());
                fresh
            } else {
                e.id.clone()
            };
            store.add_quick_format(e.kind.as_str(), &id, &e.text)?;
            cur.added.push(wind_quick_input::FormatEntry {
                id,
                kind: e.kind,
                text: e.text.clone(),
                position: cur.added.len() as i32,
            });
            outcome.formats += 1;
        }
        Ok(remap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cand_id_roundtrip() {
        let id = quick_cand_id(FormatKind::Date, "date.lunar");
        assert_eq!(id, "quick:date:date.lunar");
        let (kind, fid) = parse_quick_cand_id(&id).unwrap();
        assert_eq!(kind, FormatKind::Date);
        assert_eq!(fid, "date.lunar");
    }

    #[test]
    fn all_kinds_roundtrip() {
        for k in [
            FormatKind::Date,
            FormatKind::MonthDay,
            FormatKind::YearMonth,
            FormatKind::Number,
            FormatKind::Calc,
        ] {
            let (kind, _) = parse_quick_cand_id(&quick_cand_id(k, "x.y")).unwrap();
            assert_eq!(kind, k, "kind={} 未能往返", k.as_str());
        }
    }

    /// ★ 非快捷输入的候选 id 不得被误解析——短语候选也放在同一个 `Candidate::id` 字段里。
    #[test]
    fn foreign_ids_are_rejected() {
        assert!(parse_quick_cand_id("").is_none());
        assert!(parse_quick_cand_id("phrase:date:$Y年").is_none(), "短语 id");
        assert!(parse_quick_cand_id("quick:").is_none());
        assert!(parse_quick_cand_id("quick:date").is_none(), "缺格式 id");
        assert!(parse_quick_cand_id("quick:date:").is_none(), "空格式 id");
        assert!(parse_quick_cand_id("quick:weather:x").is_none(), "未知类别");
    }

    /// 格式 id 里含冒号时，只按第一个冒号切分，其余归 format_id。
    #[test]
    fn format_id_may_contain_colon() {
        let (_, fid) = parse_quick_cand_id("quick:date:a:b").unwrap();
        assert_eq!(fid, "a:b");
    }
}
