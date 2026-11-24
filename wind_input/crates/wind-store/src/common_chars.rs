//! 常用字表的用户增删（候选右键「设为生僻字 / 设为常用字」+ 词库管理界面）。
//!
//! ## 只存用户碰过的字，不存整表
//!
//! ```text
//! 出厂 data/schemas/common_chars.txt（8104 字）
//!   ↓ 用户目录整份覆盖（Config::resolve_schema_resource，高级用户手写，可选）
//! 基表
//!   ↓ 本表的稀疏覆盖（普通用户点右键）
//! 最终常用字集
//! ```
//!
//! 与 [`crate::quick_format`] 同一条纪律：GUI 调整绝不回写 `common_chars.txt`，否则会抢走
//! 高级用户手写文件的所有权，也让普通用户点两下右键就永久脱离出厂表的升版。
//! 稀疏存储还让出厂新增的字自动跟随基表判定，无需「新字默认算哪边」的规则。
//!
//! ## 键不带方案：这是全局字级属性
//!
//! | | 作用域 | 键 |
//! |---|---|---|
//! | shadow（候选调整） | 一次输入：这个方案、这个码 | `(方案, 输入码)` |
//! | 本模块 | 全局：这个字在所有方案、所有码下 | `字` |
//!
//! 「某个字常不常用」是语言学属性，与方案无关。右键菜单文案要讲明这个差异
//! （「设为生僻字（全局）」对「隐藏此候选」）。

use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 一条用户覆盖。
///
/// `common` 两个方向都要存：用户既会把常用字降级，也会把生僻字升级（专业用字、人名用字）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommonCharOverride {
    /// 被覆盖的**字素簇**（用户眼里的一个字符）。
    ///
    /// 类型是 `String` 而不是 `char`：`👨‍👩‍👧` 是 5 个码位、`🇨🇳` 是 2 个，屏幕上都只有一个
    /// 图形。字段名保持 `ch` 是为了**向后兼容**——备份包里已有的 `{"ch":"槮",...}` 照旧
    /// 解析得出来（JSON 字符串 → String 恒成立），换名字才会让老备份读不回来。
    pub ch: String,
    /// `true` = 用户强制判为常用；`false` = 用户强制判为生僻。
    pub common: bool,
}

/// 表 value 的序列化形态。
///
/// 只有一个字段却仍用结构体而非裸 bool：将来要加「来源」（右键点的 / 导入的）或
/// 「时间」时，裸 `true` 无处扩展，而多一层对象是向后兼容的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// 旧 redb 表的 value 形态（P4a 迁移解码 / 测试造旧库用）。
pub(crate) struct CommonCharRecord {
    pub(crate) common: bool,
}

/// 文件形态：`common: [字…]`、`uncommon: [字…]`（P4a 起真源 `<text_dir>/common_chars.yaml`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CommonFile {
    #[serde(default)]
    common: Vec<String>,
    #[serde(default)]
    uncommon: Vec<String>,
}

/// 说明行之后的字段提示。
const COMMON_HELP: &str = "# common = 强制常用，uncommon = 强制生僻\n";

fn parse_common(s: &str) -> anyhow::Result<BTreeMap<String, bool>> {
    if s.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let f: CommonFile =
        serde_yaml::from_str(s).map_err(|e| anyhow::anyhow!("常用字覆盖文件解析失败：{e}"))?;
    let mut m = BTreeMap::new();
    for c in f.uncommon {
        if !c.is_empty() {
            m.insert(c, false);
        }
    }
    for c in f.common {
        if !c.is_empty() {
            m.insert(c, true);
        }
    }
    Ok(m)
}

pub(crate) fn render_common(m: &BTreeMap<String, bool>) -> String {
    let f = CommonFile {
        common: m
            .iter()
            .filter(|(_, v)| **v)
            .map(|(k, _)| k.clone())
            .collect(),
        uncommon: m
            .iter()
            .filter(|(_, v)| !**v)
            .map(|(k, _)| k.clone())
            .collect(),
    };
    format!(
        "{}\n{COMMON_HELP}{}",
        wind_userdata::text::banner::COMMON_CHARS,
        serde_yaml::to_string(&f).unwrap_or_default()
    )
}

impl Store {
    pub fn common_chars_path(&self) -> std::path::PathBuf {
        self.user_text_dir().join("common_chars.yaml")
    }

    pub(crate) fn common_doc(&self) -> &wind_userdata::text::doc::TextDoc {
        self.common_doc
            .get_or_init(|| wind_userdata::text::doc::TextDoc::open(self.common_chars_path()))
    }

    /// 当前有效的覆盖：文件手改坏了按最后一次好数据（§2.5）。
    fn common_map(&self) -> BTreeMap<String, bool> {
        self.common_good.get(self.common_doc(), &parse_common)
    }

    /// 读改写全部覆盖，一次提交；全空则删文件。文件解析不了 ⇒ Err、什么都不写（M-1）。
    fn modify_common(&self, f: &mut dyn FnMut(&mut BTreeMap<String, bool>)) -> anyhow::Result<()> {
        let doc = self.common_doc();
        self.check_not_editing(doc.path())?;
        doc.modify(&mut |cur| {
            let mut m = cur
                .map(parse_common)
                .transpose()
                .map_err(|e| crate::last_good::broken_file_error(doc, e))?
                .unwrap_or_default();
            f(&mut m);
            Ok((!m.is_empty()).then(|| render_common(&m)))
        })?;
        // 刚写成功的内容就是最新的好数据：之后文件被改坏也能按它服务
        self.common_map();
        Ok(())
    }

    /// 写一条覆盖（同字重复写即改写方向）。
    pub fn set_common_char_override(&self, key: &str, common: bool) -> anyhow::Result<()> {
        self.modify_common(&mut |m| {
            m.insert(key.to_string(), common);
        })
    }

    /// **一次提交**写一批覆盖：`Some(common)` 写入、`None` 删除。返回真正写入的条数。
    ///
    /// 整类批量（按 Unicode 块设常用/生僻）用它：调用方每写一条还要回灌一次运行时镜像，
    /// 那把锁正是候选过滤每次按键都要拿的，逐条写会把它拿上几百次。
    pub fn apply_common_char_overrides(
        &self,
        items: &[(String, Option<bool>)],
    ) -> anyhow::Result<usize> {
        let mut written = 0usize;
        self.modify_common(&mut |m| {
            written = 0;
            for (key, dir) in items {
                match dir {
                    Some(common) => {
                        m.insert(key.clone(), *common);
                        written += 1;
                    }
                    None => {
                        m.remove(key);
                    }
                }
            }
        })?;
        Ok(written)
    }

    /// 撤销某字的覆盖，回到出厂判定。`false` = 本就没有覆盖。
    ///
    /// 与「设为常用字」**不是**一回事：出厂判生僻的字，撤销覆盖后仍是生僻。
    pub fn remove_common_char_override(&self, key: &str) -> anyhow::Result<bool> {
        let mut existed = false;
        self.modify_common(&mut |m| existed = m.remove(key).is_some())?;
        Ok(existed)
    }

    /// 某字是否有覆盖，以及方向。`None` = 未覆盖（由基表判定）。
    pub fn get_common_char_override(&self, key: &str) -> anyhow::Result<Option<bool>> {
        Ok(self.common_map().get(key).copied())
    }

    /// 全部覆盖，按字的 UTF-8 字节序升序（对单字符即码位序）。
    pub fn list_common_char_overrides(&self) -> anyhow::Result<Vec<CommonCharOverride>> {
        Ok(self
            .common_map()
            .into_iter()
            .map(|(ch, common)| CommonCharOverride { ch, common })
            .collect())
    }

    /// 清空全部覆盖（词库管理界面的「全部恢复出厂」）。返回清掉的条数。
    pub fn clear_common_char_overrides(&self) -> anyhow::Result<usize> {
        self.check_not_editing(self.common_doc().path())?;
        let n = self.common_map().len();
        crate::last_good::preserve_if_unparsable(self.common_doc(), &parse_common)?;
        self.common_doc().replace(None)?;
        Ok(n)
    }

    /// 导出为 JSONL（备份 / 迁移）。每行 `{"ch":"槮","common":true}`。
    pub fn export_common_chars_jsonl(&self) -> anyhow::Result<String> {
        let mut s = String::new();
        for o in self.list_common_char_overrides()? {
            s.push_str(&serde_json::to_string(&o)?);
            s.push('\n');
        }
        Ok(s)
    }

    /// 从 JSONL 导入，返回写入条数。坏行跳过（与 [`Self::list_common_char_overrides`]
    /// 同一条纪律：一行坏数据不该让整次还原失败）。
    pub fn import_common_chars_jsonl(&self, text: &str) -> anyhow::Result<usize> {
        let normalized = wind_utils::text::normalize_input(text);
        let text = normalized.as_ref();
        let mut n = 0usize;
        let mut items = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(o) = serde_json::from_str::<CommonCharOverride>(line) else {
                continue;
            };
            items.push((o.ch, Some(o.common)));
            n += 1;
        }
        self.apply_common_char_overrides(&items)?;
        Ok(n)
    }
}

#[cfg(test)]
#[path = "common_chars_tests.rs"]
mod tests;
