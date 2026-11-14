//! 单个方案的设置（手机设置页「方案详情」）：读当前值 / 恢复后的值 / 是否已覆盖，写方案覆盖层。
//!
//! # 落点与 PC 同一份
//!
//! 全部写 `schema_overrides/{id}.toml`（核心 `read_schema` 深合并到方案文件之上），键与方案文件
//! 段名相同——PC 设置页写出的文件这里能读，反之亦然。只调 `EngineManager` 现成的读写口，不另起
//! 一套合并逻辑。
//!
//! PC 走 RPC `schema.getConfig` / `schema.saveConfig`，本体在 `wind-webdata`；那个 crate 刻意不进
//! 移动端依赖闭包（会把 fontdb 等拉进来），故这里按键读改写覆盖层，语义与 saveConfig 的
//! `json_diff(方案文件, 新值)` 对齐：**新值等于方案文件里的原始值就删键**，否则写键。
//!
//! # 为什么按键读改写，而不是整份重写
//!
//! 覆盖文件里还有本页不管的东西（扩展词库开关 `dictionaries`、`key_actions`……）。整份重写或
//! 像 PC 的 `schema.resetConfig` 那样整文件删除，会把扩展词库开关一起清掉。
//!
//! 规划见 WindInputAndroid `docs/schema-settings-plan.md`。

use toml::Value;
use wind_config::{Config, Schema};
use wind_coordinator::web_host::WebDataHost;

use crate::MobileCore;

/// 设置项在详情页里归哪张卡片（对应 PC 方案设置对话框的 TAB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaSettingGroup {
    /// 方案参数（双拼布局）
    Basic,
    /// 自定义行为：`[punct]` / `[candidate]` / `[phrases]`，所有方案都有
    Behavior,
    /// 方案覆盖：码表全局配置的方案级覆盖（`[engine.codetable]` 行为字段）
    CodeTable,
    /// 辅助码：本方案是否启用（tri-state 覆盖全局基线）
    AuxCode,
    /// 实验性功能：整句输入
    Experimental,
}

/// 值的形态。值在边界上一律是字符串（`"true"` / `"3"` / `"english"`），写入时按这里解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaSettingKind {
    Bool,
    /// 非负整数
    Int,
    /// 枚举：取值必须在 [`SchemaSetting::options`] 里
    Choice,
}

/// 一项方案设置。
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaSetting {
    /// 覆盖层里的点分路径（与方案文件段名相同），如 `punct.mode`、`engine.codetable.top_code_commit`
    pub key: String,
    pub group: SchemaSettingGroup,
    pub kind: SchemaSettingKind,
    /// 当前生效值
    pub value: String,
    /// 删掉用户覆盖后的值（方案文件 ⊕ 全局基线）
    pub default_value: String,
    /// 覆盖层里有这个键。只能问覆盖层：合并值分不出是方案作者写的还是用户写的
    pub overridden: bool,
    /// [`SchemaSettingKind::Choice`] 的值域
    pub options: Vec<String>,
}

/// 方案信息头 + 按方案类型筛好的设置项。
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaSettings {
    pub id: String,
    pub name: String,
    /// `pinyin` / `shuangpin` / `codetable` / `mixed` / `english`
    pub engine_type: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub items: Vec<SchemaSetting>,
}

const PUNCT_MODES: [&str; 3] = ["follow", "chinese", "english"];
const CANDIDATE_LAYOUTS: [&str; 3] = ["follow", "vertical", "horizontal"];
const FREQ_STRATEGIES: [&str; 3] = ["top", "step", "position"];

/// 方案没写双拼布局时核心用的布局（wind-engine `DEFAULT_SHUANGPIN_LAYOUT`；本 crate 不依赖
/// wind-engine，只为这一个常量不值得加一条依赖边）。
const DEFAULT_SHUANGPIN_LAYOUT: &str = "xiaohe";

/// 码表覆盖项：相对 `engine.codetable` 的路径与形态。
///
/// = PC 可覆盖集合（`wind-setting` `schema_codetable.rs` 的 `SPEC_BEHAVIOR_FIELDS` + `frequency.*` +
/// `english_merge.*`）∩ 手机设置清单「码表」节已有的项。`auto_phrase.*` 核心 `CodeTableSpec`
/// 没有方案级段，不列（同 PC `visible_keys`）。
const CODETABLE_KEYS: &[(&str, SchemaSettingKind)] = &[
    ("auto_commit_at_full", SchemaSettingKind::Bool),
    ("clear_on_empty_max", SchemaSettingKind::Bool),
    ("top_code_commit", SchemaSettingKind::Bool),
    ("punct_commit", SchemaSettingKind::Bool),
    ("show_code_hint", SchemaSettingKind::Bool),
    ("single_char", SchemaSettingKind::Bool),
    ("frequency.enabled", SchemaSettingKind::Bool),
    ("english_merge.enable", SchemaSettingKind::Bool),
    ("auto_commit_min_len", SchemaSettingKind::Int),
    ("short_code_yield_level", SchemaSettingKind::Int),
    ("frequency.strategy", SchemaSettingKind::Choice),
];

impl MobileCore {
    /// 方案信息与它的设置项（按方案类型筛过）。未知方案 → Err。
    pub fn schema_settings(&self, schema_id: &str) -> Result<SchemaSettings, String> {
        let mgr = self.coord.engine_mgr();
        let merged = mgr
            .schema_merged(schema_id)
            .ok_or_else(|| format!("方案不存在: {schema_id}"))?;
        let base = mgr.schema_base(schema_id).unwrap_or_else(|| merged.clone());
        let merged_v = Value::try_from(&merged).map_err(ser_err)?;
        let base_v = Value::try_from(&base).map_err(ser_err)?;
        let ov = mgr
            .get_schema_override(schema_id)
            .unwrap_or_else(empty_table);
        let engine_type = engine_type_of(&merged);

        let mut items = Vec::new();
        // 两段的值都取方案自身（合并 / 不含覆盖），`follow` 是值域里的一档，不需要全局值。
        let behavior = [
            ("punct.mode", SchemaSettingKind::Choice, &PUNCT_MODES[..]),
            (
                "candidate.layout",
                SchemaSettingKind::Choice,
                &CANDIDATE_LAYOUTS[..],
            ),
            ("phrases.enabled", SchemaSettingKind::Bool, &[][..]),
        ];
        for (key, kind, options) in behavior {
            items.push(SchemaSetting {
                key: key.to_string(),
                group: SchemaSettingGroup::Behavior,
                kind,
                value: get_path(&merged_v, key).map(fmt).unwrap_or_default(),
                default_value: get_path(&base_v, key).map(fmt).unwrap_or_default(),
                overridden: get_path(&ov, key).is_some(),
                options: options.iter().map(|s| s.to_string()).collect(),
            });
        }

        if engine_type == "shuangpin" {
            let layout = |s: &Schema| {
                let l = &s.engine.pinyin.shuangpin.layout;
                if l.is_empty() {
                    DEFAULT_SHUANGPIN_LAYOUT.to_string()
                } else {
                    l.clone()
                }
            };
            let key = "engine.pinyin.shuangpin.layout";
            items.push(SchemaSetting {
                key: key.to_string(),
                group: SchemaSettingGroup::Basic,
                kind: SchemaSettingKind::Choice,
                value: layout(&merged),
                default_value: layout(&base),
                overridden: get_path(&ov, key).is_some(),
                options: mgr
                    .shuangpin_layouts()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect(),
            });
        }

        if engine_type == "codetable" {
            // 码表项的值要折叠全局基线（普通 / overlay 两种基线，UI 算不出来），取核心现成的两份快照。
            let eff = Value::try_from(mgr.effective_codetable(schema_id)).map_err(ser_err)?;
            let followed = Value::try_from(mgr.followed_codetable(schema_id)).map_err(ser_err)?;
            for (rel, kind) in CODETABLE_KEYS {
                let key = format!("engine.codetable.{rel}");
                items.push(SchemaSetting {
                    overridden: get_path(&ov, &key).is_some(),
                    key,
                    group: SchemaSettingGroup::CodeTable,
                    kind: *kind,
                    value: get_path(&eff, rel).map(fmt).unwrap_or_default(),
                    default_value: get_path(&followed, rel).map(fmt).unwrap_or_default(),
                    options: if *kind == SchemaSettingKind::Choice {
                        FREQ_STRATEGIES.iter().map(|s| s.to_string()).collect()
                    } else {
                        Vec::new()
                    },
                });
            }
            // 整句输入只给码表（PC `sentence_input_seed`）：混输配着拼音子引擎，超码长由拼音接管，
            // 码表整句不会触发——给一个开了没反应的开关比不给更糟。
            let key = "engine.codetable.sentence_input";
            items.push(SchemaSetting {
                key: key.to_string(),
                group: SchemaSettingGroup::Experimental,
                kind: SchemaSettingKind::Bool,
                value: merged.engine.codetable.sentence_input.to_string(),
                default_value: base.engine.codetable.sentence_input.to_string(),
                overridden: get_path(&ov, key).is_some(),
                options: Vec::new(),
            });
        }

        if (engine_type == "pinyin" || engine_type == "shuangpin") && self.aux_usable(&merged) {
            let global = Config::load(Some(&self.data_dir))
                .map(|c| c.schema.pinyin.aux_code.enabled)
                .unwrap_or(false);
            let key = "engine.aux_code.enabled";
            items.push(SchemaSetting {
                key: key.to_string(),
                group: SchemaSettingGroup::AuxCode,
                kind: SchemaSettingKind::Bool,
                value: merged.engine.aux_code.enabled.unwrap_or(global).to_string(),
                default_value: base.engine.aux_code.enabled.unwrap_or(global).to_string(),
                overridden: get_path(&ov, key).is_some(),
                options: Vec::new(),
            });
        }

        Ok(SchemaSettings {
            id: schema_id.to_string(),
            name: mgr.schema_name(schema_id),
            engine_type: engine_type.to_string(),
            version: merged.schema.version.clone(),
            author: merged.schema.author.clone(),
            description: merged.schema.description.clone(),
            items,
        })
    }

    /// 写一项方案设置。`value = None` 即恢复（删掉覆盖层的这个键）。
    ///
    /// 只接受 [`Self::schema_settings`] 为这个方案列出的键（类型不符的方案没有的项一律拒绝）；
    /// 值按该项形态解析、校验值域。新值等于方案文件原值时删键（同 PC saveConfig 的稀疏 diff）。
    /// 写完即使该方案引擎失效；生效要另发 [`crate::MobileCommand::ReloadConfig`]。
    pub fn set_schema_setting(
        &self,
        schema_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), String> {
        self.ensure_writable()?;
        let settings = self.schema_settings(schema_id)?;
        let item = settings
            .items
            .iter()
            .find(|i| i.key == key)
            .ok_or_else(|| format!("方案「{schema_id}」没有设置项 {key}"))?;
        let new = match value {
            None => None,
            Some(raw) => {
                let v = parse_value(item, raw)?;
                let base = self
                    .coord
                    .engine_mgr()
                    .schema_base(schema_id)
                    .map(|s| Value::try_from(s).map_err(ser_err))
                    .transpose()?;
                let same_as_file = base.as_ref().and_then(|b| get_path(b, key)) == Some(&v);
                (!same_as_file).then_some(v)
            }
        };
        self.write_override_keys(schema_id, &[(key, new)])
    }

    /// 「全部恢复默认」：删掉本页管理的全部键。覆盖层里的其它内容（扩展词库开关等）原样保留。
    pub fn reset_schema_settings(&self, schema_id: &str) -> Result<(), String> {
        self.ensure_writable()?;
        let settings = self.schema_settings(schema_id)?;
        let keys: Vec<(&str, Option<Value>)> = settings
            .items
            .iter()
            .map(|i| (i.key.as_str(), None))
            .collect();
        self.write_override_keys(schema_id, &keys)
    }

    /// 读改写覆盖层：`Some` 写键、`None` 删键（顺带删空掉的父表）。表删空了就删文件。
    /// 写 / 删都使该方案引擎失效（混输成员由核心扇出），再重建跨方案派生配置（同 PC saveConfig）。
    fn write_override_keys(
        &self,
        schema_id: &str,
        changes: &[(&str, Option<Value>)],
    ) -> Result<(), String> {
        let mgr = self.coord.engine_mgr();
        let mut ov = match mgr.get_schema_override(schema_id) {
            Some(v) => v,
            // 读不出来有两种：文件不存在（从空表起步）、文件存在但写坏了（拒绝——拿空表写回
            // 会把里面的扩展词库开关、按键绑定等整份冲掉）
            None if mgr
                .schema_override_path(schema_id)
                .is_some_and(|p| p.exists()) =>
            {
                return Err(format!(
                    "方案覆盖文件 {schema_id}.toml 格式有误，未修改；请先修正或删除该文件"
                ));
            }
            None => empty_table(),
        };
        for (key, v) in changes {
            match v {
                Some(v) => set_path(&mut ov, key, v.clone())?,
                None => remove_path(&mut ov, key),
            }
        }
        let empty = ov.as_table().is_some_and(|t| t.is_empty());
        let r = if empty {
            mgr.delete_schema_override(schema_id).map(|_| ())
        } else {
            mgr.write_schema_override(schema_id, &ov)
        };
        r.map_err(|e| format!("写方案覆盖失败: {e}"))?;
        self.coord.refresh_schema_derived_config();
        Ok(())
    }

    /// 方案声明的辅助码来源里至少有一个装着。
    ///
    /// PC 的判据只是 `files` 非空；手机上出厂不带笔画方案与 `aux_code/` 码表，照搬会给拼音、
    /// 双拼各一个开了没反应的开关。引用方案（`schema:<id>`）看已装码表方案，文件看各层的
    /// `schemas/aux_code/`——与 PC「辅助码来源」下拉的候选集同源（`aux_code_source_options`）。
    fn aux_usable(&self, merged: &Schema) -> bool {
        let files = &merged.engine.aux_code.files;
        if files.is_empty() {
            return false;
        }
        let opts = self
            .coord
            .engine_mgr()
            .aux_code_source_options(&merged.schema.id);
        files.iter().any(
            |f| match f.strip_prefix(wind_config::schema::AUX_SCHEMA_SOURCE_PREFIX) {
                Some(id) => opts.schemas.iter().any(|(s, _)| s == id),
                None => opts.files.iter().any(|(rel, _)| rel == f),
            },
        )
    }
}

/// 界面上的类型：核心的引擎类型，拼音再分全拼 / 双拼（PC 的 TAB 集合就按这五类分）。
fn engine_type_of(s: &Schema) -> &'static str {
    match wind_coordinator::dict_transfer_api::resolve_engine_type(s) {
        "pinyin" if s.engine.pinyin.scheme.eq_ignore_ascii_case("shuangpin") => "shuangpin",
        t => t,
    }
}

fn parse_value(item: &SchemaSetting, raw: &str) -> Result<Value, String> {
    let bad = || format!("{} 的取值不合法: {raw}", item.key);
    match item.kind {
        SchemaSettingKind::Bool => raw.parse::<bool>().map(Value::Boolean).map_err(|_| bad()),
        SchemaSettingKind::Int => raw
            .parse::<u32>()
            .map(|n| Value::Integer(i64::from(n)))
            .map_err(|_| bad()),
        SchemaSettingKind::Choice => item
            .options
            .iter()
            .any(|o| o == raw)
            .then(|| Value::String(raw.to_string()))
            .ok_or_else(bad),
    }
}

fn ser_err(e: toml::ser::Error) -> String {
    format!("序列化方案配置失败: {e}")
}

fn empty_table() -> Value {
    Value::Table(toml::Table::new())
}

/// 值的字符串形态（边界上的统一表示）。
fn fmt(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(root, |cur, k| cur.get(k))
}

fn set_path(root: &mut Value, path: &str, leaf: Value) -> Result<(), String> {
    let mut parts: Vec<&str> = path.split('.').collect();
    let last = parts.pop().unwrap_or_default();
    let mut cur = root;
    for k in parts {
        let t = cur
            .as_table_mut()
            .ok_or_else(|| format!("覆盖文件结构异常：{path} 的上级不是表"))?;
        cur = t.entry(k).or_insert_with(empty_table);
    }
    cur.as_table_mut()
        .ok_or_else(|| format!("覆盖文件结构异常：{path} 的上级不是表"))?
        .insert(last.to_string(), leaf);
    Ok(())
}

/// 删键，并逐级删掉因此变空的父表——留一个空的 `[engine.codetable]` 不影响合并，但「表删空就删
/// 文件」的判断会因此失灵，用户层永远留着一个只有空段的文件。
fn remove_path(root: &mut Value, path: &str) {
    fn go(cur: &mut Value, parts: &[&str]) {
        let Some(t) = cur.as_table_mut() else { return };
        match parts {
            [] => {}
            [last] => {
                t.remove(*last);
            }
            [head, rest @ ..] => {
                if let Some(child) = t.get_mut(*head) {
                    go(child, rest);
                    if child.as_table().is_some_and(|c| c.is_empty()) {
                        t.remove(*head);
                    }
                }
            }
        }
    }
    let parts: Vec<&str> = path.split('.').collect();
    go(root, &parts);
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn set_then_remove_prunes_empty_parents_but_keeps_siblings() {
        let mut v: Value =
            toml::from_str("[[dictionaries]]\nid = \"a\"\nenabled = false\n").unwrap();
        set_path(
            &mut v,
            "engine.codetable.frequency.enabled",
            Value::Boolean(true),
        )
        .unwrap();
        set_path(&mut v, "punct.mode", Value::String("english".into())).unwrap();
        assert_eq!(
            get_path(&v, "engine.codetable.frequency.enabled"),
            Some(&Value::Boolean(true))
        );
        remove_path(&mut v, "engine.codetable.frequency.enabled");
        assert!(v.get("engine").is_none(), "删空的父表一并删掉: {v}");
        remove_path(&mut v, "punct.mode");
        let t = v.as_table().unwrap();
        assert_eq!(
            t.keys().collect::<Vec<_>>(),
            ["dictionaries"],
            "别的段原样保留"
        );
        remove_path(&mut v, "no.such.key"); // 不存在的键：无事发生
    }

    #[test]
    fn set_rejects_non_table_parent() {
        let mut v: Value = toml::from_str("punct = 1\n").unwrap();
        assert!(set_path(&mut v, "punct.mode", Value::String("english".into())).is_err());
    }

    #[test]
    fn parse_value_validates_by_kind() {
        let item = |kind, options: &[&str]| SchemaSetting {
            key: "k".into(),
            group: SchemaSettingGroup::Behavior,
            kind,
            value: String::new(),
            default_value: String::new(),
            overridden: false,
            options: options.iter().map(|s| s.to_string()).collect(),
        };
        let b = item(SchemaSettingKind::Bool, &[]);
        assert_eq!(parse_value(&b, "true"), Ok(Value::Boolean(true)));
        assert!(parse_value(&b, "yes").is_err());
        let n = item(SchemaSettingKind::Int, &[]);
        assert_eq!(parse_value(&n, "3"), Ok(Value::Integer(3)));
        assert!(parse_value(&n, "-1").is_err());
        let c = item(SchemaSettingKind::Choice, &PUNCT_MODES);
        assert_eq!(
            parse_value(&c, "english"),
            Ok(Value::String("english".into()))
        );
        assert!(parse_value(&c, "latin").is_err());
    }
}

#[cfg(test)]
mod core_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use wind_coordinator::Coordinator;

    use super::*;
    use crate::{KeySource, PressKey};

    fn data_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    fn has_data() -> bool {
        if data_dir().join("schemas/wubi86.schema.toml").exists() {
            return true;
        }
        if std::env::var_os("CI").is_some() {
            eprintln!("跳过：缺少出厂数据（CI 环境）");
            return false;
        }
        panic!("缺少出厂数据 {}，本地不允许跳过", data_dir().display());
    }

    /// `root` 下独立的 redb 与覆盖目录（不走 `MobileCore::new`：它改进程环境变量）。
    fn core_at(root: &Path) -> MobileCore {
        let store = Arc::new(wind_store::Store::open(root.join("s.redb")).unwrap());
        let mut cfg = Config::default();
        cfg.schema.available = ["wubi86", "pinyin", "shuangpin", "wubi86_pinyin", "english"]
            .map(String::from)
            .to_vec();
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        let coord = Coordinator::new_headless_with_store_override(
            cfg,
            Some(&data_dir()),
            store,
            Some(root.join("overrides")),
        );
        MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: data_dir(),
            auto_spaced: AtomicBool::new(false),
            auto_space: AtomicBool::new(true),
            touch_quick_trigger: AtomicBool::new(false),
        }
    }

    fn root(tag: &str) -> PathBuf {
        let r = std::env::temp_dir().join(format!("wind_mobile_ss_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        std::fs::create_dir_all(&r).unwrap();
        r
    }

    fn item<'a>(s: &'a SchemaSettings, key: &str) -> &'a SchemaSetting {
        s.items.iter().find(|i| i.key == key).unwrap_or_else(|| {
            panic!(
                "缺 {key}: {:?}",
                s.items.iter().map(|i| &i.key).collect::<Vec<_>>()
            )
        })
    }

    fn groups(s: &SchemaSettings) -> Vec<SchemaSettingGroup> {
        let mut g: Vec<_> = s.items.iter().map(|i| i.group).collect();
        g.dedup();
        g
    }

    #[test]
    fn items_follow_engine_type() {
        if !has_data() {
            return;
        }
        let r = root("types");
        let m = core_at(&r);
        use SchemaSettingGroup::*;

        let wubi = m.schema_settings("wubi86").unwrap();
        assert_eq!(wubi.engine_type, "codetable");
        assert_eq!(groups(&wubi), [Behavior, CodeTable, Experimental]);
        assert_eq!(
            wubi.items.iter().filter(|i| i.group == CodeTable).count(),
            CODETABLE_KEYS.len()
        );
        assert_eq!(item(&wubi, "punct.mode").value, "follow");
        assert_eq!(item(&wubi, "phrases.enabled").value, "true");

        let py = m.schema_settings("pinyin").unwrap();
        assert_eq!(py.engine_type, "pinyin");
        // build_dev 装着 stroke 方案（pinyin 引用 schema:stroke），辅助码可用
        assert_eq!(groups(&py), [Behavior, AuxCode]);

        let sp = m.schema_settings("shuangpin").unwrap();
        assert_eq!(sp.engine_type, "shuangpin");
        assert_eq!(groups(&sp), [Behavior, Basic, AuxCode]);
        let layout = item(&sp, "engine.pinyin.shuangpin.layout");
        assert_eq!(layout.value, "xiaohe");
        assert!(
            layout.options.iter().any(|o| o == "mspy"),
            "{:?}",
            layout.options
        );

        let mix = m.schema_settings("wubi86_pinyin").unwrap();
        assert_eq!(mix.engine_type, "mixed");
        assert_eq!(groups(&mix), [Behavior], "混输不给整句输入、码表覆盖");

        let en = m.schema_settings("english").unwrap();
        assert_eq!(en.engine_type, "english");
        assert_eq!(groups(&en), [Behavior]);
        // 英文方案文件自带 [punct] / [phrases]：生效值 = 方案文件值，不算用户覆盖
        let p = item(&en, "punct.mode");
        assert_eq!(
            (p.value.as_str(), p.default_value.as_str(), p.overridden),
            ("english", "english", false)
        );
        assert_eq!(item(&en, "phrases.enabled").value, "false");

        assert!(m.schema_settings("nope").is_err());
        let _ = std::fs::remove_dir_all(&r);
    }

    /// 没装辅助码来源时不给开关：出厂 APK 正是这样（不带 stroke 方案与 aux_code/ 码表）。
    #[test]
    fn aux_hidden_when_no_declared_source_is_installed() {
        if !has_data() {
            return;
        }
        let r = root("aux");
        let m = core_at(&r);
        let ov = r.join("overrides");
        std::fs::create_dir_all(&ov).unwrap();
        std::fs::write(
            ov.join("pinyin.toml"),
            "[engine.aux_code]\nfiles = [\"schema:no_such\", \"aux_code/no_such.txt\"]\n",
        )
        .unwrap();
        m.coord.engine_mgr().invalidate_schema("pinyin");
        let py = m.schema_settings("pinyin").unwrap();
        assert!(
            py.items
                .iter()
                .all(|i| i.group != SchemaSettingGroup::AuxCode),
            "{:?}",
            py.items
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn write_prune_reset_keep_unrelated_override_content() {
        if !has_data() {
            return;
        }
        let r = root("write");
        let m = core_at(&r);
        let file = r.join("overrides/wubi86.toml");

        // 先有一条本页不管的覆盖：扩展词库开关
        let dict = m
            .schema_dicts("wubi86")
            .into_iter()
            .next()
            .expect("wubi86 有附加词库");
        m.set_schema_dict_enabled("wubi86", &dict.id, !dict.enabled)
            .unwrap();

        m.set_schema_setting("wubi86", "punct.mode", Some("english"))
            .unwrap();
        let top = item(
            &m.schema_settings("wubi86").unwrap(),
            "engine.codetable.top_code_commit",
        )
        .clone();
        let flipped = (top.value != "true").to_string();
        m.set_schema_setting("wubi86", &top.key, Some(&flipped))
            .unwrap();
        m.set_schema_setting("wubi86", "engine.codetable.sentence_input", Some("true"))
            .unwrap();

        let s = m.schema_settings("wubi86").unwrap();
        let p = item(&s, "punct.mode");
        assert_eq!(
            (p.value.as_str(), p.default_value.as_str(), p.overridden),
            ("english", "follow", true)
        );
        let t = item(&s, &top.key);
        assert_eq!((t.value.as_str(), t.overridden), (flipped.as_str(), true));
        assert_eq!(t.default_value, top.default_value, "恢复后的值不随覆盖变");
        assert!(item(&s, "engine.codetable.sentence_input").overridden);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("[punct]") && text.contains("dictionaries"),
            "{text}"
        );

        // 选回方案文件的值 = 删键（稀疏，同 PC saveConfig）
        m.set_schema_setting("wubi86", "punct.mode", Some("follow"))
            .unwrap();
        assert!(!item(&m.schema_settings("wubi86").unwrap(), "punct.mode").overridden);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("[punct]"));

        // 单项恢复
        m.set_schema_setting("wubi86", &top.key, None).unwrap();
        let t = item(&m.schema_settings("wubi86").unwrap(), &top.key).clone();
        assert_eq!((t.value, t.overridden), (top.value.clone(), false));

        // 全部恢复：本页的键清光，扩展词库开关留着
        m.reset_schema_settings("wubi86").unwrap();
        let s = m.schema_settings("wubi86").unwrap();
        assert!(s.items.iter().all(|i| !i.overridden), "{:?}", s.items);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("dictionaries") && !text.contains("engine"),
            "{text}"
        );
        let d = m
            .schema_dicts("wubi86")
            .into_iter()
            .find(|d| d.id == dict.id)
            .unwrap();
        assert_eq!(d.enabled, !dict.enabled);

        // 只剩本页的键时，全部恢复把文件删掉
        m.set_schema_dict_enabled("wubi86", &dict.id, dict.enabled)
            .unwrap();
        m.set_schema_setting("wubi86", "phrases.enabled", Some("false"))
            .unwrap();
        let _ = std::fs::write(&file, "[phrases]\nenabled = false\n");
        m.reset_schema_settings("wubi86").unwrap();
        assert!(!file.exists(), "覆盖表删空就删文件");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn broken_override_file_is_not_overwritten() {
        if !has_data() {
            return;
        }
        let r = root("broken");
        let m = core_at(&r);
        let file = r.join("overrides/wubi86.toml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let broken = "[[dictionaries]]\nid = \"x\"\nenabled = \n";
        std::fs::write(&file, broken).unwrap();
        let e = m.set_schema_setting("wubi86", "phrases.enabled", Some("false"));
        assert!(e.unwrap_err().contains("格式有误"), "坏文件要拒绝写入");
        assert!(
            m.reset_schema_settings("wubi86").is_err(),
            "全部恢复也不能删坏文件"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            broken,
            "坏文件原样保留"
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn rejects_unknown_key_wrong_type_and_bad_value() {
        if !has_data() {
            return;
        }
        let r = root("reject");
        let m = core_at(&r);
        let e = m.set_schema_setting("pinyin", "engine.codetable.top_code_commit", Some("true"));
        assert!(
            e.unwrap_err().contains("没有设置项"),
            "拼音方案没有码表覆盖"
        );
        assert!(
            m.set_schema_setting("wubi86", "punct.mode", Some("latin"))
                .is_err()
        );
        assert!(
            m.set_schema_setting("wubi86", "engine.codetable.auto_commit_min_len", Some("x"))
                .is_err()
        );
        assert!(
            m.set_schema_setting("wubi86", "schema.name", Some("x"))
                .is_err()
        );
        assert!(
            m.set_schema_setting("nope", "punct.mode", Some("english"))
                .is_err()
        );
        assert!(
            !r.join("overrides/wubi86.toml").exists(),
            "拒绝的写入不落盘"
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    /// 按一个字符键，返回屏幕上多出来的字：没被吃的键由宿主原样插入（英文标点就是这条路）。
    fn type_char(m: &MobileCore, c: char) -> String {
        let out = m.press(
            PressKey::Char {
                ch: c,
                shifted: false,
            },
            KeySource::Touch,
        );
        if !out.consumed {
            return c.to_string();
        }
        out.ops
            .iter()
            .filter_map(|o| match o {
                wind_host::EditOp::Commit(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// 走宿主同一条切方案命令（`SelectSchema`，切完即同步方案级状态）。
    fn select(m: &MobileCore, id: &str) {
        let i = m
            .schemas()
            .iter()
            .position(|s| s.id == id)
            .expect("方案在列表里");
        m.run_command(crate::MobileCommand::SelectSchema(i));
        assert_eq!(m.coord.engine_mgr().active_schema_id(), id);
    }

    /// 写进覆盖层的标点态在打字时真的生效：切走再切回（代际变化）后，五笔下逗号出半角。
    /// 改的是当前方案时要切一次才生效——核心语义（本代际内用户手动切的标点态胜出），同 PC。
    #[test]
    fn punct_override_reaches_typing_after_switch() {
        if !has_data() {
            return;
        }
        let r = root("typing");
        let m = core_at(&r);
        assert_eq!(type_char(&m, ','), "，", "出厂中文标点");
        m.set_schema_setting("wubi86", "punct.mode", Some("english"))
            .unwrap();
        select(&m, "pinyin");
        select(&m, "wubi86");
        assert_eq!(type_char(&m, ','), ",", "方案级英文标点");
        m.set_schema_setting("wubi86", "punct.mode", None).unwrap();
        select(&m, "pinyin");
        assert_eq!(type_char(&m, ','), "，", "切到跟随全局的方案即还原");
        select(&m, "wubi86");
        assert_eq!(type_char(&m, ','), "，", "恢复后回到全局");
        let _ = std::fs::remove_dir_all(&r);
    }
}
