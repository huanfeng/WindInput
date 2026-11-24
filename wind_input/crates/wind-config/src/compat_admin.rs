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
#[path = "compat_admin_tests.rs"]
mod tests;

/// 规则身份带窗口条件后：列表如实列出每条规则；按进程名的接口只认纯进程键那一条，
/// 带 class / title 的接口按完整身份定位。
#[cfg(test)]
#[path = "compat_admin_window_identity_tests.rs"]
mod window_identity_tests;

/// P4：窗口规则的管理接口（按完整身份定位、改匹配条件、复制为新规则、身份校验）。
#[cfg(test)]
#[path = "compat_admin_window_admin_tests.rs"]
mod window_admin_tests;
