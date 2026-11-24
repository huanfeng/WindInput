//! 字符类的设置页读写：列类、改类属性、新建 / 删自建类、恢复默认、清理被它压住的逐条覆盖。
//!
//! 设计见 `docs/design/charset-classification.md`。用户层是 `data/charsets/<key>.yaml`
//! （`wind_store::charsets`）；要整份手改走词库页的「编辑文件」（`data.beginEdit`
//! `kind: charsets`），载入时按出厂校验。
//!
//! # 两种操作的差别必须让用户看见（§7.3）
//!
//! | | 改类的 `default` | 逐条覆盖（候选右键 / 词库管理） |
//! |---|---|---|
//! | 作用面 | 整个类，不问词库里有没有 | 只作用于点过的那个字 |
//! | 跟随出厂更新 | ✅ 出厂给类补了新成员，一并生效 | 只有当时扫到的那些 |
//! | 可逆 | 改回去即可 | 要反向再来一次 |
//! | 优先级 | 低 | 高，压住前者 |
//!
//! 最后一行是「配了没反应」的现成来源：库里躺着上千条逐条覆盖时，改类的 `default` 毫无反应。
//! 故 [`CharsetClassRow::override_count`] 必须显示出来，并给一个「清理冗余」的出口。

use std::collections::BTreeMap;

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
    /// 出厂类（data / custom 两层里有这个 key）。`ranges` 只读、不能删，只能「恢复默认」。
    /// 反之是用户自建类：范围可改、可删。
    pub builtin: bool,
    /// 用户层对这个类有调整。决定「恢复默认」能不能点。
    pub overridden: bool,
    /// 有多少条逐条覆盖压在这个类上（§7.2）。
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

        // 被停用的类不在 registry 里（装配时就 remove 了），但必须列出来——否则
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
    /// 写库与热载必须在同一处完成。分开的话就多出一条「写了但没生效」的路径，
    /// 而它的表现是「设置页改完没反应、重启才生效」——本仓反复出现的那类缺陷。
    pub(crate) fn charset_edit(&self, key: &str, edit: &CharsetEdit) -> anyhow::Result<()> {
        // 存在性判据不能只看 registry：被停用的类不在里面，只看它的话用户停用
        // 之后就再也启用不回来（报「没有名为 X 的字符类」）。用户层里留着那份 `key: X`
        // 的调整，正是「这个类存在过」的凭据。
        let mut doc = self.user_doc(key)?;
        anyhow::ensure!(
            self.engine_mgr.charsets().class_by_key(key).is_some()
                || !charset_def::is_empty_override(&doc),
            "没有名为「{key}」的字符分类"
        );

        apply_edit(&mut doc, edit)?;
        self.save_user_doc(&doc)?;
        self.reload_charsets();
        debug!("字符类「{key}」已更新：{edit:?}");
        Ok(())
    }

    /// 新建一个空的自建分类：只有文件名（key）、显示名与可选的默认判定 / 优先级。
    /// 成员与码位范围之后在设置页用「编辑文件」补。
    pub(crate) fn charset_create(
        &self,
        key: &str,
        name: &str,
        edit: &CharsetEdit,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            charset_def::is_valid_key(key),
            "文件名不合法：不能为空、首尾不能有空格，不能含 / \\ . : * ? \" < > |，最多 64 字节"
        );
        // 只差大小写也算重名：一类一个文件，Windows 的文件名不分大小写。用户层按文件
        // 判（不按解析结果）：手改写坏的那份解析不出来、列表里看不见，新建同名会把它整份覆盖。
        let lower = key.to_lowercase();
        let files: Vec<String> = self
            .charset_store()?
            .list_charset_docs()?
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let taken = self
            .engine_mgr
            .charset_factory()
            .keys()
            .chain(files.iter())
            .any(|k| k.to_lowercase() == lower);
        anyhow::ensure!(!taken, "已有名为「{key}」的字符分类");
        let name = name.trim();
        let mut doc = CharsetDoc {
            def: charset_def::CharsetDef {
                key: key.to_string(),
                name: Some(if name.is_empty() { key } else { name }.to_string()),
                ..Default::default()
            },
            added: Vec::new(),
            removed: Vec::new(),
        };
        apply_edit(&mut doc, edit)?;
        // 不给优先级就排在出厂的常用汉字表（10）之前——留空走默认 100 的话，
        // 对汉字的表态会被它整个盖住，见 NEW_CLASS_ORDER。
        doc.def.order.get_or_insert(NEW_CLASS_ORDER);
        self.save_user_doc(&doc)?;
        self.reload_charsets();
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

    /// 删一个自建类。出厂类不能删，只能停用或恢复默认。
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
            "没有名为「{key}」的字符分类"
        );
        self.reload_charsets();
        Ok(())
    }

    /// 清理压在某个类上的冗余逐条覆盖：方向与当前默认判定相同的那些。
    ///
    /// 判据复用写端已有的那条（`is_cluster_common_by_default(k) != common` 才值得留），
    /// 不新写一份——两处判据分叉的表现是「设置页说有 N 条，清理完还剩几条」。
    pub(crate) fn charset_clear_redundant(
        &self,
        key: &str,
    ) -> anyhow::Result<CharsetCleanupOutcome> {
        let store = self.charset_store()?;
        let reg = self.engine_mgr.charsets();
        anyhow::ensure!(
            reg.class_by_key(key).is_some(),
            "没有名为「{key}」的字符分类"
        );

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
    /// 归属按 [`wind_candidate::CharsetRegistry::class_of`]——它给的是仲裁赢家，
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
            .ok_or_else(|| anyhow::anyhow!("无持久化存储，字符分类的调整无处落盘"))
    }

    /// 用户层现有的全部调整，按 key 索引。没有 store 时为空。
    ///
    /// 与装配走的是同一个 `user_docs_from_store`：读写两侧必须同源，否则设置页读 A、
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
    /// 给空壳而不是 `None`：调用方拿到它就能直接改字段再存回去，不必在「有没有这条」
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

    /// 存一份调整：空壳则删记录（与 `compat.toml` 的 `update_user_raw` 同一条纪律：
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

/// 新建的自建类缺省的 `order`，比所有出厂类都小。
///
/// 不走 [`charset_def::DEFAULT_ORDER`]（100）：`common_han` 以 `scope: han` 对整个汉字域表态，
/// 缺省 100 的自建类排在它后面永不生效，且 `shadowed_keys` 查不出这种形态（见设计文档 §6.5.1）。
/// 也不改 `DEFAULT_ORDER`：那会让已有自建类的仲裁结果静默翻转。
///
/// 代价：新类（5）会压过走 `DEFAULT_ORDER` 的老自建类；新类彼此打平，靠 key 字典序破；
/// 钉住它的测试只喂 `data/`，定制层 `data_custom/` 发 `order < 5` 的类测试不会响。
const NEW_CLASS_ORDER: i32 = 5;

/// 把一次编辑叠加到用户层的那份 doc 上。
///
/// `"inherit"` 写成 `None` 而不是某个具体值：那是「本层不表态、沿用下层」，与
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
#[path = "handle_charset_tests.rs"]
mod tests;
