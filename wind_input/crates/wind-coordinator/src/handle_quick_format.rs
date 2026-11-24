//! 快捷输入格式表的用户调整（右键调序 / 停用 / 恢复默认）。
//!
//! 三个落点：
//!
//! | 落点 | 内容 | 谁写 |
//! |---|---|---|
//! | `data/system.quick.toml` | 格式模板与出厂顺序 | 出厂 / 高级用户手写 |
//! | `<userdata>/data/quick_format.toml` | 用户的调序与停用 | 本模块（右键） |
//! | [`Coordinator::quick_adjust`] | 上一行的运行时镜像 | 本模块（写入时同步） |
//!
//! GUI 调整绝不回写 `system.quick.toml`：重写会丢手写文件的注释排版，且让用户点两下右键
//! 就永久脱离出厂更新。
//!
//! 不复用候选调整（shadow）：shadow 按 `(方案, 输入码)` 索引，而快捷输入的输入码是
//! `2026.6.19` 这种具体值，次日就失效；故另用一张按类别索引的表。也不复用
//! `candidate_op_scope`，它问的是「有没有词库落点」。

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
    /// 粒度是整类：被停用的格式不在候选里，右键点不到单条（单条恢复在设置页）。
    ResetKind,
}

impl QuickFormatOp {
    /// 复用候选菜单的动作枚举。两处语义有偏移，菜单标签在 `show_candidate_menu` 里相应改写：
    /// `Delete` 对格式是「不再显示这种写法」，`Reset` 对格式是「恢复整类」。
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
            // 常用/生僻标记是字的属性，格式候选是求值结果。菜单侧本就不给，这里是第二道闸。
            C::ToggleCommon => None,
            // 格式没有编码可言，菜单侧 quick 分支也不给这一项。
            C::HideHere => None,
            // 格式排序只有置顶 / 上移 / 下移，菜单侧 quick 分支不给这一项。
            C::PinAt(_) => None,
        }
    }
}

/// 右键作用域：这条候选属于哪一类格式、id 是什么、在本类里排第几。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickFormatScope {
    pub kind: FormatKind,
    pub format_id: String,
    /// 该条在本类候选中的下标（上移/下移的基准）。
    ///
    /// 不是页内或全列表下标：候选可能混着多类，而 `position` 是组内语义，
    /// 用全列表下标会让「上移一位」跳过好几条。
    pub index_in_kind: usize,
}

/// 候选 id 的前缀。与短语的 `phrase:` 同在 `Candidate::id` 里，靠前缀分辨归属。
const ID_PREFIX: &str = "quick:";

/// 生成快捷输入候选的稳定 id：`quick:{kind}:{格式 id}`。
///
/// 候选文本逐次不同，右键要认的是格式，按文本认人必然失配。
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
            // 未知类别（来自更高版本）连同条目一起丢弃：渲染按 kind 查表，留着也匹配不上；warn 留迹。
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
    /// 返回副本而不是持锁引用：候选生成期间会调外部求值逻辑，持读锁跨越那段会死锁。表极小。
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
    /// 判据独立于 `candidate_op_scope`（那个问有没有词库落点），混用会让格式调整不可用或误落主方案词库。
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

    /// 同 [`Self::quick_format_scope`]，但按全量绝对下标判（移动端只有绝对下标）。
    pub(crate) fn quick_format_scope_at(
        &self,
        state: &State,
        idx: usize,
    ) -> Option<QuickFormatScope> {
        let (kind, format_id) = parse_quick_cand_id(&state.candidates.get(idx)?.id)?;
        // 组内下标：只数同类的快捷候选。
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
    /// 判据必须与菜单构造侧同源（都是 [`Coordinator::quick_format_scope`]），否则点了没反应且日志干净。
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

    /// 同 [`Self::candidate_or_quick_format_op`]，但按全量绝对下标定位（移动端长按菜单）。
    /// 不借道翻页：翻页会改掉用户可见的页状态。
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
    /// 页内版与绝对下标版共用这一份，免得加动作时漏改一边。
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
        // 对格式候选无语义的动作原样忽略（只可能来自热键或协议回传）。
        let Some(qop) = QuickFormatOp::from_candidate_op(op) else {
            return true;
        };
        self.apply_quick_format_op(&scope, qop);
        // 立即重排。必须走 mix 路径：快捷输入的候选在 `mix_buffer` 上，用主路径
        // `update_candidates`（读恒空的 `input_buffer`）会清空候选窗。
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.update_mix_candidates(&mut state);
        self.notify_ui_update(&state);
        true
    }

    /// 执行一次格式调整：写库 → 回灌镜像。
    ///
    /// 两步都要做，只写库不回灌就要重启才生效。
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
/// 与 [`QuickFormatScope`] 不同层：本结构是格式表全貌，含被停用的条目。
///
/// 两个下标别混用：[`Self::display_pos`] 是列表行号（含停用项），只给人看；
/// [`Self::move_index`] 是候选里的下标，是写操作唯一认的口径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickFormatRow {
    /// 类别（`date` / `month_day` / `year_month` / `number` / `calc`）。
    pub kind: &'static str,
    pub id: String,
    /// 模板原文（`农历$LMD`）。
    pub text: String,
    /// 列表行号，1-based，含停用项。
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
    /// 别拿 [`Self::adjusted`] 当它用：出厂条目只能停用、能恢复默认；用户条目能删、能改模板，
    /// 没有默认可回。
    pub user: bool,
    /// 示例效果。渲染不出时为空串（见 [`Coordinator::quick_format_samples`]）。
    pub sample: String,
}

/// 设置页对一条格式的直接编辑。
///
/// 与右键菜单的 [`QuickFormatOp`] 刻意不合并：菜单是相对当前候选位置的动作，
/// 设置页是对格式表状态的直接编辑（移到第 N 位、启用/停用、单条恢复）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickFormatEdit {
    /// 移到候选内的 0-based 下标（越界由渲染期 clamp）。
    MoveTo(usize),
    /// 启用 / 停用（双向；菜单只能停用）。
    SetEnabled(bool),
    /// 恢复单条的默认（位置 + 启用状态）。
    ResetEntry,
    /// 恢复整类默认，忽略 `id` 参数。
    ResetKind,
}

impl Coordinator {
    /// 设置页要的全部行（按类别分组，组内按显示序）。
    pub fn quick_format_rows(&self) -> Vec<QuickFormatRow> {
        let samples = self.quick_format_samples();
        let mut out = Vec::new();
        for &kind in FormatKind::ALL {
            let adjust = self.quick_adjust_of(kind);
            // 启用项下标必须与候选同源：取 `entries_of_view` 的启用序（即候选顺序，有测试钉住）。
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
    /// 不新增渲染路径：用示例输入串跑真实的候选生成，示例与实际输出必然一致。
    ///
    /// 调整只清 `moved`/`disabled`、保留 `added`：停用条目也要有示例，用户条目只在 `added` 里，
    /// 传空调整会让自加条目的示例恒为空。
    ///
    /// 取不到示例的条目为空串；这只说明这组样本值渲染不出，不等于这条永远不出。
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
        // `QuickSource::Date` 按输入形态分派到 date / month_day / year_month，三种形态各跑一次。
        // 日期样本用今天，用户一眼能对上号。后两条的尾点不是笔误：缓冲里有第二个小数点才归
        // 日期（见 `wind_quick_input::has_second_dot`），少了它这两类示例整列空白。
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

    /// 设置页的一次编辑：写库 + 回灌运行时镜像。合成一个方法，调用方结构上无法只做一半。
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

    /// 一批对单条格式的编辑（`SetEnabled` / `ResetEntry`，可跨类别）：写库一次、回灌一次。
    /// 整类 / 移动这类不按条的编辑没有批量形式。
    pub fn edit_quick_formats(
        &self,
        items: &[(FormatKind, String)],
        edit: QuickFormatEdit,
    ) -> anyhow::Result<()> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if items.is_empty() {
            return Ok(());
        }
        let items: Vec<(String, String)> = items
            .iter()
            .map(|(k, id)| (k.as_str().to_string(), id.clone()))
            .collect();
        match edit {
            QuickFormatEdit::SetEnabled(enabled) => {
                store.edit_quick_formats(&items, &mut |rec, id| {
                    rec.set_enabled(id, enabled);
                    Ok(())
                })?
            }
            QuickFormatEdit::ResetEntry => store.edit_quick_formats(&items, &mut |rec, id| {
                rec.apply_reset(id);
                Ok(())
            })?,
            QuickFormatEdit::MoveTo(_) | QuickFormatEdit::ResetKind => {
                anyhow::bail!("这类编辑没有批量形式")
            }
        }
        self.reload_quick_adjust();
        Ok(())
    }

    /// 一批 [`Self::delete_quick_format`]：任一条是出厂条目 ⇒ 整批不删。
    pub fn delete_quick_formats(&self, items: &[(FormatKind, String)]) -> anyhow::Result<()> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if items.is_empty() {
            return Ok(());
        }
        let items: Vec<(String, String)> = items
            .iter()
            .map(|(k, id)| (k.as_str().to_string(), id.clone()))
            .collect();
        store.delete_quick_formats(&items)?;
        self.reload_quick_adjust();
        Ok(())
    }

    /// 新增一条用户自定义格式，返回分配到的 id。
    ///
    /// 出厂条目的模板不可改（见 `docs/design/quick-input-format-table.md` §11.1）：「停用 + 自加 +
    /// 调序」已覆盖这个需求，开 `overrides` 就得处理出厂表升级时的三方合并。模板只存在 `added` 里。
    pub fn add_quick_format(&self, kind: FormatKind, text: &str) -> anyhow::Result<String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 保存前拒绝并给原因，与文件加载「剔除 + warn」刻意相反：这里用户正等着回话。
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
    /// 实际写入的自定义条目数。被跳过的（模板非法、重复）不计在这里，进 [`Self::skipped`]。
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
    /// 预览不判重复（那要模拟写入过程）；实际导入时被跳过的会进 `skipped`。
    pub formats: usize,
    pub skipped: Vec<String>,
    /// 会被改动的类别（`date` / `number` …），给用户一个「影响范围」的直观交代。
    pub kinds: Vec<&'static str>,
}

impl Coordinator {
    /// 导出用户改动为 TOML 文本。
    ///
    /// 数据取自 store（真相源）而不是运行时镜像，免得某次回灌漏了导出就与实际不符。
    pub fn export_quick_format(&self) -> anyhow::Result<String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let mut settings = wind_quick_input::user_file::UserSettings::default();
        for (kind, rec) in store.list_quick_format()? {
            // 无法解析的类别（来自更高版本）：跳过，但要 warn，否则「导出→导入」悄悄丢一段。
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
    /// 逐条重放既有原语（`move_quick_format` / `set_quick_format_enabled`），不另写合并逻辑，
    /// 结果与手动操作必然一致。
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
            // 逐类清空，非单事务；中途失败与「导入了一部分」同量级，可接受。
            // 用 `clear_`（含自定义条目）而非 `reset_`：replace 是用文件状态覆盖现状。
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
            // 必须逆序重放：`moved` 是 LIFO（index 0 优先级最高），`move_quick_format` 每次插到队首，
            // 顺序遍历会让最老的规则占队首。
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

    /// 写入文件里的自定义条目，返回被改过 id 的映射（旧 id → 新 id）。
    ///
    /// 本机已有同 id 用户条目时要换 id，`[[adjust]]` 里引用旧 id 的规则必须按映射改写，
    /// 否则会悄悄指向本机原有的那条。
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
        // 每类状态在内存里推进：生成不冲突的 id 要看到刚导入的那几条，不必每条整表回灌。
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
            // 模板与出厂或已有条目逐字相同：跳过并报告。
            if let Some(dup) = self
                .quick_formats
                .duplicate_text_of(e.kind, &e.text, cur, None)
            {
                outcome
                    .skipped
                    .push(format!("条目 {}：与已有的 {dup} 模板相同", e.id));
                continue;
            }
            // id 冲突（内容必然不同，相同的上一步已跳过）：换新 id 并记映射。
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

    /// 非快捷输入的候选 id 不得被误解析（短语候选也放在 `Candidate::id` 里）。
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
