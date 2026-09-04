//! 字符类的设置页读写：列类、改类属性、恢复默认、清理被它压住的逐条覆盖。
//!
//! 设计见 `docs/design/charset-classification.md`。落点是 `{user_config}/charsets/*.yaml`
//! （**不进 redb**，§3.3：定义要能手写、能分享、服务在线时能看）。
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

use tracing::{debug, warn};
use wind_config::Config;
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
    /// 内置区块类——`ranges` 只读（§4.3：改范围请新建自己的类）。
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

impl crate::Coordinator {
    /// 全部字符类，按 `order` 升序（与仲裁顺序一致，用户看到的先后就是生效的先后）。
    pub(crate) fn charset_rows(&self) -> Vec<CharsetClassRow> {
        let reg = self.engine_mgr.charsets();
        let user = self.user_docs();
        let counts = self.override_counts_by_class();

        let mut rows: Vec<CharsetClassRow> = reg
            .classes()
            .iter()
            .map(|c| {
                let doc = user.get(&c.key);
                CharsetClassRow {
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
                    builtin: doc.is_none_or(|d| d.def.ranges.is_none()) && !c.ranges.is_empty(),
                    overridden: doc.is_some(),
                    override_count: counts.get(&c.key).copied().unwrap_or(0),
                }
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
                    builtin: false,
                    overridden: true,
                    override_count: counts.get(key).copied().unwrap_or(0),
                });
            }
        }
        rows.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));
        rows
    }

    /// 改一个类的属性：写用户层 + 立即热载。
    ///
    /// ⚠️ **写盘与热载必须在同一处完成**。分开的话就多出一条「写了但没生效」的路径，
    /// 而它的表现是「设置页改完没反应、重启才生效」——本仓反复出现的那类缺陷。
    pub(crate) fn charset_edit(&self, key: &str, edit: &CharsetEdit) -> anyhow::Result<()> {
        let Some(dir) = Config::user_config_dir() else {
            anyhow::bail!("没有用户配置目录，改动无处落盘");
        };
        self.charset_edit_in(&dir, key, edit)
    }

    /// [`Self::charset_edit`] 的实体，落点由调用方给。
    ///
    /// ⚠️ 拆这一层是为了**测试不去写用户的真实配置目录**：`Config::user_config_dir()`
    /// 是进程全局的，测试直接调外层就会在开发者自己的 `%APPDATA%` 里留下文件。
    pub(crate) fn charset_edit_in(
        &self,
        dir: &std::path::Path,
        key: &str,
        edit: &CharsetEdit,
    ) -> anyhow::Result<()> {
        // ⚠️ 存在性判据不能只看 registry：**被停用的类不在里面**，只看它的话用户停用
        // 之后就再也启用不回来（报「没有名为 X 的字符类」）。用户层里留着那份 `key: X`
        // 的调整，正是「这个类存在过」的凭据。
        let mut doc = charset_def::load_user_doc(dir, key);
        anyhow::ensure!(
            self.engine_mgr.charsets().class_by_key(key).is_some()
                || !charset_def::is_empty_override(&doc),
            "没有名为「{key}」的字符类"
        );

        apply_edit(&mut doc, edit)?;
        charset_def::save_user_doc(dir, &doc)?;
        self.reload_charsets();
        debug!("字符类「{key}」已更新：{edit:?}");
        Ok(())
    }

    /// 撤掉用户层对某个类的全部调整（删那份文件）。
    pub(crate) fn charset_reset(&self, key: &str) -> anyhow::Result<()> {
        let Some(dir) = Config::user_config_dir() else {
            anyhow::bail!("没有用户配置目录");
        };
        self.charset_reset_at(&dir, key)
    }

    /// [`Self::charset_reset`] 的实体，落点由调用方给（理由同 `charset_edit_in`）。
    pub(crate) fn charset_reset_at(&self, dir: &std::path::Path, key: &str) -> anyhow::Result<()> {
        let empty = CharsetDoc {
            def: wind_config::charset_def::CharsetDef {
                key: key.to_string(),
                ..Default::default()
            },
            added: Vec::new(),
            removed: Vec::new(),
        };
        charset_def::save_user_doc(dir, &empty)?;
        self.reload_charsets();
        Ok(())
    }

    /// 清理压在某个类上的**冗余**逐条覆盖：方向与当前默认判定相同的那些。
    ///
    /// ★ 判据复用写端已有的那条（`is_cluster_common_by_default(k) != common` 才值得留），
    /// 不新写一份——两处判据分叉的表现是「设置页说有 N 条，清理完还剩几条」。
    pub(crate) fn charset_clear_redundant(
        &self,
        key: &str,
    ) -> anyhow::Result<CharsetCleanupOutcome> {
        let Some(store) = self.store.as_ref() else {
            anyhow::bail!("无持久化存储");
        };
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

    /// 重新装配 registry（改完 `charsets/` 后立即生效）。
    fn reload_charsets(&self) {
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
    /// 用户层现有的全部调整，按 key 索引。
    ///
    /// ★ 目录问 [`wind_engine::manager::EngineManager::charsets_user_dir`] 而不是直接调
    /// `Config::user_config_dir()`：装配用的是前者，读写两侧必须同源，否则设置页读 A、
    /// 装配扫 B，表现就是「改了没反应」而两边各自看都没错。
    fn user_docs(&self) -> BTreeMap<String, CharsetDoc> {
        let Some(dir) = self.engine_mgr.charsets_user_dir() else {
            return BTreeMap::new();
        };
        charset_def::load_layer(&dir.join(charset_def::CHARSETS_DIR_NAME))
            .into_iter()
            .map(|d| (d.def.key.clone(), d))
            .collect()
    }
}

/// 把一次编辑叠加到用户层的那份 doc 上。
///
/// ★ `"inherit"` 写成 `None` 而不是某个具体值：那是「本层不表态、沿用下层」，与
/// 「显式设成常用」不是一回事——前者跟随出厂更新，后者钉死。
fn apply_edit(doc: &mut CharsetDoc, edit: &CharsetEdit) -> anyhow::Result<()> {
    if let Some(v) = &edit.default_common {
        doc.def.default = match v.as_str() {
            "common" => Some(Commonality::Common),
            "rare" => Some(Commonality::Rare),
            "inherit" => None,
            other => anyhow::bail!("default 只能是 common / rare / inherit，收到「{other}」"),
        };
    }
    if let Some(v) = edit.no_freq {
        doc.def.no_freq = Some(v);
    }
    if let Some(v) = edit.in_rare {
        doc.def.in_rare = Some(v);
    }
    if let Some(v) = edit.enabled {
        // ⚠️ `enabled: true` 存成 `None` 而不是 `Some(true)`：真值就是默认值，写进去只会
        // 留一条永远等于默认的记录，还让「恢复默认」判不出这份 doc 其实是空的。
        doc.def.enabled = (!v).then_some(false);
    }
    if let Some(v) = edit.order {
        doc.def.order = Some(v);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! 设置页的端到端契约：**从改一个类，到候选判定真的变了**。
    //!
    //! ★ 本仓最典型的失效形态是「配置四层就位、消费点却在不可达的调用点上」——开关配了
    //! 毫无反应，且没有任何报错。这组测试从 `charset_edit_in` 的入参出发一路走到常用性
    //! 判定，中间任何一环断掉都会红：
    //!
    //! ```text
    //! charset_edit_in → save_user_doc（写 yaml）
    //!                 → rebuild_charsets（扫目录 + 装配）→ verdict_of → is_string_common
    //! ```
    //!
    //! ⚠️ 用**临时用户目录**（`set_charsets_user_dir`），绝不碰开发者真实的 `%APPDATA%`
    //! ——那样不仅留垃圾，留下的文件还会反过来影响下一次测试。

    use super::*;
    use crate::Coordinator;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

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
        let d = std::env::temp_dir().join(format!("wind_charset_settings_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 带出厂字符类的 headless coordinator，用户层指向临时目录。
    fn coord(tag: &str) -> (Arc<Coordinator>, PathBuf) {
        let user = tmp_user_dir(tag);
        let c = Coordinator::new_headless(Config::default(), Some(&data_dir()));
        c.engine_mgr.set_charsets_user_dir(Some(user.clone()));
        // 目录换了要重装一次，否则读的还是构造时那份（走的是真实用户目录）。
        c.engine_mgr.rebuild_charsets(&Config::default());
        (c, user)
    }

    /// 判定入口：一个字素簇现在算不算常用。
    fn is_common(c: &Coordinator, s: &str) -> bool {
        let cc = c.common_chars.read().unwrap();
        cc.is_string_common(s, &c.engine_mgr.charsets())
    }

    /// ★★ 改 emoji 类的 `default` 到候选判定生效，一条链走通。
    ///
    /// 出厂时 emoji 类**不表态**（`default` 空），故 `😀` 兜底判常用；用户把它设成生僻之后
    /// 立刻改判——不需要重启、不需要切方案。
    #[test]
    fn setting_a_class_default_reaches_the_verdict() {
        let (c, user) = coord("default");

        assert!(is_common(&c, "😀"), "出厂 emoji 类不表态 ⇒ 兜底判常用");

        c.charset_edit_in(
            &user,
            "emoji",
            &CharsetEdit {
                default_common: Some("rare".into()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(!is_common(&c, "😀"), "改完 default 应立即生效");
        assert!(is_common(&c, "我"), "常用汉字不该被带累");

        // ⚠️ 文件真的落盘了，而不是只改了内存镜像。
        let f = user.join("charsets").join("emoji.yaml");
        assert!(f.is_file(), "改动必须落盘，否则重启就没了");
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("default: rare"));
        assert!(
            !text.contains("ranges:"),
            "只写用户改过的字段——写全量会把出厂值固化进用户层"
        );

        let _ = std::fs::remove_dir_all(&user);
    }

    /// `inherit` 是「本层不表态」，与显式设成常用**不是一回事**。
    ///
    /// 前者跟随出厂更新（出厂哪天给 emoji 定了调，用户跟着走），后者钉死。二者在 UI 上是
    /// 三个不同的档，判据是「写出去的 yaml 里还有没有 default 这一行」。
    #[test]
    fn inherit_removes_the_opinion_rather_than_pinning_it() {
        let (c, user) = coord("inherit");
        let f = user.join("charsets").join("emoji.yaml");

        c.charset_edit_in(
            &user,
            "emoji",
            &CharsetEdit {
                default_common: Some("rare".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!is_common(&c, "😀"));

        c.charset_edit_in(
            &user,
            "emoji",
            &CharsetEdit {
                default_common: Some("inherit".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(is_common(&c, "😀"), "改回 inherit 应恢复到不表态");
        assert!(
            !f.exists(),
            "整份调整已空 ⇒ 该删文件而不是留一份只有 key 的空壳"
        );

        let _ = std::fs::remove_dir_all(&user);
    }

    /// 免词频与生僻准入两个并集属性同样是「改完即生效」。
    #[test]
    fn union_flags_take_effect_immediately() {
        let (c, user) = coord("union");
        let reg = c.engine_mgr.charsets();
        assert!(!reg.no_freq("😀"), "出厂不免词频");
        assert!(!reg.in_rare("😀"), "出厂不进生僻模式");

        c.charset_edit_in(
            &user,
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

        let _ = std::fs::remove_dir_all(&user);
    }

    /// `enabled: false` 把一个类整个停掉；恢复之后它回来。
    #[test]
    fn disabling_a_class_removes_it_and_enabling_brings_it_back() {
        let (c, user) = coord("enabled");
        assert!(c.engine_mgr.charsets().class_by_key("emoji").is_some());

        c.charset_edit_in(
            &user,
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

        c.charset_edit_in(
            &user,
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
        assert!(
            !user.join("charsets").join("emoji.yaml").exists(),
            "enabled=true 是默认值，不该留一条永远等于默认的记录"
        );

        let _ = std::fs::remove_dir_all(&user);
    }

    /// ★★ 停用的类**仍要出现在列表里**，否则「停用」是不可逆的。
    ///
    /// 停用的类不在 registry 里（装配时就 remove 了）。只列 registry 的话，用户点完停用
    /// 那一行就从界面上消失了，再也没有入口把它启用回来——而配置文件里它还在。
    #[test]
    fn a_disabled_class_stays_visible_so_it_can_be_re_enabled() {
        let (c, user) = coord("visible");
        c.charset_edit_in(
            &user,
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

        let _ = std::fs::remove_dir_all(&user);
    }

    /// `charset_reset` 撤掉这个类的全部调整。
    #[test]
    fn resetting_drops_every_adjustment() {
        let (c, user) = coord("reset");
        c.charset_edit_in(
            &user,
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

        c.charset_reset_at(&user, "emoji").unwrap();
        assert!(is_common(&c, "😀"), "恢复默认后判定该回到出厂");
        assert!(!c.engine_mgr.charsets().no_freq("😀"));
        assert!(!user.join("charsets").join("emoji.yaml").exists());

        let _ = std::fs::remove_dir_all(&user);
    }

    /// 不存在的类要报错，不能静默写出一份永远不生效的文件。
    #[test]
    fn editing_an_unknown_class_fails_loudly() {
        let (c, user) = coord("unknown");
        let e = c
            .charset_edit_in(
                &user,
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
            !user.join("charsets").is_dir()
                || std::fs::read_dir(user.join("charsets")).unwrap().count() == 0,
            "拒绝之后不该留下任何文件"
        );
        let _ = std::fs::remove_dir_all(&user);
    }

    /// `default` 只认三个值，别的当场报错。
    ///
    /// ⚠️ 静默忽略非法值的表现是「点了没反应」，而 RPC 调用方（设置页）拿不到任何线索。
    #[test]
    fn an_invalid_default_value_is_rejected() {
        let (c, user) = coord("badvalue");
        let e = c
            .charset_edit_in(
                &user,
                "emoji",
                &CharsetEdit {
                    default_common: Some("maybe".into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(e.to_string().contains("common / rare / inherit"), "{e}");
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 列表把出厂那些类都列出来，且顺序 = 仲裁顺序。
    #[test]
    fn rows_list_every_class_in_arbitration_order() {
        let (c, user) = coord("rows");
        let rows = c.charset_rows();

        for key in ["emoji", "common_han", "符号", "基本汉字", "其它"] {
            assert!(rows.iter().any(|r| r.key == key), "字符类列表里缺 {key}");
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

        let _ = std::fs::remove_dir_all(&user);
    }
}
