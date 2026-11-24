//! 字符类的**用户层**（设置页「字符集」→「字符分类」：「+ 添加」走 `charset.create`，整份手改走
//! `data.beginEdit {kind: charsets}`；备份还原）。
//!
//! ## 文件是真相源（P4a 起，设计 §2.4）
//!
//! ```text
//! 出厂 data/charsets/*.yaml（只读，随安装包）
//!   ↓ 定制 data_custom/charsets/*.yaml（只读）
//!   ↓ 用户层 <userdata>/data/charsets/<key>.yaml（自建类、对出厂类的字段覆盖与成员增删）
//! registry
//! ```
//!
//! 曾经放进 redb，唯一的理由是「程序和人同写一个文件，后保存者赢、另一方静默丢失」。现在程序
//! 的写入是显式的整份替换（设置页保存、新建，「这份就是新内容」），手改文件经
//! `sync_charsets` 在配置重载 / `dict.reload` / 结束编辑时生效。重建时按出厂校验
//! （`wind_engine::charset_assembly::validated_user_docs`），不合规的登记问题。仍存在的一种
//! 冲突：设置页保存与编辑器同时保存同一个类，后写者胜（设置页的写入是显式整份替换，不复核）。
//!
//! ## 内容就是 yaml 文本本身
//!
//! 本 crate **不认识** `CharsetDoc`（`wind-config` 依赖方向反着），也不该认识：格式
//! 只在 `wind_config::charset_def` 里一份，解析与写出都在那边。这里存的是那份文本的
//! 字节，备份时原样进包、编辑时原样导出，一个格式三处用，不会有「库里的形态」与
//! 「文件里的形态」两套东西要对齐。
//!
//! 键是类的 `key`（也是 `exclude_blocks` / `include_blocks` 里写的那个名字），不带方案
//! ——字符类与 [`crate::common_chars`] 同为全局属性，理由见那边的模块文档。

use crate::store::Store;

impl Store {
    /// 字符集用户层目录：`<text_dir>/charsets/`（P4a 起真源）。
    pub fn charsets_dir(&self) -> std::path::PathBuf {
        self.user_text_dir().join("charsets")
    }

    /// 某类的用户层文件。`key` 是一个路径分量（越界 / 非法 ⇒ Err）。
    pub fn charset_path(&self, key: &str) -> anyhow::Result<std::path::PathBuf> {
        wind_userdata::engine::check_schema_id(key)
            .map_err(|_| anyhow::anyhow!("非法字符分类 key：{key:?}"))?;
        Ok(self.charsets_dir().join(format!("{key}.yaml")))
    }

    /// 读一个类的用户层文本；`None` = 用户没动过这个类。
    pub fn get_charset_doc(&self, key: &str) -> anyhow::Result<Option<String>> {
        let p = self.charset_path(key)?;
        Ok(wind_userdata::text::fold::read_text(&p)?
            .map(|(b, _)| String::from_utf8_lossy(&b).into_owned()))
    }

    /// 整份写入（同 key 覆盖；显式回读 / 设置页保存都是「这份就是新内容」，不复核指纹）。
    /// 文本合法性由调用方负责——本层不解析。
    pub fn set_charset_doc(&self, key: &str, text: &str) -> anyhow::Result<()> {
        let p = self.charset_path(key)?;
        self.check_not_editing(&p)?;
        wind_userdata::text::fold::commit_text(&p, text.as_bytes(), None)?;
        self.remember_charsets_fp();
        Ok(())
    }

    /// 删掉一个类的用户层（自建类 = 整个类消失；出厂类 = 回到出厂）。`false` = 本就没有。
    pub fn remove_charset_doc(&self, key: &str) -> anyhow::Result<bool> {
        let p = self.charset_path(key)?;
        self.check_not_editing(&p)?;
        let existed = match std::fs::remove_file(&p) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        self.remember_charsets_fp();
        Ok(existed)
    }

    /// 全部用户层，按 key 升序。量级是几十条，全量返回。名字不合法的文件跳过（WARN）。
    pub fn list_charset_docs(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.charsets_dir()) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("yaml") {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            if wind_userdata::engine::check_schema_id(key).is_err() {
                tracing::warn!("字符集用户层：跳过文件名不合法的条目");
                continue;
            }
            if let Some(text) = self.get_charset_doc(key)? {
                out.push((key.to_string(), text));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// 清空用户层（备份「替换」模式用）。返回删掉的条数。
    pub fn clear_charset_docs(&self) -> anyhow::Result<usize> {
        let docs = self.list_charset_docs()?;
        // 先全查一遍：有一个在编辑就整体不动，不清到一半
        for (k, _) in &docs {
            self.check_not_editing(&self.charset_path(k)?)?;
        }
        for (k, _) in &docs {
            self.remove_charset_doc(k)?;
        }
        Ok(docs.len())
    }

    /// 用户层目录的整体指纹（各文件名与内容指纹的有序拼接）。
    fn charsets_fp(&self) -> String {
        let mut parts: Vec<String> = self
            .list_charset_docs()
            .unwrap_or_default()
            .into_iter()
            .map(|(k, t)| {
                format!(
                    "{k}={}",
                    wind_userdata::text::fold::fingerprint(Some(t.as_bytes()))
                )
            })
            .collect();
        parts.sort();
        parts.join(";")
    }

    pub(crate) fn remember_charsets_fp(&self) {
        let fp = self.charsets_fp();
        *self.charsets_fp.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp);
    }

    /// 手改的字符集用户层文件重新载入：目录内容变了返回 true（调用方据此重建字符集注册表）。
    pub fn sync_charsets(&self) -> bool {
        let fp = self.charsets_fp();
        let mut g = self.charsets_fp.lock().unwrap_or_else(|e| e.into_inner());
        let changed = g.as_deref().is_some_and(|old| old != fp);
        *g = Some(fp);
        changed
    }
}

#[cfg(test)]
#[path = "charsets_tests.rs"]
mod tests;
