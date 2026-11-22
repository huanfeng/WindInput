//! 快捷输入格式表的**用户调整**（右键菜单调序 / 停用）。
//!
//! 与 [`crate::shadow`] 是**两套**东西，别混：
//!
//! | | 作用域 | 键 |
//! |---|---|---|
//! | shadow（候选调整） | 一次输入（这个码下的这些字） | `(方案, 输入码)` |
//! | 本模块（格式调整） | 一类输入（所有日期 / 所有数字） | `格式类别` |
//!
//! 快捷输入的「输入码」是 `2026.6.19` 这种具体值。若把格式调整存进 shadow，用户右键
//! 把农历排到最前，存下的键就是那一天；次日打 `2026.6.20` 键不匹配，调整凭空消失。
//! 症状是「当时有效、隔天失效」，间歇性发作，极难排查。故另起一张表，键只到类别。
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
//! 两类用户各写各的落点，互不干扰。**GUI 调整绝不回写 `system.quick.toml`**——那会
//! 抢走高级用户手写文件的所有权（重写丢注释），更糟的是让普通用户点两下右键就
//! 永久脱离出厂更新，而他毫不知情（整份覆盖的代价必须是知情选择）。
//!
//! ## 为什么存稀疏的 (id, position) 而不是完整顺序
//!
//! 完整 id 序列存下来后，将来出厂新增一条格式，它不在序列里，就得再定一条
//! 「新增格式排哪」的规则——怎么定都会让人意外。稀疏记录下**没被碰过的条目不出现在
//! 存储里**，于是新增格式自然落在它的基表位置。
//!
//! 规则的「应用」由调用方完成（`wind_quick_input::FormatAdjust`），本模块只管存取——
//! 与 shadow 同一条纪律：store 不依赖业务类型。

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
/// 没有 `position` 字段：顺序由 `Vec` 的插入序表达（新条目落本类末尾），
/// 之后要挪位置走 [`QuickFormatRecord::moved`] 那套现成规则——出厂条目与用户条目
/// 共用同一个调序机制，不给用户条目另开一条「它自己的顺序」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFormatAdded {
    pub id: String,
    pub text: String,
}

/// 某一类（`date` / `month_day` / `year_month` / `number` / `calc`）的用户数据。
///
/// 三个字段分两种性质，**清理时必须区别对待**：`moved`/`disabled` 是对基表的**调整**
/// （清掉即回到出厂），`added` 是用户**自己的内容**（清掉就没了，不可逆）。
/// 见 [`Store::reset_quick_format_kind`] 与 [`Store::clear_quick_format_kind`]。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFormatRecord {
    /// 被移动过的格式。**LIFO，index 0 = 最新**，应用时逆序遍历（最新的最后应用、
    /// 优先级最高），与 [`crate::shadow::ShadowRecord::pinned`] 同构。
    #[serde(default)]
    pub moved: Vec<QuickFormatMove>,
    /// 被停用的格式 id。
    ///
    /// 叫 disabled 而不是 hidden 是刻意的：设置页的既有能力是 `set_enabled`（启用开关），
    /// 被停用的行**在管理界面里仍然可见**、能再打开。若语义定成「隐藏」，那一行就该从
    /// 列表消失，用户便再也点不到它——右键菜单里它本来就已经不出现了。
    #[serde(default)]
    pub disabled: Vec<String>,
    /// 用户自己加的条目（不在 `system.quick.toml` 里）。
    ///
    /// 落在这张表而不是回写格式表文件：回写会抢走高级用户手写文件的所有权，
    /// 也会让普通用户永久脱离出厂更新（见模块头）。放这里则两类用户各写各的落点。
    ///
    /// ⚠️ 与 `moved`/`disabled` 不同，它**不是**「对出厂的调整」而是用户的内容本身，
    /// 所以「恢复默认」不得删它。
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
    /// 出厂条目走不到这里：它们不在 `added` 里，故返回 `false`——「不许编辑出厂模板」
    /// 这条约束由数据结构本身兜住，不靠调用方自觉。
    pub fn set_text(&mut self, id: &str, text: &str) -> bool {
        match self.added.iter_mut().find(|a| a.id == id) {
            Some(a) => {
                a.text = text.to_string();
                true
            }
            None => false,
        }
    }

    /// 删除用户条目，**连带清掉它的调序与停用规则**；`false` = 没有这条。
    ///
    /// 不清规则的话会留下指向已删条目的孤儿：本身无害（应用时找不到目标即跳过），
    /// 但用户重新加一条同 id 的条目时，那条旧规则会突然复活并把它挪到意外的位置。
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

    /// 一批对单条格式的编辑（`(kind, id)`，可跨类别）：读改写整份文件**一次**。`f` 对任一条报错
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

    /// 恢复某一类的默认顺序与显示：清 `moved`/`disabled`，**保留用户自定义条目**。
    ///
    /// 这也是**停用之后的唯一出口**（在没有设置页时）：被停用的格式不出现在候选里，
    /// 右键点不到，只能整类重置。
    ///
    /// ⚠️ 曾经是 `t.remove(kind)`（整键删除）。`added` 落进同一条记录后，那个写法会让
    /// 用户点一下右键的「恢复默认」就永久丢掉手写的模板——不可逆、无预警，而且症状会被
    /// 归因成「我的自定义条目自己消失了」。要连用户条目一起清的场合走
    /// [`Self::clear_quick_format_kind`]，那是导入 replace 的语义，不是「恢复默认」的。
    pub fn reset_quick_format_kind(&self, kind: &str) -> anyhow::Result<()> {
        self.modify_quick_format(kind, |rec| {
            rec.moved.clear();
            rec.disabled.clear();
        })
    }

    /// 清空某一类的**全部**用户数据，含自定义条目。
    ///
    /// 只给「导入并替换」用：那个动作的语义是「用文件里的状态覆盖我现在的状态」，
    /// 留着旧的自定义条目会得到「文件里的 + 我原有的」，那不叫替换。
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
mod tests {
    use super::*;

    fn store() -> (Store, std::path::PathBuf) {
        // 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞
        let p = std::env::temp_dir().join(format!(
            "wind_quick_format_test_{}_{:?}.redb",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        (Store::open(&p).unwrap(), p)
    }

    /// P5：用户层是 TOML（对齐出厂 system.quick.toml）；手改坏了按最后一次好数据服务、拒写、
    /// 不重写文件（M-1）；修好后恢复。
    #[test]
    fn quick_format_is_toml_and_survives_broken_edit() {
        let (s, p) = store();
        s.set_quick_format_enabled("date", "date.lunar", false)
            .unwrap();
        let f = s.quick_format_path();
        assert!(f.ends_with("quick_format.toml"));
        let txt = std::fs::read_to_string(&f).unwrap();
        assert!(toml::from_str::<toml::Value>(&txt).is_ok(), "{txt}");
        std::fs::write(&f, "kinds = [[[").unwrap();
        s.quick_doc().sync();
        assert_eq!(
            s.get_quick_format("date").unwrap().unwrap().disabled,
            vec!["date.lunar".to_string()],
            "按最后一次好数据服务"
        );
        assert!(matches!(
            wind_userdata::issues::list_under(&f)[0].kind,
            wind_userdata::issues::IssueKind::Isolated
        ));
        assert!(s.set_quick_format_enabled("date", "x", false).is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "kinds = [[[");
        std::fs::write(&f, txt).unwrap();
        s.quick_doc().sync();
        s.set_quick_format_enabled("date", "x", false).unwrap();
        assert!(wind_userdata::issues::list_under(&f).is_empty(), "修好就清");
        let _ = std::fs::remove_file(&p);
    }

    /// 审查 M-2：整份还原遇到改坏的文件 ⇒ 坏文件另存留存，还原照常完成。
    #[test]
    fn replace_import_over_broken_file_keeps_it_aside() {
        let (s, p) = store();
        s.move_quick_format("date", "date.lunar", 0).unwrap();
        let y = s.export_quick_format_yaml().unwrap();
        let f = s.quick_format_path();
        std::fs::write(&f, "kinds = [[[").unwrap();
        s.quick_doc().sync();
        s.import_quick_format_yaml(&y, true).unwrap();
        assert_eq!(
            s.get_quick_format("date").unwrap().unwrap().moved[0].id,
            "date.lunar"
        );
        let kept = std::fs::read_dir(s.user_text_dir().join(".backup"))
            .unwrap()
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("quick_format.toml.corrupt-")
            });
        assert!(kept);
        wind_userdata::issues::clear(&f);
        let _ = std::fs::remove_file(&p);
    }

    /// 备份包里的快捷格式仍是 YAML（老备份照样能还原，新备份老版本也能读）。
    #[test]
    fn backup_export_stays_yaml_and_round_trips() {
        let (s, p) = store();
        s.move_quick_format("date", "date.lunar", 0).unwrap();
        let y = s.export_quick_format_yaml().unwrap();
        assert!(serde_yaml::from_str::<serde_yaml::Value>(&y).is_ok(), "{y}");
        assert!(y.contains("date.lunar"));
        s.import_quick_format_yaml("", true).unwrap();
        assert!(s.get_quick_format("date").unwrap().is_none());
        s.import_quick_format_yaml(&y, true).unwrap();
        assert_eq!(
            s.get_quick_format("date").unwrap().unwrap().moved[0].id,
            "date.lunar"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn move_then_read_back() {
        let (s, p) = store();
        s.move_quick_format("date", "date.lunar", 0).unwrap();
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert_eq!(rec.moved.len(), 1);
        assert_eq!(rec.moved[0].id, "date.lunar");
        assert_eq!(rec.moved[0].position, 0);
        let _ = std::fs::remove_file(&p);
    }

    /// ★ LIFO：同一条格式再次移动时顶替旧规则，不是叠加两条。
    #[test]
    fn moving_same_entry_twice_replaces_not_appends() {
        let (s, p) = store();
        s.move_quick_format("date", "date.lunar", 3).unwrap();
        s.move_quick_format("date", "date.lunar", 0).unwrap();
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert_eq!(rec.moved.len(), 1, "同 id 只留最新一条");
        assert_eq!(rec.moved[0].position, 0);
        let _ = std::fs::remove_file(&p);
    }

    /// 最新的规则排在 index 0（应用时逆序遍历，故它最后生效、优先级最高）。
    #[test]
    fn newest_move_is_first() {
        let (s, p) = store();
        s.move_quick_format("date", "a", 1).unwrap();
        s.move_quick_format("date", "b", 2).unwrap();
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert_eq!(rec.moved[0].id, "b", "最新的在队首");
        assert_eq!(rec.moved[1].id, "a");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn disable_and_reenable() {
        let (s, p) = store();
        s.set_quick_format_enabled("date", "date.basic", false)
            .unwrap();
        assert_eq!(
            s.get_quick_format("date").unwrap().unwrap().disabled,
            vec!["date.basic".to_string()]
        );
        s.set_quick_format_enabled("date", "date.basic", true)
            .unwrap();
        // 记录变空 → 整条键被删除，而不是留一条空记录
        assert!(s.get_quick_format("date").unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }

    /// 重复停用同一条不产生重复项。
    #[test]
    fn disabling_twice_is_idempotent() {
        let (s, p) = store();
        s.set_quick_format_enabled("date", "x", false).unwrap();
        s.set_quick_format_enabled("date", "x", false).unwrap();
        assert_eq!(
            s.get_quick_format("date").unwrap().unwrap().disabled.len(),
            1
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reset_entry_clears_both_move_and_disable() {
        let (s, p) = store();
        s.move_quick_format("date", "x", 0).unwrap();
        s.set_quick_format_enabled("date", "x", false).unwrap();
        s.move_quick_format("date", "y", 1).unwrap();
        s.reset_quick_format_entry("date", "x").unwrap();
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert!(!rec.has_rule("x"), "x 的调整应清干净");
        assert!(rec.has_rule("y"), "不该连累其它条目");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reset_kind_clears_everything() {
        let (s, p) = store();
        s.move_quick_format("date", "x", 0).unwrap();
        s.set_quick_format_enabled("date", "y", false).unwrap();
        s.move_quick_format("number", "z", 0).unwrap();
        s.reset_quick_format_kind("date").unwrap();
        assert!(s.get_quick_format("date").unwrap().is_none());
        assert!(
            s.get_quick_format("number").unwrap().is_some(),
            "只清本类，别的类不动"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// ★ 键只到类别，不带方案/输入码——这正是本表与 shadow 的分界。
    #[test]
    fn kinds_are_independent_and_global() {
        let (s, p) = store();
        s.move_quick_format("date", "date.lunar", 0).unwrap();
        s.move_quick_format("number", "number.thousands", 0)
            .unwrap();
        let all = s.list_quick_format().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "date", "按 kind 升序");
        assert_eq!(all[1].0, "number");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_kind_reads_none() {
        let (s, p) = store();
        assert!(s.get_quick_format("date").unwrap().is_none());
        assert!(s.list_quick_format().unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn add_then_read_back() {
        let (s, p) = store();
        s.add_quick_format("date", "date.u1", "$Y/$M/$D").unwrap();
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert_eq!(rec.added.len(), 1);
        assert_eq!(rec.added[0].id, "date.u1");
        assert_eq!(rec.added[0].text, "$Y/$M/$D");
        assert!(rec.is_added("date.u1"));
        assert!(!rec.is_added("date.cn"), "出厂条目不属于 added");
        let _ = std::fs::remove_file(&p);
    }

    /// ★ 只有用户条目能改模板。出厂 id 走到这里必须返回 false——「不许编辑出厂模板」
    /// 由数据结构兜住（它们不在 `added` 里），不靠调用方自觉。
    #[test]
    fn set_text_refuses_factory_entries() {
        let (s, p) = store();
        s.add_quick_format("date", "date.u1", "$Y").unwrap();
        assert!(s.set_quick_format_text("date", "date.u1", "$M").unwrap());
        assert!(
            !s.set_quick_format_text("date", "date.cn", "篡改").unwrap(),
            "出厂条目不可改写"
        );
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert_eq!(rec.added[0].text, "$M");
        assert_eq!(rec.added.len(), 1, "失败的改写不该添出一条来");
        let _ = std::fs::remove_file(&p);
    }

    /// ★ 删条目必须连带清掉它的调序规则。留着孤儿规则本身无害（应用时找不到目标即跳过），
    /// 但用户重加一条同 id 的条目时，那条旧规则会突然复活、把它挪到意外的位置。
    #[test]
    fn delete_also_clears_its_rules() {
        let (s, p) = store();
        s.add_quick_format("date", "date.u1", "$Y").unwrap();
        s.move_quick_format("date", "date.u1", 0).unwrap();
        s.set_quick_format_enabled("date", "date.u1", false)
            .unwrap();
        s.move_quick_format("date", "date.cn", 2).unwrap();

        assert!(s.delete_quick_format("date", "date.u1").unwrap());
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert!(rec.added.is_empty());
        assert!(!rec.has_rule("date.u1"), "自己的规则要一起清掉");
        assert!(rec.has_rule("date.cn"), "别人的规则不受影响");

        assert!(
            !s.delete_quick_format("date", "date.u1").unwrap(),
            "删第二次报 false"
        );
        assert!(
            !s.delete_quick_format("date", "date.cn").unwrap(),
            "出厂条目删不掉"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// ★★ 「恢复默认」清调整、**留用户条目**。
    ///
    /// 反向对照：把实现换回 `t.remove(kind)` 这条就红——那正是它防的回归
    /// （用户点一下右键的「恢复默认」就永久丢掉手写模板，不可逆且无预警）。
    #[test]
    fn reset_kind_keeps_added_entries() {
        let (s, p) = store();
        s.add_quick_format("date", "date.u1", "$Y").unwrap();
        s.move_quick_format("date", "date.lunar", 0).unwrap();
        s.set_quick_format_enabled("date", "date.cn", false)
            .unwrap();

        s.reset_quick_format_kind("date").unwrap();
        let rec = s.get_quick_format("date").unwrap().unwrap();
        assert!(rec.moved.is_empty(), "调序已清");
        assert!(rec.disabled.is_empty(), "停用已清");
        assert_eq!(rec.added.len(), 1, "★ 用户条目必须留下");
        assert_eq!(rec.added[0].text, "$Y");
        let _ = std::fs::remove_file(&p);
    }

    /// 没有用户条目时，「恢复默认」应把整条键删掉（不留空记录）——与改造前同行为。
    #[test]
    fn reset_kind_without_added_removes_the_key() {
        let (s, p) = store();
        s.move_quick_format("date", "date.lunar", 0).unwrap();
        s.reset_quick_format_kind("date").unwrap();
        assert!(s.get_quick_format("date").unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }

    /// 「导入并替换」用的清空**含**用户条目——replace 的语义是「用文件覆盖现状」，
    /// 留着旧条目会得到「文件里的 + 我原有的」。与上一个测试刻意相反。
    #[test]
    fn clear_kind_removes_added_too() {
        let (s, p) = store();
        s.add_quick_format("date", "date.u1", "$Y").unwrap();
        s.add_quick_format("number", "number.u1", "$N").unwrap();
        s.clear_quick_format_kind("date").unwrap();
        assert!(s.get_quick_format("date").unwrap().is_none());
        assert!(s.get_quick_format("number").unwrap().is_some(), "只清本类");
        let _ = std::fs::remove_file(&p);
    }

    /// 只有用户条目、没有任何调整的记录**不是空的**，不能被 `modify` 的收尾逻辑删掉。
    #[test]
    fn record_with_only_added_is_not_empty() {
        let mut rec = QuickFormatRecord::default();
        assert!(rec.is_empty());
        rec.add_entry("date.u1", "$Y");
        assert!(!rec.is_empty(), "★ 否则加完一条就被当空记录删掉");
    }

    /// 快捷格式调整落用户目录文件；重开读回。
    #[test]
    fn quick_format_lives_in_user_dir() {
        let p = std::env::temp_dir().join(format!("wind_qf_file_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let s = Store::open(&p).unwrap();
            s.add_quick_format("date", "my", "yyyy").unwrap();
            s.set_quick_format_enabled("date", "iso", false).unwrap();
            let f = std::fs::read_to_string(s.quick_format_path()).unwrap();
            assert!(f.contains("my") && f.contains("iso"), "{f}");
        }
        let s = Store::open(&p).unwrap();
        let r = s.get_quick_format("date").unwrap().unwrap();
        assert_eq!(r.added[0].id, "my");
        assert_eq!(r.disabled, vec!["iso".to_string()]);
        s.clear_quick_format_kind("date").unwrap();
        assert!(s.get_quick_format("date").unwrap().is_none());
    }
}
