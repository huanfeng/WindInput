//! 常用字表的用户覆盖：运行时镜像装载 + 候选右键「设为生僻字 / 设为常用字」。
//!
//! 真相在 store（`wind_store::common_chars`，key = 一个字素簇，不带方案），这里维护
//! `Coordinator::common_chars` 这份内存镜像，写库后必须立刻重灌，否则要重启才生效。
//!
//! 与候选调整（shadow）的分界：shadow 只作用于「这个方案、这个码」，本模块作用于全局
//! （所有方案所有码）。两者在右键菜单里挨着，文案必须把作用域说出来。

use tracing::{debug, warn};

/// 设置页列表的一行。
/// 设置页列表的一行。
///
/// 列的是全表（出厂字 + 用户加的），用户最常问「这个字现在算不算常用」；改过的那些靠
/// [`Self::overridden`] 标出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonCharRow {
    /// 这一行的字素簇文本（用户眼里的一个字符）。用 `String` 而非 `char`：覆盖表里可以有
    /// `👨‍👩‍👧`、`🇨🇳` 这样的多码位条目。
    pub text: String,
    /// 当前生效的判定（用户改过就是用户设的，否则跟随默认）。
    pub common: bool,
    /// 默认判定（出厂字表说了算）。界面靠它显示「默认 → 现在」的对照。
    pub base_common: bool,
    /// 这一行被用户改过。决定「恢复默认」能不能点。
    pub overridden: bool,
    /// 所属 Unicode 块的中文名（[`wind_candidate::block_of`]），如「注音符号」，光看字形
    /// 分不清这些字符是什么。范围文本不存在这里，消费方自己调
    /// `block_of_cluster(t).range_text()`，免得格式化逻辑抄两份。
    pub block: &'static str,
    /// 决定这个字常用性的那个字符类的 key（`None` = 没有类命中它）。
    ///
    /// 与 `block` 是两个问题、绑着两个操作：`block` 是所在 Unicode 区块，绑「整类批量」
    /// （扫码位区间、写覆盖）；`class` 是谁决定了常用性，绑「改类的 default」（写一行 yaml）。
    /// 不能拿 class 顶掉 block，两个都给、各自标清楚。
    pub class: Option<String>,
    /// 这个块能不能整类批量操作（[`wind_candidate::block_allows_bulk_edit`]）。
    /// 汉字块恒 `false`（一次误点就是七千多条覆盖），设置页据此不显示该菜单项。
    pub block_bulk_editable: bool,
}

/// 某个字的当前状态（设置页「添加」时的预览与校验）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommonCharState {
    /// 这个字符在默认字表里有没有说法（[`wind_candidate::is_common_scope`]）。
    ///
    /// 纯提示，界面不得据此拒绝添加：任何字符都可登记，`false` 只表示「这条是纯用户规则」
    /// （注音、假名、结构描述符设生僻正是这种情况）。
    pub governed: bool,
    /// 出厂判定。
    pub base_common: bool,
    /// 用户覆盖方向；`None` = 跟随出厂。
    pub over: Option<bool>,
}

/// 「按类型批量」的预览与结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonCharBulkOutcome {
    /// 块名，回显用（用户点的是「注音符号」，报告里也该这么说）。
    pub block: String,
    /// 当前词库里命中的字符数。
    pub chars: usize,
    /// 这些字符出现在多少条词条里。
    /// 这些字符出现在多少条词条里。界面必须显示它：`，` 只是 1 个字符却出现在几百条
    /// 词条里，只报字符数会严重低估影响面。
    pub entries: usize,
    /// 真正落库的覆盖条数（与默认同向的不写，同 `apply_common_target`）。
    pub written: usize,
    /// 命中的字符本身，供界面回显「将影响这些字」。
    pub sample: String,
}

/// 设置页对一个字的编辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommonCharEdit {
    /// 设为常用（`true`）/ 生僻（`false`）。与出厂同向时等价于 [`Self::Reset`]。
    Set(bool),
    /// 撤销覆盖，回到出厂判定。
    Reset,
    /// 清空全部覆盖（整表恢复出厂）。此时 `ch` 参数被忽略。
    ClearAll,
}

/// 候选文本作为「常用字标记」对象时的状态。
///
/// 刻意不带「是否已有覆盖」：菜单只有一项，点回出厂方向即等于恢复
/// （见 [`crate::Coordinator::toggle_common_char`]），没有第二个菜单项需要靠它灰显。
pub(crate) struct CommonCharMark {
    /// 目标字素簇（用户眼里的一个字符，可能由多个码位组成）。
    pub text: String,
    /// 当前判定（含用户覆盖）。菜单据此二选一：判常用就给「设为生僻字」，反之亦然。
    pub common: bool,
}

/// 候选文本能不能被标记，能则返回那个字（[`wind_candidate::single_markable_char`]）。
///
/// 不限定汉字（字根、结构符、注音、假名也要能标），前提是读端 `is_string_common`
/// 覆盖优先，否则会存下永不被查询的死记录。「一个字」按 Unicode 字素簇算而不是 `char`
/// 数：`⚽️`、`👨‍👩‍👧`、`🇨🇳` 在屏幕上都只是一个图形。
pub(crate) fn common_char_of(text: &str) -> Option<&str> {
    wind_candidate::single_markable_char(text)
}

impl crate::Coordinator {
    /// 从 store 装载用户覆盖到运行时镜像。启动时一次；每次写库后也走它。
    ///
    /// headless（无 store）时保持空覆盖 = 纯出厂判定。
    pub(crate) fn reload_common_chars(&self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let rows = match store.list_common_char_overrides() {
            Ok(v) => v,
            Err(e) => {
                warn!("常用字覆盖: 读取失败，本次按出厂判定: {e}");
                return;
            }
        };
        let n = rows.len();
        self.common_chars
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .set_overrides(rows.into_iter().map(|o| (o.ch, o.common)));
        debug!("常用字覆盖: 装载 {n} 条");
    }

    /// 取某个候选文本的标记状态；不可标记时 `None`（菜单据此不给这两项）。
    pub(crate) fn common_char_mark(&self, text: &str) -> Option<CommonCharMark> {
        // 无 store 就没有落点：给了入口而写端无处可写会「点得动却毫无反应」。
        self.store.as_ref()?;
        let g = common_char_of(text)?;
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        Some(CommonCharMark {
            text: g.to_string(),
            common: cc.is_cluster_common(g, &self.engine_mgr.charsets()),
        })
    }

    /// 设置页列表：全表（出厂字按字表原序 + 用户加的追加在后），可按 `query` 过滤。
    ///
    /// 数据全部取自内存镜像而不另读 store：镜像与过滤层用同一份数据，界面显示的判定才与
    /// 实际生效的一致。`query` 非空时只保留出现在查询串里的字（打一个字或粘一整句都行）；
    /// `only_modified` 为真时只留改过的行。
    pub(crate) fn common_char_rows(&self, query: &str, only_modified: bool) -> Vec<CommonCharRow> {
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        // 列表最多 8000+ 行，registry 在循环外取一次。
        let cs = self.engine_mgr.charsets();
        let q = query.trim();
        cc.list_all(&cs)
            .into_iter()
            // 搜索按子串包含。多码位簇用 `chars().contains` 判不出来（`⚽️` 会永远搜不到）。
            .filter(|(t, _, _)| q.is_empty() || q.contains(t.as_str()))
            .filter(|(t, _, _)| !only_modified || cc.override_of_cluster(t).is_some())
            .map(|(text, base_common, common)| {
                let blk = wind_candidate::block_of_cluster(&text);
                let class = cs.class_of(&text).map(|c| {
                    if c.name.is_empty() {
                        c.key.clone()
                    } else {
                        c.name.clone()
                    }
                });
                CommonCharRow {
                    overridden: cc.override_of_cluster(&text).is_some(),
                    text,
                    common,
                    base_common,
                    block: blk.name,
                    class,
                    block_bulk_editable: wind_candidate::block_allows_bulk_edit(&blk),
                }
            })
            .collect()
    }

    /// 按「某个字所属的 Unicode 块」批量设常用/生僻。`apply=false` 时只预览、不写库。
    ///
    /// 入口是当前选中的那一行而不是码位区间：用户想的是「这类东西别出来」，类型从行推导。
    /// 汉字块一律拒绝（[`wind_candidate::block_allows_bulk_edit`]）：那些块由默认字表逐字
    /// 管着，一次误点就是七千多条覆盖。
    pub(crate) fn common_char_bulk_by_block(
        &self,
        text: &str,
        common: bool,
        apply: bool,
    ) -> anyhow::Result<CommonCharBulkOutcome> {
        let blk = wind_candidate::block_of_cluster(text);
        if !wind_candidate::block_allows_bulk_edit(&blk) {
            anyhow::bail!("「{}」由默认字表逐字管辖，不支持整类操作", blk.name);
        }
        let scan = self.engine_mgr.scan_chars_in_range(blk.start, blk.end);

        let mut out = CommonCharBulkOutcome {
            block: blk.name.to_string(),
            chars: scan.chars.len(),
            entries: scan.entries,
            written: 0,
            sample: scan.chars.iter().collect(),
        };
        if !apply {
            return Ok(out);
        }
        let Some(store) = self.store.as_ref() else {
            anyhow::bail!("无持久化存储");
        };
        // 一个事务写完、最后统一回灌一次，不要在循环里调 `apply_common_target`：那条路每字
        // 一次 fsync 提交加一次整表回灌（取写锁，与按键时候选过滤的读锁相撞），上千字的
        // 块就是上千次卡住输入。读锁在循环外取一次：默认判定只看基表，不随本次写入变化。
        let plan: Vec<(String, Option<bool>)> = {
            let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
            let cs = self.engine_mgr.charsets();
            scan.chars
                .iter()
                .map(|c| {
                    let key = c.to_string();
                    // 与默认同向的删覆盖、不留记录（判据同 `apply_common_target`）。
                    let dir =
                        (cc.is_cluster_common_by_default(&key, &cs) != common).then_some(common);
                    (key, dir)
                })
                .collect()
        };
        out.written = store.apply_common_char_overrides(&plan)?;
        self.reload_common_chars();
        debug!(
            "常用字批量: {} [{}] {} 字 / {} 条词条 → {}，落库 {}",
            blk.name,
            blk.range_text(),
            out.chars,
            out.entries,
            if common { "常用" } else { "生僻" },
            out.written
        );
        Ok(out)
    }

    /// 某个字的当前状态：设置页「添加」时用来预览与校验。
    pub(crate) fn common_char_state(&self, ch: &str) -> CommonCharState {
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        CommonCharState {
            // 量词与 `default_verdict_of` 对齐（簇里「有没有」受管辖的 char）：`鬱`+VS16
            // 按「恰好一个 char」判会说管不着，与同结构体里的 `base_common` 自相矛盾。
            governed: ch.chars().any(wind_candidate::is_common_scope),
            base_common: cc.is_cluster_common_by_default(ch, &self.engine_mgr.charsets()),
            over: cc.override_of_cluster(ch),
        }
    }

    /// 设置页对一个字的编辑：写库 + 回灌镜像一并完成。
    ///
    /// 只拒绝非单个可登记字符（空白、控制字符等，[`wind_candidate::single_markable_char`]），
    /// 不按「是不是汉字」设限。返回 Err 而不是静默忽略，界面才说得清为什么没写进去。
    pub(crate) fn common_char_edit(&self, ch: &str, edit: CommonCharEdit) -> anyhow::Result<()> {
        let Some(store) = self.store.as_ref() else {
            anyhow::bail!("无持久化存储");
        };
        match edit {
            CommonCharEdit::ClearAll => {
                let n = store.clear_common_char_overrides()?;
                debug!("常用字覆盖: 清空 {n} 条");
                self.reload_common_chars();
            }
            CommonCharEdit::Reset => {
                self.clear_common_char(ch);
            }
            CommonCharEdit::Set(common) => {
                if wind_candidate::single_markable_char(ch).is_none() {
                    anyhow::bail!("只能登记单个字符（空白与控制字符除外）");
                }
                self.apply_common_target(ch, common);
            }
        }
        Ok(())
    }

    /// 一批 [`Self::common_char_edit`]（`Set` / `Reset`）：先校验整批（任一个字不合法整批不写），
    /// 一次提交、镜像只回灌一次，「设回出厂方向即删覆盖」判据同 [`Self::apply_common_target`]。
    /// 与单字不同，写库失败报错：整批要么全成要么不动。
    pub(crate) fn common_char_edit_batch(
        &self,
        chars: &[String],
        edit: CommonCharEdit,
    ) -> anyhow::Result<()> {
        let Some(store) = self.store.as_ref() else {
            anyhow::bail!("无持久化存储");
        };
        if chars.is_empty() {
            return Ok(());
        }
        let plan: Vec<(String, Option<bool>)> = match edit {
            CommonCharEdit::ClearAll => anyhow::bail!("清空全部覆盖没有批量形式"),
            CommonCharEdit::Reset => chars.iter().map(|c| (c.clone(), None)).collect(),
            CommonCharEdit::Set(common) => {
                if let Some(bad) = chars
                    .iter()
                    .find(|c| wind_candidate::single_markable_char(c).is_none())
                {
                    anyhow::bail!("只能登记单个字符（空白与控制字符除外）：「{bad}」");
                }
                let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
                let cs = self.engine_mgr.charsets();
                chars
                    .iter()
                    .map(|c| {
                        let dir =
                            (cc.is_cluster_common_by_default(c, &cs) != common).then_some(common);
                        (c.clone(), dir)
                    })
                    .collect()
            }
        };
        store.apply_common_char_overrides(&plan)?;
        debug!("常用字覆盖: 批量 {} 字", plan.len());
        self.reload_common_chars();
        Ok(())
    }

    /// 页内第 `page_local` 个候选的标记状态（测试/诊断用）：`(字, 当前是否判常用)`；
    /// `None` = 右键菜单不给「设为生僻字 / 设为常用字」这一项。
    ///
    /// 菜单可用性与写端准入共用 [`Self::common_char_mark`]，断言本函数等于同时锁住两条通路。
    pub fn debug_common_char_mark(&self, page_local: usize) -> Option<(String, bool)> {
        let text = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let (start, end) = self.page_range(&state);
            let idx = start + page_local;
            if idx >= end || idx >= state.candidates.len() {
                return None;
            }
            state.candidates[idx].text.clone()
        };
        self.common_char_mark(&text).map(|m| (m.text, m.common))
    }

    /// 写一条覆盖并立刻重灌镜像。`common` = 设为常用字 / 设为生僻字。
    ///
    /// 返回是否写成功，调用方据此决定要不要重建候选。
    pub(crate) fn set_common_char(&self, ch: &str, common: bool) -> bool {
        let Some(store) = self.store.as_ref() else {
            return false;
        };
        if let Err(e) = store.set_common_char_override(ch, common) {
            warn!("常用字覆盖: 写入失败 ch={ch} common={common}: {e}");
            return false;
        }
        debug!(
            "常用字覆盖: 设 {ch} → {}",
            if common { "常用" } else { "生僻" }
        );
        self.reload_common_chars();
        true
    }

    /// 撤销某字的覆盖，回到出厂判定。
    ///
    /// 与「设为常用字」不是一回事：出厂判生僻的字撤销后仍是生僻。
    pub(crate) fn clear_common_char(&self, ch: &str) -> bool {
        let Some(store) = self.store.as_ref() else {
            return false;
        };
        match store.remove_common_char_override(ch) {
            Ok(existed) => {
                debug!("常用字覆盖: 撤销 {ch}（原本有覆盖={existed}）");
                self.reload_common_chars();
                existed
            }
            Err(e) => {
                warn!("常用字覆盖: 撤销失败 ch={ch}: {e}");
                false
            }
        }
    }

    /// 把某个字的判定设成 `common`，返回是否真的有变化。
    ///
    /// 目标方向等于出厂判定时删覆盖而不是写同向记录：库里只留「与出厂不同」的字，出厂表
    /// 升版时自动跟随；也因此不需要单独的「恢复出厂」菜单项，点回去就是恢复。
    ///
    /// 右键与设置页共用本函数，别各写一份「要不要删」的判断。
    pub(crate) fn apply_common_target(&self, ch: &str, common: bool) -> bool {
        let cs = self.engine_mgr.charsets();
        let base = self
            .common_chars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_cluster_common_by_default(ch, &cs);
        if common == base {
            self.clear_common_char(ch)
        } else {
            self.set_common_char(ch, common)
        }
    }

    /// 候选右键「设为生僻字 / 设为常用字」的写端。
    pub(crate) fn toggle_common_char(&self, state: &mut crate::coordinator::State, text: &str) {
        let Some(mark) = self.common_char_mark(text) else {
            return;
        };
        // 目标 = 当前判定取反。菜单文案正是按 `mark.common` 二选一的，两边同源。
        let target = !mark.common;
        if !self.apply_common_target(&mark.text, target) {
            warn!("常用字标记: {} 写库未生效，跳过重建", mark.text);
            return;
        }
        let before = state.candidates.len();
        // 直接辅助码的前缀快照带着旧的常用字判定（过滤与排序都吃它），原地重建前作废。
        state.direct_aux_prev = None;
        // 重建候选而不只重绘：`is_common` 一变，过滤与排序都会变（混输拼音精确档拿它当
        // 提档准入）。必须按模式分派：特殊 / 生僻字 / 反查模式的编码在 `special_buffer`，
        // 走 `update_candidates`（读恒空的 `input_buffer`）会把候选窗当场清空。
        if matches!(
            state.active,
            Some(crate::pipeline::ModeKind::Special(_))
                | Some(crate::pipeline::ModeKind::RareChar)
                | Some(crate::pipeline::ModeKind::Reverse)
        ) {
            // 丢弃「全码自动上屏」意向：编码没变，用户只是在标记，不该凭空上屏。
            let _ = self.update_special_candidates(state);
        } else {
            self.update_candidates(state);
        }
        // 诊断用：「判定变了吗」与「候选面变了吗」分开打。智能档下孤儿码位的字降级后
        // 候选面可以不变，属正确表现；`生效=false` 才是真没写进去。
        let now_common = self
            .common_chars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_cluster_common(&mark.text, &self.engine_mgr.charsets());
        debug!(
            "常用字标记: {} {}→{}（生效={}）；候选 {} → {} 条，码={}",
            mark.text,
            if mark.common { "常用" } else { "生僻" },
            if target { "常用" } else { "生僻" },
            now_common == target,
            before,
            state.candidates.len(),
            state.input_buffer
        );
        self.notify_ui_update(state);
    }
}

// ───────── 导入导出（设置页「词库管理 · 常用字」的导入/导出按钮）─────────
//
// 与整份备份（`wind-transfer` 的 `userdata/common_chars.jsonl` 段）分工不同：那条是换机
// 还原，这条是单独分发一份「我的常用字判断」，故格式要人看得懂、手改得动。

/// 导出文件的格式标记键。
///
/// 导入端靠它认出「这是一份常用字调整文件」，不带标记的 TOML 一律拒绝，否则误选别的
/// 导出文件只会得到一句「已导入 0 条」。
const COMMON_CHARS_FILE_TAG: &str = "wind_common_chars";

/// 解析结果：字与目标判定，加上跳过的条目及原因。
struct ParsedCommonChars {
    entries: Vec<(String, bool)>,
    skipped: Vec<String>,
}

/// 导入预览（只读，不写库）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommonCharsImportPreview {
    /// 文件里「设为常用」的字数。
    pub common: usize,
    /// 文件里「设为生僻」的字数。
    pub rare: usize,
    /// 解析期跳过的条目及原因（非单个可登记字符、坏行……）。
    pub skipped: Vec<String>,
}

/// 导入结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommonCharsImportOutcome {
    /// 真正落库的覆盖条数。
    pub imported: usize,
    /// 与本机默认判定同向、因而不需要覆盖的字数。
    ///
    /// 单独报出：两台机器默认字表可能不同版本，同向的不留记录才对（见
    /// [`crate::Coordinator::apply_common_target`]），但不解释就像丢了数据。
    pub same_as_default: usize,
    /// 跳过的条目及原因。
    pub skipped: Vec<String>,
}

/// 把一组字符按 TOML 数组字面量输出（不自己拼引号，交给 toml 转义）。
fn toml_array_literal(items: &[String]) -> String {
    toml::Value::Array(items.iter().cloned().map(toml::Value::String).collect()).to_string()
}

/// 把一串字按 TOML 字符串字面量输出（不自己拼引号，交给 toml 转义）。
fn toml_string_literal(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// 把导出条目分成「能安全拼进文本串的」与「必须单列成数组项的」。
/// 判据是往返等价而不是码位数：接到文本串末尾后重新按字素簇切开，末簇仍是它本身且
/// 总簇数恰好加一才算安全。`🇨` 与 `🇳` 各自单码位，拼在一起却合成国旗；往返自检不必
/// 逐条列举 Unicode 组合规则。
fn partition_for_export(items: &[String]) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut n = 0usize; // text 里已有的簇数，省得每轮重数
    let mut seq = Vec::new();
    for it in items {
        // 多码位的一律进数组段，文本段保持简洁、好手改。
        let mut probe = text.clone();
        probe.push_str(it);
        let clusters: Vec<&str> = wind_candidate::split_markable_clusters(&probe).collect();
        if it.chars().count() == 1
            && clusters.len() == n + 1
            && clusters.last() == Some(&it.as_str())
        {
            text = probe;
            n += 1;
        } else {
            seq.push(it.clone());
        }
    }
    (text, seq)
}

/// 解析导入文件。TOML 为主格式，JSONL 是备份包里那一段的原始形态。
///
/// 认 JSONL 是为了让用户直接导入从备份包里掏出的 `userdata/common_chars.jsonl`；
/// 首个有效行以 `{` 开头即是。
fn parse_common_chars_file(content: &str) -> anyhow::Result<ParsedCommonChars> {
    // 先规整行尾再探测：孤立 \r 会让整份文件算成一行，格式在第一步就判错。
    let normalized = wind_utils::text::normalize_input(content);
    let content = normalized.as_ref();
    let first = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'));
    if first.is_some_and(|l| l.starts_with('{')) {
        return Ok(parse_common_chars_jsonl(content));
    }

    let table: toml::Table = content
        .parse::<toml::Table>()
        .map_err(|e| anyhow::anyhow!("不是一份有效的常用字调整文件: {e}"))?;
    if !table.contains_key(COMMON_CHARS_FILE_TAG) {
        anyhow::bail!("不是一份常用字调整文件（缺少 {COMMON_CHARS_FILE_TAG} 标记）");
    }
    let mut out = ParsedCommonChars {
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    // 两段两个方向，段缺失合法。`common`/`rare` 是字符串（一串单字符，好手写），
    // `common_seq`/`rare_seq` 是数组（多码位字符单独成项）。每个段两种形态都认，
    // 手写时不必记哪段该用什么形态。
    for (key, common) in [
        ("common", true),
        ("rare", false),
        ("common_seq", true),
        ("rare_seq", false),
    ] {
        let Some(v) = table.get(key) else { continue };
        match v {
            toml::Value::Array(items) => {
                for it in items {
                    match it.as_str() {
                        Some(t) => push_common_char_entry(&mut out, t, common),
                        None => out.skipped.push(format!("{key}：非字符串项，已跳过")),
                    }
                }
            }
            // 按字素簇切，不能按 `char` 切（`👨‍👩‍👧` 会被拆成 5 条垃圾记录）。
            toml::Value::String(s) => {
                for g in wind_candidate::split_markable_clusters(s) {
                    push_common_char_entry(&mut out, g, common);
                }
            }
            _ => out
                .skipped
                .push(format!("{key}：应为字符串或数组，已跳过整段")),
        }
    }
    Ok(out)
}

/// JSONL：每行 `{"ch":"槮","common":true}`。坏行跳过而非整份失败——与
/// [`wind_store::Store::import_common_chars_jsonl`] 同一条纪律。
fn parse_common_chars_jsonl(content: &str) -> ParsedCommonChars {
    let mut out = ParsedCommonChars {
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match serde_json::from_str::<wind_store::common_chars::CommonCharOverride>(line) {
            Ok(o) => push_common_char_entry(&mut out, &o.ch, o.common),
            Err(e) => out.skipped.push(format!("第 {} 行：{e}", i + 1)),
        }
    }
    out
}

/// 收一条，收不下的如实报告。
///
/// 准入与右键写端同为 [`wind_candidate::single_markable_char`]，这道只是最后一层数据卫生。
fn push_common_char_entry(out: &mut ParsedCommonChars, text: &str, common: bool) {
    match wind_candidate::single_markable_char(text) {
        Some(g) => out.entries.push((g.to_string(), common)),
        None => out
            .skipped
            .push(format!("{text:?} 不是单个可登记的字符，已跳过")),
    }
}

impl crate::Coordinator {
    /// 导出用户调整为 TOML 文本。
    ///
    /// 数据取自 store（真相源）而不是运行时镜像：镜像万一某次回灌漏了，导出的文件就与
    /// 实际不符。导出的是稀疏调整而非全表：全表快照到对方机器上会把默认字也钉成显式覆盖，
    /// 从此脱离默认表升版。
    pub fn export_common_chars(&self) -> anyhow::Result<String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let (mut common, mut rare): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for o in store.list_common_char_overrides()? {
            if o.common { &mut common } else { &mut rare }.push(o.ch);
        }
        // 单字符走文本段、多码位走数组段：不登记 emoji 序列的用户文件里不会出现数组段。
        let (common_text, common_seq) = partition_for_export(&common);
        let (rare_text, rare_seq) = partition_for_export(&rare);
        let mut out = format!(
            "# 清风输入法 · 常用字调整（只记录与默认不同的字）\n\
             # common = 判为常用；rare = 判为生僻\n\
             {COMMON_CHARS_FILE_TAG} = 1\n\
             common = {}\n\
             rare = {}\n",
            toml_string_literal(&common_text),
            toml_string_literal(&rare_text),
        );
        // 空数组段一律不写，免得让人以为漏配了什么。
        if !common_seq.is_empty() {
            out.push_str(&format!(
                "# 多码位字符（emoji 序列、国旗等）单独成项，避免与相邻字符黏连\n\
                 common_seq = {}\n",
                toml_array_literal(&common_seq)
            ));
        }
        if !rare_seq.is_empty() {
            if common_seq.is_empty() {
                out.push_str("# 多码位字符（emoji 序列、国旗等）单独成项，避免与相邻字符黏连\n");
            }
            out.push_str(&format!("rare_seq = {}\n", toml_array_literal(&rare_seq)));
        }
        Ok(out)
    }

    /// 导入预览：只解析与计数，不写任何东西。
    pub fn preview_common_chars_import(
        &self,
        content: &str,
    ) -> anyhow::Result<CommonCharsImportPreview> {
        let parsed = parse_common_chars_file(content)?;
        Ok(CommonCharsImportPreview {
            common: parsed.entries.iter().filter(|(_, c)| *c).count(),
            rare: parsed.entries.iter().filter(|(_, c)| !*c).count(),
            skipped: parsed.skipped,
        })
    }

    /// 导入用户调整。`replace` 为真时先清空现有全部调整。
    ///
    /// 与默认同向的字不写记录，判据同 [`Self::apply_common_target`]（为省每条一次整表回灌
    /// 而内联）：照单全收会把对方默认里本就有的字钉死，拿不到默认表升版。回灌只在最后做一次。
    pub fn import_common_chars(
        &self,
        content: &str,
        replace: bool,
    ) -> anyhow::Result<CommonCharsImportOutcome> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let parsed = parse_common_chars_file(content)?;

        if replace {
            // replace 语义是「用文件里的状态覆盖现状」，旧覆盖要先清掉。
            store.clear_common_char_overrides()?;
        }

        let mut outcome = CommonCharsImportOutcome {
            skipped: parsed.skipped,
            ..Default::default()
        };
        {
            // 读锁在循环外取一次：base 判定不随本次写入变化。回灌要写锁，故单独作用域。
            let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
            let cs = self.engine_mgr.charsets();
            for (ch, common) in parsed.entries {
                if common == cc.is_cluster_common_by_default(&ch, &cs) {
                    store.remove_common_char_override(&ch)?;
                    outcome.same_as_default += 1;
                } else {
                    store.set_common_char_override(&ch, common)?;
                    outcome.imported += 1;
                }
            }
        }
        self.reload_common_chars();
        debug!(
            "常用字导入: 写入 {} 条，同默认 {} 条，跳过 {} 条（replace={replace}）",
            outcome.imported,
            outcome.same_as_default,
            outcome.skipped.len()
        );
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "handle_common_chars_tests.rs"]
mod tests;
