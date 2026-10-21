//! 用户短语存储（redb）
//!
//! 与 Go 版本 `wind_input/internal/store/phrases.go` 对齐。短语是**全局**的（不分方案）：
//! code（触发码）→ text（上屏内容，可为字面量或 cmdbar 模板如 `$date`）。
//!
//! P3 起真源是 `<userdata>/user/phrases.dict.yaml`（设计 §2.2）：系统短语来自
//! `system.phrases.toml`（协调器经 [`Store::set_system_phrases`] 交进来，只在内存），文件里只记
//! 用户行与「对系统条目的修改」行（`overrides_system = 1`），读时叠加。旧 redb `PHRASES` 表
//! （key=`"{code}\0{text}"`，value = PhraseValue 的 JSON）只供迁移读。

use crate::store::{PHRASES, Store};
use serde::{Deserialize, Serialize};

/// 待同步的系统短语（来自 TOML，已做 platform 过滤）。
#[derive(Debug, Clone)]
pub struct SystemPhrase {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub position: i32,
    /// 分类（`""` = 未分类）。供方案级 `[phrases] categories` 过滤，见
    /// `docs/design/schema-scoped-behavior.md` §6。
    pub category: String,
    /// 出厂启用（TOML `enabled`，缺省开）。用户的开关写在修改行里，优先于它。
    pub enabled: bool,
}

/// 系统短语同步统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

/// 短语记录（code/text 来自 key，其余来自 value）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhraseRecord {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub position: i32,
    pub enabled: bool,
    pub is_system: bool,
    /// 这条**用户短语遮蔽了同键的系统条目**（`add_phrase` / wdict 导入撞上原系统行）。
    ///
    /// 主键只有 `(code, text)` 一把，系统的与用户的「同款」短语在库里无法并存两行。归属规则
    /// 是**用户优先**：撞键时该行转为用户行（`is_system=false`），系统条目随之从系统短语列表
    /// 隐去，输入期生效的是用户那条。本位只记录「它曾是系统条目」，供 UI 标注来源。
    ///
    /// 与 `sync_system_phrases` 的既有语义一致——那里对用户行的处理正是「跳过，让用户行
    /// 遮蔽同键的系统条目」。可逆性由两条路保证：「系统恢复默认」经
    /// [`Store::reclaim_system_phrases`] 认领回系统归属；「清空用户短语」删掉该行后
    /// sync 会重新插入系统条目（调用方须补一次 sync，见 `resync_system_phrases_after_user_reset`）。
    #[serde(default)]
    pub overrides_system: bool,
    /// 分类（`""` = 未分类）。方案级 `[phrases]` 按它做白/黑名单过滤。
    ///
    /// ⚠️ 分类 UI 落地之前所有存量记录都是 `""`，此时任何非空白名单都会把短语全部滤掉。
    #[serde(default)]
    pub category: String,
}

/// value 部分（text/code 存于 key）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PhraseValue {
    #[serde(default)]
    weight: i32,
    #[serde(default)]
    position: i32,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    is_system: bool,
    #[serde(default)]
    overrides_system: bool,
    /// 分类（`""` = 未分类）。`serde(default)` 保证既有记录反序列化后为空串。
    #[serde(default)]
    category: String,
}

/// 分发导入时一条短语相对本地库的落点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseImportStatus {
    /// 库里没有，会新增。
    New,
    /// 已有同款**用户**短语，导入是空操作。
    ExistsUser,
    /// 撞上同款**系统**短语——导入会把它转成用户行并遮蔽系统条目
    /// （见 [`PhraseRecord::overrides_system`]）。这是接收者最需要被告知的一种。
    ShadowsSystem,
}

/// 分发导入结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhraseImportReport {
    pub added: usize,
    pub skipped_existing: usize,
    /// `added` 中遮蔽了系统条目的条数（是 `added` 的子集，不另计）。
    pub shadowed_system: usize,
}

/// 新建用户短语的默认权重（手动新增与分发导入共用，两处各写一份必然漂移）。
///
/// 短语与码表精确候选**按权重竞争**（`PHRASE_WEIGHT_BASE`(40M) 类别硬顶已删除，
/// 见 candidate-sorting-rules.md §5.1），所以这个默认值直接决定「新建的短语打不打得出」。
/// 此处曾是 1 —— 那会输给几乎每一条码表词条（五笔主库 min=120），在 40M 时代无所谓，
/// 现在是让新短语默认沉底。
///
/// 取 1800 的依据：五笔主库 median=941、p99=9000，1800 越过约 90% 的条目，
/// 又留足余量给用户手动上调（约定值域 0~10000）。与系统短语常用档 800~2000 同轴。
pub const DEFAULT_USER_PHRASE_WEIGHT: i32 = 1800;

fn default_true() -> bool {
    true
}

/// key: "{code}\0{text}"
fn phrase_key(code: &str, text: &str) -> String {
    format!("{code}\u{0}{text}")
}

/// 拆分 key → (code, text)
fn split_phrase_key(key: &str) -> Option<(&str, &str)> {
    let mut it = key.splitn(2, '\u{0}');
    Some((it.next()?, it.next()?))
}

/// 短语文件的列（设计 §2.2）。
pub const PHRASE_COLS: &[&str] = &[
    "code",
    "text",
    "weight",
    "position",
    "enabled",
    "category",
    "overrides_system",
];

/// 文件里的一行。`overrides_system = true`：对同键系统条目的修改（归属仍是系统）；否则是用户行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRow {
    code: String,
    text: String,
    weight: i32,
    position: i32,
    enabled: bool,
    category: String,
    overrides_system: bool,
}

impl FileRow {
    fn fields(&self) -> Vec<String> {
        vec![
            self.code.clone(),
            self.text.clone(),
            self.weight.to_string(),
            self.position.to_string(),
            if self.enabled { "1" } else { "0" }.into(),
            self.category.clone(),
            if self.overrides_system { "1" } else { "0" }.into(),
        ]
    }

    fn key(&self) -> (String, String) {
        (self.code.clone(), self.text.clone())
    }
}

fn file_rows(doc: &wind_userdata::text::table::TableDoc) -> Vec<FileRow> {
    let flag = |s: &str, default: bool| match s.trim() {
        "" => default,
        v => v != "0",
    };
    doc.rows
        .iter()
        .filter(|r| !doc.get(r, "code").is_empty() && !doc.get(r, "text").is_empty())
        .map(|r| FileRow {
            code: doc.get(r, "code").to_string(),
            text: doc.get(r, "text").to_string(),
            weight: doc
                .get(r, "weight")
                .trim()
                .parse()
                .unwrap_or(DEFAULT_USER_PHRASE_WEIGHT),
            position: doc.get(r, "position").trim().parse().unwrap_or(0),
            enabled: flag(doc.get(r, "enabled"), true),
            category: doc.get(r, "category").to_string(),
            overrides_system: flag(doc.get(r, "overrides_system"), false),
        })
        .collect()
}

/// 叠加视图（设计 §2.2）：系统条目（来自 TOML，内存）∪ 文件行。修改行改写同键系统条目的字段、
/// 归属仍是系统；用户行与系统条目同键 ⇒ 遮蔽它（`overrides_system = true`）；系统里已没有的
/// 修改行不呈现（TOML 同步时删掉）。按 `code\0text` 升序。
fn overlay(sys: &[SystemPhrase], rows: &[FileRow]) -> Vec<PhraseRecord> {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<(String, String), PhraseRecord> = sys
        .iter()
        .map(|e| {
            (
                (e.code.clone(), e.text.clone()),
                PhraseRecord {
                    code: e.code.clone(),
                    text: e.text.clone(),
                    weight: e.weight,
                    position: e.position,
                    enabled: e.enabled,
                    is_system: true,
                    overrides_system: false,
                    category: e.category.clone(),
                },
            )
        })
        .collect();
    for r in rows {
        let in_sys = m.get(&r.key()).is_some_and(|p| p.is_system);
        if r.overrides_system && !in_sys {
            continue;
        }
        m.insert(
            r.key(),
            PhraseRecord {
                code: r.code.clone(),
                text: r.text.clone(),
                weight: r.weight,
                position: r.position,
                enabled: r.enabled,
                is_system: r.overrides_system,
                overrides_system: !r.overrides_system && in_sys,
                category: r.category.clone(),
            },
        );
    }
    m.into_values().collect()
}

impl Store {
    /// 用户短语文件：`<text_dir>/phrases.dict.yaml`（全局，不分方案）。
    pub fn phrases_path(&self) -> std::path::PathBuf {
        self.user_text_dir().join("phrases.dict.yaml")
    }

    pub(crate) fn phrase_table(&self) -> std::sync::Arc<wind_userdata::text::table::TextTable> {
        self.phrase_table
            .get_or_init(|| {
                std::sync::Arc::new(wind_userdata::text::table::TextTable::open(
                    self.phrases_path(),
                    PHRASE_COLS,
                    "phrases",
                ))
            })
            .clone()
    }

    fn system_phrases(&self) -> std::sync::Arc<Vec<SystemPhrase>> {
        self.phrase_sys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn file_phrase_rows(&self) -> Vec<FileRow> {
        file_rows(&self.phrase_table().doc())
    }

    /// 改写若干键的文件行（`None` = 删掉该键的行），一次提交。写前按文件现状重新解析（手改优先）。
    fn put_phrase_rows(
        &self,
        rows: Vec<((String, String), Option<FileRow>)>,
    ) -> anyhow::Result<()> {
        self.check_not_editing(&self.phrases_path())?;
        if rows.is_empty() {
            return Ok(());
        }
        let edits: Vec<_> = rows
            .into_iter()
            .map(|((c, t), r)| (vec![c, t], r.map(|r| vec![r.fields()]).unwrap_or_default()))
            .collect();
        self.phrase_table().edit(&edits, &["code", "text"])
    }

    fn find_phrase(&self, code: &str, text: &str) -> Option<PhraseRecord> {
        self.phrase_table().sync();
        overlay(&self.system_phrases(), &self.file_phrase_rows())
            .into_iter()
            .find(|p| p.code == code && p.text == text)
    }

    /// 设定系统短语（来自 `system.phrases.toml`，已做 platform 过滤）。协调器每次启动都调；短语迁移
    /// 标记缺失时先把旧表迁成文本（要 TOML 才分得清系统行被改过没有）。
    pub fn set_system_phrases(&self, entries: &[SystemPhrase]) -> anyhow::Result<()> {
        *self.phrase_sys.write().unwrap_or_else(|e| e.into_inner()) =
            std::sync::Arc::new(entries.to_vec());
        self.migrate_p3_phrases_if_needed(entries)
    }

    /// 列举全部短语（叠加视图，按 code\0text 升序）。
    pub fn list_phrases(&self) -> anyhow::Result<Vec<PhraseRecord>> {
        Ok(overlay(&self.system_phrases(), &self.file_phrase_rows()))
    }

    /// 新增/覆盖一条用户短语。**归属用户优先**：撞上同键系统条目即遮蔽它（列表里标
    /// [`PhraseRecord::overrides_system`]）；同键的修改行被这条用户行取代。
    pub fn add_phrase(
        &self,
        code: &str,
        text: &str,
        position: i32,
        weight: i32,
    ) -> anyhow::Result<()> {
        self.put_phrase_rows(vec![(
            (code.into(), text.into()),
            Some(FileRow {
                code: code.into(),
                text: text.into(),
                weight,
                position,
                enabled: true,
                // 新建的用户短语未分类。分类目前只能由系统短语 TOML 声明或将来的 GUI 设置。
                category: String::new(),
                overrides_system: false,
            }),
        )])
    }

    /// 删除一条短语的文件行（不存在静默成功）。删的是用户行或修改行；系统条目随之恢复默认。
    pub fn remove_phrase(&self, code: &str, text: &str) -> anyhow::Result<()> {
        self.put_phrase_rows(vec![((code.into(), text.into()), None)])
    }

    /// 编辑短语：可改 code/text/position/weight，保留 enabled 与分类。系统条目不改键 ⇒ 写修改行
    /// （归属仍是系统）；系统条目改键 ⇒ 原键写一行停用的修改行、新键作为用户行。
    pub fn update_phrase(
        &self,
        code: &str,
        text: &str,
        new_code: Option<&str>,
        new_text: Option<&str>,
        position: Option<i32>,
        weight: Option<i32>,
    ) -> anyhow::Result<()> {
        let cur = self.find_phrase(code, text);
        let nc = new_code.unwrap_or(code);
        let nt = new_text.unwrap_or(text);
        let is_system = cur.as_ref().is_some_and(|c| c.is_system);
        let (w0, p0, en, cat) = cur
            .map(|c| (c.weight, c.position, c.enabled, c.category))
            .unwrap_or((0, 0, true, String::new()));
        let moved = nc != code || nt != text;
        let new_row = FileRow {
            code: nc.into(),
            text: nt.into(),
            weight: weight.unwrap_or(w0),
            position: position.unwrap_or(p0),
            enabled: en,
            category: cat.clone(),
            overrides_system: is_system && !moved,
        };
        let mut rows = Vec::new();
        if moved {
            let old = if is_system {
                Some(FileRow {
                    code: code.into(),
                    text: text.into(),
                    weight: w0,
                    position: p0,
                    enabled: false,
                    category: cat,
                    overrides_system: true,
                })
            } else {
                None
            };
            rows.push(((code.to_string(), text.to_string()), old));
        }
        rows.push(((nc.to_string(), nt.to_string()), Some(new_row)));
        self.put_phrase_rows(rows)
    }

    /// 设置启停。系统条目写修改行；都不存在 ⇒ 写一条用户行（同旧语义）。
    pub fn set_phrase_enabled(&self, code: &str, text: &str, enabled: bool) -> anyhow::Result<()> {
        let row = match self.find_phrase(code, text) {
            Some(c) => FileRow {
                code: code.into(),
                text: text.into(),
                weight: c.weight,
                position: c.position,
                enabled,
                category: c.category,
                overrides_system: c.is_system,
            },
            None => FileRow {
                code: code.into(),
                text: text.into(),
                weight: 0,
                position: 0,
                enabled,
                category: String::new(),
                overrides_system: false,
            },
        };
        self.put_phrase_rows(vec![((code.into(), text.into()), Some(row))])
    }

    /// 系统短语同步哈希的文件：`<text_dir>/phrases.sys_hash`。它描述的是「短语文件里的修改行
    /// 已按哪一版系统 TOML 归一过」，所以跟着短语文件（用户目录）走——放本机状态目录的话，换机 /
    /// 清 state 后哈希缺失会触发一次同步，把用户对系统短语的修改重置回出厂。
    pub fn phrase_sys_hash_path(&self) -> std::path::PathBuf {
        self.user_text_dir().join("phrases.sys_hash")
    }

    /// TOML 内容哈希标记（判断是否需要重新同步系统短语），见 [`Self::phrase_sys_hash_path`]。
    pub fn phrase_sys_hash(&self) -> anyhow::Result<Option<String>> {
        let p = self.phrase_sys_hash_path();
        // 跳过说明行（没有说明行的旧文件同样读得出）
        Ok(
            wind_userdata::text::fold::read_text(&p)?.and_then(|(b, _)| {
                let text = String::from_utf8_lossy(&b);
                wind_utils::text::normalize_input(&text)
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_string)
            }),
        )
    }

    pub fn set_phrase_sys_hash(&self, h: &str) -> anyhow::Result<()> {
        let p = self.phrase_sys_hash_path();
        let text = format!("{}\n{h}\n", wind_userdata::text::banner::PHRASE_SYS_HASH);
        wind_userdata::text::fold::commit_text(&p, text.as_bytes(), None)
    }

    /// 旧 META 里的 `phrase_sys_hash`（P4a 迁移用）。
    pub(crate) fn legacy_phrase_sys_hash(&self) -> anyhow::Result<Option<String>> {
        self.meta_get("phrase_sys_hash")
    }

    /// 系统短语 TOML 变了：设定新的系统条目；修改行的 weight / position / category 回到 TOML 值、
    /// 保留 enabled；TOML 里已没有的键，其修改行删掉。用户行不动（遮蔽照旧）。
    pub fn sync_system_phrases(&self, entries: &[SystemPhrase]) -> anyhow::Result<SyncStats> {
        use std::collections::{HashMap, HashSet};
        let prev: HashSet<(String, String)> = self
            .system_phrases()
            .iter()
            .map(|e| (e.code.clone(), e.text.clone()))
            .collect();
        *self.phrase_sys.write().unwrap_or_else(|e| e.into_inner()) =
            std::sync::Arc::new(entries.to_vec());
        self.phrase_table().sync();
        let rows = self.file_phrase_rows();
        let users: HashSet<(String, String)> = rows
            .iter()
            .filter(|r| !r.overrides_system)
            .map(FileRow::key)
            .collect();
        let wanted: HashMap<(String, String), &SystemPhrase> = entries
            .iter()
            .map(|e| ((e.code.clone(), e.text.clone()), e))
            .collect();
        let mut stats = SyncStats::default();
        for k in &prev {
            if !wanted.contains_key(k) && !users.contains(k) {
                stats.removed += 1;
            }
        }
        for k in wanted.keys() {
            if users.contains(k) {
                continue;
            }
            if prev.contains(k) {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
        }
        let mut edits = Vec::new();
        for r in rows.iter().filter(|r| r.overrides_system) {
            match wanted.get(&r.key()) {
                Some(e) => {
                    let nr = FileRow {
                        weight: e.weight,
                        position: e.position,
                        category: e.category.clone(),
                        ..r.clone()
                    };
                    if &nr != r {
                        edits.push((r.key(), Some(nr)));
                    }
                }
                None => edits.push((r.key(), None)),
            }
        }
        self.put_phrase_rows(edits)?;
        Ok(stats)
    }

    /// 系统短语（叠加视图里归属系统的），按 key 升序，不分页。
    pub fn list_system_phrases(&self) -> anyhow::Result<Vec<PhraseRecord>> {
        Ok(self
            .list_phrases()?
            .into_iter()
            .filter(|p| p.is_system)
            .collect())
    }

    /// 用户短语分页（`is_system=false`，含遮蔽了系统条目的行——它们归属用户）。
    /// prefix 非空时按 code/text 包含过滤后再分页。返回 (页内行, 过滤后总数)。
    pub fn list_user_phrases_paged(
        &self,
        prefix: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<PhraseRecord>, usize)> {
        let mut all: Vec<PhraseRecord> = self
            .list_phrases()?
            .into_iter()
            .filter(|p| !p.is_system)
            .collect();
        if let Some(q) = prefix {
            let q = q.trim();
            if !q.is_empty() {
                all.retain(|p| p.code.contains(q) || p.text.contains(q));
            }
        }
        let total = all.len();
        let page = all.into_iter().skip(offset).take(limit).collect();
        Ok((page, total))
    }

    /// 输入期短语集：全部 enabled 短语（系统+用户）。
    pub fn enabled_phrases_for_input(&self) -> anyhow::Result<Vec<PhraseRecord>> {
        Ok(self
            .list_phrases()?
            .into_iter()
            .filter(|p| p.enabled)
            .collect())
    }

    /// 系统"恢复默认"：修改行的开关回到出厂值（TOML `enabled`，缺省开）。返回改动条数。
    ///
    /// 不是一律打开：出厂就关着的条目（时间戳这类）恢复默认后应当仍是关的。
    pub fn reset_system_enabled(&self) -> anyhow::Result<usize> {
        use std::collections::HashMap;
        self.phrase_table().sync();
        let defaults: HashMap<(String, String), bool> = self
            .system_phrases()
            .iter()
            .map(|e| ((e.code.clone(), e.text.clone()), e.enabled))
            .collect();
        let edits: Vec<_> = self
            .file_phrase_rows()
            .into_iter()
            .filter(|r| r.overrides_system)
            .filter_map(|r| {
                let default = defaults.get(&r.key()).copied().unwrap_or(true);
                (r.enabled != default).then(|| {
                    (
                        r.key(),
                        Some(FileRow {
                            enabled: default,
                            ..r
                        }),
                    )
                })
            })
            .collect();
        let n = edits.len();
        self.put_phrase_rows(edits)?;
        Ok(n)
    }

    /// 用户"清空"：删全部用户行（含遮蔽系统条目的），保留对系统条目的修改。返回删除条数。
    /// 被遮蔽的系统条目随之重新呈现，无需再补 sync。
    pub fn reset_user_phrases(&self) -> anyhow::Result<usize> {
        self.phrase_table().sync();
        let edits: Vec<_> = self
            .file_phrase_rows()
            .into_iter()
            .filter(|r| !r.overrides_system)
            .map(|r| (r.key(), None))
            .collect();
        let n = edits.len();
        self.put_phrase_rows(edits)?;
        Ok(n)
    }

    /// 导出全部用户短语为 wdict 文本（与用户短语列表同口径：`is_system=false`，
    /// 含遮蔽系统条目的行——那是用户写下的定义，漏导等于备份丢数据）。
    pub fn export_user_phrases_wdict(&self, exported_at: &str) -> anyhow::Result<String> {
        let rows: Vec<crate::wdict::PhraseIo> = self
            .list_phrases()?
            .into_iter()
            .filter(|p| !p.is_system)
            .map(|p| crate::wdict::PhraseIo {
                code: p.code,
                text: p.text,
                weight: p.weight,
                position: p.position,
                enabled: p.enabled,
            })
            .collect();
        Ok(crate::wdict::export_phrases_wdict(&rows, exported_at))
    }

    /// 导入用户短语（合并 upsert，一次提交）。返回 (导入条数, 跳过条数)。撞上同键系统条目即遮蔽它。
    pub fn import_user_phrases_wdict(&self, text: &str) -> anyhow::Result<(usize, usize)> {
        let (rows, skipped) =
            crate::wdict::parse_phrases_wdict(text).map_err(|e| anyhow::anyhow!(e))?;
        let imported = rows.len();
        let edits = rows
            .into_iter()
            .map(|r| {
                let row = FileRow {
                    code: r.code,
                    text: r.text,
                    weight: r.weight,
                    position: r.position,
                    enabled: r.enabled,
                    // 分发导入的短语不带分类（`PhraseIo` 是跨设备传输格式），导入后落为未分类。
                    category: String::new(),
                    overrides_system: false,
                };
                (row.key(), Some(row))
            })
            .collect();
        self.put_phrase_rows(edits)?;
        Ok((imported, skipped))
    }

    /// 分发导入时一条短语的落点（只新增、不改动既有条目）。
    pub fn plan_phrase_import(
        &self,
        items: &[(String, String)],
    ) -> anyhow::Result<Vec<PhraseImportStatus>> {
        let all = self.list_phrases()?;
        Ok(items
            .iter()
            .map(
                |(code, text)| match all.iter().find(|p| &p.code == code && &p.text == text) {
                    None => PhraseImportStatus::New,
                    Some(cur) if cur.is_system => PhraseImportStatus::ShadowsSystem,
                    Some(_) => PhraseImportStatus::ExistsUser,
                },
            )
            .collect())
    }

    /// 分发导入：新条目**追加到末尾**（position 由本地重新分配），已存在的用户短语原样跳过
    /// （连 enabled 也不动）。一次提交。`text` 必须已是存储域文本。
    pub fn import_phrases_appending(
        &self,
        items: &[(String, String)],
        weight: i32,
    ) -> anyhow::Result<PhraseImportReport> {
        let all = self.list_phrases()?;
        let mut next_pos = all
            .iter()
            .map(|p| p.position)
            .max()
            .unwrap_or(-1)
            .saturating_add(1);
        let mut rep = PhraseImportReport::default();
        let mut edits = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (code, text) in items {
            if !seen.insert((code.clone(), text.clone())) {
                rep.skipped_existing += 1;
                continue;
            }
            match all.iter().find(|p| &p.code == code && &p.text == text) {
                Some(cur) if !cur.is_system => {
                    rep.skipped_existing += 1;
                    continue;
                }
                Some(_) => rep.shadowed_system += 1,
                None => {}
            }
            let row = FileRow {
                code: code.clone(),
                text: text.clone(),
                weight,
                position: next_pos,
                enabled: true,
                category: String::new(),
                overrides_system: false,
            };
            edits.push((row.key(), Some(row)));
            next_pos = next_pos.saturating_add(1);
            rep.added += 1;
        }
        self.put_phrase_rows(edits)?;
        Ok(rep)
    }

    /// 设定系统条目（同 [`Self::set_system_phrases`]，不改任何文件行）；返回此前不在系统列表、
    /// 也没有文件行的条目数——即「补回来」的条数。系统条目总在叠加视图里，清空用户短语后被遮蔽
    /// 的条目自动重新呈现，本函数只为保持旧接口的语义。
    pub fn ensure_system_phrases(&self, entries: &[SystemPhrase]) -> anyhow::Result<usize> {
        let before: std::collections::HashSet<(String, String)> = self
            .list_phrases()?
            .into_iter()
            .map(|p| (p.code, p.text))
            .collect();
        self.set_system_phrases(entries)?;
        Ok(entries
            .iter()
            .filter(|e| !before.contains(&(e.code.clone(), e.text.clone())))
            .count())
    }

    /// 「恢复默认」：键在 TOML 里的用户行**认领回系统**（转成修改行，字段保留；紧随其后的
    /// `sync_system_phrases` 把 weight/position 刷回 TOML）。返回认领条数。
    pub fn reclaim_system_phrases(&self, entries: &[SystemPhrase]) -> anyhow::Result<usize> {
        let keys: std::collections::HashSet<(String, String)> = entries
            .iter()
            .map(|e| (e.code.clone(), e.text.clone()))
            .collect();
        self.phrase_table().sync();
        let edits: Vec<_> = self
            .file_phrase_rows()
            .into_iter()
            .filter(|r| !r.overrides_system && keys.contains(&r.key()))
            .map(|r| {
                (
                    r.key(),
                    Some(FileRow {
                        overrides_system: true,
                        ..r
                    }),
                )
            })
            .collect();
        let n = edits.len();
        self.put_phrase_rows(edits)?;
        Ok(n)
    }

    /// 重置为默认：清空短语文件（用户行与修改行），返回删除条数。
    pub fn reset_phrases(&self) -> anyhow::Result<usize> {
        let n = self.file_phrase_rows().len();
        self.check_not_editing(&self.phrases_path())?;
        self.phrase_table().replace_all(&[])?;
        Ok(n)
    }

    /// 手改的短语文件重新载入。变了返回 true（调用方据此重建输入期短语层）。
    pub fn sync_phrases(&self) -> bool {
        self.phrase_table().sync()
    }

    /// 测试 / 探针：往旧 `PHRASES` 表写一条（模拟升级前的库）。
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn legacy_put_phrase(
        &self,
        code: &str,
        text: &str,
        weight: i32,
        position: i32,
        enabled: bool,
        is_system: bool,
        overrides_system: bool,
        category: &str,
    ) -> anyhow::Result<()> {
        let key = phrase_key(code, text);
        let bytes = serde_json::to_vec(&PhraseValue {
            weight,
            position,
            enabled,
            is_system,
            overrides_system,
            category: category.into(),
        })?;
        self.with_db(|db| {
            let txn = db.begin_write()?;
            txn.open_table(PHRASES)?
                .insert(key.as_str(), bytes.as_slice())?;
            txn.commit()?;
            Ok(())
        })
    }

    /// 旧 `PHRASES` 表全部行（迁移用）。
    pub(crate) fn legacy_phrase_rows(&self) -> anyhow::Result<Vec<PhraseRecord>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(PHRASES)?;
            let mut out = Vec::new();
            for item in t.range::<&str>(..)? {
                let (k, v) = item?;
                if let (Some((code, text)), Ok(val)) = (
                    split_phrase_key(k.value()),
                    serde_json::from_slice::<PhraseValue>(v.value()),
                ) {
                    out.push(PhraseRecord {
                        code: code.to_string(),
                        text: text.to_string(),
                        weight: val.weight,
                        position: val.position,
                        enabled: val.enabled,
                        is_system: val.is_system,
                        overrides_system: val.overrides_system,
                        category: val.category,
                    });
                }
            }
            Ok(out)
        })
    }

    /// 迁移用：把旧表行转成文件行，写入文件里还没有的键（文件里已有的以文件为准），一次提交。
    pub(crate) fn merge_legacy_phrases(&self, sys: &[SystemPhrase]) -> anyhow::Result<usize> {
        use std::collections::{HashMap, HashSet};
        let sysm: HashMap<(&str, &str), &SystemPhrase> = sys
            .iter()
            .map(|e| ((e.code.as_str(), e.text.as_str()), e))
            .collect();
        self.phrase_table().sync();
        let have: HashSet<(String, String)> =
            self.file_phrase_rows().iter().map(FileRow::key).collect();
        let mut edits = Vec::new();
        for p in self.legacy_phrase_rows()? {
            let row = if !p.is_system {
                FileRow {
                    code: p.code,
                    text: p.text,
                    weight: p.weight,
                    position: p.position,
                    enabled: p.enabled,
                    category: p.category,
                    overrides_system: false,
                }
            } else {
                match sysm.get(&(p.code.as_str(), p.text.as_str())) {
                    // TOML 里已没有的旧系统行：同旧 sync，丢弃
                    None => continue,
                    Some(e)
                        if e.weight == p.weight
                            && e.position == p.position
                            && e.category == p.category
                            && p.enabled =>
                    {
                        continue;
                    }
                    Some(_) => FileRow {
                        code: p.code,
                        text: p.text,
                        weight: p.weight,
                        position: p.position,
                        enabled: p.enabled,
                        category: p.category,
                        overrides_system: true,
                    },
                }
            };
            if !have.contains(&row.key()) {
                edits.push((row.key(), Some(row)));
            }
        }
        let n = edits.len();
        self.put_phrase_rows(edits)?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    /// 出厂关闭的系统短语：列表里呈现为关、不进输入期短语集；用户打开后生效；
    /// 「恢复默认」回到出厂的关，而不是一律打开。
    #[test]
    fn system_phrase_default_off_survives_restore_defaults() {
        let path = tmp("wind_phrases_default_off.redb");
        let s = Store::open(&path).unwrap();
        let sys = vec![
            SystemPhrase {
                code: "tmts".into(),
                text: "$ts".into(),
                weight: 800,
                position: 0,
                category: String::new(),
                enabled: false,
            },
            SystemPhrase {
                code: "rq".into(),
                text: "$date".into(),
                weight: 1000,
                position: 0,
                category: String::new(),
                enabled: true,
            },
        ];
        s.set_system_phrases(&sys).unwrap();
        let enabled_of = |code: &str| {
            s.list_phrases()
                .unwrap()
                .into_iter()
                .find(|p| p.code == code)
                .map(|p| p.enabled)
        };
        assert_eq!(enabled_of("tmts"), Some(false), "出厂关闭照样列出、标为关");
        s.set_phrase_enabled("tmts", "$ts", true).unwrap();
        s.set_phrase_enabled("rq", "$date", false).unwrap();
        assert_eq!(enabled_of("tmts"), Some(true), "用户打开后生效");
        s.reset_system_enabled().unwrap();
        assert_eq!(enabled_of("tmts"), Some(false), "恢复默认回到出厂的关");
        assert_eq!(enabled_of("rq"), Some(true), "出厂开的恢复成开");
    }

    #[test]
    fn sync_system_phrases_add_update_remove() {
        let path = tmp("wind_phrases_sync.redb");
        let s = Store::open(&path).unwrap();
        // 首轮：加两条系统短语
        let v1 = vec![
            SystemPhrase {
                code: "rq".into(),
                text: "$date".into(),
                weight: 1000,
                position: 0,
                category: String::new(),
                enabled: true,
            },
            SystemPhrase {
                code: "em".into(),
                text: "（＾＿＾）".into(),
                weight: 1000,
                position: 0,
                category: String::new(),
                enabled: true,
            },
        ];
        let st = s.sync_system_phrases(&v1).unwrap();
        assert_eq!((st.added, st.updated, st.removed), (2, 0, 0));
        // 用户关掉一条系统短语
        s.set_phrase_enabled("em", "（＾＿＾）", false).unwrap();
        // 次轮：em 改权重 + 删 rq + 加新 nn；em 的 enabled 应保留 false
        let v2 = vec![
            SystemPhrase {
                code: "em".into(),
                text: "（＾＿＾）".into(),
                weight: 500,
                position: 0,
                category: String::new(),
                enabled: true,
            },
            SystemPhrase {
                code: "nn".into(),
                text: "你好".into(),
                weight: 1000,
                position: 0,
                category: String::new(),
                enabled: true,
            },
        ];
        let st2 = s.sync_system_phrases(&v2).unwrap();
        assert_eq!((st2.added, st2.updated, st2.removed), (1, 1, 1));
        let list = s.list_phrases().unwrap();
        let em = list.iter().find(|p| p.code == "em").unwrap();
        assert_eq!(em.weight, 500, "内容更新");
        assert!(!em.enabled, "开关保留");
        assert!(em.is_system);
        assert!(!list.iter().any(|p| p.code == "rq"), "过时系统短语删除");
        assert!(list.iter().any(|p| p.code == "nn"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sync_keeps_user_phrases() {
        let path = tmp("wind_phrases_sync_user.redb");
        let s = Store::open(&path).unwrap();
        s.add_phrase("me", "自定义", 0, 1).unwrap(); // 用户短语 is_system=false
        s.sync_system_phrases(&[SystemPhrase {
            code: "sys".into(),
            text: "系统".into(),
            weight: 1,
            position: 0,
            category: String::new(),
            enabled: true,
        }])
        .unwrap();
        // 再同步（sys 消失）应删 sys 但保留用户 me
        s.sync_system_phrases(&[]).unwrap();
        let list = s.list_phrases().unwrap();
        assert!(
            list.iter().any(|p| p.code == "me"),
            "用户短语不受系统同步影响"
        );
        assert!(!list.iter().any(|p| p.code == "sys"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn phrase_sys_hash_persist() {
        let path = tmp("wind_phrases_hash.redb");
        let s = Store::open(&path).unwrap();
        assert_eq!(s.phrase_sys_hash().unwrap(), None);
        s.set_phrase_sys_hash("abc123").unwrap();
        assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("abc123"));
        let _ = std::fs::remove_file(&path);
    }

    /// 没有说明行的旧标记文件照常读出。
    #[test]
    fn phrase_sys_hash_reads_file_without_banner() {
        let path = tmp("wind_phrases_hash_old.redb");
        let s = Store::open(&path).unwrap();
        std::fs::create_dir_all(s.user_text_dir()).unwrap();
        std::fs::write(s.phrase_sys_hash_path(), "abc123\n").unwrap();
        assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("abc123"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_system_and_user_split() {
        let path = tmp("wind_phrases_split.redb");
        let s = Store::open(&path).unwrap();
        s.sync_system_phrases(&[
            SystemPhrase {
                code: "a".into(),
                text: "甲".into(),
                weight: 1,
                position: 0,
                category: String::new(),
                enabled: true,
            },
            SystemPhrase {
                code: "b".into(),
                text: "乙".into(),
                weight: 1,
                position: 0,
                category: String::new(),
                enabled: true,
            },
        ])
        .unwrap();
        s.add_phrase("u1", "用户一", 0, 1).unwrap();
        s.add_phrase("u2", "用户二", 0, 1).unwrap();
        assert_eq!(s.list_system_phrases().unwrap().len(), 2);
        let (page, total) = s.list_user_phrases_paged(None, 0, 10).unwrap();
        assert_eq!(total, 2);
        assert_eq!(page.len(), 2);
        assert!(page.iter().all(|p| !p.is_system));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn user_paging_and_prefix() {
        let path = tmp("wind_phrases_page.redb");
        let s = Store::open(&path).unwrap();
        for i in 0..5 {
            s.add_phrase(&format!("c{i}"), &format!("词{i}"), 0, 1)
                .unwrap();
        }
        let (p0, total) = s.list_user_phrases_paged(None, 0, 2).unwrap();
        assert_eq!(total, 5);
        assert_eq!(p0.len(), 2);
        let (p2, _) = s.list_user_phrases_paged(None, 4, 2).unwrap();
        assert_eq!(p2.len(), 1, "末页不足一页");
        // prefix 过滤
        let (pf, tf) = s.list_user_phrases_paged(Some("c3"), 0, 10).unwrap();
        assert_eq!(tf, 1);
        assert_eq!(pf[0].code, "c3");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enabled_for_input_and_resets() {
        let path = tmp("wind_phrases_enabled.redb");
        let s = Store::open(&path).unwrap();
        s.sync_system_phrases(&[SystemPhrase {
            code: "a".into(),
            text: "甲".into(),
            weight: 1,
            position: 0,
            category: String::new(),
            enabled: true,
        }])
        .unwrap();
        s.add_phrase("u", "用户", 0, 1).unwrap();
        s.set_phrase_enabled("a", "甲", false).unwrap(); // 禁用系统
        let inp = s.enabled_phrases_for_input().unwrap();
        assert!(inp.iter().all(|p| p.enabled));
        assert!(!inp.iter().any(|p| p.code == "a"), "禁用项不入输入集");
        assert!(inp.iter().any(|p| p.code == "u"));
        // 系统恢复默认：全部重新启用
        let n = s.reset_system_enabled().unwrap();
        assert_eq!(n, 1);
        assert!(
            s.enabled_phrases_for_input()
                .unwrap()
                .iter()
                .any(|p| p.code == "a")
        );
        // 用户清空
        assert_eq!(s.reset_user_phrases().unwrap(), 1);
        assert!(!s.list_phrases().unwrap().iter().any(|p| !p.is_system));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_phrase_crud() {
        let path = tmp("wind_phrases_crud.redb");
        let s = Store::open(&path).unwrap();
        s.add_phrase("rq", "2026-06-20", 0, 1).unwrap();
        s.add_phrase("yx", "user@example.com", 0, 1).unwrap();
        assert_eq!(s.list_phrases().unwrap().len(), 2);

        // 启停
        s.set_phrase_enabled("rq", "2026-06-20", false).unwrap();
        let rq = s
            .list_phrases()
            .unwrap()
            .into_iter()
            .find(|p| p.code == "rq")
            .unwrap();
        assert!(!rq.enabled);

        // 改 code（键迁移）
        s.update_phrase("yx", "user@example.com", Some("mail"), None, None, Some(5))
            .unwrap();
        let list = s.list_phrases().unwrap();
        assert!(list.iter().any(|p| p.code == "mail" && p.weight == 5));
        assert!(!list.iter().any(|p| p.code == "yx"));

        // 删除
        s.remove_phrase("rq", "2026-06-20").unwrap();
        assert_eq!(s.list_phrases().unwrap().len(), 1);

        // 重置清空
        assert_eq!(s.reset_phrases().unwrap(), 1);
        assert_eq!(s.list_phrases().unwrap().len(), 0);
        let _ = std::fs::remove_file(&path);
    }

    /// 导入撞键与 `add_phrase` 同口径：wdict 导入一条与系统短语同款的记录 → 遮蔽系统条目。
    ///
    /// 导入是撞键的高发路径（用户常在导出文件里手工增删行），归属规则必须与手工新增一致，
    /// 否则同一件事经两条入口会得到相反的归属。
    #[test]
    fn import_duplicate_shadows_system_entry() {
        let path = tmp("wind_phrases_keep_sys.redb");
        let s = Store::open(&path).unwrap();
        let sys = [SystemPhrase {
            code: "date".into(),
            text: "二〇二六年".into(),
            weight: 9,
            position: 5,
            category: String::new(),
            enabled: true,
        }];
        s.sync_system_phrases(&sys).unwrap();

        let wd = crate::wdict::export_phrases_wdict(
            &[crate::wdict::PhraseIo {
                code: "date".into(),
                text: "二〇二六年".into(),
                weight: 7,
                position: 3,
                enabled: true,
            }],
            "2026-07-21T00:00:00+08:00",
        );
        s.import_user_phrases_wdict(&wd).unwrap();

        let (rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
        assert_eq!(total, 1, "导入的条目归用户");
        assert!(rows[0].overrides_system, "记录遮蔽关系");
        assert_eq!((rows[0].weight, rows[0].position), (7, 3), "用导入的定义");
        assert!(s.list_system_phrases().unwrap().is_empty(), "系统条目隐去");

        // 同样可撤销：恢复默认认领回系统。
        assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 1);
        assert_eq!(s.list_system_phrases().unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// 撞键归属：用户建一条与系统短语完全同款的短语 → **用户那条生效**，系统条目隐去。
    ///
    /// ⚠️ 这个位置的行为被反转过两次，方向相反：早期转用户行但**不可自愈**（系统条目再也
    /// 回不来，现象是「系统短语莫名少一条」）；改成保留系统归属后变成反方向的
    /// 「用户新建的短语在用户列表看不到」。现在回到用户优先，本测试同时锁住**撤销路径**
    /// ——缺了它就会退回第一种毛病。
    #[test]
    fn user_phrase_shadows_system_entry() {
        let path = tmp("wind_phrases_shadow_sys.redb");
        let s = Store::open(&path).unwrap();
        let sys = [SystemPhrase {
            code: "date".into(),
            text: "$Y年$M月$D日".into(),
            weight: 1000,
            position: 1,
            category: String::new(),
            enabled: true,
        }];
        s.sync_system_phrases(&sys).unwrap();

        // 用户建了一条一模一样的（同 code 同 text），并给了自己的权重/位置。
        s.add_phrase("date", "$Y年$M月$D日", 9, 5000).unwrap();

        // ① 出现在用户短语列表，且用的是用户写的 weight/position。
        let (user_rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
        assert_eq!(total, 1, "用户建的短语必须在用户列表可见");
        assert_eq!((user_rows[0].weight, user_rows[0].position), (5000, 9));
        assert!(!user_rows[0].is_system, "归属转为用户");
        assert!(user_rows[0].overrides_system, "记录它遮蔽了系统条目");

        // ② 系统短语列表里隐去（只有一行，已归用户）。
        assert!(
            s.list_system_phrases().unwrap().is_empty(),
            "被遮蔽的系统条目从系统列表隐去"
        );

        // ③ 输入期生效的是用户那条。
        let inp = s.enabled_phrases_for_input().unwrap();
        assert_eq!(inp.len(), 1);
        assert_eq!(inp[0].weight, 5000, "输入期用用户定义");

        // ④ 导出（备份）含它，否则用户写的定义备份即丢。
        assert!(
            s.export_user_phrases_wdict("t")
                .unwrap()
                .contains("$Y年$M月$D日"),
            "遮蔽行须随用户短语导出"
        );

        // ⑤ 撤销路径：「系统恢复默认」→ reclaim 认领回系统 + sync 刷回 TOML 定义。
        assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 1);
        s.sync_system_phrases(&sys).unwrap();
        let after = s.list_system_phrases().unwrap();
        assert_eq!(after.len(), 1, "系统条目回来了");
        assert_eq!(
            (after[0].weight, after[0].position),
            (1000, 1),
            "还原成定义值"
        );
        assert!(!after[0].overrides_system);
        assert_eq!(s.list_user_phrases_paged(None, 0, 99).unwrap().1, 0);
        let _ = std::fs::remove_file(&path);
    }

    /// 撤销路径二：「清空用户短语」删掉遮蔽行后，系统条目**立即**重新呈现（P3 叠加模型：系统条目
    /// 来自 TOML、只在内存，从不随用户行一起消失——当年「系统短语莫名少一条」的病根不复存在）。
    #[test]
    fn clearing_user_phrases_restores_shadowed_system_entry() {
        let path = tmp("wind_phrases_clear_restore.redb");
        let s = Store::open(&path).unwrap();
        let sys = [SystemPhrase {
            code: "date".into(),
            text: "$Y年$M月$D日".into(),
            weight: 1000,
            position: 1,
            category: String::new(),
            enabled: true,
        }];
        s.sync_system_phrases(&sys).unwrap();
        s.add_phrase("date", "$Y年$M月$D日", 9, 5000).unwrap();

        // 清空用户短语：遮蔽行归属用户，会被删掉 → 系统条目以 TOML 定义值重新呈现。
        assert_eq!(s.reset_user_phrases().unwrap(), 1);
        let after = s.list_system_phrases().unwrap();
        assert_eq!(after.len(), 1, "系统条目应被补回");
        assert_eq!((after[0].weight, after[0].position), (1000, 1));
        let _ = std::fs::remove_file(&path);
    }

    /// `ensure_system_phrases` **绝不改动已存在的行**（用 sync 代替它，一次「清空用户短语」就会
    /// 把用户在系统短语列表里改过的权重/次序重置回 TOML 默认值）。P3 叠加模型下系统条目总在，
    /// 删掉修改行即恢复默认，没有「缺口」可补，返回 0。
    #[test]
    fn ensure_system_phrases_only_fills_gaps() {
        let path = tmp("wind_phrases_ensure.redb");
        let s = Store::open(&path).unwrap();
        let sys = [
            SystemPhrase {
                code: "a".into(),
                text: "甲".into(),
                weight: 1000,
                position: 1,
                category: String::new(),
                enabled: true,
            },
            SystemPhrase {
                code: "b".into(),
                text: "乙".into(),
                weight: 1000,
                position: 2,
                category: String::new(),
                enabled: true,
            },
        ];
        s.sync_system_phrases(&sys).unwrap();

        // 用户在系统短语列表里改了「甲」的权重、并把「乙」停用。
        s.update_phrase("a", "甲", None, None, Some(9), Some(5555))
            .unwrap();
        s.set_phrase_enabled("b", "乙", false).unwrap();
        // 删掉「乙」的修改行 ⇒ 乙恢复默认（启用），不会出现缺口
        s.remove_phrase("b", "乙").unwrap();

        assert_eq!(s.ensure_system_phrases(&sys).unwrap(), 0, "没有缺口");

        let list = s.list_system_phrases().unwrap();
        let a = list.iter().find(|p| p.code == "a").unwrap();
        assert_eq!(
            (a.weight, a.position),
            (5555, 9),
            "已存在的行一字不改——这正是不能用 sync 的原因"
        );
        assert!(
            list.iter().any(|p| p.code == "b" && p.enabled),
            "删掉修改行 = 恢复默认"
        );

        // 幂等：再补一次不重复插入。
        assert_eq!(s.ensure_system_phrases(&sys).unwrap(), 0);
        let _ = std::fs::remove_file(&path);
    }

    /// 对照：同样的场景下 `sync_system_phrases` **会**重置用户的编辑。
    /// 锁住这个差异，避免日后有人「顺手统一成 sync」。
    #[test]
    fn sync_overwrites_user_edits_on_system_rows() {
        let path = tmp("wind_phrases_sync_overwrites.redb");
        let s = Store::open(&path).unwrap();
        let sys = [SystemPhrase {
            code: "a".into(),
            text: "甲".into(),
            weight: 1000,
            position: 1,
            category: String::new(),
            enabled: true,
        }];
        s.sync_system_phrases(&sys).unwrap();
        s.update_phrase("a", "甲", None, None, Some(9), Some(5555))
            .unwrap();

        s.sync_system_phrases(&sys).unwrap();
        let a = s.list_system_phrases().unwrap().pop().unwrap();
        assert_eq!(
            (a.weight, a.position),
            (1000, 1),
            "sync 以 TOML 为准（供恢复默认/文件变更用），故补齐场景不能用它"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 编辑既有系统短语（改权重）**不**改归属——那是在系统列表里正常调整，不产生遮蔽关系。
    #[test]
    fn editing_system_phrase_keeps_system_ownership() {
        let path = tmp("wind_phrases_edit_no_mark.redb");
        let s = Store::open(&path).unwrap();
        s.sync_system_phrases(&[SystemPhrase {
            code: "em".into(),
            text: "（＾＿＾）".into(),
            weight: 1000,
            position: 0,
            category: String::new(),
            enabled: true,
        }])
        .unwrap();
        s.update_phrase("em", "（＾＿＾）", None, None, None, Some(42))
            .unwrap();
        let row = s.list_system_phrases().unwrap().pop().unwrap();
        assert_eq!(row.weight, 42, "编辑生效");
        assert!(!row.overrides_system, "编辑不产生遮蔽关系");
        assert_eq!(s.list_user_phrases_paged(None, 0, 99).unwrap().1, 0);
        let _ = std::fs::remove_file(&path);
    }

    /// 存量修复：已被降级的行，「恢复默认」应认领回系统归属。
    #[test]
    fn reclaim_restores_downgraded_system_rows() {
        let path = tmp("wind_phrases_reclaim.redb");
        let s = Store::open(&path).unwrap();
        let sys = [SystemPhrase {
            code: "date".into(),
            text: "二〇二六年".into(),
            weight: 9,
            position: 5,
            category: String::new(),
            enabled: true,
        }];
        // 手工制造受损现场：用户行在先，且与系统条目同键
        s.add_phrase("date", "二〇二六年", 0, 1).unwrap();
        s.sync_system_phrases(&sys).unwrap();
        assert!(
            s.list_system_phrases().unwrap().is_empty(),
            "受损现场：系统列表应为空"
        );

        assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 1);
        assert_eq!(s.list_system_phrases().unwrap().len(), 1);
        assert!(s.list_user_phrases_paged(None, 0, 99).unwrap().0.is_empty());
        // 幂等：再认领一次不重复计数
        assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sync_does_not_overwrite_user_row() {
        let path = tmp("wind_phrases_no_overwrite_user.redb");
        let s = Store::open(&path).unwrap();
        // 先建用户行 (bj, 北京)，is_system=false
        s.add_phrase("bj", "北京", 0, 1).unwrap();
        // 同步含同键的系统条目
        s.sync_system_phrases(&[SystemPhrase {
            code: "bj".into(),
            text: "北京".into(),
            weight: 9,
            position: 0,
            category: String::new(),
            enabled: true,
        }])
        .unwrap();
        // 用户行应保持 is_system=false，不被系统化
        let row = s
            .list_phrases()
            .unwrap()
            .into_iter()
            .find(|p| p.code == "bj")
            .unwrap();
        assert!(!row.is_system, "用户行不应被系统短语覆写为 is_system=true");
        // 模拟系统条目移除（sync 空列表）：用户行不应被删
        s.sync_system_phrases(&[]).unwrap();
        let list = s.list_phrases().unwrap();
        assert!(
            list.iter().any(|p| p.code == "bj" && !p.is_system),
            "系统条目移除后用户行不应被静默删除"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn export_import_user_phrases_roundtrip() {
        let path = tmp("wind_phrases_io.redb");
        let s = Store::open(&path).unwrap();
        s.add_phrase("bj", "北京", 0, 1000).unwrap();
        s.add_phrase("ml", "多行\n内容", 2, 500).unwrap();
        let text = s
            .export_user_phrases_wdict("2026-07-02T00:00:00+08:00")
            .unwrap();
        // 清空后再导入
        s.reset_user_phrases().unwrap();
        assert_eq!(s.list_user_phrases_paged(None, 0, 99).unwrap().1, 0);
        let (imported, skipped) = s.import_user_phrases_wdict(&text).unwrap();
        assert_eq!((imported, skipped), (2, 0));
        let (rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
        assert_eq!(total, 2);
        assert!(
            rows.iter()
                .any(|p| p.code == "ml" && p.text == "多行\n内容"),
            "多行往返无损"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn import_upsert_merges() {
        let path = tmp("wind_phrases_import_merge.redb");
        let s = Store::open(&path).unwrap();
        s.add_phrase("bj", "北京", 0, 1).unwrap();
        // 导入含同键(权重不同)+新键
        let text = crate::wdict::export_phrases_wdict(
            &[
                crate::wdict::PhraseIo {
                    code: "bj".into(),
                    text: "北京".into(),
                    weight: 9,
                    position: 0,
                    enabled: true,
                },
                crate::wdict::PhraseIo {
                    code: "sh".into(),
                    text: "上海".into(),
                    weight: 1,
                    position: 0,
                    enabled: true,
                },
            ],
            "t",
        );
        let (imported, _) = s.import_user_phrases_wdict(&text).unwrap();
        assert_eq!(imported, 2);
        let (rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
        assert_eq!(total, 2, "同键合并不新增行");
        assert_eq!(
            rows.iter().find(|p| p.code == "bj").unwrap().weight,
            9,
            "同键更新权重"
        );
        let _ = std::fs::remove_file(&path);
    }

    fn sys(code: &str, text: &str, w: i32) -> SystemPhrase {
        SystemPhrase {
            code: code.into(),
            text: text.into(),
            weight: w,
            position: 0,
            category: String::new(),
            enabled: true,
        }
    }

    /// 短语落文本：用户行进 phrases.dict.yaml；系统条目不进文件；修改系统条目写一行 overrides_system=1。
    #[test]
    fn phrases_live_in_text_as_overlay() {
        let s = Store::open(tmp("wind_ph_text.redb")).unwrap();
        s.set_system_phrases(&[sys("rq", "{date}", 100), sys("sj", "{time}", 100)])
            .unwrap();
        s.add_phrase("yx", "me@x.com", 0, 1800).unwrap();
        s.set_phrase_enabled("sj", "{time}", false).unwrap();
        let f = std::fs::read_to_string(s.phrases_path()).unwrap();
        assert!(f.contains("yx\tme@x.com\t1800\t0\t1\t\t0"), "{f}");
        assert!(f.contains("sj\t{time}\t100\t0\t0\t\t1"), "{f}");
        assert!(!f.contains("rq\t"), "未改的系统条目不进文件");
        let input: Vec<String> = s
            .enabled_phrases_for_input()
            .unwrap()
            .into_iter()
            .map(|p| p.code)
            .collect();
        assert!(input.contains(&"rq".to_string()) && input.contains(&"yx".to_string()));
        assert!(!input.contains(&"sj".to_string()));
        let sj = s
            .list_system_phrases()
            .unwrap()
            .into_iter()
            .find(|p| p.code == "sj")
            .unwrap();
        assert!(sj.is_system && !sj.enabled);
    }

    /// TOML 升级 ⇒ 修改行回到新 TOML 的权重、保留禁用；TOML 删掉的条目连同修改行消失。
    #[test]
    fn toml_upgrade_resets_edits_but_keeps_disabled() {
        let s = Store::open(tmp("wind_ph_upgrade.redb")).unwrap();
        s.set_system_phrases(&[sys("a", "甲", 10), sys("b", "乙", 10), sys("c", "丙", 10)])
            .unwrap();
        s.set_phrase_enabled("a", "甲", false).unwrap();
        s.update_phrase("b", "乙", None, None, None, Some(99))
            .unwrap();
        s.set_phrase_enabled("c", "丙", false).unwrap();
        let new = [sys("a", "甲", 20), sys("b", "乙", 30)];
        s.set_system_phrases(&new).unwrap();
        s.sync_system_phrases(&new).unwrap();
        let all = s.list_phrases().unwrap();
        let get = |c: &str| all.iter().find(|p| p.code == c).cloned();
        assert_eq!(get("a").map(|p| (p.weight, p.enabled)), Some((20, false)));
        assert_eq!(get("b").map(|p| (p.weight, p.enabled)), Some((30, true)));
        assert!(get("c").is_none());
        assert!(
            !std::fs::read_to_string(s.phrases_path())
                .unwrap()
                .contains("丙")
        );
    }

    /// 用户行遮蔽系统条目；清空用户短语后系统条目恢复，对系统条目的修改保留。
    #[test]
    fn user_rows_shadow_and_reset_keeps_system_edits() {
        let s = Store::open(tmp("wind_ph_shadowing.redb")).unwrap();
        s.set_system_phrases(&[sys("a", "甲", 10), sys("b", "乙", 10)])
            .unwrap();
        s.add_phrase("a", "甲", 3, 500).unwrap();
        let a = s
            .list_phrases()
            .unwrap()
            .into_iter()
            .find(|p| p.code == "a")
            .unwrap();
        assert!(!a.is_system && a.overrides_system && a.weight == 500);
        s.set_phrase_enabled("b", "乙", false).unwrap();
        s.reset_user_phrases().unwrap();
        let all = s.list_phrases().unwrap();
        let a = all.iter().find(|p| p.code == "a").unwrap();
        assert!(a.is_system && a.weight == 10);
        assert!(
            !all.iter().find(|p| p.code == "b").unwrap().enabled,
            "对系统条目的修改保留"
        );
    }

    /// phrase_sys_hash 离开 redb：落用户目录（跟着短语文件走），重开读回。
    #[test]
    fn phrase_sys_hash_lives_beside_phrases_file() {
        let p = tmp("wind_ph_hash_state.redb");
        {
            let s = Store::open(&p).unwrap();
            s.set_phrase_sys_hash("h1").unwrap();
            assert!(s.phrase_sys_hash_path().is_file());
            assert_eq!(s.phrase_sys_hash_path().parent(), Some(s.user_text_dir()));
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
    }

    /// 审查 I-4：系统短语同步哈希随用户目录走——本机状态目录是新的（换机 / 清 state）时不得丢失，
    /// 否则会触发一次同步、把用户对系统短语的修改重置回出厂。
    #[test]
    fn phrase_sys_hash_roams_with_user_dir() {
        let base = std::env::temp_dir().join(format!("wind_ph_hash_roam_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let loc = |st: &str| crate::store::StateLoc {
            state_dir: base.join(st),
            dir_id: "d".into(),
            text_dir: base.join(crate::DATA_DIR_NAME),
            legacy_done: false,
        };
        {
            let s = Store::open_with_state(base.join("a.redb"), loc("state1")).unwrap();
            s.set_phrase_sys_hash("h1").unwrap();
        }
        let s = Store::open_with_state(base.join("b.redb"), loc("state2")).unwrap();
        assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
    }
}
