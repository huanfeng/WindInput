//! 字符类的**用户层**（设置页「字符集分类」+ 外部编辑回读 + 备份还原）。
//!
//! ## 库是唯一真相源，文件只是交换格式
//!
//! ```text
//! 出厂 data/charsets/*.yaml（只读，随安装包）
//!   ↓ 定制 data_custom/charsets/*.yaml（只读）
//!   ↓ 本表（用户层：自建类、对出厂类的字段覆盖与成员增删）
//! registry
//!
//! 本表 ⇄ yaml 文本 ⇄ 编辑态临时文件 / 备份包 charsets/<key>.yaml
//! ```
//!
//! 用户层曾是 `{user_config}/charsets/` 目录（2026-09-04，未发布即改）。改成表的理由
//! 只有一条：**程序和人不能写同一个文件**。设置页改一个开关要重写文件，用户此时若在
//! 编辑器里开着那份文件，谁后保存谁赢，另一方的改动静默丢失。落进库之后，程序只写
//! 库，人只改导出的副本，回读是一次显式动作——冲突只剩「回读那一刻」，可以提示。
//!
//! ## value 就是 yaml 文本本身
//!
//! 本 crate **不认识** `CharsetDoc`（`wind-config` 依赖方向反着），也不该认识：格式
//! 只在 `wind_config::charset_def` 里一份，解析与写出都在那边。这里存的是那份文本的
//! 字节，备份时原样进包、编辑时原样导出，一个格式三处用，不会有「库里的形态」与
//! 「文件里的形态」两套东西要对齐。
//!
//! 键是类的 `key`（也是 `exclude_blocks` / `include_blocks` 里写的那个名字），不带方案
//! ——字符类与 [`crate::common_chars`] 同为全局属性，理由见那边的模块文档。

use crate::store::{CHARSET_USER, Store};
use redb::{ReadableTable, ReadableTableMetadata};

impl Store {
    /// 读一个类的用户层文本；`None` = 用户没动过这个类。
    pub fn get_charset_doc(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(CHARSET_USER)?;
            Ok(t.get(key)?
                .map(|g| String::from_utf8_lossy(g.value()).into_owned()))
        })
    }

    /// 整份写入（同 key 覆盖）。文本合法性由调用方负责——本层不解析。
    pub fn set_charset_doc(&self, key: &str, text: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(CHARSET_USER)?;
                t.insert(key, text.as_bytes())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 删掉一个类的用户层（自建类 = 整个类消失；出厂类 = 回到出厂）。
    /// `false` = 本就没有。
    pub fn remove_charset_doc(&self, key: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let existed;
            {
                let mut t = txn.open_table(CHARSET_USER)?;
                existed = t.remove(key)?.is_some();
            }
            txn.commit()?;
            Ok(existed)
        })
    }

    /// 全部用户层，按 key 升序（redb 键序）。量级是几十条，全量返回。
    pub fn list_charset_docs(&self) -> anyhow::Result<Vec<(String, String)>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(CHARSET_USER)?;
            let mut out = Vec::new();
            for item in t.iter()? {
                let (k, v) = item?;
                let key = k.value().to_string();
                // 空键只可能来自手工改库，跳过而非报错——一条坏记录不该让整层失效。
                if key.is_empty() {
                    continue;
                }
                out.push((key, String::from_utf8_lossy(v.value()).into_owned()));
            }
            Ok(out)
        })
    }

    /// 清空用户层（备份「替换」模式用）。返回删掉的条数。
    pub fn clear_charset_docs(&self) -> anyhow::Result<usize> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(CHARSET_USER)?;
                n = t.len()? as usize;
                t.retain(|_, _| false)?;
            }
            txn.commit()?;
            Ok(n)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞。
    fn store(tag: &str) -> Store {
        let p = std::env::temp_dir().join(format!(
            "wind_charsets_test_{}_{}_{:?}.redb",
            std::process::id(),
            tag,
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        Store::open(&p).unwrap()
    }

    #[test]
    fn set_get_remove_roundtrip() {
        let s = store("a");
        assert_eq!(s.get_charset_doc("emoji").unwrap(), None);

        s.set_charset_doc("emoji", "key: emoji\ndefault: rare\n")
            .unwrap();
        assert_eq!(
            s.get_charset_doc("emoji").unwrap().as_deref(),
            Some("key: emoji\ndefault: rare\n")
        );

        // 同 key 覆盖，不是追加。
        s.set_charset_doc("emoji", "key: emoji\n").unwrap();
        assert_eq!(
            s.get_charset_doc("emoji").unwrap().as_deref(),
            Some("key: emoji\n")
        );

        assert!(s.remove_charset_doc("emoji").unwrap());
        assert!(
            !s.remove_charset_doc("emoji").unwrap(),
            "二次删除返回 false"
        );
        assert_eq!(s.get_charset_doc("emoji").unwrap(), None);
    }

    /// 文本原样进出——多行、中文、`...` 分隔符、行首 `-`，一个字节都不能动。
    /// 备份和编辑态都靠它：这里若做任何规范化，回读时 diff 就对不上出厂。
    #[test]
    fn text_is_stored_verbatim() {
        let s = store("verbatim");
        let text = "---\nkey: 我的符号\nname: 我的符号\nranges: [U+2600-U+26FF]\n...\n★\n-☯\n\n";
        s.set_charset_doc("我的符号", text).unwrap();
        assert_eq!(
            s.get_charset_doc("我的符号").unwrap().as_deref(),
            Some(text)
        );
    }

    #[test]
    fn list_is_sorted_by_key_and_clear_empties_it() {
        let s = store("list");
        s.set_charset_doc("zeta", "key: zeta\n").unwrap();
        s.set_charset_doc("alpha", "key: alpha\n").unwrap();
        s.set_charset_doc("emoji", "key: emoji\n").unwrap();

        let keys: Vec<String> = s
            .list_charset_docs()
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["alpha", "emoji", "zeta"]);

        assert_eq!(s.clear_charset_docs().unwrap(), 3);
        assert!(s.list_charset_docs().unwrap().is_empty());
        assert_eq!(s.clear_charset_docs().unwrap(), 0, "清空空表不报错");
    }
}
