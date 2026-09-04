//! 常用字表的**用户覆盖**：运行时镜像装载 + 候选右键「设为生僻字 / 设为常用字」。
//!
//! 真相在 store（`wind_store::common_chars`，key = 一个字素簇，不带方案），这里维护
//! `Coordinator::common_chars` 这份内存镜像，并在写库后立刻重灌——「设了没反应、
//! 重启才生效」正是镜像没回灌造成的，本仓已在别处栽过。
//!
//! ## 与候选调整（shadow）的分界
//!
//! | | 作用域 | 用户看到的 |
//! |---|---|---|
//! | shadow | 这个方案、这个码 | 「隐藏此候选」只在这个码下没了 |
//! | 本模块 | **全局**，所有方案所有码 | 「设为生僻字」在哪儿打它都降级 |
//!
//! 两者在右键菜单里挨着，文案必须把作用域说出来，否则用户会两个都试一遍再困惑于
//! 表现为何不同。

use tracing::{debug, warn};

/// 设置页列表的一行。
///
/// 列的是**全表**（出厂字 + 用户加的），不是只列改过的那几条：用户来这个页面最常问的
/// 是「这个字现在算不算常用」，只列改动答不了。改过的那些靠 [`Self::overridden`] 标出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonCharRow {
    /// 这一行的**字素簇文本**（用户眼里的一个字符）。类型是 `String` 而非 `char`：
    /// 覆盖表里可以有 `👨‍👩‍👧`（5 个码位）、`🇨🇳`（2 个）这样的条目。
    pub text: String,
    /// **当前生效**的判定（用户改过就是用户设的，否则跟随默认）。
    pub common: bool,
    /// 默认判定（出厂字表说了算）。界面靠它显示「默认 → 现在」的对照。
    pub base_common: bool,
    /// 这一行被用户改过。决定「恢复默认」能不能点——没改过的行点它没有意义。
    pub overridden: bool,
    /// 所属 Unicode 块的中文名（[`wind_candidate::block_of`]），如「注音符号」。
    ///
    /// 光看字形分不清这些东西是什么——issue #83 的用户为此把整张码表喂给 AI 分类、再手工
    /// 逐个试，才弄明白哪些会显示哪些不会。类型列就是把那份工作内建进来。
    ///
    /// ⚠️ 范围文本（`2FF0-2FFF`）**不存在这里**：消费方自己调 `block_of_cluster(t).range_text()`，
    /// 免得把同一份格式化逻辑抄第二份。
    pub block: &'static str,
    /// **决定这个字常用性的那个字符类**的 key（`None` = 没有类命中它）。
    ///
    /// ★ 与 `block` 是两个不同的问题，各自绑着一个不同的操作：
    ///
    /// | | 回答什么 | 绑着哪个操作 |
    /// |---|---|---|
    /// | `block` | 它在哪个 Unicode 区块 | **整类批量**：扫那个码位区间 → 写上千条 redb 覆盖 |
    /// | `class` | 谁决定了它的常用性 | **改类的 default**：写一行 yaml，作用于整个类 |
    ///
    /// ⇒ 不能拿 class 顶掉 block：用户会看着「emoji」去点一个按「表情符号」块工作的
    /// 按钮。两个都给，各自标清楚。
    pub class: Option<String>,
    /// 这个块能不能整类批量操作（[`wind_candidate::block_allows_bulk_edit`]）。
    ///
    /// ⛔ 汉字块恒 `false`：对着一行「我」点「将『基本汉字』全部设为生僻」，一次误点就是
    /// 七千多条覆盖，整张常用字表当场作废。设置页据此**不显示**该菜单项。
    pub block_bulk_editable: bool,
}

/// 某个字的当前状态（设置页「添加」时的预览与校验）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommonCharState {
    /// 这个字符在**默认字表**里有没有说法（[`wind_candidate::is_common_scope`]）。
    ///
    /// ⚠️ 纯提示，**界面不得据此拒绝添加**（issue #83 起任何字符都可登记）。`false` 只表示
    /// 「默认字表管不着它，这条是纯用户规则」——用户给注音、假名、结构描述符设生僻时正是
    /// 这种情况，而那恰恰是本功能要支持的用法。
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
    ///
    /// ★ **界面必须显示它**：`，` 只是 1 个字符却出现在 326 条词条里，只报字符数会让人
    /// 严重低估影响面。预览就是为这一条存在的。
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
/// 两处放宽都出自 issue #83，各修掉一类「右键里没有这一项」：
///
/// 1. **不再限定汉字**。曾用 `is_common_scope`（只认汉字与 PUA），于是字根、间架结构符、
///    注音、假名这些用户点名要关掉的候选反而标不了。放开的前提是读端 `is_string_common`
///    **先**改成覆盖优先——顺序反了会存下一批永不被查询的死记录，且全程无报错。
/// 2. **「一个字」按 Unicode 字素簇算，不是 `char` 数**。`⚽️`(2 个码位)、`👨‍👩‍👧`(5 个)、
///    `🇨🇳`(2 个)、`1️⃣`(3 个) 在屏幕上都只有一个图形——**用户分辨不出的差别，不该决定
///    能不能设置**。按 `chars().count() == 1` 判会把整类 emoji 挡在门外。
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
        // 无 store 就没有落点：菜单给了入口而写端无处可写，是那种「点得动却毫无反应」
        // 的静默错配。headless 与未初始化存储一律不给。
        self.store.as_ref()?;
        let g = common_char_of(text)?;
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        Some(CommonCharMark {
            text: g.to_string(),
            common: cc.is_cluster_common(g, &self.engine_mgr.charsets()),
        })
    }

    /// 设置页列表：**全表**（出厂字按字表原序 + 用户加的追加在后），可按 `query` 过滤。
    ///
    /// 数据全部取自**内存镜像**，不再单独读一次 store：镜像与过滤层用的是同一份数据，
    /// 分头取值会让界面显示的判定与实际生效的那份悄悄错开。
    ///
    /// `query` 非空时只保留「出现在查询串里」的字——用户想查某个字就直接把它打进搜索框，
    /// 粘一整句进去则列出这句话里的所有字，两种用法都成立。
    ///
    /// `only_modified` 为真时只留改过的行：全表 8104 条里自己动过的那几个，翻页是找不到的。
    pub(crate) fn common_char_rows(&self, query: &str, only_modified: bool) -> Vec<CommonCharRow> {
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        // 列表最多 8000+ 行，registry 在循环外取一次。
        let cs = self.engine_mgr.charsets();
        let q = query.trim();
        cc.list_all(&cs)
            .into_iter()
            // 搜索按**子串包含**：查询串里出现过这个簇即命中。多码位簇用 `chars().contains`
            // 判不出来（那是按单码位比的），`⚽️` 会永远搜不到。
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
    /// 入口是**当前选中的那一行**而不是让用户填码位区间：他在候选里看见 `ㄅ` 觉得烦，
    /// 心里想的是「这类东西别出来」，而不是「3100 到 312F 别出来」。类型从行推导，
    /// 用户不必先知道它叫什么、码位在哪。
    ///
    /// ⛔ 汉字块一律拒绝（[`wind_candidate::block_allows_bulk_edit`]）：列表里 8104 个默认字
    /// 全是汉字，对着一行「我」点「将『基本汉字』全部设为生僻」，一次误点就是七千多条覆盖，
    /// 整张常用字表当场作废。那些块本就有默认字表逐字管着，要调也该逐字调。
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
        // ⚠️ **一个事务写完、最后统一回灌一次**，不要在循环里调 `apply_common_target`：
        // 那条路每写一个字都是一次 fsync 提交，外加一次 `reload_common_chars()`（全表重读
        // + 镜像重建，取的还是**写锁**，与候选过滤每次按键都要拿的读锁相撞）。「表情符号」
        // 这类块在 emoji 词库下可能上千个字符，逐条走就是上千次提交、上千次卡住输入。
        //
        // 读锁在循环外取一次：默认判定只看基表，不随本次写入变化。
        let plan: Vec<(String, Option<bool>)> = {
            let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
            let cs = self.engine_mgr.charsets();
            scan.chars
                .iter()
                .map(|c| {
                    let key = c.to_string();
                    // 与默认同向的删覆盖、不留记录（判据与单字写端 `apply_common_target`
                    // 同源，只是这里把「取默认判定」与「写库」拆开以便批处理）。
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

    /// 某个字的当前状态：设置页「添加」时用来预览与**校验**。
    pub(crate) fn common_char_state(&self, ch: &str) -> CommonCharState {
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        CommonCharState {
            // ⚠️ 量词要与 `default_verdict_of` 对齐（那边问的是「簇里**有没有**受管辖的
            // char」）：`鬱`+VS16 这种簇，默认字表确实管着它里面的 `鬱`。按「恰好一个
            // char」判会说「管不着」，而同一结构体里的 `base_common` 又报出只可能来自
            // 默认字表的判定——两个字段自相矛盾。
            governed: ch.chars().any(wind_candidate::is_common_scope),
            base_common: cc.is_cluster_common_by_default(ch, &self.engine_mgr.charsets()),
            over: cc.override_of_cluster(ch),
        }
    }

    /// 设置页对一个字的编辑：写库 + 回灌镜像一并完成。
    ///
    /// ⚠️ 只拒绝空白与控制字符（[`wind_candidate::is_markable`]）。**不再按「是不是汉字」
    /// 设限**：用户可以给任何字符登记常用/生僻，读端一律认（issue #83）。
    /// 这里返回 Err 而不是静默忽略，界面才说得清为什么没写进去。
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

    /// 页内第 `page_local` 个候选的标记状态（测试/诊断用）：`(字, 当前是否判常用)`；
    /// `None` = 右键菜单不给「设为生僻字 / 设为常用字」这一项。
    ///
    /// 菜单可用性与写端准入共用 [`Self::common_char_mark`]，故断言本函数
    /// **等于同时锁住两条通路**——它们错配的表现是「点得动却毫无反应」，没有日志。
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
    /// 返回是否写成功——调用方据此决定要不要重建候选（写失败还重建纯属白跑一轮）。
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
    /// 与「设为常用字」**不是**一回事：出厂判生僻的字撤销后仍是生僻。
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
    /// ## 切到出厂方向时**删覆盖**，而不是写一条同向记录
    ///
    /// 用户把「的」设成生僻（存 `false`，与出厂相反），过一会儿又设回常用：目标方向
    /// 恰好等于出厂判定，此时删掉那条覆盖让它重新跟随出厂，而不是存一条 `true`。
    /// 两个好处：
    /// - 库里永远只有「与出厂不同」的字，设置页列出来的就是一份干净的「我改过的」；
    /// - 出厂表将来升版时这个字自动跟随，不会被一条冗余记录钉死在旧判定上。
    ///
    /// 由此也**不需要单独的「恢复出厂」菜单项**——同一项点回去就是恢复。
    ///
    /// ★ 右键与设置页共用本函数。两条入口各写一份「要不要删」的判断，迟早会漂移成
    /// 「右键点回去干净、设置页改回去留一条冗余」这种没人看得出的差别。
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
        // 重建候选：`is_common` 一变，过滤（智能 / 常用字档）与**排序**都会跟着变——
        // 后者容易被忘：混输的拼音精确档拿 `is_common` 当提档准入（`is_pinyin_exact_tier`），
        // 只重绘不重建的话，用户会看到「标记了，但候选顺序还是老样子」。
        //
        // ⚠️ 必须按模式分派：主路径的 `update_candidates` 读 `input_buffer`，特殊模式下它
        // 恒为空——走错分支的后果不是「不刷新」而是候选窗当场清空。
        // 生僻字模式一并走这支：它的编码同样在 `special_buffer`，漏掉就会落进 else 的
        // `update_candidates`（读恒空的 `input_buffer`）⇒ 用户刚把一个字设成常用，
        // 候选窗当场清空。而这恰恰是生僻字模式里最常做的操作。
        if matches!(
            state.active,
            Some(crate::pipeline::ModeKind::Special(_)) | Some(crate::pipeline::ModeKind::RareChar)
        ) {
            // 返回值是「全码策略请求自动上屏」的意向，此处刻意丢弃：编码一个字没变，
            // 用户只是在标记字的常用性，凭空上屏是错的。
            let _ = self.update_special_candidates(state);
        } else {
            self.update_candidates(state);
        }
        // ★ 诊断「点了没反应」用：把「判定真的变了吗」与「候选面因此变了吗」分开打。
        //
        // 这两件事**经常不同时发生**，而混在一起看会把正常行为误判成缺陷：智能档只在
        // 同码位**还有别的常用字**时才压得住降级的字；若它是孤儿码位，判定变了而候选面
        // 一模一样，属正确表现。反过来，`生效=false` 才是真的没写进去。
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
// 与整份备份（`wind-transfer` 的 `userdata/common_chars.jsonl` 段）分工不同：那条是
// 换机还原，整份配置一起走；这条是**单独分发一份「我的常用字判断」**——同一个人的第二
// 台机器、或者把自己整理的一批生僻字给同行。故格式要人看得懂、手改得动。

/// 导出文件的格式标记键。
///
/// 导入端靠它认出「这是一份常用字调整文件」，不带标记的 TOML 一律拒绝。没有标记的话，
/// 用户误选了快捷输入的导出文件时解析照样通过、两个段都读不到，界面只会说一句
/// 「已导入 0 条」——他会以为是文件坏了，而不是选错了。
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
    /// 解析期跳过的条目及原因（非汉字、多字符、坏行……）。
    pub skipped: Vec<String>,
}

/// 导入结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommonCharsImportOutcome {
    /// 真正落库的覆盖条数。
    pub imported: usize,
    /// 与本机默认判定**同向**、因而不需要覆盖的字数。
    ///
    /// 单独报出来而不并进 `imported`：两台机器的默认字表可能不同版本，用户在 A 机上
    /// 亲手设过的字到了 B 机可能本就是默认，此时库里不留记录才是对的（见
    /// [`crate::Coordinator::apply_common_target`]）。但「导入 100 条却只写了 30 条」
    /// 若不解释，看起来就像丢了数据。
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
///
/// ★ 判据是**往返等价**，不是「码位数」：把这一条接到文本串末尾后重新按字素簇切开，
/// 末簇仍是它本身、且总簇数恰好加一，才算安全。
///
/// 为什么不能按码位数判：`🇨` 与 `🇳` **各自都是单码位**，拼进同一个字符串却会合成一个
/// 国旗 `🇨🇳`——读回来就少了一条、多了一条本不存在的。变体选择符、组合附加符号同理。
/// 要按码位数判就得先列举「哪些单码位是危险的」，而**逐条列举 Unicode 规则正是这个
/// issue 反复吃亏的写法**。往返自检不必知道规则是什么，它直接验结果。
fn partition_for_export(items: &[String]) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut n = 0usize; // text 里已有的簇数，省得每轮重数
    let mut seq = Vec::new();
    for it in items {
        // 多码位的一律进数组段：它们本就是「一个图形多个码位」，混在文本串里既难读也
        // 难手工编辑，而用户要的正是「文本段保持简洁」。
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
/// **兼容 JSONL 是有实际出路的**：用户从备份包里掏出 `userdata/common_chars.jsonl`
/// 想单独导进来时，那份文件是现成的，认不出它就只能让他手工转换。识别成本也低——
/// 首个有效行以 `{` 开头即是。
fn parse_common_chars_file(content: &str) -> anyhow::Result<ParsedCommonChars> {
    let first = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'));
    if first.is_some_and(|l| l.starts_with('{')) {
        return Ok(parse_common_chars_jsonl(content));
    }

    let table: toml::Table = toml::from_str(content)
        .map_err(|e| anyhow::anyhow!("不是一份有效的常用字调整文件: {e}"))?;
    if !table.contains_key(COMMON_CHARS_FILE_TAG) {
        anyhow::bail!("不是一份常用字调整文件（缺少 {COMMON_CHARS_FILE_TAG} 标记）");
    }
    let mut out = ParsedCommonChars {
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    // 两段分别是两个方向。段缺失是合法的（用户只降级过、没升级过）。
    // 四个段：`common`/`rare` 是**字符串**（一串单字符，好手写，也是旧版的形态），
    // `common_seq`/`rare_seq` 是**数组**（多码位字符单独成项）。两种并存而不是二选一：
    // 从不登记 emoji 序列的用户，文件里根本不会出现 `*_seq` 段，格式与旧版一样简洁。
    //
    // 每个段两种形态都认（字符串段也接受数组、反之亦然），手写文件时不必记住哪个段该用
    // 什么形态——这道宽容不花什么代价，却省掉一类「格式没写对、导入 0 条」的困惑。
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
            // 按**字素簇**切，不能按 `char` 切——那会把 `👨‍👩‍👧` 拆成 5 条垃圾记录。
            // 对旧文件里清一色的单码位汉字，两种切法结果相同，故向后兼容。
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
/// ⚠️ 准入与右键写端同为 [`wind_candidate::is_markable`]（issue #83 起放开到全字符）。
/// 导入文件里的空白本就会被上游按行/按字切掉，这道只是最后一层数据卫生。
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
    /// 数据取自 **store**（真相源）而不是运行时镜像，与 `export_quick_format` 同一条判据：
    /// 镜像是热路径读缓存，万一某次回灌漏了，从它导出就会写出一份与实际不符的文件，
    /// 而这种偏差要等导到另一台机器才看得出来。
    ///
    /// 导出的是**稀疏调整**而非 8104 字全表：全表快照到了对方机器上，会把「对方默认表里
    /// 本来就有的字」也钉成显式覆盖，从此脱离默认表升版；而且文件里根本分不出哪几个字
    /// 是这个人真正表达过的意见。
    pub fn export_common_chars(&self) -> anyhow::Result<String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let (mut common, mut rare): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for o in store.list_common_char_overrides()? {
            if o.common { &mut common } else { &mut rare }.push(o.ch);
        }
        // 单字符走文本段、多码位走数组段：绝大多数用户一个 emoji 序列都不会登记，
        // 他们的文件里因此**根本不会出现数组段**，格式与旧版一样简洁、好手工编辑。
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
        // 空数组段一律不写：一行 `rare_seq = []` 对从不用 emoji 的人是纯噪音，
        // 还会让人以为自己漏配了什么。
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

    /// 导入预览：只解析与计数，**不写任何东西**。
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
    /// ## ★ 与默认同向的字**不写记录**
    ///
    /// 逐条走的是 [`Self::apply_common_target`] 的同一条判据（这里为省掉每条一次的整表
    /// 回灌而内联，见下）：目标方向等于本机默认时删覆盖、不写同向记录。两台机器的默认
    /// 字表可能是不同版本，照单全收会把对方默认里本就有的字钉死在当前判定上，从此拿不到
    /// 默认表升版——而这件事导入时毫无异样，几个版本之后才会有人发现某个字一直不对。
    ///
    /// 回灌只在最后做一次（镜像是整表替换），与 `import_quick_format` 同。
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
            // replace 的语义是「用文件里的状态覆盖现状」：留着旧覆盖会得到
            // 「文件里的 + 我原有的」。
            store.clear_common_char_overrides()?;
        }

        let mut outcome = CommonCharsImportOutcome {
            skipped: parsed.skipped,
            ..Default::default()
        };
        {
            // 读锁在循环外取一次：base 判定不随本次写入变化（覆盖不影响基表），
            // 逐条取锁纯属白付。回灌要写锁，故这一段单独作用域。
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
mod tests {
    use super::{common_char_of, partition_for_export};

    /// 导出分段：单字符进文本串，多码位单独成项。
    ///
    /// ★ 关键在最后一组：`🇨` 与 `🇳` **各自都是单码位**，却不能拼进同一个文本串——
    /// 拼了就合成一个国旗 `🇨🇳`，读回来少一条、多一条本不存在的。判据是往返自检而不是
    /// 「数码位」，故不必列举「哪些单码位是危险的」。
    #[test]
    fn export_puts_plain_chars_in_text_and_sequences_in_array() {
        // 常见情况：全是单字符 ⇒ 数组段为空 ⇒ 导出文件里根本不会出现它。
        let (text, seq) = partition_for_export(&[
            "槮".to_string(),
            "鬱".to_string(),
            "ㄅ".to_string(),
            "⿰".to_string(),
        ]);
        assert_eq!(text, "槮鬱ㄅ⿰");
        assert!(seq.is_empty(), "没有多码位字符时不该产生数组段");

        // 多码位一律单列。
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let ball = "\u{26BD}\u{FE0F}";
        let (text, seq) = partition_for_export(&[
            "槮".to_string(),
            ball.to_string(),
            "鬱".to_string(),
            family.to_string(),
        ]);
        assert_eq!(text, "槮鬱");
        assert_eq!(seq, vec![ball.to_string(), family.to_string()]);

        // 单码位但会与相邻字符黏连的：必须挤出去。
        let (text, seq) = partition_for_export(&[
            "\u{1F1E8}".to_string(), // 🇨
            "\u{1F1F3}".to_string(), // 🇳
        ]);
        assert_eq!(text, "\u{1F1E8}", "第一个安全");
        assert_eq!(
            seq,
            vec!["\u{1F1F3}".to_string()],
            "第二个接上去会合成国旗，必须单列"
        );
    }

    #[test]
    fn accepts_single_han_and_pua() {
        for s in ["我", "鬱", "\u{E831}", "\u{20000}"] {
            assert_eq!(common_char_of(s), Some(s), "{s} 应放行（键就是它自己）");
        }
    }

    /// 多码位的一个图形也算一个字符——用户报的 `⚽️`，以及 ZWJ 序列、国旗、keycap。
    ///
    /// 键是**整簇**：只登记组合，单个成员不受牵连（见 wind-candidate 的
    /// `multi_code_point_cluster_override_takes_effect`）。
    #[test]
    fn accepts_multi_code_point_graphemes() {
        for (s, what) in [
            ("\u{26BD}\u{FE0F}", "变体选择符 ⚽️"),
            ("\u{1F44D}\u{1F3FB}", "肤色修饰符"),
            (
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
                "ZWJ 家庭序列",
            ),
            ("\u{1F1E8}\u{1F1F3}", "国旗"),
            ("\u{0031}\u{FE0F}\u{20E3}", "keycap"),
        ] {
            assert_eq!(common_char_of(s), Some(s), "{what} 应放行");
        }
    }

    #[test]
    fn rejects_phrases() {
        // 「常用」是字级属性，词组没有——给词组存覆盖，读端逐字判定时永远看不到它。
        assert_eq!(common_char_of("我们"), None);
        assert_eq!(common_char_of(""), None);
    }

    /// 非汉字字符**现在一律放行**（issue #83：词库管理全范围放开）。
    ///
    /// 取代旧的 `rejects_out_of_scope_chars`——那条钉的是相反的行为，理由是读端会忽略
    /// 域外覆盖、放行等于存死记录。读端改成覆盖优先后那个理由不再成立，两条断言必然互斥。
    /// 用户点名要能关掉的字根、间架结构符、注音、假名，全都在这一批里。
    #[test]
    fn accepts_non_han_chars() {
        for s in ["、", "，", "①", "℃", "あ", "ㄅ", "⿰", "😀", "A", "7"] {
            assert_eq!(common_char_of(s), Some(s), "{s} 应放行");
        }
    }

    /// 空白与控制字符仍然拒绝——数据卫生，不是作用域判断：它们不会作为候选出现，
    /// 登记进去只会在列表里显示成一行空白，用户既看不出是什么也点不掉。
    #[test]
    fn rejects_blank_and_control_chars() {
        for s in [" ", "\t", "\u{3000}", "\u{0}"] {
            assert_eq!(common_char_of(s), None, "{s:?} 不该放行");
        }
    }
}
