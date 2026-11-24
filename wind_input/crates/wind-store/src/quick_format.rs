//! 快捷输入格式表的用户调整（右键菜单调序 / 停用）。
//!
//! 与 [`crate::shadow`] 是两套东西，别混：
//!
//! | | 作用域 | 键 |
//! |---|---|---|
//! | shadow（候选调整） | 一次输入（这个码下的这些字） | `(方案, 输入码)` |
//! | 本模块（格式调整） | 一类输入（所有日期 / 所有数字） | `格式类别` |
//!
//! 快捷输入的「输入码」是 `2026.6.19` 这种具体值，若存进 shadow，调整只对那一天有效、
//! 次日即失效。故另起一张表，键只到类别。
//!
//! ## 与格式表文件的关系
//!
//! ```text
//! 出厂 data/system.quick.toml
//!   ↓ 用户整份覆盖（高级用户手写，可选）
//! 基表
//!   ↓ 本表的 moved / disabled（普通用户点右键）
//! 最终候选
//! ```
//!
//! 两类用户各写各的落点。GUI 调整绝不回写 `system.quick.toml`：那会抢走手写文件的所有权
//! （重写丢注释），也让普通用户不知情地永久脱离出厂更新。
//!
//! ## 为什么存稀疏的 (id, position) 而不是完整顺序
//!
//! 存完整序列的话，出厂新增的格式不在序列里，还得另定「排哪」的规则。稀疏记录只存被碰过的
//! 条目，新增格式自然落在基表位置。
//!
//! 规则的应用由调用方完成（`wind_quick_input::FormatAdjust`），本模块只管存取：store 不依赖业务类型。

use crate::store::Store;
use serde::{Deserialize, Serialize};

/// 一条移动规则：把格式 `id` 固定到组内下标 `position`（0 = 首位）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFormatMove {
    pub id: String,
    pub position: usize,
}

/// 用户自己加的一条格式。
///
/// 没有 `position` 字段：插入序即顺序（新条目落本类末尾），调序与出厂条目共用
/// [`QuickFormatRecord::moved`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFormatAdded {
    pub id: String,
    pub text: String,
}

/// 某一类（`date` / `month_day` / `year_month` / `number` / `calc`）的用户数据。
///
/// 清理时须区别对待：`moved`/`disabled` 是对基表的调整（清掉即回到出厂），`added` 是用户
/// 自己的内容（清掉不可逆）。
/// 见 [`Store::reset_quick_format_kind`] 与 [`Store::clear_quick_format_kind`]。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFormatRecord {
    /// 被移动过的格式。LIFO，index 0 = 最新，应用时逆序遍历（最新的最后应用、
    /// 优先级最高），与 [`crate::shadow::ShadowRecord::pinned`] 同构。
    #[serde(default)]
    pub moved: Vec<QuickFormatMove>,
    /// 被停用的格式 id。
    ///
    /// 叫 disabled 不叫 hidden：被停用的行在设置页里仍可见、能再打开；若是「隐藏」，
    /// 用户就再也点不到它了。
    #[serde(default)]
    pub disabled: Vec<String>,
    /// 用户自己加的条目（不在 `system.quick.toml` 里）。
    ///
    /// 不回写格式表文件的理由见模块头。它是用户的内容而非对出厂的调整，「恢复默认」不得删它。
    #[serde(default)]
    pub added: Vec<QuickFormatAdded>,
}

impl QuickFormatRecord {
    pub fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.disabled.is_empty() && self.added.is_empty()
    }

    /// 追加一条用户条目（调用方负责 id 唯一与模板校验）。
    pub fn add_entry(&mut self, id: &str, text: &str) {
        self.added.push(QuickFormatAdded {
            id: id.to_string(),
            text: text.to_string(),
        });
    }

    /// 改写用户条目的模板；`false` = 没有这条（不是用户条目，或已被删）。
    ///
    /// 出厂条目不在 `added` 里，故返回 `false`：「不许编辑出厂模板」由数据结构兜住。
    pub fn set_text(&mut self, id: &str, text: &str) -> bool {
        match self.added.iter_mut().find(|a| a.id == id) {
            Some(a) => {
                a.text = text.to_string();
                true
            }
            None => false,
        }
    }

    /// 删除用户条目，连带清掉它的调序与停用规则；`false` = 没有这条。
    /// 不清的话，日后重加同 id 条目时旧规则会复活。
    pub fn remove_entry(&mut self, id: &str) -> bool {
        let before = self.added.len();
        self.added.retain(|a| a.id != id);
        if self.added.len() == before {
            return false;
        }
        self.apply_reset(id);
        true
    }

    /// 这个 id 是不是用户自己加的条目。
    pub fn is_added(&self, id: &str) -> bool {
        self.added.iter().any(|a| a.id == id)
    }

    /// 移动：把 `id` 固定到 `position`。同 id 的旧规则被顶替（LIFO，新规则插队首）。
    pub fn apply_move(&mut self, id: &str, position: usize) {
        self.moved.retain(|m| m.id != id);
        self.moved.insert(
            0,
            QuickFormatMove {
                id: id.to_string(),
                position,
            },
        );
    }

    /// 启用 / 停用某条格式。
    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        self.disabled.retain(|d| d != id);
        if !enabled {
            self.disabled.push(id.to_string());
        }
    }

    /// 清除某条格式的全部调整（恢复它的出厂位置与启用状态）。
    pub fn apply_reset(&mut self, id: &str) {
        self.moved.retain(|m| m.id != id);
        self.disabled.retain(|d| d != id);
    }

    /// 该格式是否被调整过（供菜单项灰显判断）。
    pub fn has_rule(&self, id: &str) -> bool {
        self.moved.iter().any(|m| m.id == id) || self.disabled.iter().any(|d| d == id)
    }
}

/// 文件形态：`kinds: {<kind>: QuickFormatRecord}`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct QuickFile {
    #[serde(default)]
    kinds: std::collections::BTreeMap<String, QuickFormatRecord>,
}

/// 说明行之后的语法提示。
const QUICK_HELP: &str = "# [kinds.<类别>] 下：disabled = 停用的格式 id；[[kinds.<类别>.moved]] id / position = 固定到组内第几位；\n\
# [[kinds.<类别>.added]] id / text = 自己加的格式。出厂格式表见 system.quick.toml。\n";

/// 用户目录里的 `quick_format.toml`（P5 起）。
fn parse_quick(s: &str) -> anyhow::Result<QuickFile> {
    if s.trim().is_empty() {
        return Ok(QuickFile::default());
    }
    toml::from_str(s).map_err(|e| anyhow::anyhow!("快捷格式文件解析失败：{e}"))
}

/// YAML 形态：备份包条目 `userdata/quick_format.yaml`（备份包格式不变）。
pub(crate) fn parse_quick_yaml(s: &str) -> anyhow::Result<QuickFile> {
    if s.trim().is_empty() {
        return Ok(QuickFile::default());
    }
    serde_yaml::from_str(s).map_err(|e| anyhow::anyhow!("快捷格式（YAML）解析失败：{e}"))
}

pub(crate) fn render_quick(qf: &QuickFile) -> anyhow::Result<String> {
    Ok(format!(
        "{}\n{QUICK_HELP}{}",
        wind_userdata::text::banner::QUICK_FORMAT,
        toml::to_string(qf)?
    ))
}

impl Store {
    /// 快捷格式调整的真源：`<data>/quick_format.toml`（P5 起；P4a 是同目录的 `.yaml`）。
    pub fn quick_format_path(&self) -> std::path::PathBuf {
        self.user_text_dir().join("quick_format.toml")
    }

    pub(crate) fn quick_doc(&self) -> &wind_userdata::text::doc::TextDoc {
        self.quick_doc
            .get_or_init(|| wind_userdata::text::doc::TextDoc::open(self.quick_format_path()))
    }

    /// 当前有效的调整：文件手改坏了按最后一次好数据（§2.5）。
    fn quick_file(&self) -> QuickFile {
        self.quick_good.get(self.quick_doc(), &parse_quick)
    }

    /// 读改写整份快捷格式调整；全表为空则删文件。文件解析不了 ⇒ Err、什么都不写（M-1）。
    fn modify_quick_file(&self, f: &mut dyn FnMut(&mut QuickFile)) -> anyhow::Result<()> {
        let doc = self.quick_doc();
        self.check_not_editing(doc.path())?;
        doc.modify(&mut |cur| {
            let mut qf = cur
                .map(parse_quick)
                .transpose()
                .map_err(|e| crate::last_good::broken_file_error(doc, e))?
                .unwrap_or_default();
            f(&mut qf);
            qf.kinds.retain(|_, r| !r.is_empty());
            if qf.kinds.is_empty() {
                return Ok(None);
            }
            render_quick(&qf).map(Some)
        })?;
        // 刚写成功的内容就是最新的好数据：之后文件被改坏也能按它服务
        self.quick_file();
        Ok(())
    }

    /// 一批对单条格式的编辑（`(kind, id)`，可跨类别）：读改写整份文件一次。`f` 对任一条报错
    /// ⇒ 整批不写。
    pub fn edit_quick_formats(
        &self,
        items: &[(String, String)],
        f: &mut dyn FnMut(&mut QuickFormatRecord, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let doc = self.quick_doc();
        self.check_not_editing(doc.path())?;
        doc.modify(&mut |cur| {
            let mut qf = cur
                .map(parse_quick)
                .transpose()
                .map_err(|e| crate::last_good::broken_file_error(doc, e))?
                .unwrap_or_default();
            for (kind, id) in items {
                f(qf.kinds.entry(kind.clone()).or_default(), id)?;
            }
            qf.kinds.retain(|_, r| !r.is_empty());
            if qf.kinds.is_empty() {
                return Ok(None);
            }
            render_quick(&qf).map(Some)
        })?;
        self.quick_file();
        Ok(())
    }

    /// 一批 [`Self::delete_quick_format`]：任一条不是用户条目 ⇒ 整批不写、报出它的 id。
    pub fn delete_quick_formats(&self, items: &[(String, String)]) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        let items: Vec<(String, String)> =
            items.iter().filter(|k| seen.insert(*k)).cloned().collect();
        self.edit_quick_formats(&items, &mut |rec, id| {
            if !rec.remove_entry(id) {
                anyhow::bail!("{id} 不是自定义条目，出厂条目只能停用");
            }
            Ok(())
        })
    }

    /// 读改写一类格式的调整；改完为空则删除该类。
    fn modify_quick_format(
        &self,
        kind: &str,
        f: impl FnOnce(&mut QuickFormatRecord),
    ) -> anyhow::Result<()> {
        let mut f = Some(f);
        self.modify_quick_file(&mut |qf| {
            if let Some(f) = f.take() {
                f(qf.kinds.entry(kind.to_string()).or_default());
            }
        })
    }

    /// 把某条格式移到组内下标 `position`。
    pub fn move_quick_format(&self, kind: &str, id: &str, position: usize) -> anyhow::Result<()> {
        self.modify_quick_format(kind, |rec| rec.apply_move(id, position))
    }

    /// 启用 / 停用某条格式。
    pub fn set_quick_format_enabled(
        &self,
        kind: &str,
        id: &str,
        enabled: bool,
    ) -> anyhow::Result<()> {
        self.modify_quick_format(kind, |rec| rec.set_enabled(id, enabled))
    }

    /// 恢复单条格式的默认（位置 + 启用状态）。
    pub fn reset_quick_format_entry(&self, kind: &str, id: &str) -> anyhow::Result<()> {
        self.modify_quick_format(kind, |rec| rec.apply_reset(id))
    }

    /// 追加一条用户自定义格式。
    pub fn add_quick_format(&self, kind: &str, id: &str, text: &str) -> anyhow::Result<()> {
        self.modify_quick_format(kind, |rec| rec.add_entry(id, text))
    }

    /// 改写用户条目的模板。返回 `false` = 不存在这条用户条目（出厂条目也走这里返回 false）。
    pub fn set_quick_format_text(&self, kind: &str, id: &str, text: &str) -> anyhow::Result<bool> {
        let mut hit = false;
        self.modify_quick_format(kind, |rec| hit = rec.set_text(id, text))?;
        Ok(hit)
    }

    /// 删除用户条目（连带它的调序/停用规则）。返回 `false` = 不存在这条用户条目。
    pub fn delete_quick_format(&self, kind: &str, id: &str) -> anyhow::Result<bool> {
        let mut hit = false;
        self.modify_quick_format(kind, |rec| hit = rec.remove_entry(id))?;
        Ok(hit)
    }

    /// 恢复某一类的默认顺序与显示：清 `moved`/`disabled`，保留用户自定义条目。
    /// 没有设置页时，这是停用之后的唯一出口（被停用的格式右键点不到）。
    ///
    /// 别改成整键删除：那会让「恢复默认」丢掉用户手写的模板。连用户条目一起清走
    /// [`Self::clear_quick_format_kind`]。
    pub fn reset_quick_format_kind(&self, kind: &str) -> anyhow::Result<()> {
        self.modify_quick_format(kind, |rec| {
            rec.moved.clear();
            rec.disabled.clear();
        })
    }

    /// 清空某一类的全部用户数据，含自定义条目。只给「导入并替换」用。
    /// 面向用户的「恢复默认」一律走 [`Self::reset_quick_format_kind`]。
    pub fn clear_quick_format_kind(&self, kind: &str) -> anyhow::Result<()> {
        self.modify_quick_file(&mut |qf| {
            qf.kinds.remove(kind);
        })
    }

    /// 导出整份快捷格式调整（备份用；没有调整 ⇒ 空串）。
    pub fn export_quick_format_yaml(&self) -> anyhow::Result<String> {
        let qf = self.quick_file();
        if qf.kinds.is_empty() {
            return Ok(String::new());
        }
        Ok(serde_yaml::to_string(&qf)?)
    }

    /// 从备份导入：`replace` ⇒ 整份替换；否则只补本地没有的类（本地已调整的类以本地为准）。
    pub fn import_quick_format_yaml(&self, text: &str, replace: bool) -> anyhow::Result<usize> {
        let qf = parse_quick_yaml(text)?;
        let rows: Vec<_> = qf.kinds.into_iter().collect();
        let n = rows.len();
        self.import_quick_format_rows(rows, replace)?;
        Ok(n)
    }

    /// 批量写入若干类的调整，一次提交（迁移 / 备份还原用）。
    pub(crate) fn import_quick_format_rows(
        &self,
        rows: Vec<(String, QuickFormatRecord)>,
        replace: bool,
    ) -> anyhow::Result<()> {
        self.check_not_editing(self.quick_doc().path())?;
        if replace {
            // 整份还原是用户显式的整体动作：坏文件先留存，再写
            crate::last_good::preserve_if_unparsable(self.quick_doc(), &parse_quick)?;
        }
        let mut rows = Some(rows);
        self.modify_quick_file(&mut |qf| {
            if replace {
                qf.kinds.clear();
            }
            for (k, r) in rows.take().unwrap_or_default() {
                qf.kinds.entry(k).or_insert(r);
            }
        })
    }

    /// 取某类的调整（无则 `None`）。
    pub fn get_quick_format(&self, kind: &str) -> anyhow::Result<Option<QuickFormatRecord>> {
        Ok(self.quick_file().kinds.remove(kind))
    }

    /// 列出全部类别的调整（设置页与启动装载用），按 kind 升序。
    pub fn list_quick_format(&self) -> anyhow::Result<Vec<(String, QuickFormatRecord)>> {
        Ok(self.quick_file().kinds.into_iter().collect())
    }
}

#[cfg(test)]
#[path = "quick_format_tests.rs"]
mod tests;
