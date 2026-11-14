//! 兼容规则的分层管理：视图 + 字段级差异写入 + 禁用 + 还原（设置端 `compat.*` RPC 的纯逻辑层）。
//!
//! 只写**用户层**；系统层（`data/compat.toml`、`data_custom`）永不写。用户层记的是**相对系统层的
//! 差异**（原则与语法见 [`crate::compat_overlay`]），所以：
//! - 改一个字段只写这一个字段，系统规则的其余字段继续继承（出厂新增的修复自动送达）；
//! - 改成与系统一致的值，这个键（乃至整条）自动消失；
//! - 「还原」= 删掉用户层的痕迹，「清除」= 显式 `unset`（回到跟随全局），两者不同。
//!
//! 三段（`[[apps]]` / `[[initial_mode_scope]]` / `[[commit_newline]]`）共用一套逻辑，只有字段校验
//! 用到各自的结构体类型。设计见 `docs/design/compat-settings-ui.md`。

use crate::app_compat::{
    AppCompatRule, COMPAT_FILE_NAME, CommitNewlineRule, InitialModeScopeRule, USER_COMPAT_HEADER,
    load_raw, sanitize_raw, write_atomic,
};
pub use crate::compat_overlay::Problem;
pub use crate::compat_overlay::RuleKey;
use crate::compat_overlay::{
    ANY_PROCESS, FieldEdit, META_KEYS, Obj, Raw, RuleId, apply_edits, compose, effective_keys,
    field_value_problem, has_registered_diff, is_disabled, is_meta_key, normalize, overlay,
    overlay_raw, parse_raw, process_of, render_raw, rule_id, sanitize, window_key_of,
};
use crate::compat_schema::{COMPAT_FIELDS, Kind, known_keys};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// 三段规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Apps,
    InitialModeScope,
    CommitNewline,
}

impl Section {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "apps" => Some(Self::Apps),
            "initial_mode_scope" => Some(Self::InitialModeScope),
            "commit_newline" => Some(Self::CommitNewline),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apps => "apps",
            Self::InitialModeScope => "initial_mode_scope",
            Self::CommitNewline => "commit_newline",
        }
    }
}

const ALL_SECTIONS: [Section; 3] = [
    Section::Apps,
    Section::InitialModeScope,
    Section::CommitNewline,
];

/// 按段选出对应的结构体类型，在 `$body` 里以 `$T` 引用。
macro_rules! with_section {
    ($sec:expr, $T:ident => $body:expr) => {
        match $sec {
            Section::Apps => {
                type $T = AppCompatRule;
                $body
            }
            Section::InitialModeScope => {
                type $T = InitialModeScopeRule;
                $body
            }
            Section::CommitNewline => {
                type $T = CommitNewlineRule;
                $body
            }
        }
    };
}

/// 一条规则在分层视图里的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleState {
    /// 只来自系统层，未被动过（或用户层的差异全是冗余）。
    System,
    /// 系统规则被用户改写（用户层有真正的差异）。
    Modified,
    /// 用户新增（系统层没有这个进程）。
    User,
    /// 被禁用（用户层或更下层写了 `disabled = true`）。
    Disabled,
}

/// 某个字段的值是谁定的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    /// 系统与用户都没设，走全局 / 默认。
    Default,
    /// 出厂设了，用户没动。
    System,
    /// 用户覆盖了（一处真正的、与系统不同的差异）。
    User,
    /// 出厂设了，用户用 `unset` 取消了它（回到跟随全局）。
    Cleared,
}

/// 某个字段的逐字段视图：界面据此显示「谁定的」与可做的操作，不必自己从 `effective` / `system` /
/// `user` 三份数据推导——`effective` 会省略裸 bool 的 `false` 与 `0`，「用户设为关」与「用户清除」
/// 在里面长得一样，自己推导极易写错。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldView {
    pub source: FieldSource,
    /// 运行时生效的值，按控件类型给**类型化的默认值**（Bool = false，Int = 0，TextList = []，
    /// 其余未设为 null），界面不必知道「序列化省略默认值」这回事。
    pub value: Value,
    /// 系统层设的值；系统层没设为 `null`。
    pub system_value: Value,
    /// 用户层真正起作用的值；没有为 `null`（写错的、与系统一致的冗余都不算）。
    pub user_value: Value,
}

/// 一条规则的视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    /// 进程名（原样）；不限进程的规则为 `"*"`。
    pub process: String,
    /// 顶层窗口类名模式（原样）；没有为空串。仅 `[[apps]]`。
    pub class: String,
    /// 顶层窗口标题模式（原样）；没有为空串。仅 `[[apps]]`。
    pub title: String,
    /// 规范化的规则身份（进程名 / 类名 / 标题规范化后以 U+001F 连接）：界面认「同一条」用它，
    /// 写接口仍按 `process` / `class` / `title` 定位（大小写、首尾空白不同算同一条）。
    pub key: String,
    pub state: RuleState,
    /// 叠加后的生效内容（运行时真正看到的；对象，只含非默认字段；不含 `disabled`）。
    pub effective: Value,
    /// 系统层的原貌；用户新增的规则为 `None`。
    pub system: Option<Value>,
    /// 用户层真正起作用的差异字段名（含 `unset` 里的）。用户新增的规则 = 它的全部字段。
    pub overridden: Vec<String>,
    /// 用户层里是否有这一条（`state = system` 时也可能为 true：全是冗余差异的旧文件）。
    pub has_user_entry: bool,
    /// 用户层里这一条的原始内容（含 `disabled` / `unset`）；没有则 `None`。
    /// 禁用状态下用它才能看出「禁用前用户改过哪些字段」。
    pub user: Option<Value>,
    /// 用户层这一条里认不出的键（手写拼错的、或更新版本才有的字段）。它们对运行时没有任何效果，
    /// 所以不影响 `state` / `overridden`；单独列出让界面能提示，也保证写回时原样保留。
    pub unknown_keys: Vec<String>,
    /// 本段每个已登记字段的逐字段视图（键 = 字段名，齐全，不遗漏）。
    pub fields: BTreeMap<String, FieldView>,
    /// 规则身份（跨层认「同一条」的键）。对外序列化的是 [`Self::key`]。
    #[serde(skip)]
    pub(crate) id: RuleId,
}

/// 规范化身份的对外字符串形式（[`RuleView::key`]）。三项都不含控制字符（校验保证），
/// 用 U+001F 分隔不会撞。
pub fn rule_key_string(id: &RuleId) -> String {
    format!("{}\u{1f}{}\u{1f}{}", id.process, id.class, id.title)
}

/// 窗口条件模式的最大长度（字符数）。标题上报也截断在 256 字符，更长的模式不可能命中。
pub const MAX_PATTERN_CHARS: usize = 256;

/// 窗口条件（类名 / 标题）模式校验：trim 后不含控制字符、不超过 [`MAX_PATTERN_CHARS`] 个字符。
/// 空串合法（= 没有这项条件）。返回 trim 后的模式。
pub fn validate_pattern(what: &str, raw: &str) -> Result<String, String> {
    let p = raw.trim();
    if p.chars().any(|c| c.is_control()) {
        return Err(format!("{what}不能含控制字符"));
    }
    if p.chars().count() > MAX_PATTERN_CHARS {
        return Err(format!("{what}不能超过 {MAX_PATTERN_CHARS} 个字符"));
    }
    Ok(p.to_string())
}

/// 规则身份校验（写接口的入口）：进程名过 [`validate_process`]（`*` 合法 = 所有应用），
/// 类名 / 标题过 [`validate_pattern`]。`[[apps]]` 三项至少一项非空；附属两段只认具体进程名，
/// 不能带窗口条件、也不能是 `*`（运行时按进程名直查，`*` 永远不会命中）。
pub fn validate_key(sec: Section, key: &RuleKey) -> Result<RuleKey, String> {
    let class = validate_pattern("类名", &key.class)?;
    let title = validate_pattern("标题", &key.title)?;
    let process = match key.process.trim() {
        "" => String::new(),
        p => validate_process(p)?,
    };
    if sec == Section::Apps {
        if process.is_empty() && class.is_empty() && title.is_empty() {
            return Err("进程名、类名、标题至少要填一项".into());
        }
    } else {
        if !class.is_empty() || !title.is_empty() {
            return Err(format!("[[{}]] 不支持类名 / 标题条件", sec.as_str()));
        }
        if process.is_empty() {
            return Err("进程名不能为空".into());
        }
        if process == ANY_PROCESS {
            return Err(format!("[[{}]] 不支持 \"*\"，请填具体进程名", sec.as_str()));
        }
    }
    Ok(RuleKey {
        process,
        class,
        title,
    })
}

/// 进程名校验：trim 后非空、不含路径分隔符与控制字符。返回 trim 后的名字。
/// `*` 合法（= 所有应用；附属两段另由 [`validate_key`] 拒绝）。
pub fn validate_process(raw: &str) -> Result<String, String> {
    let p = raw.trim();
    if p.is_empty() {
        return Err("进程名不能为空".into());
    }
    if p.contains('/') || p.contains('\\') {
        return Err("进程名不能含路径分隔符".into());
    }
    if p.chars().any(|c| c.is_control()) {
        return Err("进程名不能含控制字符".into());
    }
    Ok(p.to_string())
}

/// 一行经结构体往返后的样子（运行时真正看到的；省略默认值；不含 `unset`）。
fn typed_obj<T: DeserializeOwned + Serialize>(row: &Obj) -> Obj {
    let mut r = row.clone();
    r.remove("unset");
    match serde_json::from_value::<T>(Value::Object(r.clone())) {
        Ok(t) => match serde_json::to_value(&t) {
            Ok(Value::Object(o)) => o,
            _ => r,
        },
        Err(_) => r,
    }
}

/// 补丁里一个键的校验：不许碰元键、必须是登记过的字段，值的类型与值域过 [`field_value_problem`]。
///
/// 只靠反序列化是不够的：容错反序列化会把错类型 / 错取值悄悄吞成「跟随全局」，
/// 键名拼错则被 serde 直接忽略——两者都会让调用方以为写成功了。
fn check_patch_key<T: DeserializeOwned>(section: &str, key: &str, v: &Value) -> Result<(), String> {
    if is_meta_key(section, key) {
        return Err(format!("字段 {key} 不能通过补丁修改"));
    }
    if !known_keys(section).contains(&key) {
        return Err(format!("未知字段 {key}"));
    }
    match field_value_problem::<T>(section, key, v) {
        Some(problem) => Err(problem),
        None => Ok(()),
    }
}

/// 补丁 → 字段编辑。`null` = 清除（回到跟随全局）；非 null 先过校验。
fn patch_to_edits<T: DeserializeOwned>(
    section: &str,
    patch: &Map<String, Value>,
) -> Result<Vec<FieldEdit>, String> {
    let mut edits = Vec::new();
    for (k, v) in patch {
        check_patch_key::<T>(section, k, v)?;
        edits.push(if v.is_null() {
            FieldEdit::Clear(k.clone())
        } else {
            FieldEdit::Set(k.clone(), v.clone())
        });
    }
    Ok(edits)
}

fn view_in<T: DeserializeOwned + Serialize>(
    sec_name: &str,
    sys: &[Obj],
    user: &[Obj],
) -> Vec<RuleView> {
    // 叠加用清理后的副本（无效值当成没写），差异与冗余判断仍看用户层原始内容。
    let (clean_user, _) = sanitize::<T>(sec_name, user);
    let known = known_keys(sec_name);
    overlay(sec_name, sys.to_vec(), &clean_user)
        .into_iter()
        .filter_map(|eff| {
            let id = rule_id(sec_name, &eff)?;
            let name = match process_of(&eff).trim() {
                "" => ANY_PROCESS.to_string(),
                p => p.to_string(),
            };
            let (class, title) = if is_meta_key(sec_name, "class") {
                (window_key_of(&eff, "class"), window_key_of(&eff, "title"))
            } else {
                (String::new(), String::new())
            };
            let same = |r: &&Obj| rule_id(sec_name, r).as_ref() == Some(&id);
            let s = sys.iter().find(same);
            let u = user.iter().find(same);
            // 规范化后还有**已登记字段层面**的差异，才算「用户层有真正的差异」；
            // 认不出的键对运行时没有效果，不能让规则显示成已修改。
            let mut normalized = u.cloned();
            if let Some(row) = normalized.as_mut() {
                normalize::<T>(sec_name, s, row, true);
            }
            let has_diff = normalized
                .as_ref()
                .is_some_and(|row| has_registered_diff::<T>(sec_name, row));
            let state = if is_disabled(&eff) {
                RuleState::Disabled
            } else if s.is_none() {
                RuleState::User
            } else if has_diff {
                RuleState::Modified
            } else {
                RuleState::System
            };
            let mut effective = typed_obj::<T>(&eff);
            effective.remove("disabled");
            let system = s.map(|r| {
                let mut o = typed_obj::<T>(r);
                o.remove("disabled");
                Value::Object(o)
            });
            let mut overridden = BTreeSet::new();
            if has_diff && let Some(row) = &normalized {
                overridden.extend(effective_keys::<T>(sec_name, row));
                if let Some(items) = row.get("unset").and_then(Value::as_array) {
                    overridden.extend(items.iter().filter_map(Value::as_str).map(str::to_string));
                }
            }
            let unknown_keys: Vec<String> = u
                .map(|r| {
                    r.keys()
                        .filter(|k| !is_meta_key(sec_name, k) && !known.contains(&k.as_str()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let typed = typed_obj::<T>(&eff);
            let user_keys: BTreeSet<String> = normalized
                .as_ref()
                .map(|r| effective_keys::<T>(sec_name, r).into_iter().collect())
                .unwrap_or_default();
            let cleared: BTreeSet<String> = normalized
                .as_ref()
                .and_then(|r| r.get("unset"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let fields = COMPAT_FIELDS
                .iter()
                .filter(|f| f.section == sec_name)
                .map(|meta| {
                    let key = meta.key;
                    let system_value = s.and_then(|r| r.get(key)).cloned().unwrap_or(Value::Null);
                    let user_value = if user_keys.contains(key) {
                        normalized
                            .as_ref()
                            .and_then(|r| r.get(key))
                            .cloned()
                            .unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    };
                    let source = if !user_value.is_null() {
                        FieldSource::User
                    } else if cleared.contains(key) {
                        FieldSource::Cleared
                    } else if !system_value.is_null() {
                        FieldSource::System
                    } else {
                        FieldSource::Default
                    };
                    let value = typed.get(key).cloned().unwrap_or_else(|| match meta.kind {
                        Kind::Bool => Value::Bool(false),
                        Kind::Int => Value::from(0),
                        Kind::TextList => Value::Array(Vec::new()),
                        Kind::TriBool | Kind::Enum | Kind::Text => Value::Null,
                    });
                    (
                        key.to_string(),
                        FieldView {
                            source,
                            value,
                            system_value,
                            user_value,
                        },
                    )
                })
                .collect();
            Some(RuleView {
                key: rule_key_string(&id),
                id,
                process: name,
                class,
                title,
                state,
                effective: Value::Object(effective),
                system,
                overridden: overridden.into_iter().collect(),
                has_user_entry: u.is_some(),
                user: u.map(|r| Value::Object(r.clone())),
                unknown_keys,
                fields,
            })
        })
        .collect()
}

/// 系统层（`data` + `data_custom` 叠加）与用户层的两层快照。
#[derive(Clone)]
pub struct Layers {
    /// 已 [`sanitize_raw`] 的系统层（系统层永不写回，可以直接用清理后的）。
    system: Raw,
    /// 用户层**原始**内容（要写回的那份，不能清理，见 [`sanitize`]）。
    user: Raw,
    /// 各层里被当成「没写」忽略的无效值的说明（`compat.list` 的 `warnings`）。
    warnings: Vec<Problem>,
}

fn rows(raw: &Raw, sec: Section) -> &Vec<Obj> {
    match sec {
        Section::Apps => &raw.apps,
        Section::InitialModeScope => &raw.initial_mode_scope,
        Section::CommitNewline => &raw.commit_newline,
    }
}

/// 严格读用户层：不存在 ⇒ 空；读到但语法错、或有内容读不进来（会在重写时丢掉）⇒ `Err`。
///
/// 运行时加载（`AppCompat::load`）对这两种情况都是跳过并留 WARN；管理界面的写操作不能这样——
/// 静默按空集重写会抹掉用户已有的内容。
fn read_user(path: &Path) -> Result<Raw, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let raw = parse_raw(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if raw.skipped.is_empty() {
                Ok(raw)
            } else {
                Err(format!(
                    "{}: 有内容无法读入（{}）",
                    path.display(),
                    raw.skipped.join("；")
                ))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Raw::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

impl Layers {
    pub fn load(
        data_dir: Option<&Path>,
        custom_dir: Option<&Path>,
        user_dir: Option<&Path>,
    ) -> Result<Self, String> {
        let mut system = Raw::default();
        let mut warnings = Vec::new();
        for dir in [data_dir, custom_dir].into_iter().flatten() {
            if let Some(layer) = load_raw(&dir.join(COMPAT_FILE_NAME)) {
                let (clean, report) = sanitize_raw(&layer);
                warnings.extend(report);
                system = overlay_raw(system, &clean);
            }
        }
        let user = match user_dir {
            Some(u) => read_user(&u.join(COMPAT_FILE_NAME))?,
            None => Raw::default(),
        };
        warnings.extend(sanitize_raw(&user).1);
        Ok(Self {
            system,
            user,
            warnings,
        })
    }

    /// 各层里被当成「没写」忽略的无效值的说明。
    pub fn warnings(&self) -> &[Problem] {
        &self.warnings
    }

    /// 设置页的规则列表（含带窗口条件的规则，各自一条）。
    pub fn view(&self, sec: Section) -> Vec<RuleView> {
        with_section!(sec, T => view_in::<T>(sec.as_str(), rows(&self.system, sec), rows(&self.user, sec)))
    }

    /// 单条规则的视图（按身份；传进程名即纯进程键那一条，同进程带窗口条件的不算）。
    pub fn view_of(&self, sec: Section, key: impl Into<RuleKey>) -> Option<RuleView> {
        let id = key.into().id(sec.as_str())?;
        self.view_of_id(sec, &id)
    }

    fn view_of_id(&self, sec: Section, id: &RuleId) -> Option<RuleView> {
        self.view(sec).into_iter().find(|v| &v.id == id)
    }

    fn exists_id(&self, sec: Section, id: &RuleId) -> bool {
        rows(&self.system, sec)
            .iter()
            .chain(rows(&self.user, sec))
            .any(|r| rule_id(sec.as_str(), r).as_ref() == Some(id))
    }

    fn in_system(&self, sec: Section, id: &RuleId) -> bool {
        rows(&self.system, sec)
            .iter()
            .any(|r| rule_id(sec.as_str(), r).as_ref() == Some(id))
    }

    /// 解析身份并确认规则存在。**不做 [`validate_key`]**：现存的行可能是手写的、或旧版本写下的
    /// 不合规身份（超长 / 控制字符 / 附属段写 `*`），它们要能被改、被禁用、被改名成合规的，
    /// 而不是只能删。校验只管「写出新身份」的那一侧（新建、rename / copy 的 `to`）。
    fn existing(&self, sec: Section, key: impl Into<RuleKey>) -> Result<(RuleKey, RuleId), String> {
        let key = key.into();
        let id = key.id(sec.as_str()).ok_or("规则身份无效")?;
        if !self.exists_id(sec, &id) {
            return Err("无此规则".into());
        }
        Ok((key, id))
    }

    fn user_rows_mut(&mut self, sec: Section) -> &mut Vec<Obj> {
        match sec {
            Section::Apps => &mut self.user.apps,
            Section::InitialModeScope => &mut self.user.initial_mode_scope,
            Section::CommitNewline => &mut self.user.commit_newline,
        }
    }

    fn apply(&mut self, sec: Section, key: &RuleKey, edits: &[FieldEdit]) {
        let (sys, user) = match sec {
            Section::Apps => (&self.system.apps, &mut self.user.apps),
            Section::InitialModeScope => (
                &self.system.initial_mode_scope,
                &mut self.user.initial_mode_scope,
            ),
            Section::CommitNewline => (&self.system.commit_newline, &mut self.user.commit_newline),
        };
        with_section!(sec, T => apply_edits::<T>(sec.as_str(), sys, user, key, edits, true));
    }

    /// 按字段补丁写入：只写补丁里的字段（`null` = 清除），其余字段照旧继承系统层。
    /// 规则不存在即新建（用户新增的规则）。
    pub fn upsert(
        &mut self,
        sec: Section,
        key: impl Into<RuleKey>,
        patch: &Map<String, Value>,
    ) -> Result<(), String> {
        let key = key.into();
        // 现存规则只解析身份（理由见 [`Self::existing`]）；新建才校验。
        let key = match key.id(sec.as_str()) {
            Some(id) if self.exists_id(sec, &id) => key,
            _ => validate_key(sec, &key)?,
        };
        let edits = with_section!(sec, T => patch_to_edits::<T>(sec.as_str(), patch))?;
        self.apply(sec, &key, &edits);
        Ok(())
    }

    /// 还原单个字段：删掉用户层关于它的一切痕迹（值与 `unset`），继承系统层。
    pub fn reset_field(
        &mut self,
        sec: Section,
        key: impl Into<RuleKey>,
        field: &str,
    ) -> Result<(), String> {
        if META_KEYS.contains(&field) {
            return Err(format!("字段 {field} 不可还原"));
        }
        let (key, _) = self.existing(sec, key)?;
        self.apply(sec, &key, &[FieldEdit::Inherit(field.to_string())]);
        Ok(())
    }

    /// 禁用 / 启用。启用 = 去掉用户层的禁用差异；系统层自己就禁用的规则则写显式 `disabled = false`。
    pub fn set_disabled(
        &mut self,
        sec: Section,
        key: impl Into<RuleKey>,
        flag: bool,
    ) -> Result<(), String> {
        let (key, _) = self.existing(sec, key)?;
        self.apply(
            sec,
            &key,
            &[FieldEdit::Set("disabled".into(), Value::Bool(flag))],
        );
        Ok(())
    }

    /// 还原单条：删掉用户层同身份的条目。用户新增的规则等于删除。返回是否删除过。
    pub fn reset(&mut self, sec: Section, key: impl Into<RuleKey>) -> bool {
        let Some(id) = key.into().id(sec.as_str()) else {
            return false;
        };
        let id = Some(id);
        let list = self.user_rows_mut(sec);
        let before = list.len();
        list.retain(|r| rule_id(sec.as_str(), r) != id);
        list.len() != before
    }

    /// 改匹配条件（规则身份）。**只允许用户新增的规则**：出厂规则（系统层有同身份那条，含被用户
    /// 改过 / 禁用的）的匹配条件只读——用户层只记差异，改了身份就不再是「对那条出厂规则的修改」，
    /// 而出厂那条会原样回来；想要不同条件请 [`Self::copy`]。新身份已被别的规则占用时拒绝（否则两条
    /// 会被叠成一条，用户看着少了一条）。只改写法（大小写、首尾空白）也走这里。
    pub fn rename(
        &mut self,
        sec: Section,
        from: impl Into<RuleKey>,
        to: impl Into<RuleKey>,
    ) -> Result<(), String> {
        let (_, from_id) = self.existing(sec, from)?;
        if self.in_system(sec, &from_id) {
            return Err("出厂规则的匹配条件不能修改，请用「复制为新规则」".into());
        }
        let to = validate_key(sec, &to.into())?;
        let to_id = to.id(sec.as_str()).ok_or("规则身份无效")?;
        if to_id != from_id && self.exists_id(sec, &to_id) {
            return Err("已有匹配条件相同的规则".into());
        }
        let name = sec.as_str();
        let new_ident = to.to_obj(name);
        let list = self.user_rows_mut(sec);
        let Some(row) = list
            .iter_mut()
            .find(|r| rule_id(name, r).as_ref() == Some(&from_id))
        else {
            return Err("无此规则".into());
        };
        for k in ["process", "class", "title"] {
            if k == "process" || is_meta_key(name, k) {
                row.remove(k);
            }
        }
        // 身份键插在前面不必要：渲染按 META_KEYS 排序。
        row.extend(new_ident);
        Ok(())
    }

    /// 复制为新规则：把 `from` 叠加后的生效内容（不含禁用状态）写成用户层一条新身份 `to` 的规则。
    /// 出厂规则想换匹配条件走这里。`to` 已存在时拒绝。
    pub fn copy(
        &mut self,
        sec: Section,
        from: impl Into<RuleKey>,
        to: impl Into<RuleKey>,
    ) -> Result<(), String> {
        let (_, from_id) = self.existing(sec, from)?;
        let to = validate_key(sec, &to.into())?;
        let to_id = to.id(sec.as_str()).ok_or("规则身份无效")?;
        if self.exists_id(sec, &to_id) {
            return Err("已有匹配条件相同的规则".into());
        }
        let name = sec.as_str();
        let clean_user = sanitize_rows(self, sec);
        let eff = overlay(name, rows(&self.system, sec).clone(), &clean_user)
            .into_iter()
            .find(|r| rule_id(name, r).as_ref() == Some(&from_id))
            .ok_or("无此规则")?;
        let mut row = to.to_obj(name);
        for (k, v) in eff {
            // 身份键（新身份已写好）与禁用状态不抄。`unset` 要抄：它在合成里跨级生效
            // （取消更低级规则设下的字段），丢了它复制品在同一级上的效果就与源规则不同。
            let skip = matches!(k.as_str(), "process" | "disabled")
                || (is_meta_key(name, &k) && matches!(k.as_str(), "class" | "title"));
            if !skip {
                row.insert(k, v);
            }
        }
        let keep = with_section!(sec, T => normalize::<T>(name, None, &mut row, true));
        if !keep {
            return Err("源规则没有可复制的设置".into());
        }
        self.user_rows_mut(sec).push(row);
        Ok(())
    }

    /// 清空用户层三段。
    pub fn reset_all(&mut self) {
        self.user = Raw::default();
    }

    /// 用户层渲染成文件全文（含固定文件头）。三段一并渲染，防止整份重写时漏段；
    /// 渲染结果自检不通过时返回 `Err`，绝不落盘一份失真的文件。
    pub fn render_user(&self) -> Result<String, String> {
        render_raw(USER_COMPAT_HEADER, &self.user)
    }

    /// 落盘用户层：唯一临时名 → 写满 → fsync → rename（见 [`write_atomic`]）。
    /// 调用方须在读到写完的全程持有 [`crate::app_compat::lock_user_compat`]。
    pub fn save(&self, user_dir: &Path) -> std::io::Result<()> {
        let text = self.render_user().map_err(std::io::Error::other)?;
        std::fs::create_dir_all(user_dir)?;
        write_atomic(&user_dir.join(COMPAT_FILE_NAME), &text)
    }
}

// ───────────────────────── 导出 / 导入 ─────────────────────────

/// 导出范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    /// 仅用户层（我的差异）。方便分享，也是默认。
    User,
    /// 系统层与用户层叠加后的全部生效规则（剔除已禁用的）。
    Effective,
}

impl ExportScope {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "effective" => Some(Self::Effective),
            _ => None,
        }
    }
}

/// 导入模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// 按字段叠加到用户层同名规则上，用户层里没被提到的条目和字段保留。
    Merge,
    /// 先清空用户层再导入。
    Replace,
}

impl ImportMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "merge" => Some(Self::Merge),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

/// 导入预览里的一条。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ImportItem {
    pub section: &'static str,
    /// 显示用的进程名（不限进程为 `"*"`）。
    pub process: String,
    /// 窗口条件（原样；没有为空串）。
    pub class: String,
    pub title: String,
    /// `add` / `override` / `disable` / `unchanged`；`remove` 是 replace 模式下会被清掉的
    /// 用户条目（导入文本里没提到它）。
    pub action: &'static str,
}

/// 导入预览：会发生什么，以及导入文本里有哪些会被忽略 / 回落 / 拒绝的内容。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub items: Vec<ImportItem>,
    /// 不认识的键（`<段>.<键>`）或整段。它们不会生效。
    pub ignored_keys: Vec<String>,
    /// 值写错、被丢弃的原值。
    pub fallbacks: Vec<String>,
    /// 被整条拒绝、不会导入的规则及原因（进程名非法等）。
    pub rejected: Vec<String>,
}

/// 导入文本里的一条规则（已剔除不认识的键与无效取值）。
struct IncomingRule {
    section: Section,
    /// 显示用的进程名（没写 process 的窗口规则为 `"*"`）。
    process: String,
    id: RuleId,
    fields: Obj,
}

struct Incoming {
    rules: Vec<IncomingRule>,
    ignored: Vec<String>,
    fallbacks: Vec<String>,
    rejected: Vec<String>,
}

/// 严格解析导入文本：语法错 ⇒ `Err`（带行号）。同时收集不认识的键、无效取值与被拒绝的规则。
///
/// 导入的每个键都要过与补丁同样的值域校验，但**不整条拒绝**：无效取值只丢这一个键并报告，
/// 因为分享的片段里混进一个写错的值很常见。否则写错的值会原样落进用户层文件。
fn parse_incoming(text: &str) -> Result<Incoming, String> {
    let raw = parse_raw(text).map_err(|e| format!("导入内容不是合法的 TOML: {e}"))?;
    let mut inc = Incoming {
        rules: Vec::new(),
        ignored: Vec::new(),
        fallbacks: Vec::new(),
        rejected: Vec::new(),
    };
    // 顶层不认识的整段也要报告（parse_raw 只认三段，别的被跳过）。
    if let Ok(root) = text.parse::<toml::Table>() {
        for name in root.keys() {
            if Section::parse(name).is_none() {
                inc.ignored.push(name.clone());
            }
        }
    }
    for sec in ALL_SECTIONS {
        let known = known_keys(sec.as_str());
        for row in rows(&raw, sec) {
            // 身份认不出（`[[apps]]` 的 process / class / title 全空或类型不对；附属段缺 process）⇒ 整条拒绝。
            let Some(id) = rule_id(sec.as_str(), row) else {
                let why = match row.get("process").and_then(Value::as_str) {
                    Some(p) => match validate_process(p) {
                        Err(e) => format!("{p:?} {e}"),
                        Ok(_) => format!("{p:?} 的 class / title 不是字符串"),
                    },
                    None => "缺少 process".to_string(),
                };
                inc.rejected.push(format!("{}: {why}", sec.as_str()));
                continue;
            };
            let mut fields = Obj::new();
            let process = match row.get("process").and_then(Value::as_str).map(str::trim) {
                Some(p) if !p.is_empty() => match validate_process(p) {
                    Ok(p) => {
                        fields.insert("process".into(), Value::String(p.clone()));
                        p
                    }
                    Err(e) => {
                        inc.rejected.push(format!("{}: {p:?} {e}", sec.as_str()));
                        continue;
                    }
                },
                // 只写窗口条件的 `[[apps]]` 规则：不限进程。
                _ => ANY_PROCESS.to_string(),
            };
            let window_keys: &[&str] = if is_meta_key(sec.as_str(), "class") {
                &["class", "title"]
            } else {
                &[]
            };
            for k in window_keys {
                if let Some(p) = row.get(*k).and_then(Value::as_str).map(str::trim)
                    && !p.is_empty()
                {
                    fields.insert(k.to_string(), Value::String(p.to_string()));
                }
            }
            // 与写接口同一套身份校验（模式含控制字符 / 过长、附属段写 `*`）。
            let key = RuleKey::new(
                fields.get("process").and_then(Value::as_str).unwrap_or(""),
                &window_key_of(&fields, "class"),
                &window_key_of(&fields, "title"),
            );
            if let Err(e) = validate_key(sec, &key) {
                inc.rejected
                    .push(format!("{}: {:?} {e}", sec.as_str(), process));
                continue;
            }
            for (k, v) in row {
                if k == "process" || window_keys.contains(&k.as_str()) {
                    continue;
                }
                match k.as_str() {
                    "comment" => {
                        if v.is_string() {
                            fields.insert(k.clone(), v.clone());
                        }
                    }
                    "disabled" => {
                        if v.is_boolean() {
                            fields.insert(k.clone(), v.clone());
                        } else {
                            inc.fallbacks.push(v.to_string());
                        }
                    }
                    "unset" => {
                        let mut keep = Vec::new();
                        for item in v.as_array().into_iter().flatten() {
                            match item.as_str() {
                                Some(name) if known.contains(&name) => {
                                    keep.push(Value::String(name.to_string()))
                                }
                                Some(name) => {
                                    inc.ignored.push(format!("{}.unset:{name}", sec.as_str()))
                                }
                                None => inc.fallbacks.push(item.to_string()),
                            }
                        }
                        if !keep.is_empty() {
                            fields.insert(k.clone(), Value::Array(keep));
                        }
                    }
                    _ if !known.contains(&k.as_str()) => {
                        inc.ignored.push(format!("{}.{k}", sec.as_str()));
                    }
                    _ => {
                        match with_section!(sec, T => field_value_problem::<T>(sec.as_str(), k, v))
                        {
                            None => {
                                fields.insert(k.clone(), v.clone());
                            }
                            Some(_) => inc.fallbacks.push(v.to_string()),
                        }
                    }
                }
            }
            inc.rules.push(IncomingRule {
                section: sec,
                process,
                id,
                fields,
            });
        }
    }
    inc.ignored.sort();
    inc.ignored.dedup();
    Ok(inc)
}

struct ImportPlan {
    after: Layers,
    items: Vec<ImportItem>,
}

/// 用户层某一段清理后的副本（无效值当成没写），用来和系统层叠加。
fn sanitize_rows(l: &Layers, sec: Section) -> Vec<Obj> {
    with_section!(sec, T => sanitize::<T>(sec.as_str(), rows(&l.user, sec)).0)
}

impl Layers {
    /// 导出成 `compat.toml` 文本。用户层导出的是差异（可直接放进用户层），生效导出是叠加后的全貌。
    pub fn export(&self, scope: ExportScope) -> Result<String, String> {
        match scope {
            ExportScope::User => self.render_user(),
            ExportScope::Effective => {
                let mut raw = Raw::default();
                for sec in ALL_SECTIONS {
                    // 保留 `unset`：它是「系统层设了、叠加后没有」的记号，缺了它，把这份导出
                    // 在同一套系统层上重新导入，被取消的字段会又继承回来。
                    let eff: Vec<Obj> = overlay(
                        sec.as_str(),
                        rows(&self.system, sec).clone(),
                        &sanitize_rows(self, sec),
                    )
                    .into_iter()
                    .filter(|r| !is_disabled(r))
                    .map(|mut r| {
                        r.remove("disabled");
                        r
                    })
                    .collect();
                    match sec {
                        Section::Apps => raw.apps = eff,
                        Section::InitialModeScope => raw.initial_mode_scope = eff,
                        Section::CommitNewline => raw.commit_newline = eff,
                    }
                }
                render_raw(
                    "# 生效中的全部应用兼容规则（系统层与用户层叠加后的结果）\n\n",
                    &raw,
                )
            }
        }
    }

    /// 把一条导入规则**叠加**到用户层同名差异行上（不是整条替换），再规范化。
    fn import_rule(&mut self, r: &IncomingRule) {
        let (sys, user) = match r.section {
            Section::Apps => (&self.system.apps, &mut self.user.apps),
            Section::InitialModeScope => (
                &self.system.initial_mode_scope,
                &mut self.user.initial_mode_scope,
            ),
            Section::CommitNewline => (&self.system.commit_newline, &mut self.user.commit_newline),
        };
        let sec = r.section.as_str();
        let same = |o: &&Obj| rule_id(sec, o).as_ref() == Some(&r.id);
        let idx = user.iter().position(|u| same(&u));
        let sys_row = sys.iter().find(same);
        let base = match idx {
            Some(i) => user[i].clone(),
            None => {
                // 新建行沿用系统层的写法（大小写），没有才用导入文本的。
                let mut o = Obj::new();
                for k in ["process", "class", "title"] {
                    if let Some(v) = sys_row.and_then(|s| s.get(k)).or_else(|| r.fields.get(k)) {
                        o.insert(k.into(), v.clone());
                    }
                }
                o
            }
        };
        let mut row = compose(&base, &r.fields);
        let keep = with_section!(r.section, T => normalize::<T>(r.section.as_str(), sys_row, &mut row, true));
        match (idx, keep) {
            (Some(i), true) => user[i] = row,
            (Some(i), false) => {
                user.remove(i);
            }
            (None, true) => user.push(row),
            (None, false) => {}
        }
    }

    /// 在副本上试算导入结果。不改 `self`。
    fn plan_import(&self, incoming: &Incoming, mode: ImportMode) -> ImportPlan {
        let mut after = self.clone();
        if mode == ImportMode::Replace {
            after.reset_all();
        }
        let mut items = Vec::new();
        for r in &incoming.rules {
            after.import_rule(r);
            let disabled = r
                .fields
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let before = self.view_of_id(r.section, &r.id);
            let now = after.view_of_id(r.section, &r.id);
            let action = if disabled {
                "disable"
            } else {
                match (&before, &now) {
                    (None, Some(_)) => "add",
                    (Some(b), Some(n)) if b.effective != n.effective || b.state != n.state => {
                        "override"
                    }
                    _ => "unchanged",
                }
            };
            let window = |k: &str| {
                r.fields
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            items.push(ImportItem {
                section: r.section.as_str(),
                process: r.process.clone(),
                class: window("class"),
                title: window("title"),
                action,
            });
        }
        if mode == ImportMode::Replace {
            for sec in ALL_SECTIONS {
                for v in self.view(sec).into_iter().filter(|v| v.has_user_entry) {
                    let mentioned = incoming
                        .rules
                        .iter()
                        .any(|r| r.section == sec && r.id == v.id);
                    if !mentioned {
                        items.push(ImportItem {
                            section: sec.as_str(),
                            process: v.process,
                            class: v.class,
                            title: v.title,
                            action: "remove",
                        });
                    }
                }
            }
        }
        ImportPlan { after, items }
    }

    /// 导入预览：不落盘，不改 `self`。
    pub fn import_preview(&self, text: &str, mode: ImportMode) -> Result<ImportPreview, String> {
        let incoming = parse_incoming(text)?;
        let plan = self.plan_import(&incoming, mode);
        Ok(ImportPreview {
            items: plan.items,
            ignored_keys: incoming.ignored,
            fallbacks: incoming.fallbacks,
            rejected: incoming.rejected,
        })
    }

    /// 应用导入到 `self`（调用方随后负责 `save`）。
    pub fn import_apply(&mut self, text: &str, mode: ImportMode) -> Result<ImportPreview, String> {
        let incoming = parse_incoming(text)?;
        let plan = self.plan_import(&incoming, mode);
        *self = plan.after;
        Ok(ImportPreview {
            items: plan.items,
            ignored_keys: incoming.ignored,
            fallbacks: incoming.fallbacks,
            rejected: incoming.rejected,
        })
    }
}

// ───────────────────────── RPC 分派 ─────────────────────────

/// 一次 RPC 的结果。`wrote` 告诉宿主是否需要重载规则表。
pub struct RpcOutcome {
    pub value: Value,
    pub wrote: bool,
}

/// 最多保留几份备份。
const BACKUP_KEEP: usize = 5;

/// 备份文件名的排序键 `(unix 秒, 同秒序号)`；不是备份文件返回 `None`。
fn backup_key(name: &str) -> Option<(u64, u32)> {
    let rest = name.strip_prefix(&format!("{COMPAT_FILE_NAME}.bak-"))?;
    let mut it = rest.splitn(2, '-');
    let secs = it.next()?.parse().ok()?;
    let n = match it.next() {
        Some(t) => t.parse().ok()?,
        None => 0,
    };
    Some((secs, n))
}

fn list_backups(user_dir: &Path) -> Vec<(String, (u64, u32))> {
    let Ok(rd) = std::fs::read_dir(user_dir) else {
        return Vec::new();
    };
    let mut v: Vec<_> = rd
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| backup_key(&n).map(|k| (n, k)))
        .collect();
    v.sort_by_key(|(_, k)| *k);
    v
}

/// 备份用户层文件为 `compat.toml.bak-<unix 秒>[-<序号>]`（不存在则什么也不做），只留最近几份。
/// 返回备份的文件名，供界面告诉用户「原文件在哪」。
///
/// 不能只留一份：连续两次导入后，最初的原件就没了，而「导入前的样子」恰恰是用户最可能想找回的。
///
/// 新备份的排序键必须**严格大于**现存的所有备份：曾经按「名字被占了就序号加一」取名，
/// 清理腾出低序号后新备份会复用它、排成最旧，下一次清理就把它自己删掉了。
fn backup_user_file(user_dir: &Path) -> Result<Option<String>, String> {
    let path = user_dir.join(COMPAT_FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 时钟回拨时也不能排到旧备份前面：以现存最新的秒数为下限。
    let key = match list_backups(user_dir).last() {
        Some((_, (newest_secs, n))) if *newest_secs >= secs => (*newest_secs, n + 1),
        _ => (secs, 0),
    };
    let name = if key.1 == 0 {
        format!("{COMPAT_FILE_NAME}.bak-{}", key.0)
    } else {
        format!("{COMPAT_FILE_NAME}.bak-{}-{}", key.0, key.1)
    };
    std::fs::copy(&path, user_dir.join(&name))
        .map_err(|e| format!("备份 {} 失败: {e}", path.display()))?;
    let all = list_backups(user_dir);
    for (old, _) in all.iter().take(all.len().saturating_sub(BACKUP_KEEP)) {
        let _ = std::fs::remove_file(user_dir.join(old));
    }
    Ok(Some(name))
}

/// `compat.*` RPC 的全部逻辑。宿主只需：调用它、按 `wrote` 决定是否重载。
pub fn rpc(
    method: &str,
    params: &Value,
    data_dir: Option<&Path>,
    custom_dir: Option<&Path>,
    user_dir: Option<&Path>,
) -> Result<RpcOutcome, String> {
    let read = |value: Value| {
        Ok(RpcOutcome {
            value,
            wrote: false,
        })
    };
    if method == "compat.schema" {
        return read(crate::compat_schema::schema_json());
    }
    let text_param = |k: &str| -> Result<&str, String> {
        params
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("缺少参数 {k}"))
    };
    let section = || -> Result<Section, String> {
        Section::parse(text_param("section")?).ok_or_else(|| "section 无效".to_string())
    };
    let need_user_dir = || user_dir.ok_or_else(|| "无用户配置目录".to_string());
    // 规则身份：`process` / `class` / `title`（都可省；旧调用只传 `process`）。三者都没给 ⇒ 缺参数。
    let key_in = |obj: &Value| -> Result<RuleKey, String> {
        let field = |k: &str| -> Result<&str, String> {
            match obj.get(k) {
                None | Some(Value::Null) => Ok(""),
                Some(Value::String(s)) => Ok(s.as_str()),
                Some(_) => Err(format!("参数 {k} 必须是字符串")),
            }
        };
        let (p, c, t) = (field("process")?, field("class")?, field("title")?);
        if ["process", "class", "title"]
            .iter()
            .all(|k| obj.get(*k).is_none_or(Value::is_null))
        {
            return Err("缺少参数 process".into());
        }
        Ok(RuleKey::new(p, c, t))
    };
    let key = || key_in(params);
    let to_key = || -> Result<RuleKey, String> {
        match params.get("to") {
            None => Err("缺少参数 to".to_string()),
            Some(v) if v.is_object() => key_in(v),
            Some(_) => Err("to 必须是对象".to_string()),
        }
    };

    // 导入的参数先解析：它决定要不要持锁、以及用户层损坏时能不能继续。
    let import_args = if method == "compat.import" {
        let content = text_param("content")?;
        let mode = match params.get("mode").and_then(Value::as_str) {
            None => ImportMode::Merge,
            Some(s) => ImportMode::parse(s).ok_or_else(|| "mode 无效".to_string())?,
        };
        let dry_run = match params.get("dryRun") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("dryRun 必须是布尔值".into()),
        };
        Some((content, mode, dry_run))
    } else {
        None
    };

    // 写方法在「读 → 改 → 写」全程持锁：右键菜单（协调器线程）与各个设置端连接线程都会写
    // 用户层，交错时后写的会把先写的整份覆盖掉。
    let writes = match method {
        "compat.upsert" | "compat.resetField" | "compat.setDisabled" | "compat.reset"
        | "compat.resetAll" | "compat.rename" | "compat.copy" => true,
        "compat.import" => import_args.is_some_and(|(_, _, dry)| !dry),
        _ => false,
    };
    let _guard = writes.then(crate::app_compat::lock_user_compat);
    // 用户目录与数据目录 / 定制目录是同一个位置时（拿不到用户配置目录的兜底），「用户层」就是系统预置
    // 文件本身：写入会把被判为冗余的出厂字段当场删掉。拒绝。
    if writes && user_dir.is_some_and(|u| Some(u) == data_dir || Some(u) == custom_dir) {
        return Err("用户配置目录与系统预置目录相同，拒绝改写".into());
    }

    // 「全部还原」与 replace 导入本来就要丢弃用户层现有内容，不该被一份读不出来的旧文件拦住——
    // 那正是用户最需要这两个按钮的时候（原文件会先备份）。其它方法要保留现有内容，只能报错。
    let recoverable = method == "compat.resetAll"
        || import_args.is_some_and(|(_, mode, _)| mode == ImportMode::Replace);

    let mut layers = match Layers::load(data_dir, custom_dir, user_dir) {
        Ok(l) => l,
        Err(e) if recoverable => {
            tracing::warn!("用户层 compat.toml 无法解析，本次操作将整份替换（原文件先备份）: {e}");
            Layers::load(data_dir, custom_dir, None)?
        }
        Err(e) => return Err(e),
    };
    let warnings = serde_json::to_value(layers.warnings()).unwrap_or(Value::Null);

    let wrote = |value: Value| Ok(RpcOutcome { value, wrote: true });
    match method {
        "compat.list" => read(json!({
            "rules": layers.view(section()?),
            "warnings": warnings,
        })),
        "compat.export" => {
            let scope = match params.get("scope").and_then(Value::as_str) {
                None => ExportScope::User,
                Some(s) => ExportScope::parse(s).ok_or_else(|| "scope 无效".to_string())?,
            };
            let content = layers.export(scope)?;
            read(json!({ "content": content }))
        }
        "compat.upsert" => {
            let dir = need_user_dir()?;
            let (sec, key) = (section()?, key()?);
            let patch = params
                .get("patch")
                .and_then(Value::as_object)
                .ok_or_else(|| "缺少参数 patch".to_string())?;
            layers.upsert(sec, &key, patch)?;
            layers.save(dir).map_err(|e| e.to_string())?;
            wrote(json!({ "ok": true, "rule": layers.view_of(sec, &key) }))
        }
        "compat.resetField" => {
            let dir = need_user_dir()?;
            let (sec, key) = (section()?, key()?);
            layers.reset_field(sec, &key, text_param("key")?)?;
            layers.save(dir).map_err(|e| e.to_string())?;
            wrote(json!({ "ok": true, "rule": layers.view_of(sec, &key) }))
        }
        "compat.setDisabled" => {
            let dir = need_user_dir()?;
            let (sec, key) = (section()?, key()?);
            let flag = params
                .get("disabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| "缺少参数 disabled".to_string())?;
            layers.set_disabled(sec, &key, flag)?;
            layers.save(dir).map_err(|e| e.to_string())?;
            wrote(json!({ "ok": true, "rule": layers.view_of(sec, &key) }))
        }
        "compat.reset" => {
            let dir = need_user_dir()?;
            let (sec, key) = (section()?, key()?);
            let removed = layers.reset(sec, &key);
            // 什么也没删就别写盘、别让宿主白白重载并广播一次。
            if removed {
                layers.save(dir).map_err(|e| e.to_string())?;
            }
            Ok(RpcOutcome {
                value: json!({ "ok": true, "removed": removed, "rule": layers.view_of(sec, &key) }),
                wrote: removed,
            })
        }
        "compat.rename" | "compat.copy" => {
            let dir = need_user_dir()?;
            let (sec, from, to) = (section()?, key()?, to_key()?);
            if method == "compat.rename" {
                layers.rename(sec, &from, &to)?;
            } else {
                layers.copy(sec, &from, &to)?;
            }
            layers.save(dir).map_err(|e| e.to_string())?;
            wrote(json!({ "ok": true, "rule": layers.view_of(sec, &to) }))
        }
        "compat.resetAll" => {
            let dir = need_user_dir()?;
            let backup = backup_user_file(dir)?;
            layers.reset_all();
            layers.save(dir).map_err(|e| e.to_string())?;
            wrote(json!({ "ok": true, "backup": backup }))
        }
        "compat.import" => {
            let (content, mode, dry_run) =
                import_args.ok_or_else(|| "内部错误：缺少导入参数".to_string())?;
            if dry_run {
                let preview = layers.import_preview(content, mode)?;
                let mut v = serde_json::to_value(&preview).map_err(|e| e.to_string())?;
                v["applied"] = json!(false);
                return read(v);
            }
            let dir = need_user_dir()?;
            let preview = layers.import_apply(content, mode)?;
            let backup = backup_user_file(dir)?;
            layers.save(dir).map_err(|e| e.to_string())?;
            let mut v = serde_json::to_value(&preview).map_err(|e| e.to_string())?;
            v["applied"] = json!(true);
            v["backup"] = json!(backup);
            wrote(v)
        }
        other => Err(format!("unknown method: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat_overlay::parse_raw;
    use serde_json::json;

    fn layers(sys: &str, usr: &str) -> Layers {
        Layers {
            system: parse_raw(sys).expect("测试夹具必须是合法 TOML"),
            user: parse_raw(usr).expect("测试夹具必须是合法 TOML"),
            warnings: Vec::new(),
        }
    }

    fn patch(v: Value) -> Map<String, Value> {
        v.as_object().cloned().expect("补丁必须是对象")
    }

    fn state_of(l: &Layers, p: &str) -> RuleState {
        l.view_of(Section::Apps, p).expect("规则应存在").state
    }

    fn user_row(l: &Layers, sec: Section, p: &str) -> Option<Obj> {
        rows(&l.user, sec)
            .iter()
            .find(|r| rule_id(sec.as_str(), r) == Some(RuleId::process_only(p)))
            .cloned()
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("wind_compat_adm_{tag}_{}", std::process::id()))
    }

    const SYS: &str = "\
[[apps]]
process = \"Feishu.exe\"
composition_start_pair_guard = true

[[apps]]
process = \"Weixin.exe\"
caret_use_top = true
stale_probe_guard = true

[[apps]]
process = \"Dota.exe\"
host_render = true
";

    const USER_A: &str = "[[apps]]\nprocess = \"Weixin.exe\"\nauto_pair = true\n";

    // ───────────── 视图 ─────────────

    #[test]
    fn view_marks_system_modified_user_disabled() {
        let usr = "\
[[apps]]
process = \"Weixin.exe\"
first_show_mode = \"wait\"

[[apps]]
process = \"Dota.exe\"
disabled = true

[[apps]]
process = \"Mine.exe\"
auto_pair = true
";
        let l = layers(SYS, usr);
        assert_eq!(state_of(&l, "Feishu.exe"), RuleState::System);
        assert_eq!(state_of(&l, "Weixin.exe"), RuleState::Modified);
        assert_eq!(state_of(&l, "Dota.exe"), RuleState::Disabled);
        assert_eq!(state_of(&l, "Mine.exe"), RuleState::User);
    }

    #[test]
    fn overridden_lists_only_the_meaningful_diff_including_unset() {
        let usr = "[[apps]]\nprocess = \"Weixin.exe\"\nauto_pair = true\ncaret_use_top = true\nunset = [\"stale_probe_guard\"]\n";
        let l = layers(SYS, usr);
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(
            v.overridden,
            vec!["auto_pair".to_string(), "stale_probe_guard".to_string()],
            "caret_use_top 与系统一致是冗余，不算差异；unset 的字段算差异"
        );
        assert!(v.system.is_some());
        assert!(
            v.effective.get("stale_probe_guard").is_none(),
            "unset 后运行时没有它"
        );
        assert_eq!(v.effective["caret_use_top"], json!(true), "没提到的继承");
    }

    #[test]
    fn user_only_rule_reports_all_its_fields_as_overridden() {
        let l = layers(
            SYS,
            "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\ncaret_use_top = true\n",
        );
        let v = l.view_of(Section::Apps, "Mine.exe").unwrap();
        assert_eq!(
            v.overridden,
            vec!["auto_pair".to_string(), "caret_use_top".to_string()]
        );
        assert!(v.system.is_none());
    }

    /// 旧文件里与系统层完全一致的冗余条目：状态是 System，但用户层里确实有这一条（还原按钮据此判断）。
    #[test]
    fn a_redundant_user_entry_is_reported_as_system_but_still_has_a_user_entry() {
        let l = layers(
            SYS,
            "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = true\n",
        );
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(v.state, RuleState::System);
        assert!(v.has_user_entry);
        assert!(v.overridden.is_empty());
    }

    #[test]
    fn view_exposes_whether_a_user_entry_exists_and_its_raw_content() {
        let mut l = layers(SYS, "");
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert!(!v.has_user_entry && v.user.is_none());
        l.upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"auto_pair": true})),
        )
        .unwrap();
        l.set_disabled(Section::Apps, "Weixin.exe", true).unwrap();
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(v.state, RuleState::Disabled);
        let user = v.user.expect("禁用状态下要能看到用户层原始条目");
        assert_eq!(user["auto_pair"], json!(true), "禁用前的改写要可见");
        assert_eq!(user["disabled"], json!(true));
    }

    // ───────────── 差异写入 ─────────────

    /// 整体原则的核心：改一个字段只写这一个字段，系统规则的其余字段继续继承。
    #[test]
    fn patch_on_a_system_rule_stores_only_the_diff() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "weixin.exe",
            &patch(json!({"first_show_mode": "wait"})),
        )
        .unwrap();
        let row = user_row(&l, Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(row["first_show_mode"], json!("wait"));
        assert!(
            row.get("caret_use_top").is_none() && row.get("stale_probe_guard").is_none(),
            "不得拷贝系统字段: {row:?}"
        );
        assert_eq!(row["process"], json!("Weixin.exe"), "沿用系统层的写法");
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(v.state, RuleState::Modified);
        assert_eq!(
            v.effective["caret_use_top"],
            json!(true),
            "系统字段照旧生效"
        );
        assert_eq!(v.effective["first_show_mode"], json!("wait"));
    }

    #[test]
    fn patch_equal_to_the_system_value_removes_the_entry() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"auto_pair": true})),
        )
        .unwrap();
        assert_eq!(state_of(&l, "Weixin.exe"), RuleState::Modified);
        l.upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"auto_pair": null})),
        )
        .unwrap();
        assert!(
            user_row(&l, Section::Apps, "Weixin.exe").is_none(),
            "回到与系统一致，条目应消失"
        );
        assert_eq!(state_of(&l, "Weixin.exe"), RuleState::System);
        l.upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"caret_use_top": true})),
        )
        .unwrap();
        assert!(
            user_row(&l, Section::Apps, "Weixin.exe").is_none(),
            "写成与系统相同的值也是冗余"
        );
    }

    /// `null` = 清除 = 回到「跟随全局」：系统层设了它就要写 unset，否则只会继承回系统值。
    #[test]
    fn null_on_a_system_field_writes_unset_and_reset_field_undoes_it() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Feishu.exe",
            &patch(json!({"composition_start_pair_guard": null})),
        )
        .unwrap();
        let row = user_row(&l, Section::Apps, "Feishu.exe").unwrap();
        assert_eq!(row["unset"], json!(["composition_start_pair_guard"]));
        let v = l.view_of(Section::Apps, "Feishu.exe").unwrap();
        assert_eq!(v.state, RuleState::Modified, "unset 是真正的差异");
        assert!(v.effective.get("composition_start_pair_guard").is_none());

        l.reset_field(Section::Apps, "Feishu.exe", "composition_start_pair_guard")
            .unwrap();
        assert!(
            user_row(&l, Section::Apps, "Feishu.exe").is_none(),
            "还原 = 继承出厂，痕迹全清"
        );
        assert_eq!(state_of(&l, "Feishu.exe"), RuleState::System);
    }

    #[test]
    fn null_on_a_field_the_system_lacks_leaves_no_trace() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"auto_pair": null})),
        )
        .unwrap();
        assert!(
            user_row(&l, Section::Apps, "Weixin.exe").is_none(),
            "空操作不该留噪声条目"
        );
    }

    #[test]
    fn explicit_false_over_a_system_true_is_kept() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Feishu.exe",
            &patch(json!({"composition_start_pair_guard": false})),
        )
        .unwrap();
        assert_eq!(
            user_row(&l, Section::Apps, "Feishu.exe").unwrap()["composition_start_pair_guard"],
            json!(false)
        );
        let v = l.view_of(Section::Apps, "Feishu.exe").unwrap();
        assert_eq!(
            v.effective["composition_start_pair_guard"],
            json!(false),
            "显式关闭必须生效"
        );
        assert_eq!(v.state, RuleState::Modified);
    }

    #[test]
    fn patch_rejects_invalid_enum_value_and_leaves_layers_untouched() {
        let mut l = layers(SYS, "");
        let err = l
            .upsert(
                Section::Apps,
                "Weixin.exe",
                &patch(json!({"first_show_mode": "bogus"})),
            )
            .unwrap_err();
        assert!(err.contains("first_show_mode"), "错误应指出字段: {err}");
        assert!(l.user.apps.is_empty());
    }

    #[test]
    fn patch_rejects_typos_wrong_types_and_empty_enum() {
        let bad = [
            json!({"caret_use_tpo": false}),
            json!({"auto_pair": 0}),
            json!({"caret_use_top": "yes"}),
            json!({"first_show_mode": ""}),
            json!({"first_show_mode": 0}),
            json!({"caret_offset_x": "12"}),
        ];
        for p in bad {
            let mut l = layers(SYS, "");
            assert!(
                l.upsert(Section::Apps, "Weixin.exe", &patch(p.clone()))
                    .is_err(),
                "补丁 {p} 应被拒绝"
            );
            assert!(l.user.apps.is_empty(), "被拒绝的补丁不得落任何东西: {p}");
        }
    }

    #[test]
    fn patch_rejects_meta_keys() {
        for p in [
            json!({"disabled": true}),
            json!({"comment": "x"}),
            json!({"process": "O.exe"}),
            json!({"unset": ["auto_pair"]}),
        ] {
            let mut l = layers(SYS, "");
            assert!(
                l.upsert(Section::Apps, "Weixin.exe", &patch(p.clone()))
                    .is_err(),
                "{p}"
            );
        }
    }

    /// P4 起改口径：用户新增规则里的显式 `false` 不再当冗余删掉——合成时它跨级压过 `*` /
    /// 更低级规则的 `true`（P1 起 `*` 对所有字段生效，「系统没有同身份行」不等于「没人设它」）。
    #[test]
    fn false_on_a_user_rule_is_kept_because_it_overrides_lower_tiers() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Mine.exe",
            &patch(json!({"auto_pair": true, "caret_use_top": false})),
        )
        .expect("bool false 是合法值");
        let row = user_row(&l, Section::Apps, "Mine.exe").unwrap();
        assert_eq!(
            row["caret_use_top"],
            json!(false),
            "用户新增规则的显式 false 保留: {row:?}"
        );
        assert_eq!(row["auto_pair"], json!(true));
    }

    #[test]
    fn process_validation_and_matching() {
        assert!(validate_process("").is_err());
        assert!(validate_process("   ").is_err());
        assert!(validate_process("a/b.exe").is_err());
        assert!(validate_process("a\\b.exe").is_err());
        assert!(validate_process("a\u{7}b.exe").is_err());
        assert_eq!(validate_process(" x.exe ").unwrap(), "x.exe");
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "  weixin.EXE ",
            &patch(json!({"auto_pair": true})),
        )
        .unwrap();
        assert_eq!(l.user.apps.len(), 1);
        assert_eq!(
            process_of(&l.user.apps[0]),
            "Weixin.exe",
            "沿用系统层的写法，不新增一条"
        );
    }

    /// 窗口类只改大小写或顺序，运行时等价，不算差异。
    #[test]
    fn classes_differing_only_in_case_or_order_are_not_a_modification() {
        let sys = "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"Progman\", \"WorkerW\"]\n";
        let mut l = layers(sys, "");
        l.upsert(
            Section::InitialModeScope,
            "explorer.exe",
            &patch(json!({"classes": ["workerw", "progman"]})),
        )
        .unwrap();
        assert!(
            l.user.initial_mode_scope.is_empty(),
            "{:?}",
            l.user.initial_mode_scope
        );
    }

    /// 元数据里登记的每个枚举可选值都必须被真实解析器接受并原样往返。
    #[test]
    fn every_registered_enum_option_roundtrips_through_the_real_parser() {
        use crate::compat_schema::{COMPAT_FIELDS, Kind};
        for f in COMPAT_FIELDS.iter().filter(|f| f.kind == Kind::Enum) {
            let sec = Section::parse(f.section).unwrap();
            for opt in f.options {
                let mut l = layers("", "");
                l.upsert(sec, "probe.exe", &patch(json!({ f.key: opt })))
                    .unwrap_or_else(|e| panic!("{}.{} = {opt} 被拒绝: {e}", f.section, f.key));
                let v = l.view_of(sec, "probe.exe").unwrap();
                assert_eq!(
                    v.effective[f.key],
                    json!(opt),
                    "{}.{} = {opt} 往返后不一致",
                    f.section,
                    f.key
                );
            }
        }
    }

    // ───────────── 还原 / 禁用 ─────────────

    #[test]
    fn reset_field_inherits_the_system_value_and_keeps_other_edits() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"caret_use_top": false, "auto_pair": true})),
        )
        .unwrap();
        l.reset_field(Section::Apps, "Weixin.exe", "caret_use_top")
            .unwrap();
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(v.effective["caret_use_top"], json!(true), "回到系统值");
        assert_eq!(v.effective["auto_pair"], json!(true), "另一个改写保留");
        assert_eq!(v.overridden, vec!["auto_pair".to_string()]);
    }

    #[test]
    fn reset_field_rejects_meta_keys_and_unknown_rules() {
        let mut l = layers(SYS, "");
        assert!(
            l.reset_field(Section::Apps, "Weixin.exe", "process")
                .is_err()
        );
        assert!(
            l.reset_field(Section::Apps, "Nope.exe", "auto_pair")
                .is_err()
        );
    }

    #[test]
    fn reset_removes_the_user_entry_and_reports_whether_it_existed() {
        let mut l = layers(SYS, USER_A);
        assert!(l.reset(Section::Apps, "weixin.exe"));
        assert_eq!(state_of(&l, "Weixin.exe"), RuleState::System);
        assert!(!l.reset(Section::Apps, "Weixin.exe"));
        let mut u = layers(SYS, "[[apps]]\nprocess = \" x.exe \"\nauto_pair = true\n");
        assert!(u.reset(Section::Apps, "x.exe"), "已存的名字带空白也要能删");
    }

    #[test]
    fn reset_all_clears_all_three_sections() {
        let usr = "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\n\n[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"X\"]\n\n[[commit_newline]]\nprocess = \"W.exe\"\nstyle = \"cr\"\n";
        let mut l = layers(SYS, usr);
        l.reset_all();
        let text = l.render_user().unwrap();
        assert!(!text.contains("[["), "{text}");
    }

    #[test]
    fn set_disabled_roundtrip_restores_prior_edits() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"auto_pair": true})),
        )
        .unwrap();
        l.set_disabled(Section::Apps, "Weixin.exe", true).unwrap();
        assert_eq!(state_of(&l, "Weixin.exe"), RuleState::Disabled);
        assert_eq!(
            user_row(&l, Section::Apps, "Weixin.exe").unwrap()["auto_pair"],
            json!(true),
            "禁用期间改写还在"
        );
        l.set_disabled(Section::Apps, "Weixin.exe", false).unwrap();
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(v.state, RuleState::Modified, "启用后回到禁用前的改写状态");
        assert_eq!(v.effective["auto_pair"], json!(true));
        assert!(
            user_row(&l, Section::Apps, "Weixin.exe")
                .unwrap()
                .get("disabled")
                .is_none()
        );
    }

    #[test]
    fn enabling_an_untouched_disabled_rule_removes_the_entry() {
        let mut l = layers(SYS, "");
        l.set_disabled(Section::Apps, "Weixin.exe", true).unwrap();
        l.set_disabled(Section::Apps, "Weixin.exe", false).unwrap();
        assert!(
            user_row(&l, Section::Apps, "Weixin.exe").is_none(),
            "与系统一致就不该留条目"
        );
        assert_eq!(state_of(&l, "Weixin.exe"), RuleState::System);
    }

    #[test]
    fn set_disabled_on_unknown_process_errors_and_user_only_rules_are_allowed() {
        let mut l = layers(SYS, "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\n");
        assert_eq!(
            l.set_disabled(Section::Apps, "Nope.exe", true).unwrap_err(),
            "无此规则"
        );
        l.set_disabled(Section::Apps, "Mine.exe", true).unwrap();
        assert_eq!(state_of(&l, "Mine.exe"), RuleState::Disabled);
        l.set_disabled(Section::Apps, "Mine.exe", false).unwrap();
        assert_eq!(state_of(&l, "Mine.exe"), RuleState::User);
    }

    // ───────────── 段之间 ─────────────

    #[test]
    fn sections_are_independent() {
        let sys = "[[apps]]\nprocess = \"explorer.exe\"\nauto_pair = true\n\n[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"CabinetWClass\"]\n";
        let mut l = layers(sys, "");
        l.upsert(
            Section::Apps,
            "explorer.exe",
            &patch(json!({"caret_use_top": true})),
        )
        .unwrap();
        assert!(
            l.user.initial_mode_scope.is_empty(),
            "改 apps 不能牵连 initial_mode_scope"
        );
        assert_eq!(
            l.view_of(Section::InitialModeScope, "explorer.exe")
                .unwrap()
                .state,
            RuleState::System
        );
    }

    #[test]
    fn scope_and_newline_sections_support_patch_and_disable() {
        let sys = "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"CabinetWClass\"]\n\n[[commit_newline]]\nprocess = \"WINWORD.EXE\"\nstyle = \"cr\"\n";
        let mut l = layers(sys, "");
        l.upsert(
            Section::InitialModeScope,
            "explorer.exe",
            &patch(json!({"classes": ["CabinetWClass", "Progman"]})),
        )
        .unwrap();
        assert_eq!(
            l.view_of(Section::InitialModeScope, "explorer.exe")
                .unwrap()
                .state,
            RuleState::Modified
        );
        l.set_disabled(Section::CommitNewline, "WINWORD.EXE", true)
            .unwrap();
        assert_eq!(
            l.view_of(Section::CommitNewline, "WINWORD.EXE")
                .unwrap()
                .state,
            RuleState::Disabled
        );
        assert!(
            l.upsert(
                Section::CommitNewline,
                "WINWORD.EXE",
                &patch(json!({"style": "bogus"}))
            )
            .is_err()
        );
    }

    // ───────────── 读写与文件 ─────────────

    #[test]
    fn strict_load_reports_syntax_error_with_path() {
        let dir = tmp("syntax");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(COMPAT_FILE_NAME), "[[apps\nprocess=").unwrap();
        let err = match Layers::load(None, None, Some(&dir)) {
            Err(e) => e,
            Ok(_) => panic!("语法错必须报错，而不是静默按空集处理"),
        };
        assert!(
            err.contains(COMPAT_FILE_NAME) && err.contains("line"),
            "应带路径与行号: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_user_file_is_an_empty_layer() {
        assert!(
            Layers::load(None, None, Some(&tmp("missing")))
                .unwrap()
                .user
                .apps
                .is_empty()
        );
    }

    #[test]
    fn save_is_atomic_and_preserves_other_sections_and_unknown_keys() {
        let dir = tmp("save");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(COMPAT_FILE_NAME),
            "[[commit_newline]]\nprocess = \"WINWORD.EXE\"\nstyle = \"cr\"\n\n[[apps]]\nprocess = \"Old.exe\"\nfuture_key = 3\nauto_pair = true\n",
        )
        .unwrap();
        let mut l = Layers::load(None, None, Some(&dir)).unwrap();
        l.upsert(
            Section::Apps,
            "Mine.exe",
            &patch(json!({"auto_pair": true})),
        )
        .unwrap();
        l.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(COMPAT_FILE_NAME)).unwrap();
        for needle in [
            "[[commit_newline]]",
            "Mine.exe",
            "Old.exe",
            "future_key = 3",
        ] {
            assert!(text.contains(needle), "写回后丢了 {needle}: {text}");
        }
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不应残留临时文件: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ───────────── 导出 / 导入 ─────────────

    #[test]
    fn export_user_contains_only_the_diff() {
        let l = layers(SYS, USER_A);
        let text = l.export(ExportScope::User).unwrap();
        assert!(text.contains("auto_pair = true"), "{text}");
        assert!(
            !text.contains("caret_use_top") && !text.contains("Feishu"),
            "用户导出只含差异: {text}"
        );
    }

    #[test]
    fn export_effective_expands_the_overlay_and_drops_disabled_but_keeps_unset() {
        let usr = format!(
            "{USER_A}\n[[apps]]\nprocess = \"Dota.exe\"\ndisabled = true\n\n[[apps]]\nprocess = \"Feishu.exe\"\nunset = [\"composition_start_pair_guard\"]\n"
        );
        let l = layers(SYS, &usr);
        let text = l.export(ExportScope::Effective).unwrap();
        assert!(
            text.contains("caret_use_top = true") && text.contains("auto_pair = true"),
            "叠加后的全貌: {text}"
        );
        assert!(
            !text.contains("Dota") && !text.contains("disabled"),
            "被禁用的不导出: {text}"
        );
        assert!(
            !text.contains("composition_start_pair_guard = "),
            "unset 掉的字段不该以取值形式出现: {text}"
        );
        // 保留 unset 记号：同一套系统层上重新导入，被取消的字段不能又继承回来。
        let mut b = layers(SYS, "");
        b.import_apply(&text, ImportMode::Replace).unwrap();
        let (va, vb) = (
            l.view_of(Section::Apps, "Feishu.exe").unwrap(),
            b.view_of(Section::Apps, "Feishu.exe").unwrap(),
        );
        assert_eq!(va.effective, vb.effective, "生效导出回导后结果必须一致");
    }

    #[test]
    fn export_then_import_replace_roundtrips() {
        let usr = format!(
            "{USER_A}\n[[apps]]\nprocess = \"Dota.exe\"\ndisabled = true\n\n[[apps]]\nprocess = \"Feishu.exe\"\nunset = [\"composition_start_pair_guard\"]\n"
        );
        let a = layers(SYS, &usr);
        let text = a.export(ExportScope::User).unwrap();
        let mut b = layers(SYS, "");
        b.import_apply(&text, ImportMode::Replace).unwrap();
        for p in ["Feishu.exe", "Weixin.exe", "Dota.exe"] {
            let (va, vb) = (
                a.view_of(Section::Apps, p).unwrap(),
                b.view_of(Section::Apps, p).unwrap(),
            );
            assert_eq!((va.state, &va.effective), (vb.state, &vb.effective), "{p}");
        }
    }

    #[test]
    fn import_preview_classifies_add_override_disable_unchanged() {
        let incoming = "\
[[apps]]
process = \"Brand.exe\"
auto_pair = true

[[apps]]
process = \"Weixin.exe\"
auto_pair = true

[[apps]]
process = \"Dota.exe\"
disabled = true

[[apps]]
process = \"Feishu.exe\"
composition_start_pair_guard = true
";
        let p = layers(SYS, "")
            .import_preview(incoming, ImportMode::Merge)
            .unwrap();
        let act = |n: &str| p.items.iter().find(|i| i.process == n).map(|i| i.action);
        assert_eq!(act("Brand.exe"), Some("add"));
        assert_eq!(act("Weixin.exe"), Some("override"));
        assert_eq!(act("Dota.exe"), Some("disable"));
        assert_eq!(
            act("Feishu.exe"),
            Some("unchanged"),
            "与系统完全一致 = 无变化"
        );
    }

    #[test]
    fn import_preview_does_not_mutate_layers() {
        let l = layers(SYS, USER_A);
        let before: Vec<_> = l
            .view(Section::Apps)
            .into_iter()
            .map(|v| (v.process, v.state))
            .collect();
        let _ = l
            .import_preview(
                "[[apps]]\nprocess = \"Brand.exe\"\nauto_pair = true\n",
                ImportMode::Replace,
            )
            .unwrap();
        let after: Vec<_> = l
            .view(Section::Apps)
            .into_iter()
            .map(|v| (v.process, v.state))
            .collect();
        assert_eq!(before, after);
    }

    /// 导入按字段叠加：稀疏片段不得让系统规则里的其它字段丢失，显式 false 能覆盖出厂的 true。
    #[test]
    fn import_sparse_snippet_keeps_the_system_fields_and_explicit_false_overrides() {
        let mut l = layers(SYS, "");
        l.import_apply(
            "[[apps]]\nprocess = \"weixin.exe\"\nfirst_show_mode = \"wait\"\n",
            ImportMode::Merge,
        )
        .unwrap();
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(v.effective["caret_use_top"], json!(true), "出厂修复不能丢");
        assert_eq!(v.effective["stale_probe_guard"], json!(true));
        assert_eq!(v.effective["first_show_mode"], json!("wait"));
        l.import_apply(
            "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = false\n",
            ImportMode::Merge,
        )
        .unwrap();
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert!(
            v.effective.get("caret_use_top").is_none(),
            "显式 false 应生效: {:?}",
            v.effective
        );
        assert_eq!(
            v.effective["first_show_mode"],
            json!("wait"),
            "之前导入的字段仍在（叠加而非替换）"
        );
    }

    #[test]
    fn import_understands_unset() {
        let mut l = layers(SYS, "");
        l.import_apply(
            "[[apps]]\nprocess = \"Weixin.exe\"\nunset = [\"caret_use_top\"]\n",
            ImportMode::Merge,
        )
        .unwrap();
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert!(v.effective.get("caret_use_top").is_none());
        assert_eq!(v.state, RuleState::Modified);
    }

    #[test]
    fn import_merge_keeps_unmentioned_user_entries_and_replace_drops_them() {
        let mut m = layers(SYS, "[[apps]]\nprocess = \"E.exe\"\nauto_pair = true\n");
        m.import_apply(
            "[[apps]]\nprocess = \"C.exe\"\nauto_pair = true\n",
            ImportMode::Merge,
        )
        .unwrap();
        assert!(
            m.view_of(Section::Apps, "E.exe").is_some()
                && m.view_of(Section::Apps, "C.exe").is_some()
        );
        let mut r = layers(SYS, "[[apps]]\nprocess = \"E.exe\"\nauto_pair = true\n");
        r.import_apply(
            "[[apps]]\nprocess = \"C.exe\"\nauto_pair = true\n",
            ImportMode::Replace,
        )
        .unwrap();
        assert!(
            r.view_of(Section::Apps, "E.exe").is_none(),
            "Replace 要先清空用户层"
        );
    }

    #[test]
    fn import_reports_unknown_keys_and_sections() {
        let text = "[[apps]]\nprocess = \"A.exe\"\nbogus_key = 1\nauto_pair = true\n\n[[mystery]]\nx = 1\n";
        let p = layers(SYS, "")
            .import_preview(text, ImportMode::Merge)
            .unwrap();
        assert!(
            p.ignored_keys.contains(&"apps.bogus_key".to_string()),
            "{:?}",
            p.ignored_keys
        );
        assert!(
            p.ignored_keys.contains(&"mystery".to_string()),
            "{:?}",
            p.ignored_keys
        );
    }

    /// 无效取值只丢这一个键并报告，**且不能原样落进用户层文件**。
    #[test]
    fn import_drops_invalid_values_reports_them_and_never_persists_them() {
        let text = "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"bogus\"\nauto_pair = true\n";
        let mut l = layers(SYS, "");
        let p = l.import_apply(text, ImportMode::Merge).unwrap();
        assert!(
            p.fallbacks.iter().any(|f| f.contains("bogus")),
            "{:?}",
            p.fallbacks
        );
        let row = user_row(&l, Section::Apps, "A.exe").unwrap();
        assert!(
            row.get("first_show_mode").is_none(),
            "无效值不得落盘: {row:?}"
        );
        assert_eq!(row["auto_pair"], json!(true), "同一条里的有效字段照常导入");
    }

    #[test]
    fn import_rejects_invalid_process_names_and_reports_them() {
        let mut l = layers(SYS, "");
        let p = l.import_apply("[[apps]]\nprocess = \" a/b.exe \"\nauto_pair = true\n\n[[apps]]\nprocess = \"ok.exe\"\nauto_pair = true\n", ImportMode::Merge).unwrap();
        assert_eq!(p.rejected.len(), 1, "{:?}", p.rejected);
        assert!(l.user.apps.iter().all(|r| !process_of(r).contains('/')));
        assert!(l.view_of(Section::Apps, "ok.exe").is_some());
    }

    #[test]
    fn import_syntax_error_is_an_error_not_silent() {
        let err = layers(SYS, "")
            .import_preview("[[apps\nprocess=", ImportMode::Merge)
            .unwrap_err();
        assert!(err.contains("line"), "应带行号: {err}");
    }

    #[test]
    fn importing_the_same_content_twice_is_idempotent() {
        let text = "[[apps]]\nprocess = \"Weixin.exe\"\nauto_pair = true\n";
        let mut l = layers(SYS, "");
        assert_eq!(
            l.import_apply(text, ImportMode::Merge).unwrap().items[0].action,
            "override"
        );
        assert_eq!(
            l.import_apply(text, ImportMode::Merge).unwrap().items[0].action,
            "unchanged"
        );
    }

    // ───────────── RPC ─────────────

    /// 用户目录里所有 `compat.toml.bak-*` 备份，按名字排序。
    fn backups(dir: &Path) -> Vec<std::path::PathBuf> {
        let prefix = format!("{COMPAT_FILE_NAME}.bak-");
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(&prefix))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    fn rpc_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let (d, u) = (tmp(&format!("{tag}_d")), tmp(&format!("{tag}_u")));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&u);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(COMPAT_FILE_NAME), SYS).unwrap();
        (d, u)
    }

    fn call(m: &str, p: Value, d: &Path, u: &Path) -> Result<RpcOutcome, String> {
        rpc(m, &p, Some(d), None, Some(u))
    }

    fn cleanup(d: &Path, u: &Path) {
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(u);
    }

    #[test]
    fn rpc_list_returns_views_and_warnings() {
        let (d, u) = rpc_dirs("list");
        let r = call("compat.list", json!({"section": "apps"}), &d, &u).unwrap();
        assert!(!r.wrote);
        assert_eq!(r.value["rules"].as_array().unwrap().len(), 3);
        assert!(r.value["warnings"].is_array());
        cleanup(&d, &u);
    }

    #[test]
    fn rpc_unknown_method_and_bad_section_error() {
        let (d, u) = rpc_dirs("bad");
        assert!(call("compat.bogus", json!({}), &d, &u).is_err());
        assert!(call("compat.list", json!({"section": "x"}), &d, &u).is_err());
        assert!(call("compat.list", json!({}), &d, &u).is_err());
        cleanup(&d, &u);
    }

    #[test]
    fn rpc_write_methods_report_wrote_true_and_persist() {
        let (d, u) = rpc_dirs("write");
        let r = call(
            "compat.upsert",
            json!({"section": "apps", "process": "Weixin.exe", "patch": {"auto_pair": true}}),
            &d,
            &u,
        )
        .unwrap();
        assert!(r.wrote);
        assert_eq!(r.value["rule"]["state"], json!("modified"));
        assert!(
            std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
                .unwrap()
                .contains("auto_pair")
        );

        let r = call(
            "compat.setDisabled",
            json!({"section": "apps", "process": "Feishu.exe", "disabled": true}),
            &d,
            &u,
        )
        .unwrap();
        assert!(r.wrote);
        assert_eq!(r.value["rule"]["state"], json!("disabled"));

        let r = call(
            "compat.resetField",
            json!({"section": "apps", "process": "Weixin.exe", "key": "auto_pair"}),
            &d,
            &u,
        )
        .unwrap();
        assert!(r.wrote);
        assert_eq!(r.value["rule"]["state"], json!("system"));

        let r = call(
            "compat.reset",
            json!({"section": "apps", "process": "Feishu.exe"}),
            &d,
            &u,
        )
        .unwrap();
        assert!(r.wrote);
        assert_eq!(r.value["removed"], json!(true));

        // 只读方法不写
        for (m, p) in [
            ("compat.list", json!({"section": "apps"})),
            ("compat.schema", json!({})),
            ("compat.export", json!({"scope": "effective"})),
        ] {
            assert!(!call(m, p, &d, &u).unwrap().wrote, "{m} 不应报告写入");
        }
        cleanup(&d, &u);
    }

    #[test]
    fn rpc_upsert_rejects_bad_value_and_does_not_touch_disk() {
        let (d, u) = rpc_dirs("reject");
        let err = call(
            "compat.upsert",
            json!({"section": "apps", "process": "Weixin.exe", "patch": {"first_show_mode": "bogus"}}),
            &d,
            &u,
        )
        .err()
        .expect("非法取值必须被拒绝");
        assert!(err.contains("first_show_mode"), "{err}");
        assert!(
            !u.join(COMPAT_FILE_NAME).exists(),
            "被拒绝的写入不应产生文件"
        );
        cleanup(&d, &u);
    }

    #[test]
    fn rpc_write_without_user_dir_errors() {
        let (d, u) = rpc_dirs("nodir");
        let r = rpc(
            "compat.upsert",
            &json!({"section": "apps", "process": "A.exe", "patch": {"auto_pair": true}}),
            Some(&d),
            None,
            None,
        );
        assert_eq!(r.err().as_deref(), Some("无用户配置目录"));
        cleanup(&d, &u);
    }

    // ───────────── 旧键 ignore_host_ime_close（读取别名）经设置端全链路 ─────────────

    const LEGACY_SYS: &str = "[[apps]]\nprocess = \"X60.exe\"\nignore_host_ime_switch = true\n";
    const LEGACY_USER: &str = "[[apps]]\nprocess = \"X60.exe\"\nignore_host_ime_close = false\nfuture_key = 3\n\n\
                               [[apps]]\nprocess = \"Old.exe\"\nignore_host_ime_close = true\n";

    fn legacy_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let (d, u) = (tmp(&format!("{tag}_d")), tmp(&format!("{tag}_u")));
        for (dir, text) in [(&d, LEGACY_SYS), (&u, LEGACY_USER)] {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(COMPAT_FILE_NAME), text).unwrap();
        }
        (d, u)
    }

    fn listed_rule(d: &Path, u: &Path, process: &str) -> Value {
        let r = call("compat.list", json!({"section": "apps"}), d, u).unwrap();
        r.value["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["process"] == json!(process))
            .unwrap_or_else(|| panic!("列表里没有 {process}"))
            .clone()
    }

    /// 用户文件写旧键：列表在新键下看到用户值，也不把旧键报成不认识的键。
    #[test]
    fn rpc_list_shows_legacy_key_under_the_new_key() {
        let (d, u) = legacy_dirs("lg_list");
        let x60 = listed_rule(&d, &u, "X60.exe");
        let f = &x60["fields"]["ignore_host_ime_switch"];
        assert_eq!(f["userValue"], json!(false), "{x60}");
        assert_eq!(f["systemValue"], json!(true), "{x60}");
        assert_eq!(x60["unknownKeys"], json!(["future_key"]), "{x60}");
        assert!(x60["fields"].get("ignore_host_ime_close").is_none());
        let old = listed_rule(&d, &u, "Old.exe");
        assert_eq!(
            old["fields"]["ignore_host_ime_switch"]["userValue"],
            json!(true),
            "{old}"
        );
        cleanup(&d, &u);
    }

    /// 对写了旧键的规则清除（patch null）/ 还原（resetField）：结果按新键算，文件里不残留旧键，
    /// 不认识的键原样保留。
    #[test]
    fn rpc_clear_and_reset_field_on_a_legacy_key_leave_only_the_new_key() {
        let (d, u) = legacy_dirs("lg_clear");
        let r = call(
            "compat.upsert",
            json!({"section": "apps", "process": "X60.exe", "patch": {"ignore_host_ime_switch": null}}),
            &d,
            &u,
        )
        .unwrap();
        assert!(r.wrote);
        assert_eq!(
            r.value["rule"]["fields"]["ignore_host_ime_switch"]["source"],
            json!("cleared"),
            "{}",
            r.value
        );
        let text = std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap();
        assert!(!text.contains("ignore_host_ime_close"), "{text}");
        assert!(
            text.contains("unset = [\"ignore_host_ime_switch\"]"),
            "{text}"
        );
        assert!(text.contains("future_key = 3"), "{text}");
        cleanup(&d, &u);

        let (d, u) = legacy_dirs("lg_reset");
        let r = call(
            "compat.resetField",
            json!({"section": "apps", "process": "X60.exe", "key": "ignore_host_ime_switch"}),
            &d,
            &u,
        )
        .unwrap();
        assert!(r.wrote);
        let f = &r.value["rule"]["fields"]["ignore_host_ime_switch"];
        assert_eq!(f["source"], json!("system"), "{}", r.value);
        assert_eq!(f["value"], json!(true), "还原 = 继承出厂");
        let text = std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap();
        assert!(!text.contains("ignore_host_ime_close"), "{text}");
        assert!(text.contains("future_key = 3"), "{text}");
        assert!(
            text.contains("ignore_host_ime_switch = true"),
            "别的规则里的旧键换成新键、值不变: {text}"
        );
        cleanup(&d, &u);
    }

    /// 导入含旧键的片段：不报不认识的键，落盘用新键。
    #[test]
    fn import_of_a_legacy_key_is_understood_and_persisted_as_the_new_key() {
        let text = "[[apps]]\nprocess = \"New.exe\"\nignore_host_ime_close = true\n";
        let mut l = layers(LEGACY_SYS, "");
        let p = l.import_preview(text, ImportMode::Merge).unwrap();
        assert!(p.ignored_keys.is_empty(), "{:?}", p.ignored_keys);
        assert!(p.fallbacks.is_empty(), "{:?}", p.fallbacks);
        l.import_apply(text, ImportMode::Merge).unwrap();
        let row = user_row(&l, Section::Apps, "New.exe").expect("导入后应有用户条目");
        assert_eq!(row.get("ignore_host_ime_switch"), Some(&json!(true)));
        assert!(!row.contains_key("ignore_host_ime_close"));
        assert!(
            !l.render_user().unwrap().contains("ignore_host_ime_close"),
            "落盘文本不残留旧键"
        );
    }

    #[test]
    fn rpc_import_dry_run_does_not_write() {
        let (d, u) = rpc_dirs("dry");
        let r = call(
            "compat.import",
            json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "dryRun": true}),
            &d,
            &u,
        )
        .unwrap();
        assert!(!r.wrote);
        assert_eq!(r.value["applied"], json!(false));
        assert_eq!(r.value["items"][0]["action"], json!("add"));
        assert!(!u.join(COMPAT_FILE_NAME).exists());
        cleanup(&d, &u);
    }

    #[test]
    fn rpc_import_backs_up_existing_user_file() {
        let (d, u) = rpc_dirs("bak");
        std::fs::create_dir_all(&u).unwrap();
        let original = "[[apps]]\nprocess = \"Old.exe\"\nauto_pair = true\n";
        std::fs::write(u.join(COMPAT_FILE_NAME), original).unwrap();
        let r = call(
            "compat.import",
            json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "mode": "replace"}),
            &d,
            &u,
        )
        .unwrap();
        assert!(r.wrote);
        assert_eq!(r.value["applied"], json!(true));
        let found = backups(&u);
        assert_eq!(found.len(), 1, "应恰好留下一份备份: {found:?}");
        assert_eq!(
            std::fs::read_to_string(&found[0]).unwrap(),
            original,
            "备份必须是导入前的原文"
        );
        assert!(
            r.value["backup"].as_str().is_some(),
            "响应里要告诉界面备份文件名: {}",
            r.value
        );
        assert!(
            std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
                .unwrap()
                .contains("New.exe")
        );
        cleanup(&d, &u);
    }

    #[test]
    fn rpc_reset_all_backs_up_and_clears() {
        let (d, u) = rpc_dirs("all");
        std::fs::create_dir_all(&u).unwrap();
        let original = "[[apps]]\nprocess = \"Old.exe\"\nauto_pair = true\n";
        std::fs::write(u.join(COMPAT_FILE_NAME), original).unwrap();
        let r = call("compat.resetAll", json!({}), &d, &u).unwrap();
        assert!(r.wrote);
        let found = backups(&u);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(std::fs::read_to_string(&found[0]).unwrap(), original);
        assert!(
            !std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
                .unwrap()
                .contains("Old.exe")
        );
        cleanup(&d, &u);
    }

    #[test]
    fn rpc_write_on_broken_user_file_errors_and_keeps_it() {
        let (d, u) = rpc_dirs("broken");
        std::fs::create_dir_all(&u).unwrap();
        let broken = "[[apps\nprocess=";
        std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
        let err = call(
            "compat.upsert",
            json!({"section": "apps", "process": "A.exe", "patch": {"auto_pair": true}}),
            &d,
            &u,
        )
        .err()
        .expect("用户层语法错必须报错");
        assert!(err.contains("line"), "{err}");
        assert_eq!(
            std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap(),
            broken,
            "出错时不得改写（更不能按空集重写）用户层"
        );
        cleanup(&d, &u);
    }

    // ───────────── 独立审查（2026-09-29）提出的回归 ─────────────

    /// data_custom 层禁用的规则：视图与运行时一致（运行时没有它），且可以在用户层重新启用。
    #[test]
    fn rule_disabled_in_custom_layer_is_disabled_in_view_and_at_runtime() {
        let (d, c) = (tmp("cust_d"), tmp("cust_c"));
        for dir in [&d, &c] {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(d.join(COMPAT_FILE_NAME), SYS).unwrap();
        std::fs::write(
            c.join(COMPAT_FILE_NAME),
            "[[apps]]\nprocess = \"Weixin.exe\"\ndisabled = true\n",
        )
        .unwrap();
        let mut l = Layers::load(Some(&d), Some(&c), None).unwrap();
        assert_eq!(
            state_of(&l, "Weixin.exe"),
            RuleState::Disabled,
            "视图必须反映它被禁用"
        );
        assert!(
            crate::app_compat::AppCompat::load_layered(Some(&d), Some(&c), None)
                .get_rule("Weixin.exe")
                .is_none(),
            "运行时确实没有它"
        );
        l.set_disabled(Section::Apps, "Weixin.exe", false).unwrap();
        assert_eq!(
            state_of(&l, "Weixin.exe"),
            RuleState::Modified,
            "用户层显式 disabled = false 重新启用"
        );
        assert!(
            user_row(&l, Section::Apps, "Weixin.exe")
                .unwrap()
                .get("disabled")
                == Some(&json!(false))
        );
        cleanup(&d, &c);
    }

    /// S2 的反方向：运行时接受的写法（别名、带空白）不得被误拒。
    #[test]
    fn patch_accepts_aliases_and_padded_text() {
        let mut l = layers(SYS, "");
        l.upsert(
            Section::Apps,
            "Mine.exe",
            &patch(json!({"initial_mode": "en"})),
        )
        .expect("别名 en 是合法写法");
        l.upsert(
            Section::Apps,
            "Mine.exe",
            &patch(json!({"schema": " wubi86 "})),
        )
        .expect("带空白的方案 id 运行时会 trim，应接受");
    }

    /// S4：replace 模式的预览要让用户看到「哪些会被删掉」。
    #[test]
    fn replace_preview_lists_entries_that_will_be_removed() {
        let l = layers(SYS, "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\n");
        let p = l
            .import_preview(
                "[[apps]]\nprocess = \"Other.exe\"\nauto_pair = true\n",
                ImportMode::Replace,
            )
            .unwrap();
        assert!(
            p.items
                .iter()
                .any(|i| i.process == "Mine.exe" && i.action == "remove"),
            "{:?}",
            p.items
        );
    }

    /// S5：用户层语法坏了，界面仍要有恢复手段：resetAll 与 replace 导入不依赖解析用户层。
    #[test]
    fn reset_all_and_replace_import_recover_from_a_broken_user_file() {
        let (d, u) = rpc_dirs("recover");
        std::fs::create_dir_all(&u).unwrap();
        let broken = "[[apps\nprocess=";
        std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
        let r = call("compat.resetAll", json!({}), &d, &u).expect("resetAll 必须能恢复");
        assert!(r.wrote);
        assert!(
            std::fs::read_dir(&u)
                .unwrap()
                .filter_map(Result::ok)
                .any(|e| std::fs::read_to_string(e.path()).ok().as_deref() == Some(broken)),
            "损坏的原文件必须被备份，而不是被抹掉"
        );

        std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
        let r = call(
            "compat.import",
            json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "mode": "replace"}),
            &d,
            &u,
        )
        .expect("replace 导入必须能恢复");
        assert!(r.wrote);
        assert!(
            std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
                .unwrap()
                .contains("New.exe")
        );

        // 合并导入不能：它要保留用户层已有内容，而那份内容读不出来。
        std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
        assert!(
            call(
                "compat.import",
                json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "mode": "merge"}),
                &d,
                &u
            )
            .is_err()
        );
        cleanup(&d, &u);
    }

    /// M3：并发写不得互相覆盖（读-改-写整段持锁）。
    #[test]
    fn concurrent_writes_do_not_lose_updates() {
        let (d, u) = rpc_dirs("race");
        let (d, u) = (std::sync::Arc::new(d), std::sync::Arc::new(u));
        let handles: Vec<_> = (0..12)
            .map(|i| {
                let (d, u) = (d.clone(), u.clone());
                std::thread::spawn(move || {
                    call(
                        "compat.upsert",
                        json!({"section": "apps", "process": format!("P{i}.exe"), "patch": {"auto_pair": true}}),
                        &d,
                        &u,
                    )
                    .expect("并发写不应报错");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let r = call("compat.list", json!({"section": "apps"}), &d, &u).unwrap();
        let users = r.value["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["state"] == json!("user"))
            .count();
        assert_eq!(users, 12, "12 次并发写应全部留下，丢了 {} 条", 12 - users);
        cleanup(&d, &u);
    }

    /// 可选 7：`reset` 什么也没删时不该报告写入（否则白白写盘、重载、广播）。
    #[test]
    fn reset_of_nonexistent_user_entry_reports_no_write() {
        let (d, u) = rpc_dirs("noop_reset");
        let r = call(
            "compat.reset",
            json!({"section": "apps", "process": "Weixin.exe"}),
            &d,
            &u,
        )
        .unwrap();
        assert!(!r.wrote);
        assert_eq!(r.value["removed"], json!(false));
        cleanup(&d, &u);
    }

    /// 备份最多留 5 份，且是最新的 5 份：连续多次导入后，「导入前的样子」仍能找回。
    #[test]
    fn backups_are_rotated_and_keep_the_newest() {
        let (d, u) = rpc_dirs("rotate");
        std::fs::create_dir_all(&u).unwrap();
        for i in 0..8 {
            std::fs::write(
                u.join(COMPAT_FILE_NAME),
                format!("[[apps]]\nprocess = \"V{i}.exe\"\nauto_pair = true\n"),
            )
            .unwrap();
            call(
                "compat.import",
                json!({"content": "[[apps]]\nprocess = \"X.exe\"\nauto_pair = true\n", "mode": "replace"}),
                &d,
                &u,
            )
            .unwrap();
        }
        let found = backups(&u);
        assert_eq!(found.len(), BACKUP_KEEP, "{found:?}");
        let newest = std::fs::read_to_string(found.last().unwrap()).unwrap();
        assert!(
            newest.contains("V7.exe"),
            "最新一份应是最后一次导入前的内容: {newest}"
        );
        cleanup(&d, &u);
    }

    /// dryRun 传了非布尔值必须报错，不能悄悄当成 false 而真的写入。
    #[test]
    fn non_boolean_dry_run_is_an_error_not_a_write() {
        let (d, u) = rpc_dirs("dryrun_type");
        let err = call(
            "compat.import",
            json!({"content": "[[apps]]\nprocess = \"N.exe\"\nauto_pair = true\n", "dryRun": "true"}),
            &d,
            &u,
        )
        .err()
        .expect("dryRun 类型不对必须报错");
        assert!(err.contains("dryRun"), "{err}");
        assert!(!u.join(COMPAT_FILE_NAME).exists());
        cleanup(&d, &u);
    }

    // ───────────── 审查（第二轮）补的回归 ─────────────

    /// `compat.list` 的 warnings 曾恒为空（加载不再经反序列化，回落记录取不到）。
    #[test]
    fn warnings_report_invalid_values_from_every_layer() {
        let sys = "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"wiat\"\n";
        let usr = "[[apps]]\nprocess = \"A.exe\"\ninitial_mode = \"englsh\"\n";
        let (d, u) = (tmp("warn_d"), tmp("warn_u"));
        for (dir, text) in [(&d, sys), (&u, usr)] {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(COMPAT_FILE_NAME), text).unwrap();
        }
        let l = Layers::load(Some(&d), None, Some(&u)).unwrap();
        let w = l.warnings();
        assert_eq!(w.len(), 2, "{w:?}");
        let keys: Vec<_> = w
            .iter()
            .map(|p| (p.section.as_str(), p.process.as_str(), p.key.as_str()))
            .collect();
        assert!(
            keys.contains(&("apps", "A.exe", "first_show_mode")),
            "带上段 / 进程 / 键: {keys:?}"
        );
        assert!(
            keys.contains(&("apps", "A.exe", "initial_mode")),
            "{keys:?}"
        );
        assert!(w.iter().all(|p| !p.message.is_empty()));
        let r = rpc(
            "compat.list",
            &json!({"section": "apps"}),
            Some(&d),
            None,
            Some(&u),
        )
        .unwrap();
        assert_eq!(
            r.value["warnings"].as_array().unwrap().len(),
            2,
            "{}",
            r.value
        );
        cleanup(&d, &u);
    }

    /// 用户层写错的值在视图里也当成没写：不能盖掉下层的好值。
    #[test]
    fn an_invalid_user_value_does_not_hide_the_system_value_in_the_view() {
        let l = layers(
            "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"wait\"\n",
            "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"wiat\"\n",
        );
        let v = l.view_of(Section::Apps, "A.exe").unwrap();
        assert_eq!(v.effective["first_show_mode"], json!("wait"));
        assert_eq!(v.state, RuleState::System, "写错的值不算差异");
    }

    /// 认不出的键不该让规则显示成「已修改」，也不进 overridden；单独列在 unknown_keys 里。
    #[test]
    fn unknown_keys_do_not_make_a_rule_modified() {
        let l = layers(
            SYS,
            "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_tpo = true\n",
        );
        let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
        assert_eq!(v.state, RuleState::System);
        assert!(v.overridden.is_empty(), "{:?}", v.overridden);
        assert_eq!(v.unknown_keys, vec!["caret_use_tpo".to_string()]);
    }

    /// 下层禁用、用户层显式 `disabled = false` 重新启用：状态是 Modified，overridden 里要有 disabled。
    #[test]
    fn re_enabling_a_lower_layer_disable_is_listed_as_an_override() {
        let l = layers(
            "[[apps]]\nprocess = \"A.exe\"\ndisabled = true\ncaret_use_top = true\n",
            "[[apps]]\nprocess = \"A.exe\"\ndisabled = false\n",
        );
        let v = l.view_of(Section::Apps, "A.exe").unwrap();
        assert_eq!(v.state, RuleState::Modified);
        assert_eq!(v.overridden, vec!["disabled".to_string()]);
    }

    /// 用户层有读不进来的内容（`[apps]` 单括号）：写操作 / 列表报错，恢复类操作照常。
    #[test]
    fn unreadable_user_content_is_an_error_but_reset_all_recovers() {
        let (d, u) = rpc_dirs("skipped");
        std::fs::create_dir_all(&u).unwrap();
        let text = "[apps]\nprocess = \"lost.exe\"\n";
        std::fs::write(u.join(COMPAT_FILE_NAME), text).unwrap();
        let err = call("compat.list", json!({"section": "apps"}), &d, &u)
            .err()
            .expect("必须报错");
        assert!(err.contains("无法读入"), "{err}");
        assert!(
            call(
                "compat.upsert",
                json!({"section": "apps", "process": "A.exe", "patch": {"auto_pair": true}}),
                &d,
                &u
            )
            .is_err()
        );
        call("compat.resetAll", json!({}), &d, &u).expect("resetAll 必须能恢复");
        assert!(
            backups(&u)
                .iter()
                .any(|p| std::fs::read_to_string(p).ok().as_deref() == Some(text)),
            "原文件必须被备份"
        );
        cleanup(&d, &u);
    }

    /// 用户目录与系统预置目录相同（拿不到用户配置目录的兜底）时拒绝写入，免得删掉出厂字段。
    #[test]
    fn writes_are_refused_when_the_user_dir_is_the_system_dir() {
        let (d, u) = rpc_dirs("samedir");
        let err = rpc(
            "compat.upsert",
            &json!({"section": "apps", "process": "Weixin.exe", "patch": {"auto_pair": true}}),
            Some(&d),
            None,
            Some(&d),
        )
        .err()
        .expect("必须拒绝");
        assert!(err.contains("相同"), "{err}");
        assert_eq!(
            std::fs::read_to_string(d.join(COMPAT_FILE_NAME)).unwrap(),
            SYS,
            "系统预置文件一个字节都不能动"
        );
        // 只读方法不受限。
        assert!(
            rpc(
                "compat.list",
                &json!({"section": "apps"}),
                Some(&d),
                None,
                Some(&d)
            )
            .is_ok()
        );
        cleanup(&d, &u);
    }

    // ───────────── 逐字段视图 fields（设置端渲染的唯一依据） ─────────────

    fn field_of(l: &Layers, process: &str, key: &str) -> FieldView {
        l.view_of(Section::Apps, process)
            .unwrap_or_else(|| panic!("规则 {process} 应存在"))
            .fields
            .get(key)
            .unwrap_or_else(|| panic!("fields 缺少 {key}"))
            .clone()
    }

    #[test]
    fn fields_source_system_shows_factory_value_and_nothing_from_the_user() {
        let l = layers(SYS, "");
        let f = field_of(&l, "Weixin.exe", "caret_use_top");
        assert_eq!(f.source, FieldSource::System);
        assert_eq!(f.value, json!(true));
        assert_eq!(f.system_value, json!(true));
        assert_eq!(f.user_value, Value::Null);
    }

    #[test]
    fn fields_source_user_when_the_user_really_differs() {
        let l = layers(
            SYS,
            "[[apps]]\nprocess = \"Weixin.exe\"\nfirst_show_mode = \"wait\"\n",
        );
        let f = field_of(&l, "Weixin.exe", "first_show_mode");
        assert_eq!(f.source, FieldSource::User);
        assert_eq!(
            (f.value, f.user_value, f.system_value),
            (json!("wait"), json!("wait"), Value::Null)
        );
    }

    /// 用户显式关掉出厂打开的开关：`effective` 里这个键会消失，`fields` 必须仍然说清「用户设为关」。
    #[test]
    fn fields_distinguish_user_off_from_user_cleared() {
        let l = layers(
            SYS,
            "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = false\nunset = [\"stale_probe_guard\"]\n",
        );
        let off = field_of(&l, "Weixin.exe", "caret_use_top");
        assert_eq!(off.source, FieldSource::User, "显式关 = 用户改写");
        assert_eq!(off.value, json!(false));
        assert_eq!(off.user_value, json!(false));
        assert_eq!(off.system_value, json!(true));
        let cleared = field_of(&l, "Weixin.exe", "stale_probe_guard");
        assert_eq!(cleared.source, FieldSource::Cleared, "unset = 取消出厂设定");
        assert_eq!(
            cleared.value,
            json!(false),
            "Bool 的生效值是类型化默认值 false，不是 null"
        );
        assert_eq!(cleared.system_value, json!(true));
        assert_eq!(cleared.user_value, Value::Null);
    }

    #[test]
    fn fields_source_default_when_nobody_set_it_and_values_are_typed() {
        let l = layers(SYS, "");
        let sec_fields = l.view_of(Section::Apps, "Weixin.exe").unwrap().fields;
        let f = &sec_fields["first_show_mode"];
        assert_eq!(
            (f.source, &f.value),
            (FieldSource::Default, &Value::Null),
            "Enum 未设为 null"
        );
        assert_eq!(
            sec_fields["caret_offset_x"].value,
            json!(0),
            "Int 的默认值是 0"
        );
        assert_eq!(
            sec_fields["auto_pair"].value,
            Value::Null,
            "TriBool 未设为 null"
        );
        assert_eq!(
            sec_fields["host_render"].value,
            json!(false),
            "Bool 的默认值是 false"
        );
        let scope = layers(
            "",
            "[[initial_mode_scope]]\nprocess = \"x.exe\"\nclasses = [\"A\"]\n",
        );
        assert_eq!(
            scope
                .view_of(Section::InitialModeScope, "x.exe")
                .unwrap()
                .fields["classes"]
                .value,
            json!(["A"])
        );
    }

    #[test]
    fn fields_ignore_invalid_and_redundant_user_values() {
        let l = layers(
            SYS,
            "[[apps]]\nprocess = \"Weixin.exe\"\nfirst_show_mode = \"wiat\"\ncaret_use_top = true\n",
        );
        assert_eq!(
            field_of(&l, "Weixin.exe", "first_show_mode").source,
            FieldSource::Default,
            "写错的值当没写"
        );
        let same = field_of(&l, "Weixin.exe", "caret_use_top");
        assert_eq!(
            same.source,
            FieldSource::System,
            "与系统一致的冗余不算用户改写"
        );
        assert_eq!(same.user_value, Value::Null);
    }

    /// 每条规则的 `fields` 必须包含本段**全部**已登记字段，界面据此逐行渲染，不能漏。
    #[test]
    fn fields_cover_every_registered_field_of_the_section() {
        let l = layers(SYS, USER_A);
        for sec in [
            Section::Apps,
            Section::InitialModeScope,
            Section::CommitNewline,
        ] {
            let want: BTreeSet<_> = known_keys(sec.as_str())
                .into_iter()
                .map(str::to_string)
                .collect();
            for v in l.view(sec) {
                let got: BTreeSet<_> = v.fields.keys().cloned().collect();
                assert_eq!(got, want, "{} / {}", sec.as_str(), v.process);
            }
        }
        let l2 = layers(
            "",
            "[[commit_newline]]\nprocess = \"W.exe\"\nstyle = \"cr\"\n",
        );
        assert!(l2.view_of(Section::CommitNewline, "W.exe").is_some());
    }

    /// 对外 JSON 统一 camelCase（与导入预览、`dryRun` 参数一致）：界面不必猜每个键是哪种写法。
    #[test]
    fn view_and_schema_serialize_in_camel_case() {
        let l = layers(SYS, USER_A);
        let v = serde_json::to_value(l.view_of(Section::Apps, "Weixin.exe").unwrap()).unwrap();
        for k in [
            "hasUserEntry",
            "unknownKeys",
            "fields",
            "overridden",
            "effective",
        ] {
            assert!(v.get(k).is_some(), "缺少 {k}: {v}");
        }
        assert!(
            v.get("has_user_entry").is_none() && v.get("unknown_keys").is_none(),
            "不得留下 snake_case: {v}"
        );
        assert!(
            v["fields"]["caret_use_top"].get("systemValue").is_some(),
            "{v}"
        );
        let schema = crate::compat_schema::schema_json();
        let dep = schema["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == "candidate_x")
            .unwrap();
        assert!(
            dep.get("dependsOn").is_some() && dep.get("depends_on").is_none(),
            "{dep}"
        );
    }
}

/// 规则身份带窗口条件后：列表如实列出每条规则；按进程名的接口只认纯进程键那一条，
/// 带 class / title 的接口按完整身份定位。
#[cfg(test)]
mod window_identity_tests {
    use super::*;
    use crate::compat_overlay::parse_raw;
    use serde_json::json;

    const SYS: &str = "[[apps]]\nprocess = \"ahk.exe\"\nauto_pair = true\n\n[[apps]]\nprocess = \"ahk.exe\"\ntitle = \"Editor*\"\ninitial_mode = \"english\"\n";

    fn layers(usr: &str) -> Layers {
        Layers {
            system: parse_raw(SYS).unwrap(),
            user: parse_raw(usr).unwrap(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn process_apis_address_only_the_pure_process_rule() {
        let mut l =
            layers("[[apps]]\nprocess = \"AHK.exe\"\ntitle = \"editor*\"\ncaret_use_top = true\n");
        let views = l.view(Section::Apps);
        assert_eq!(
            views
                .iter()
                .map(|v| (v.process.as_str(), v.class.as_str(), v.title.as_str()))
                .collect::<Vec<_>>(),
            vec![("ahk.exe", "", ""), ("ahk.exe", "", "Editor*")],
            "窗口规则如实列出，条件沿用系统层写法"
        );
        assert_eq!(views[1].state, RuleState::Modified);
        let v = l.view_of(Section::Apps, "ahk.exe").unwrap();
        assert_eq!(v.state, RuleState::System, "纯进程那条没被用户动过");
        assert_eq!(v.effective["auto_pair"], json!(true));

        l.upsert(
            Section::Apps,
            "ahk.exe",
            &json!({"auto_pair": false}).as_object().unwrap().clone(),
        )
        .unwrap();
        assert_eq!(
            l.user.apps.len(),
            2,
            "新建了纯进程差异行: {:?}",
            l.user.apps
        );
        assert_eq!(
            l.user.apps[0]["caret_use_top"],
            json!(true),
            "窗口规则那行原样"
        );
        assert!(l.reset(Section::Apps, "ahk.exe"));
        assert_eq!(l.user.apps.len(), 1, "只删纯进程那条");
        assert_eq!(l.user.apps[0]["title"], json!("editor*"));
        assert!(!l.reset(Section::Apps, "ahk.exe"));
    }

    /// 只写窗口条件的规则（身份里 process = "*"）列成 `*` + 条件；按进程名（不带条件）的写入
    /// 不会落到它头上。
    #[test]
    fn window_only_rules_are_listed_with_conditions_and_process_writes_never_touch_them() {
        let mut l = layers(
            "[[apps]]\nclass = \"Chrome_WidgetWin_*\"\ncomposition_placeholder = \"zwsp\"\n\n[[apps]]\nprocess = \"ahk.exe\"\nclass = \"X\"\ndisabled = true\n",
        );
        let listed: Vec<(String, String, String)> = l
            .view(Section::Apps)
            .into_iter()
            .map(|v| (v.process, v.class, v.title))
            .collect();
        let row = |p: &str, c: &str, t: &str| (p.to_string(), c.to_string(), t.to_string());
        assert_eq!(
            listed,
            vec![
                row("ahk.exe", "", ""),
                row("ahk.exe", "", "Editor*"),
                row("*", "Chrome_WidgetWin_*", ""),
                row("ahk.exe", "X", ""),
            ]
        );
        assert!(l.view_of(Section::Apps, "*").is_none());
        let before = l.user.apps.clone();

        l.set_disabled(Section::Apps, "ahk.exe", true).unwrap();
        l.set_disabled(Section::Apps, "ahk.exe", false).unwrap();
        assert!(!l.reset(Section::Apps, "*"), "没有纯 * 规则可还原");
        assert!(
            l.set_disabled(Section::Apps, "*", true).is_err(),
            "纯 * 规则不存在"
        );
        assert_eq!(l.user.apps, before, "按进程名的写入不碰窗口规则");
        l.upsert(
            Section::Apps,
            "ahk.exe",
            &json!({"caret_use_top": true}).as_object().unwrap().clone(),
        )
        .unwrap();
        assert_eq!(
            &l.user.apps[..2],
            &before[..],
            "窗口规则原样，纯进程差异另起一行"
        );
        assert!(l.user.apps[2].get("class").is_none());
        assert!(l.reset(Section::Apps, "ahk.exe"));
        assert_eq!(l.user.apps, before);
    }

    /// replace 导入会清掉用户层的窗口规则：预览必须把它列成 remove（列表里虽不显示）。
    #[test]
    fn replace_import_previews_removal_of_window_rules() {
        let l = layers("[[apps]]\nprocess = \"ahk.exe\"\ntitle = \"T\"\nauto_pair = true\n");
        let p = l.import_preview("", ImportMode::Replace).unwrap();
        assert_eq!(
            p.items
                .iter()
                .map(|i| (i.process.as_str(), i.action))
                .collect::<Vec<_>>(),
            vec![("ahk.exe", "remove")]
        );
    }

    #[test]
    fn patching_class_or_title_is_refused_like_process() {
        let mut l = layers("");
        let err = l
            .upsert(
                Section::Apps,
                "ahk.exe",
                &json!({"class": "X"}).as_object().unwrap().clone(),
            )
            .unwrap_err();
        assert!(err.contains("不能通过补丁修改"), "{err}");
        assert!(l.reset_field(Section::Apps, "ahk.exe", "title").is_err());
    }

    /// 导入带窗口条件的规则：保留条件、按身份叠到同一条上，不能被压扁成整个进程的规则。
    #[test]
    fn import_keeps_window_conditions_in_the_identity() {
        let mut l = layers("");
        let p = l
            .import_apply(
                "[[apps]]\nprocess = \"AHK.EXE\"\ntitle = \" EDITOR* \"\ncaret_use_top = true\n\n[[apps]]\nclass = \"Chrome_WidgetWin_*\"\ncomposition_placeholder = \"zwsp\"\n\n[[apps]]\nprocess = \"x.exe\"\nclass = 1\nauto_pair = true\n",
                ImportMode::Merge,
            )
            .unwrap();
        assert_eq!(p.rejected.len(), 1, "{:?}", p.rejected);
        assert_eq!(l.user.apps.len(), 2, "{:?}", l.user.apps);
        let ahk = &l.user.apps[0];
        assert_eq!(ahk["title"], json!("Editor*"), "沿用系统层同身份那条的写法");
        assert_eq!(ahk["caret_use_top"], json!(true));
        assert!(ahk.get("initial_mode").is_none(), "只记差异");
        let chrome = &l.user.apps[1];
        assert!(chrome.get("process").is_none(), "没写 process 的保持不写");
        assert_eq!(chrome["class"], json!("Chrome_WidgetWin_*"));
        assert_eq!(
            p.items
                .iter()
                .map(|i| (i.process.as_str(), i.action))
                .collect::<Vec<_>>(),
            vec![("AHK.EXE", "override"), ("*", "add")]
        );
        assert!(
            l.view_of(Section::Apps, "ahk.exe").unwrap().user.is_none(),
            "纯进程那条没被碰"
        );
    }

    #[test]
    fn side_sections_list_class_as_an_unknown_key() {
        let l = Layers {
            system: Raw::default(),
            user: parse_raw("[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"A\"]\nclass = \"X\"\n").unwrap(),
            warnings: Vec::new(),
        };
        let v = l
            .view_of(Section::InitialModeScope, "explorer.exe")
            .unwrap();
        assert_eq!(v.unknown_keys, vec!["class".to_string()]);
    }
}

/// P4：窗口规则的管理接口（按完整身份定位、改匹配条件、复制为新规则、身份校验）。
#[cfg(test)]
mod window_admin_tests {
    use super::*;
    use crate::compat_overlay::parse_raw;
    use serde_json::json;

    const SYS: &str = "[[apps]]\nprocess = \"ahk.exe\"\nauto_pair = true\n\n[[apps]]\nprocess = \"AHK.exe\"\nclass = \"AutoHotkeyGUI\"\nfirst_show_mode = \"wait\"\ncaret_use_top = true\n\n[[apps]]\nclass = \"Chrome_WidgetWin_*\"\ncomposition_placeholder = \"zwsp\"\n";

    fn layers(usr: &str) -> Layers {
        Layers {
            system: parse_raw(SYS).unwrap(),
            user: parse_raw(usr).unwrap(),
            warnings: Vec::new(),
        }
    }

    fn patch(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn k(p: &str, c: &str, t: &str) -> RuleKey {
        RuleKey::new(p, c, t)
    }

    #[test]
    fn views_expose_conditions_and_a_normalized_key() {
        let l = layers("");
        let v = l
            .view_of(Section::Apps, k("ahk.EXE", " autohotkeygui ", ""))
            .unwrap();
        assert_eq!(
            (v.process.as_str(), v.class.as_str(), v.title.as_str()),
            ("AHK.exe", "AutoHotkeyGUI", ""),
            "原样写法"
        );
        assert_eq!(v.key, "ahk.exe\u{1f}autohotkeygui\u{1f}");
        let any = l
            .view_of(Section::Apps, k("*", "chrome_widgetwin_*", ""))
            .unwrap();
        assert_eq!(any.process, "*");
        assert_eq!(any.key, "*\u{1f}chrome_widgetwin_*\u{1f}");
        let s = serde_json::to_value(&any).unwrap();
        assert_eq!(s["class"], json!("Chrome_WidgetWin_*"));
        assert_eq!(s["title"], json!(""));
        assert!(s["key"].is_string());
        assert!(s.get("id").is_none());
    }

    #[test]
    fn upsert_on_a_system_window_rule_stores_the_diff_under_the_same_identity() {
        let mut l = layers("");
        l.upsert(
            Section::Apps,
            k("ahk.exe", "autohotkeygui", ""),
            &patch(json!({"first_show_mode": "instant"})),
        )
        .unwrap();
        assert_eq!(l.user.apps.len(), 1, "{:?}", l.user.apps);
        let row = &l.user.apps[0];
        assert_eq!(row["process"], json!("AHK.exe"), "沿用系统层写法");
        assert_eq!(row["class"], json!("AutoHotkeyGUI"));
        assert_eq!(row["first_show_mode"], json!("instant"));
        assert!(row.get("caret_use_top").is_none(), "只记差异");
        let v = l
            .view_of(Section::Apps, k("ahk.exe", "AutoHotkeyGUI", ""))
            .unwrap();
        assert_eq!(v.state, RuleState::Modified);
        assert_eq!(
            l.view_of(Section::Apps, "ahk.exe").unwrap().state,
            RuleState::System,
            "纯进程那条没被碰"
        );
    }

    #[test]
    fn upsert_creates_a_window_only_user_rule_without_a_process_key() {
        let mut l = layers("");
        l.upsert(
            Section::Apps,
            k("*", "MozillaWindowClass", ""),
            &patch(json!({"composition_placeholder": "zwsp"})),
        )
        .unwrap();
        let row = l.user.apps.last().unwrap();
        assert!(row.get("process").is_none(), "{row:?}");
        assert_eq!(row["class"], json!("MozillaWindowClass"));
        let v = l
            .view_of(Section::Apps, k("", "mozillawindowclass", ""))
            .unwrap();
        assert_eq!(v.state, RuleState::User);
        assert_eq!(v.process, "*");
    }

    #[test]
    fn disable_reset_field_and_reset_address_the_window_rule_only() {
        let mut l = layers("");
        let key = k("ahk.exe", "AutoHotkeyGUI", "");
        l.set_disabled(Section::Apps, &key, true).unwrap();
        assert_eq!(
            l.view_of(Section::Apps, &key).unwrap().state,
            RuleState::Disabled
        );
        assert_eq!(
            l.view_of(Section::Apps, "ahk.exe").unwrap().state,
            RuleState::System
        );
        l.upsert(
            Section::Apps,
            &key,
            &patch(json!({"first_show_mode": null})),
        )
        .unwrap();
        l.reset_field(Section::Apps, &key, "first_show_mode")
            .unwrap();
        assert!(
            l.user.apps[0].get("unset").is_none(),
            "{:?}",
            l.user.apps[0]
        );
        assert!(l.reset(Section::Apps, &key));
        assert!(l.user.apps.is_empty());
        assert!(
            l.set_disabled(Section::Apps, k("ahk.exe", "Nope", ""), true)
                .is_err(),
            "不存在的窗口规则"
        );
    }

    #[test]
    fn rename_changes_conditions_of_a_user_rule_in_place() {
        let mut l = layers(
            "[[apps]]\nprocess = \"x.exe\"\nclass = \"A\"\nauto_pair = true\ncomment = \"c\"\n",
        );
        l.rename(Section::Apps, k("x.exe", "a", ""), k("X.exe", "B*", "T?"))
            .unwrap();
        assert_eq!(l.user.apps.len(), 1);
        let row = &l.user.apps[0];
        assert_eq!(row["process"], json!("X.exe"));
        assert_eq!(row["class"], json!("B*"));
        assert_eq!(row["title"], json!("T?"));
        assert_eq!(row["auto_pair"], json!(true), "字段保留");
        assert_eq!(row["comment"], json!("c"));
        assert!(l.view_of(Section::Apps, k("x.exe", "A", "")).is_none());

        // 去掉全部窗口条件、改成不限进程 + 类名，都是合法的改法。
        l.rename(Section::Apps, k("x.exe", "b*", "t?"), k("*", "B*", ""))
            .unwrap();
        let row = &l.user.apps[0];
        assert!(
            row.get("process").is_none() && row.get("title").is_none(),
            "{row:?}"
        );
        // 只改写法（大小写 / 空白）也行。
        l.rename(Section::Apps, k("*", "b*", ""), k("*", " b* ", ""))
            .unwrap();
        assert_eq!(l.user.apps[0]["class"], json!("b*"));
    }

    #[test]
    fn rename_refuses_factory_rules_collisions_and_bad_targets() {
        let mut l = layers(
            "[[apps]]\nprocess = \"AHK.exe\"\nclass = \"AutoHotkeyGUI\"\nauto_pair = false\n\n[[apps]]\nprocess = \"x.exe\"\nauto_pair = true\n",
        );
        let before = l.user.clone();
        let err = l
            .rename(
                Section::Apps,
                k("ahk.exe", "AutoHotkeyGUI", ""),
                k("ahk.exe", "Other", ""),
            )
            .unwrap_err();
        assert!(
            err.contains("复制为新规则"),
            "出厂规则（含被改过的）: {err}"
        );
        let err = l
            .rename(Section::Apps, "x.exe", k("ahk.exe", "", ""))
            .unwrap_err();
        assert!(err.contains("已有"), "撞上出厂规则: {err}");
        assert!(
            l.rename(Section::Apps, "x.exe", k("", "", "")).is_err(),
            "三者全空"
        );
        assert!(
            l.rename(Section::Apps, "x.exe", k("x.exe", "a\u{7}", ""))
                .is_err(),
            "控制字符"
        );
        assert!(
            l.rename(Section::Apps, "nobody.exe", "y.exe").is_err(),
            "不存在"
        );
        assert_eq!(l.user, before, "被拒绝时不改任何东西");
    }

    #[test]
    fn copy_turns_the_effective_content_into_a_new_user_rule() {
        let mut l = layers(
            "[[apps]]\nprocess = \"AHK.exe\"\nclass = \"AutoHotkeyGUI\"\nfirst_show_mode = \"instant\"\ndisabled = true\n",
        );
        l.copy(
            Section::Apps,
            k("ahk.exe", "AutoHotkeyGUI", ""),
            k("ahk.exe", "AutoHotkeyGUI2", ""),
        )
        .unwrap();
        let v = l
            .view_of(Section::Apps, k("ahk.exe", "AutoHotkeyGUI2", ""))
            .unwrap();
        assert_eq!(v.state, RuleState::User, "新规则是用户新增、启用的");
        assert_eq!(
            v.effective["first_show_mode"],
            json!("instant"),
            "带上用户差异"
        );
        assert_eq!(v.effective["caret_use_top"], json!(true), "带上出厂字段");
        assert_eq!(
            l.view_of(Section::Apps, k("ahk.exe", "AutoHotkeyGUI", ""))
                .unwrap()
                .state,
            RuleState::Disabled,
            "源规则原样"
        );
        let err = l
            .copy(Section::Apps, "ahk.exe", k("ahk.exe", "AutoHotkeyGUI2", ""))
            .unwrap_err();
        assert!(err.contains("已有"), "{err}");
    }

    #[test]
    fn key_validation() {
        assert!(
            validate_key(Section::Apps, &k("*", "", "")).is_ok(),
            "* = 所有应用"
        );
        assert!(validate_key(Section::Apps, &k("", "X", "")).is_ok());
        assert!(validate_key(Section::Apps, &k(" ", " ", "")).is_err());
        assert!(validate_key(Section::Apps, &k("a/b.exe", "", "")).is_err());
        assert!(validate_key(Section::Apps, &k("a.exe", "x\ny", "")).is_err());
        let long = "x".repeat(MAX_PATTERN_CHARS);
        assert!(validate_key(Section::Apps, &k("a.exe", &long, "")).is_ok());
        let longer = "字".repeat(MAX_PATTERN_CHARS + 1);
        assert!(validate_key(Section::Apps, &k("a.exe", "", &longer)).is_err());
        assert!(validate_key(Section::CommitNewline, &k("a.exe", "", "")).is_ok());
        assert!(validate_key(Section::CommitNewline, &k("*", "", "")).is_err());
        assert!(validate_key(Section::InitialModeScope, &k("a.exe", "X", "")).is_err());
        assert_eq!(
            validate_key(Section::Apps, &k(" a.exe ", " X ", " T ")).unwrap(),
            k("a.exe", "X", "T"),
            "trim"
        );
    }

    #[test]
    fn problems_and_import_items_carry_the_conditions() {
        let (_, report) = crate::app_compat::sanitize_raw(
            &parse_raw(
                "[[apps]]\nprocess = \"a.exe\"\nclass = \"K\"\nfirst_show_mode = \"bogus\"\n",
            )
            .unwrap(),
        );
        assert_eq!(report.len(), 1);
        assert_eq!(
            (report[0].class.as_str(), report[0].title.as_str()),
            ("K", "")
        );
        let v = serde_json::to_value(&report[0]).unwrap();
        assert_eq!(v["class"], json!("K"));

        let l = layers("[[apps]]\nprocess = \"x.exe\"\ntitle = \"T\"\nauto_pair = true\n");
        let p = l
            .import_preview(
                "[[apps]]\nclass = \"New*\"\nauto_pair = true\n\n[[apps]]\nprocess = \"y.exe\"\nclass = \"bad\\u0007\"\nauto_pair = true\n\n[[commit_newline]]\nprocess = \"*\"\nstyle = \"cr\"\n",
                ImportMode::Replace,
            )
            .unwrap();
        let items: Vec<_> = p
            .items
            .iter()
            .map(|i| {
                (
                    i.process.as_str(),
                    i.class.as_str(),
                    i.title.as_str(),
                    i.action,
                )
            })
            .collect();
        assert_eq!(
            items,
            vec![("*", "New*", "", "add"), ("x.exe", "", "T", "remove")]
        );
        assert_eq!(p.rejected.len(), 2, "{:?}", p.rejected);
    }

    fn rpc_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "wind_compat_p4_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let (d, u) = (base.join("data"), base.join("user"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(&u).unwrap();
        std::fs::write(d.join(COMPAT_FILE_NAME), SYS).unwrap();
        (d, u)
    }

    #[test]
    fn rpc_round_trip_with_window_conditions() {
        let (d, u) = rpc_dirs("rt");
        let call = |m: &str, p: Value| rpc(m, &p, Some(&d), None, Some(&u));

        let list = call("compat.list", json!({"section": "apps"}))
            .unwrap()
            .value;
        let rules = list["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3, "窗口规则如实列出");
        assert!(
            rules
                .iter()
                .any(|r| r["class"] == json!("Chrome_WidgetWin_*"))
        );

        let out = call(
            "compat.upsert",
            json!({"section": "apps", "class": "Foo*", "patch": {"auto_pair": true}}),
        )
        .unwrap();
        assert!(out.wrote);
        assert_eq!(out.value["rule"]["process"], json!("*"));
        assert_eq!(out.value["rule"]["class"], json!("Foo*"));

        let out = call(
            "compat.rename",
            json!({"section": "apps", "process": "*", "class": "foo*", "to": {"process": "z.exe", "class": "Foo*", "title": "Doc*"}}),
        )
        .unwrap();
        assert!(out.wrote);
        assert_eq!(out.value["rule"]["title"], json!("Doc*"));
        let Err(err) = call(
            "compat.rename",
            json!({"section": "apps", "process": "ahk.exe", "to": {"process": "q.exe"}}),
        ) else {
            panic!("出厂规则不能改条件");
        };
        assert!(err.contains("复制为新规则"), "{err}");

        let out = call(
            "compat.copy",
            json!({"section": "apps", "process": "ahk.exe", "class": "AutoHotkeyGUI", "to": {"process": "ahk.exe", "class": "Other"}}),
        )
        .unwrap();
        assert_eq!(out.value["rule"]["state"], json!("user"));
        assert_eq!(
            out.value["rule"]["effective"]["first_show_mode"],
            json!("wait")
        );

        let out = call(
            "compat.setDisabled",
            json!({"section": "apps", "process": "AHK.EXE", "class": "autohotkeygui", "disabled": true}),
        )
        .unwrap();
        assert_eq!(out.value["rule"]["state"], json!("disabled"));
        assert!(
            call(
                "compat.rename",
                json!({"section": "apps", "process": "z.exe"})
            )
            .is_err(),
            "缺 to"
        );
        assert!(
            call("compat.upsert", json!({"section": "apps", "patch": {}})).is_err(),
            "缺身份"
        );
        assert!(
            call(
                "compat.upsert",
                json!({"section": "apps", "process": "a.exe", "class": 1, "patch": {}})
            )
            .is_err(),
            "类型不对"
        );
        let text = std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap();
        assert!(text.contains("title = \"Doc*\""), "{text}");
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }

    /// 复制保真：源规则带跨级的 `unset` 与显式 `false`，复制品在同一级、同一窗口上解析结果一致。
    #[test]
    fn copy_keeps_unset_and_explicit_false_so_the_copy_resolves_the_same() {
        use crate::app_compat::{AppCompat, WindowCtx};
        let sys = "[[apps]]\nprocess = \"ahk.exe\"\ncaret_use_top = true\nfirst_show_mode = \"fast\"\n\n[[apps]]\nprocess = \"ahk.exe\"\nclass = \"A\"\ncaret_use_top = false\nunset = [\"first_show_mode\"]\n";
        let mut l = Layers {
            system: parse_raw(sys).unwrap(),
            user: Raw::default(),
            warnings: Vec::new(),
        };
        l.copy(Section::Apps, k("ahk.exe", "A", ""), k("ahk.exe", "B", ""))
            .unwrap();
        let row = l.user.apps.last().unwrap();
        assert_eq!(row["caret_use_top"], json!(false), "{row:?}");
        assert_eq!(row["unset"], json!(["first_show_mode"]), "{row:?}");

        let base = std::env::temp_dir().join(format!(
            "wind_compat_p4_copy_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let (d, u) = (base.join("d"), base.join("u"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(&u).unwrap();
        std::fs::write(d.join(COMPAT_FILE_NAME), sys).unwrap();
        std::fs::write(u.join(COMPAT_FILE_NAME), l.render_user().unwrap()).unwrap();
        let c = AppCompat::load_layered(Some(&d), None, Some(&u));
        let _ = std::fs::remove_dir_all(&base);
        let at = |class: &str| {
            let r = c.resolve(&WindowCtx {
                process: "ahk.exe",
                class,
                title: "",
            });
            let r = r.rule.clone().unwrap();
            (r.caret_use_top, r.first_show_mode)
        };
        assert_eq!(at("B"), at("A"), "复制品与源规则效果相同");
        assert_eq!(at("A"), (false, None));
    }

    /// 现存的不合规身份（手写超长 / 控制字符、附属段 `*`）能改、能禁用、能改名成合规的；
    /// 只有写出新身份的一侧才校验。
    #[test]
    fn existing_non_conforming_rules_are_editable_and_only_new_identities_are_validated() {
        let long = "x".repeat(MAX_PATTERN_CHARS + 10);
        let usr = format!(
            "[[apps]]\nprocess = \"a.exe\"\nclass = \"{long}\"\nauto_pair = true\n\n[[commit_newline]]\nprocess = \"*\"\nstyle = \"cr\"\n"
        );
        let mut l = layers(&usr);
        let bad = k("a.exe", &long, "");
        l.upsert(Section::Apps, &bad, &patch(json!({"caret_use_top": true})))
            .expect("现存行可改");
        l.set_disabled(Section::Apps, &bad, true).expect("可禁用");
        l.reset_field(Section::Apps, &bad, "caret_use_top")
            .expect("可还原字段");
        l.set_disabled(Section::CommitNewline, "*", true)
            .expect("附属段的 * 行可禁用");
        assert!(
            l.rename(Section::Apps, &bad, k("a.exe", "x\u{7}", ""))
                .is_err(),
            "to 仍要校验"
        );
        l.rename(Section::Apps, &bad, k("a.exe", "Short", ""))
            .expect("能改名成合规的");
        assert!(l.view_of(Section::Apps, k("a.exe", "short", "")).is_some());
        assert!(
            l.upsert(
                Section::Apps,
                k("b.exe", &long, ""),
                &patch(json!({"auto_pair": true}))
            )
            .is_err(),
            "新建仍校验"
        );
    }

    #[test]
    fn rpc_to_must_be_an_object() {
        let (d, u) = rpc_dirs("to");
        let Err(e) = rpc(
            "compat.rename",
            &json!({"section": "apps", "process": "ahk.exe", "to": "x.exe"}),
            Some(&d),
            None,
            Some(&u),
        ) else {
            panic!("to 不是对象应报错");
        };
        assert!(e.contains("to 必须是对象"), "{e}");
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }
}
