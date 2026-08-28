//! 常用字表的**用户增删**（候选右键「设为生僻字 / 设为常用字」+ 词库管理界面）。
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
//! 与 [`crate::quick_format`] 同一条纪律：**GUI 调整绝不回写 `common_chars.txt`**。
//! 回写会抢走高级用户手写文件的所有权，更糟的是让普通用户点两下右键就永久脱离出厂
//! 更新，而他毫不知情——通用规范汉字表将来升版（改字、补字），整份覆盖过的用户拿不到。
//!
//! **稀疏还顺带解决了「出厂新增的字算常用还是生僻」**：没被碰过的字压根不在本表里，
//! 它的判定完全由基表决定，出厂表升版时自动跟随。若改存完整集合，就得再定一条
//! 「新出现的字默认算哪边」的规则——怎么定都会让人意外。
//!
//! ## 键不带方案：这是全局字级属性
//!
//! | | 作用域 | 键 |
//! |---|---|---|
//! | shadow（候选调整） | 一次输入：这个方案、这个码 | `(方案, 输入码)` |
//! | 本模块 | **全局**：这个字在所有方案、所有码下 | `字` |
//!
//! 「某个字常不常用」是语言学属性，与五笔/拼音无关（2026-08-24 与用户确认）。
//! 右键菜单的文案要把这个差异说出来（「设为生僻字（全局）」对「隐藏此候选」），
//! 否则用户会两个都试一遍，再困惑于为什么表现不一样。

use crate::store::{COMMON_CHARS, Store};
use redb::ReadableTable;
use serde::{Deserialize, Serialize};

/// 一条用户覆盖。
///
/// `common` 两个方向都要存，不能只存「被踢出常用的字」：用户既会把常用字降级
/// （某个字他从不用、老是挡路），也会把生僻字升级（专业用字、人名用字）。
/// 只存一个方向的话另一个方向就没有落点。
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
struct CommonCharRecord {
    common: bool,
}

impl Store {
    /// 写一条覆盖（同字重复写即改写方向）。
    pub fn set_common_char_override(&self, key: &str, common: bool) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&CommonCharRecord { common })?;
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(COMMON_CHARS)?;
                t.insert(key, bytes.as_slice())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// **一个事务**写一批覆盖：`Some(common)` 写入、`None` 删除。返回真正写入的条数。
    ///
    /// 整类批量（按 Unicode 块设常用/生僻）用它。逐条走 [`Self::set_common_char_override`]
    /// 的话，一个块几百上千个字符就是几百上千次 fsync 提交，而调用方每写一条还要回灌一次
    /// 运行时镜像（全表重读 + 取写锁），那把锁正是候选过滤每次按键都要拿的。
    pub fn apply_common_char_overrides(
        &self,
        items: &[(String, Option<bool>)],
    ) -> anyhow::Result<usize> {
        let mut written = 0usize;
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(COMMON_CHARS)?;
                for (key, dir) in items {
                    match dir {
                        Some(common) => {
                            let bytes = serde_json::to_vec(&CommonCharRecord { common: *common })?;
                            t.insert(key.as_str(), bytes.as_slice())?;
                            written += 1;
                        }
                        None => {
                            t.remove(key.as_str())?;
                        }
                    }
                }
            }
            txn.commit()?;
            Ok(())
        })?;
        Ok(written)
    }

    /// 撤销某字的覆盖，回到出厂判定。`false` = 本就没有覆盖。
    ///
    /// 与「设为常用字」**不是**一回事：出厂判生僻的字，撤销覆盖后仍是生僻。
    /// 设置页因此把两者分成两个动作（「恢复出厂」对「设为常用字」）；候选右键只有一项，
    /// 靠「切到出厂方向即删覆盖」把恢复合并了进去（见 `Coordinator::apply_common_target`）。
    pub fn remove_common_char_override(&self, key: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let existed;
            {
                let mut t = txn.open_table(COMMON_CHARS)?;
                existed = t.remove(key)?.is_some();
            }
            txn.commit()?;
            Ok(existed)
        })
    }

    /// 某字是否有覆盖，以及方向。`None` = 未覆盖（由基表判定）。
    pub fn get_common_char_override(&self, key: &str) -> anyhow::Result<Option<bool>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(COMMON_CHARS)?;
            Ok(t.get(key)?.and_then(|g| {
                serde_json::from_slice::<CommonCharRecord>(g.value())
                    .ok()
                    .map(|r| r.common)
            }))
        })
    }

    /// 全部覆盖，按字的码位升序（redb 键序 = UTF-8 字节序，对单字符即码位序）。
    ///
    /// 量级是「用户手工点出来的」，几十条到几百条，全量返回无压力——这正是稀疏存储
    /// 换来的：界面直接列「我的调整」，而不是让用户在 8104 个字里翻页找。
    pub fn list_common_char_overrides(&self) -> anyhow::Result<Vec<CommonCharOverride>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(COMMON_CHARS)?;
            let mut out = Vec::new();
            for item in t.iter()? {
                let (k, v) = item?;
                // 键是一个字素簇，多码位合法（`👨‍👩‍👧` 5 个码位）。只有空键才是脏数据
                // （手工改库、旧版本残留），跳过而非报错——一条坏记录不该让整个列表打不开。
                let ch = k.value().to_string();
                if ch.is_empty() {
                    continue;
                }
                let Ok(rec) = serde_json::from_slice::<CommonCharRecord>(v.value()) else {
                    continue;
                };
                out.push(CommonCharOverride {
                    ch,
                    common: rec.common,
                });
            }
            Ok(out)
        })
    }

    /// 清空全部覆盖（词库管理界面的「全部恢复出厂」）。返回清掉的条数。
    pub fn clear_common_char_overrides(&self) -> anyhow::Result<usize> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(COMMON_CHARS)?;
                let keys: Vec<String> = t
                    .iter()?
                    .filter_map(|it| it.ok().map(|(k, _)| k.value().to_string()))
                    .collect();
                n = keys.len();
                for k in &keys {
                    t.remove(k.as_str())?;
                }
            }
            txn.commit()?;
            Ok(n)
        })
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
        let mut n = 0usize;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(o) = serde_json::from_str::<CommonCharOverride>(line) else {
                continue;
            };
            self.set_common_char_override(&o.ch, o.common)?;
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞。
    /// `tag` 区分同一测试里需要的第二个库（导入导出对拷）。
    fn store(tag: &str) -> Store {
        let p = std::env::temp_dir().join(format!(
            "wind_common_chars_test_{}_{}_{:?}.redb",
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
        assert_eq!(s.get_common_char_override("槮").unwrap(), None);

        s.set_common_char_override("槮", true).unwrap();
        assert_eq!(s.get_common_char_override("槮").unwrap(), Some(true));

        // 同字改写方向，不是追加第二条。
        s.set_common_char_override("槮", false).unwrap();
        assert_eq!(s.get_common_char_override("槮").unwrap(), Some(false));
        assert_eq!(s.list_common_char_overrides().unwrap().len(), 1);

        assert!(s.remove_common_char_override("槮").unwrap());
        assert_eq!(s.get_common_char_override("槮").unwrap(), None);
        // 再删一次：没有这条，返回 false 而非报错。
        assert!(!s.remove_common_char_override("槮").unwrap());
    }

    #[test]
    fn both_directions_are_storable() {
        let s = store("b");
        // 生僻字升级为常用
        s.set_common_char_override("槮", true).unwrap();
        // 常用字降级为生僻
        s.set_common_char_override("的", false).unwrap();
        let all = s.list_common_char_overrides().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.contains(&CommonCharOverride {
            ch: "槮".into(),
            common: true
        }));
        assert!(all.contains(&CommonCharOverride {
            ch: "的".into(),
            common: false
        }));
    }

    #[test]
    fn clear_removes_all() {
        let s = store("c");
        s.set_common_char_override("槮", true).unwrap();
        s.set_common_char_override("的", false).unwrap();
        assert_eq!(s.clear_common_char_overrides().unwrap(), 2);
        assert!(s.list_common_char_overrides().unwrap().is_empty());
    }

    #[test]
    fn jsonl_roundtrip() {
        let s = store("d");
        s.set_common_char_override("槮", true).unwrap();
        s.set_common_char_override("的", false).unwrap();
        let text = s.export_common_chars_jsonl().unwrap();
        assert_eq!(text.lines().count(), 2);

        let s2 = store("d2");
        assert_eq!(s2.import_common_chars_jsonl(&text).unwrap(), 2);
        assert_eq!(s2.get_common_char_override("槮").unwrap(), Some(true));
        assert_eq!(s2.get_common_char_override("的").unwrap(), Some(false));
    }

    #[test]
    fn import_skips_bad_lines() {
        let s = store("e");
        let text =
            "{\"ch\":\"槮\",\"common\":true}\n不是 json\n\n{\"ch\":\"的\",\"common\":false}\n";
        assert_eq!(s.import_common_chars_jsonl(text).unwrap(), 2);
        assert_eq!(s.list_common_char_overrides().unwrap().len(), 2);
    }
}
