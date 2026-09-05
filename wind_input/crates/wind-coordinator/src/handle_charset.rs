//! 字符类的设置页读写：列类、改类属性、恢复默认、外部编辑（导出 / 从文件加载）、
//! 删自建类、清理被它压住的逐条覆盖。
//!
//! 设计见 `docs/design/charset-classification.md`。用户层落在 **redb**
//! （`wind_store::charsets`，value 就是 yaml 文本），不是目录——程序和人不能写同一个
//! 文件，理由见那边的模块文档。人要改就走「外部编辑」：
//!
//! ```text
//! charset_export_edit(key)  →  临时文件（完整视图，首行写明「不会被自动读取」）
//!        ↓ 用户在编辑器里改
//! charset_import_file(path) →  parse → diff 出厂 → 稀疏 diff 存库 → 热载
//! ```
//!
//! 「从文件加载」是**唯一**的回读入口，也是手写 yaml 的入口：文件里的 key 出厂有就当
//! 覆盖，没有就当自建类。一条路，不分「回读」与「导入」。
//!
//! # ★ 两种操作的差别必须让用户看见（§7.3）
//!
//! | | 改类的 `default` | 逐条覆盖（候选右键 / 词库管理） |
//! |---|---|---|
//! | 作用面 | **整个类**，不问词库里有没有 | 只作用于点过的那个字 |
//! | 跟随出厂更新 | ✅ 出厂给类补了新成员，一并生效 | ⛔ 只有当时扫到的那些 |
//! | 可逆 | 改回去即可 | 要反向再来一次 |
//! | 优先级 | 低 | **高**，压住前者 |
//!
//! 最后一行是「配了没反应」的现成来源：用户曾整类点过生僻，库里躺着上千条覆盖；他现在
//! 把类的 `default` 改成常用——**一点反应都没有**。故 [`CharsetClassRow::override_count`]
//! 必须显示出来，并给一个「清理冗余」的出口。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};
use wind_config::charset_def::{self, CharsetDoc, Commonality};

/// 设置页列表里的一行 = 一个字符类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharsetClassRow {
    /// 稳定标识，也是 `exclude_blocks` / `include_blocks` 里能写的那个名字。
    pub key: String,
    /// 显示名（缺省 = key）。
    pub name: String,
    /// 码位段文本，如 `["U+2600-U+26FF"]`。空 = 纯字表类。
    pub ranges: Vec<String>,
    /// 离散成员数（字表 + 内嵌列表）。
    pub member_count: usize,
    /// 仲裁顺序，小的优先。
    pub order: i32,
    /// 常用性：`None` = 本类不表态（默认）。
    pub default_common: Option<bool>,
    /// 免词频。
    pub no_freq: bool,
    /// 纳入生僻字模式。
    pub in_rare: bool,
    /// 有没有被停用。
    pub enabled: bool,
    /// **出厂类**（data / custom 两层里有这个 key）。`ranges` 只读、不能删，只能「恢复默认」。
    /// 反之是用户自建类：范围可改、可删。
    pub builtin: bool,
    /// 用户层对这个类有调整。决定「恢复默认」能不能点。
    pub overridden: bool,
    /// ⚠️ **有多少条逐条覆盖压在这个类上**（§7.2）。
    ///
    /// 大于 0 时界面必须提示，否则用户改了 `default` 会觉得开关坏了。
    pub override_count: usize,
}

/// 设置页对一个类的编辑。字段为 `None` = 本次不动它。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharsetEdit {
    /// `"common"` / `"rare"` / `"inherit"`（改回不表态）。
    pub default_common: Option<String>,
    pub no_freq: Option<bool>,
    pub in_rare: Option<bool>,
    pub enabled: Option<bool>,
    pub order: Option<i32>,
}

/// 清理冗余的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharsetCleanupOutcome {
    /// 检查过的覆盖条数。
    pub scanned: usize,
    /// 删掉的条数（与当前默认判定同向 ⇒ 留着没有意义）。
    pub removed: usize,
}

/// 「从文件加载」导入的一个类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharsetImported {
    pub key: String,
    pub name: String,
    /// 出厂有这个 key（导入的是覆盖），还是新建了一个类。
    pub builtin: bool,
    /// 存进用户层的：新增成员数 / 移除成员数 / 覆盖的字段数。
    pub added: usize,
    pub removed: usize,
    pub fields: usize,
}

impl crate::Coordinator {
    /// 全部字符类，按 `order` 升序（与仲裁顺序一致，用户看到的先后就是生效的先后）。
    pub(crate) fn charset_rows(&self) -> Vec<CharsetClassRow> {
        let reg = self.engine_mgr.charsets();
        let factory = self.engine_mgr.charset_factory();
        let user = self.user_docs();
        let counts = self.override_counts_by_class();

        let mut rows: Vec<CharsetClassRow> = reg
            .classes()
            .iter()
            .map(|c| CharsetClassRow {
                key: c.key.clone(),
                name: if c.name.is_empty() {
                    c.key.clone()
                } else {
                    c.name.clone()
                },
                ranges: c
                    .ranges
                    .iter()
                    .map(|(lo, hi)| {
                        if lo == hi {
                            format!("U+{lo:04X}")
                        } else {
                            format!("U+{lo:04X}-U+{hi:04X}")
                        }
                    })
                    .collect(),
                member_count: c.members.len(),
                order: c.order,
                default_common: c.default_common,
                no_freq: c.no_freq,
                in_rare: c.in_rare,
                // 停用的类压根不在 registry 里（装配时就 remove 了），故列出来的恒真。
                enabled: true,
                builtin: factory.contains_key(&c.key),
                overridden: user.contains_key(&c.key),
                override_count: counts.get(&c.key).copied().unwrap_or(0),
            })
            .collect();

        // ⚠️ **被停用的类不在 registry 里**（装配时就 remove 了），但必须列出来——否则
        // 用户停用之后界面上再也找不到它，「停用」就成了不可逆操作。
        for (key, doc) in &user {
            if doc.def.enabled == Some(false) && reg.class_by_key(key).is_none() {
                rows.push(CharsetClassRow {
                    key: key.clone(),
                    name: doc.def.display_name().to_string(),
                    // 停用的类没有装配结果可报，这几项只能给已知的那点信息。
                    ranges: doc.def.ranges.clone().unwrap_or_default(),
                    member_count: doc.added.len(),
                    order: doc.def.order_or_default(),
                    default_common: doc.def.default.map(|c| c.is_common()),
                    no_freq: doc.def.no_freq.unwrap_or(false),
                    in_rare: doc.def.in_rare.unwrap_or(false),
                    enabled: false,
                    builtin: factory.contains_key(key),
                    overridden: true,
                    override_count: counts.get(key).copied().unwrap_or(0),
                });
            }
        }
        rows.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));
        rows
    }

    /// 改一个类的属性：写库 + 立即热载。
    ///
    /// ⚠️ **写库与热载必须在同一处完成**。分开的话就多出一条「写了但没生效」的路径，
    /// 而它的表现是「设置页改完没反应、重启才生效」——本仓反复出现的那类缺陷。
    pub(crate) fn charset_edit(&self, key: &str, edit: &CharsetEdit) -> anyhow::Result<()> {
        // ⚠️ 存在性判据不能只看 registry：**被停用的类不在里面**，只看它的话用户停用
        // 之后就再也启用不回来（报「没有名为 X 的字符类」）。用户层里留着那份 `key: X`
        // 的调整，正是「这个类存在过」的凭据。
        let mut doc = self.user_doc(key)?;
        anyhow::ensure!(
            self.engine_mgr.charsets().class_by_key(key).is_some()
                || !charset_def::is_empty_override(&doc),
            "没有名为「{key}」的字符类"
        );

        apply_edit(&mut doc, edit)?;
        self.save_user_doc(&doc)?;
        self.reload_charsets();
        debug!("字符类「{key}」已更新：{edit:?}");
        Ok(())
    }

    /// 撤掉用户层对某个类的全部调整。
    ///
    /// 对自建类这等于删除（它整个都是用户层）——设置页对自建类该显示「删除」而不是
    /// 「恢复默认」，判据是 [`CharsetClassRow::builtin`]。
    pub(crate) fn charset_reset(&self, key: &str) -> anyhow::Result<()> {
        self.charset_store()?.remove_charset_doc(key)?;
        self.reload_charsets();
        Ok(())
    }

    /// 删一个**自建**类。出厂类不能删，只能停用或恢复默认。
    ///
    /// 与 [`Self::charset_reset`] 的实体一样，分开是为了让「删出厂类」在入口就被拒：
    /// 走 reset 的话出厂类会"恢复"而不是消失，用户以为删了、列表里还在。
    pub(crate) fn charset_delete(&self, key: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.engine_mgr.charset_factory().contains_key(key),
            "「{key}」是出厂类，不能删除；要去掉它请停用，要撤销调整请恢复默认"
        );
        anyhow::ensure!(
            self.charset_store()?.remove_charset_doc(key)?,
            "没有名为「{key}」的字符类"
        );
        self.reload_charsets();
        Ok(())
    }

    /// 「外部编辑」：把一个类的**完整视图**写到临时文件，返回路径给设置页去打开。
    ///
    /// 文件首行写明「不会被自动读取」——改完要回设置页「从文件加载」。冲突只剩一处：
    /// 导出后、加载前，设置页又改了同一个类的属性，加载会整条替换掉。单人本地工具，
    /// 不做版本号合并；头注释说了这是导出的副本。
    pub(crate) fn charset_export_edit(&self, key: &str) -> anyhow::Result<PathBuf> {
        let factory = self.engine_mgr.charset_factory();
        let user = self.user_doc(key)?;
        anyhow::ensure!(
            factory.contains_key(key) || !charset_def::is_empty_override(&user),
            "没有名为「{key}」的字符类"
        );
        let text = charset_def::render_edit_view(factory.get(key), &user)?;
        let path = edit_file_path(key);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, text)
            .map_err(|e| anyhow::anyhow!("写 {} 失败：{e}", path.display()))?;
        debug!("字符类「{key}」已导出到 {}", path.display());
        Ok(path)
    }

    /// 「新建类」：导出一份**模板**到临时文件，用户填完再「从文件加载」。
    ///
    /// 不做对话框：新建的全部内容（key、名称、范围、成员）本来就都在文件里，多一个只填
    /// 两项的对话框反而让用户以为建完了。key 由用户在文件里改——模板给的名字不会与出厂
    /// 撞车，加载时若撞了出厂的 key 会当作对那个类的覆盖，头注释里有说。
    pub(crate) fn charset_export_template(&self) -> anyhow::Result<PathBuf> {
        let factory = self.engine_mgr.charset_factory();
        let user = self.user_docs();
        // 找一个既不在出厂也不在用户层的 key。
        let key = (1..)
            .map(|n| format!("my_class_{n}"))
            .find(|k| !factory.contains_key(k) && !user.contains_key(k))
            .expect("总能找到一个没用过的名字");
        let doc = CharsetDoc {
            def: charset_def::CharsetDef {
                key: key.clone(),
                name: Some("我的字符类".into()),
                ranges: Some(Vec::new()),
                ..Default::default()
            },
            added: Vec::new(),
            removed: Vec::new(),
        };
        let text = charset_def::render_edit_view(None, &doc)?;
        let path = edit_file_path(&key);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, text)
            .map_err(|e| anyhow::anyhow!("写 {} 失败：{e}", path.display()))?;
        Ok(path)
    }

    /// 「从文件加载」：解析文件里的每个类，出厂有的当覆盖（diff 后存）、没有的当自建。
    ///
    /// 一个文件可以带多个类（meta 头是数组，与出厂 `blocks.yaml` 同款），逐个导入；
    /// 任一个失败整次失败、库不动——半导入的状态用户理不清。
    pub(crate) fn charset_import_file(&self, path: &Path) -> anyhow::Result<Vec<CharsetImported>> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读 {} 失败：{e}", path.display()))?;
        let docs = charset_def::parse_docs(&text)?;
        anyhow::ensure!(!docs.is_empty(), "文件里没有任何字符类");

        let factory = self.engine_mgr.charset_factory();
        let mut pending: Vec<(CharsetDoc, CharsetImported)> = Vec::new();
        for doc in docs {
            let key = doc.def.key.clone();
            let f = factory.get(&key);
            let diff = charset_def::diff_against_factory(doc, f)?;
            let name = f
                .map(|m| m.def.display_name().to_string())
                .unwrap_or_else(|| diff.def.display_name().to_string());
            let d = &diff.def;
            let fields = [
                d.name.is_some(),
                d.ranges.is_some(),
                d.file.is_some(),
                d.scope.is_some(),
                d.default.is_some(),
                d.outside.is_some(),
                d.order.is_some(),
                d.no_freq.is_some(),
                d.in_rare.is_some(),
                d.enabled.is_some(),
            ]
            .into_iter()
            .filter(|b| *b)
            .count();
            let summary = CharsetImported {
                key,
                name,
                builtin: f.is_some(),
                added: diff.added.len(),
                removed: diff.removed.len(),
                fields,
            };
            pending.push((diff, summary));
        }

        // 全部 diff 通过之后才落库。
        for (diff, _) in &pending {
            self.save_user_doc(diff)?;
        }
        self.reload_charsets();
        let out: Vec<CharsetImported> = pending.into_iter().map(|(_, s)| s).collect();
        debug!("从 {} 导入字符类：{out:?}", path.display());
        Ok(out)
    }

    /// 清理压在某个类上的**冗余**逐条覆盖：方向与当前默认判定相同的那些。
    ///
    /// ★ 判据复用写端已有的那条（`is_cluster_common_by_default(k) != common` 才值得留），
    /// 不新写一份——两处判据分叉的表现是「设置页说有 N 条，清理完还剩几条」。
    pub(crate) fn charset_clear_redundant(
        &self,
        key: &str,
    ) -> anyhow::Result<CharsetCleanupOutcome> {
        let store = self.charset_store()?;
        let reg = self.engine_mgr.charsets();
        anyhow::ensure!(reg.class_by_key(key).is_some(), "没有名为「{key}」的字符类");

        let rows = store.list_common_char_overrides()?;
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        let mut out = CharsetCleanupOutcome::default();
        let mut doomed: Vec<String> = Vec::new();
        for row in &rows {
            if reg.class_of(&row.ch).map(|c| c.key.as_str()) != Some(key) {
                continue;
            }
            out.scanned += 1;
            if cc.is_cluster_common_by_default(&row.ch, &reg) == row.common {
                doomed.push(row.ch.clone());
            }
        }
        drop(cc);

        for ch in &doomed {
            store.remove_common_char_override(ch)?;
        }
        out.removed = doomed.len();
        if out.removed > 0 {
            self.reload_common_chars();
        }
        debug!(
            "字符类「{key}」清理冗余覆盖：扫 {} 删 {}",
            out.scanned, out.removed
        );
        Ok(out)
    }

    /// 重新装配 registry（改完用户层后立即生效；备份还原后也要调一次）。
    pub(crate) fn reload_charsets(&self) {
        let cfg = self.rt().config.clone();
        self.engine_mgr.rebuild_charsets(&cfg);
    }

    /// 每个类身上压着多少条逐条覆盖。
    ///
    /// 归属按 [`wind_candidate::CharsetRegistry::class_of`]——它给的是**仲裁赢家**，
    /// 也就是「谁决定了这个字的常用性」，正是用户在这一行上想知道的那个类。
    fn override_counts_by_class(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        let Some(store) = self.store.as_ref() else {
            return out;
        };
        let rows = match store.list_common_char_overrides() {
            Ok(v) => v,
            Err(e) => {
                warn!("字符类：读逐条覆盖失败，本次不显示压制条数: {e}");
                return out;
            }
        };
        let reg = self.engine_mgr.charsets();
        for row in &rows {
            if let Some(c) = reg.class_of(&row.ch) {
                *out.entry(c.key.clone()).or_insert(0) += 1;
            }
        }
        out
    }
}

impl crate::Coordinator {
    fn charset_store(&self) -> anyhow::Result<&wind_store::Store> {
        self.store
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储，字符类的调整无处落盘"))
    }

    /// 用户层现有的全部调整，按 key 索引。没有 store 时为空。
    ///
    /// ★ 与装配走的是同一个 `user_docs_from_store`：读写两侧必须同源，否则设置页读 A、
    /// 装配读 B，表现就是「改了没反应」而两边各自看都没错。
    fn user_docs(&self) -> BTreeMap<String, CharsetDoc> {
        let Some(store) = self.store.as_deref() else {
            return BTreeMap::new();
        };
        wind_engine::charset_assembly::user_docs_from_store(store)
            .into_iter()
            .map(|d| (d.def.key.clone(), d))
            .collect()
    }

    /// 用户层里某个 key 的调整；没有就给一份只带 key 的空壳。
    ///
    /// ★ 给空壳而不是 `None`：调用方拿到它就能直接改字段再存回去，不必在「有没有这条」
    /// 上分两条路——而那两条路一旦分开，新建类的那条几乎必然少写点什么。
    fn user_doc(&self, key: &str) -> anyhow::Result<CharsetDoc> {
        let empty = CharsetDoc {
            def: charset_def::CharsetDef {
                key: key.to_string(),
                ..Default::default()
            },
            added: Vec::new(),
            removed: Vec::new(),
        };
        let Some(text) = self.charset_store()?.get_charset_doc(key)? else {
            return Ok(empty);
        };
        match charset_def::parse_doc(&text) {
            Ok(doc) if doc.def.key == key => Ok(doc),
            Ok(doc) => {
                warn!(
                    "字符类用户层：库键「{key}」与文本里的 key「{}」不一致，按空壳处理",
                    doc.def.key
                );
                Ok(empty)
            }
            Err(e) => {
                warn!("字符类用户层「{key}」解析失败，按空壳处理：{e}");
                Ok(empty)
            }
        }
    }

    /// 存一份调整：空壳则删记录（与 `compat.toml` 的 `update_user_rule` 同一条纪律：
    /// 「恢复默认」之后不能留一条只有 key 的记录）。
    fn save_user_doc(&self, doc: &CharsetDoc) -> anyhow::Result<()> {
        let store = self.charset_store()?;
        if charset_def::is_empty_override(doc) {
            store.remove_charset_doc(&doc.def.key)?;
        } else {
            store.set_charset_doc(&doc.def.key, &charset_def::render_doc(doc)?)?;
        }
        Ok(())
    }
}

/// 外部编辑文件的落点：系统临时目录下 `WindInput/charsets/<key>.yaml`。
///
/// 放临时目录而不是用户配置目录：它**不是配置**，是导出的副本，留在配置目录里用户
/// 会以为改它就生效（那正是改成库存储要消灭的误解）。`key` 已由渲染函数校验过可作文件名。
fn edit_file_path(key: &str) -> PathBuf {
    std::env::temp_dir()
        .join("WindInput")
        .join(charset_def::CHARSETS_DIR_NAME)
        .join(format!("{key}.yaml"))
}

/// 把一次编辑叠加到用户层的那份 doc 上。
///
/// ★ `"inherit"` 写成 `None` 而不是某个具体值：那是「本层不表态、沿用下层」，与
/// 显式设成常用不是一回事——前者跟随出厂更新，后者钉死。
fn apply_edit(doc: &mut CharsetDoc, edit: &CharsetEdit) -> anyhow::Result<()> {
    if let Some(v) = &edit.default_common {
        doc.def.default = match v.as_str() {
            "common" => Some(Commonality::Common),
            "rare" => Some(Commonality::Rare),
            "inherit" => None,
            other => anyhow::bail!("default 只能是 common / rare / inherit，收到「{other}」"),
        };
    }
    // 布尔属性：设成 `false` 与「不表态」在用户层里是两回事，但对出厂不表态的类等价。
    // 为了不留一条永远等于默认的记录，`false` 写成 `None`——出厂哪天把某个类改成
    // `no_freq: true`，这个用户就跟着走，而不是被自己早年一次「关掉」钉死。
    if let Some(v) = edit.no_freq {
        doc.def.no_freq = v.then_some(true);
    }
    if let Some(v) = edit.in_rare {
        doc.def.in_rare = v.then_some(true);
    }
    if let Some(v) = edit.enabled {
        doc.def.enabled = (!v).then_some(false);
    }
    if let Some(v) = edit.order {
        doc.def.order = Some(v);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! 这些测试走的是**设置页会走的那条链**：
    //!
    //! ```text
    //! charset_edit / charset_import_file → store → rebuild_charsets（装配）→ verdict_of
    //! ```
    //!
    //! ⚠️ store 在**临时目录**里（`new_headless_with_ui_at`），绝不碰开发者真实的
    //! `%APPDATA%`——那样不仅留垃圾，留下的记录还会反过来影响下一次测试。

    use super::*;
    use crate::Coordinator;
    use std::sync::Arc;
    use wind_config::Config;

    /// 仓库根下的 `data/`，出厂字符类就在它的 `charsets/` 里。
    fn data_dir() -> PathBuf {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("定位不到仓库根");
        let d = repo.join("data");
        assert!(
            d.join("charsets").is_dir(),
            "找不到出厂字符类目录 {}",
            d.join("charsets").display()
        );
        d
    }

    fn tmp_user_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wind_charset_settings_{}_{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 带出厂字符类 + 临时 store 的 headless coordinator。
    fn coord(tag: &str) -> (Arc<Coordinator>, PathBuf) {
        let user = tmp_user_dir(tag);
        let (c, _rx) =
            Coordinator::new_headless_with_ui_at(Config::default(), Some(&data_dir()), Some(&user));
        assert!(
            c.store.is_some(),
            "夹具必须开着 store，否则测的是「无处落盘」那条路"
        );
        (c, user)
    }

    /// 判定入口：一个字素簇现在算不算常用。
    fn is_common(c: &Coordinator, s: &str) -> bool {
        let cc = c.common_chars.read().unwrap();
        cc.is_string_common(s, &c.engine_mgr.charsets())
    }

    fn stored(c: &Coordinator, key: &str) -> Option<String> {
        c.store.as_ref().unwrap().get_charset_doc(key).unwrap()
    }

    /// ★★ 改 emoji 类的 `default` 到候选判定生效，一条链走通。
    ///
    /// 出厂时 emoji 类**不表态**（`default` 空），故 `😀` 兜底判常用；用户把它设成生僻之后
    /// 立刻改判——不需要重启、不需要切方案。
    #[test]
    fn setting_a_class_default_reaches_the_verdict() {
        let (c, user) = coord("default");

        assert!(is_common(&c, "😀"), "出厂 emoji 类不表态 ⇒ 兜底判常用");

        c.charset_edit(
            "emoji",
            &CharsetEdit {
                default_common: Some("rare".into()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(!is_common(&c, "😀"), "改完 default 应立即生效");
        assert!(is_common(&c, "我"), "常用汉字不该被带累");

        // ⚠️ 真的落库了，而不是只改了内存镜像。
        let text = stored(&c, "emoji").expect("改动必须落库，否则重启就没了");
        assert!(text.contains("default: rare"));
        assert!(
            !text.contains("ranges:"),
            "只写用户改过的字段——写全量会把出厂值固化进用户层"
        );

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// `inherit` 是「本层不表态」，与显式设成常用**不是一回事**。
    ///
    /// 前者跟随出厂更新（出厂哪天给 emoji 定了调，用户跟着走），后者钉死。二者在 UI 上是
    /// 三个不同的档，判据是「库里还有没有 default 这一行」。
    #[test]
    fn inherit_removes_the_opinion_rather_than_pinning_it() {
        let (c, user) = coord("inherit");

        c.charset_edit(
            "emoji",
            &CharsetEdit {
                default_common: Some("rare".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!is_common(&c, "😀"));

        c.charset_edit(
            "emoji",
            &CharsetEdit {
                default_common: Some("inherit".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(is_common(&c, "😀"), "改回 inherit 应恢复到不表态");
        assert_eq!(
            stored(&c, "emoji"),
            None,
            "整份调整已空 ⇒ 该删记录而不是留一份只有 key 的空壳"
        );

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 免词频与生僻准入两个并集属性同样是「改完即生效」。
    #[test]
    fn union_flags_take_effect_immediately() {
        let (c, user) = coord("union");
        let reg = c.engine_mgr.charsets();
        assert!(!reg.no_freq("😀"), "出厂不免词频");
        assert!(!reg.in_rare("😀"), "出厂不进生僻模式");

        c.charset_edit(
            "emoji",
            &CharsetEdit {
                no_freq: Some(true),
                in_rare: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

        let reg = c.engine_mgr.charsets();
        assert!(reg.no_freq("😀"));
        assert!(reg.in_rare("😀"));
        assert!(!reg.no_freq("我"), "汉字不该被带累");

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// `enabled: false` 把一个类整个停掉；恢复之后它回来。
    #[test]
    fn disabling_a_class_removes_it_and_enabling_brings_it_back() {
        let (c, user) = coord("enabled");
        assert!(c.engine_mgr.charsets().class_by_key("emoji").is_some());

        c.charset_edit(
            "emoji",
            &CharsetEdit {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            c.engine_mgr.charsets().class_by_key("emoji").is_none(),
            "停用的类不该还在 registry 里"
        );

        c.charset_edit(
            "emoji",
            &CharsetEdit {
                enabled: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            c.engine_mgr.charsets().class_by_key("emoji").is_some(),
            "恢复启用后该回来"
        );
        assert_eq!(
            stored(&c, "emoji"),
            None,
            "enabled=true 是默认值，不该留一条永远等于默认的记录"
        );

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// ★★ 停用的类**仍要出现在列表里**，否则「停用」是不可逆的。
    ///
    /// 停用的类不在 registry 里（装配时就 remove 了）。只列 registry 的话，用户点完停用
    /// 那一行就从界面上消失了，再也没有入口把它启用回来——而库里它还在。
    #[test]
    fn a_disabled_class_stays_visible_so_it_can_be_re_enabled() {
        let (c, user) = coord("visible");
        c.charset_edit(
            "emoji",
            &CharsetEdit {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();

        let rows = c.charset_rows();
        let row = rows
            .iter()
            .find(|r| r.key == "emoji")
            .expect("停用的类必须还在列表里，否则用户找不回它");
        assert!(!row.enabled, "该标成停用");
        assert!(row.overridden, "它的状态来自用户层调整");
        assert!(row.builtin, "停用了也还是出厂类");

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// `charset_reset` 撤掉这个类的全部调整。
    #[test]
    fn resetting_drops_every_adjustment() {
        let (c, user) = coord("reset");
        c.charset_edit(
            "emoji",
            &CharsetEdit {
                default_common: Some("rare".into()),
                no_freq: Some(true),
                order: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!is_common(&c, "😀"));

        c.charset_reset("emoji").unwrap();
        assert!(is_common(&c, "😀"), "恢复默认后判定该回到出厂");
        assert!(!c.engine_mgr.charsets().no_freq("😀"));
        assert_eq!(stored(&c, "emoji"), None);

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 不存在的类要报错，不能静默写出一条永远不生效的记录。
    #[test]
    fn editing_an_unknown_class_fails_loudly() {
        let (c, user) = coord("unknown");
        let e = c
            .charset_edit(
                "没有这个类",
                &CharsetEdit {
                    default_common: Some("rare".into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(
            e.to_string().contains("没有名为"),
            "错误信息要说清是什么问题：{e}"
        );
        assert!(
            c.store
                .as_ref()
                .unwrap()
                .list_charset_docs()
                .unwrap()
                .is_empty(),
            "拒绝之后不该留下任何记录"
        );
        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// `default` 只认三个值，别的当场报错。
    ///
    /// ⚠️ 静默忽略非法值的表现是「点了没反应」，而 RPC 调用方（设置页）拿不到任何线索。
    #[test]
    fn an_invalid_default_value_is_rejected() {
        let (c, user) = coord("badvalue");
        let e = c
            .charset_edit(
                "emoji",
                &CharsetEdit {
                    default_common: Some("maybe".into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(e.to_string().contains("common / rare / inherit"), "{e}");
        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 列表把出厂那些类都列出来，且顺序 = 仲裁顺序；出厂类标 `builtin`。
    #[test]
    fn rows_list_every_class_in_arbitration_order() {
        let (c, user) = coord("rows");
        let rows = c.charset_rows();

        for key in ["emoji", "common_han", "符号", "基本汉字", "其它"] {
            let r = rows
                .iter()
                .find(|r| r.key == key)
                .unwrap_or_else(|| panic!("字符类列表里缺 {key}"));
            assert!(r.builtin, "{key} 是出厂类");
            assert!(!r.overridden, "出厂状态下没有调整");
        }
        let orders: Vec<i32> = rows.iter().map(|r| r.order).collect();
        let mut sorted = orders.clone();
        sorted.sort();
        assert_eq!(orders, sorted, "列表顺序必须与仲裁顺序一致");

        // 出厂 emoji 类不表态；common_han 表态。
        let emoji = rows.iter().find(|r| r.key == "emoji").unwrap();
        assert_eq!(emoji.default_common, None);
        assert!(emoji.member_count > 1400, "emoji 类该带着那份精确字表");
        let han = rows.iter().find(|r| r.key == "common_han").unwrap();
        assert_eq!(han.default_common, Some(true));

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    // ── 外部编辑 ───────────────────────────────────────────────────────────

    /// ★★ 导出 → 改文件 → 从文件加载 → 判定生效，一条链走通。
    ///
    /// 这是「文本作中介」那条路的主用例：用户在编辑器里删一个 emoji、加一个字段，
    /// 加载后库里只多了那两条（稀疏），判定立即改变。
    #[test]
    fn export_edit_import_changes_the_verdict() {
        let (c, user) = coord("edit_chain");
        let path = c.charset_export_edit("emoji").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.lines().next().unwrap().contains("不会被自动读取"),
            "首行必须说明这不是配置文件"
        );
        assert!(text.contains("\n😀\n"), "完整视图里该有出厂成员");

        // 用户：把 😀 从类里拿掉、让整个类判生僻。
        let edited = text
            .replace("\n😀\n", "\n")
            .replace("\n---\n", "\n---\ndefault: rare\n");
        std::fs::write(&path, edited).unwrap();

        let out = c.charset_import_file(&path).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "emoji");
        assert!(out[0].builtin);
        assert_eq!((out[0].added, out[0].removed, out[0].fields), (0, 1, 1));

        assert!(!is_common(&c, "😃"), "整类判生僻生效");
        assert!(
            is_common(&c, "😀"),
            "被拿掉的那个不再属于 emoji 类 ⇒ 兜底判常用"
        );

        let stored = stored(&c, "emoji").unwrap();
        assert!(stored.contains("-😀"), "库里是稀疏 diff：{stored}");
        assert!(!stored.contains("😃"), "没改的成员不该进库");

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 导出后原样加载 ⇒ 库里**没有**记录。导出过不等于调整过。
    #[test]
    fn importing_an_untouched_export_leaves_no_trace() {
        let (c, user) = coord("untouched");
        let path = c.charset_export_edit("common_han").unwrap();
        let out = c.charset_import_file(&path).unwrap();
        assert_eq!((out[0].added, out[0].removed, out[0].fields), (0, 0, 0));
        assert_eq!(stored(&c, "common_han"), None);
        assert!(is_common(&c, "我"));
        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 加载一个出厂没有的 key ⇒ 自建类：列表里出现、`builtin` 为假、判定生效、可删。
    #[test]
    fn importing_a_new_key_creates_a_custom_class_that_can_be_deleted() {
        let (c, user) = coord("custom");
        let f = user.join("mine.yaml");
        std::fs::write(
            &f,
            "---\nkey: 我的生僻符号\nname: 我的生僻符号\ndefault: rare\norder: 1\n...\n★\n☆\n",
        )
        .unwrap();
        assert!(is_common(&c, "★"), "出厂不认识 ★ ⇒ 兜底判常用");

        let out = c.charset_import_file(&f).unwrap();
        assert!(!out[0].builtin);
        assert_eq!(out[0].added, 2);
        assert!(!is_common(&c, "★"), "自建类的 default 生效");

        let rows = c.charset_rows();
        let row = rows.iter().find(|r| r.key == "我的生僻符号").unwrap();
        assert!(!row.builtin);
        assert!(row.overridden);
        assert_eq!(row.member_count, 2);

        c.charset_delete("我的生僻符号").unwrap();
        assert!(is_common(&c, "★"), "删了之后判定回到出厂");
        assert!(!c.charset_rows().iter().any(|r| r.key == "我的生僻符号"));

        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// ⛔ 出厂类不能删——删了会"恢复"而不是消失，用户以为删了、列表里还在。
    #[test]
    fn deleting_a_factory_class_is_refused() {
        let (c, user) = coord("nodelete");
        let e = c.charset_delete("emoji").unwrap_err();
        assert!(e.to_string().contains("出厂类"), "{e}");
        assert!(c.engine_mgr.charsets().class_by_key("emoji").is_some());
        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 改出厂类的范围 ⇒ 加载**报错**，库不动。
    #[test]
    fn importing_changed_factory_ranges_is_refused_and_leaves_the_store_untouched() {
        let (c, user) = coord("ranges");
        let f = user.join("bad.yaml");
        std::fs::write(&f, "key: 符号\nranges: [U+0000-U+FFFF]\n").unwrap();
        let e = c.charset_import_file(&f).unwrap_err();
        assert!(e.to_string().contains("范围只读"), "{e}");
        assert_eq!(stored(&c, "符号"), None);
        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 「新建类」模板：key 不与现有的撞、首行同样声明不会被自动读取。
    #[test]
    fn the_new_class_template_has_a_fresh_key() {
        let (c, user) = coord("template");
        let path = c.charset_export_template().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.lines().next().unwrap().contains("不会被自动读取"));
        assert!(text.contains("key: my_class_1"));
        // 直接加载模板也合法——得到一个空的自建类。
        let out = c.charset_import_file(&path).unwrap();
        assert!(!out[0].builtin);
        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }
}
