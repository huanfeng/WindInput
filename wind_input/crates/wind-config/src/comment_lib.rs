//! 候选注释的命名模板与默认 / 详细两态：配置类型与出厂数据文件（`system.comments.toml`）。
//!
//! 设计见 `docs/design/candidate-comment-templates.md`。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 注释的当前态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentState {
    #[default]
    Default,
    Detail,
}

/// 切换键（值域见 `config_schema::COMMENT_SWITCH_KEY_VALUES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchKey {
    /// F1（出厂）。不走修饰键单击判定：生效时补进会话按键表（见
    /// [`crate::Config::effective_session_actions`]），有候选时按下即切换。
    #[default]
    #[serde(rename = "f1")]
    F1,
    /// 左右任一 Ctrl。
    Ctrl,
    #[serde(rename = "lctrl")]
    LCtrl,
    #[serde(rename = "rctrl")]
    RCtrl,
    /// 左右任一 Alt。
    Alt,
    #[serde(rename = "lalt")]
    LAlt,
    #[serde(rename = "ralt")]
    RAlt,
}

impl SwitchKey {
    /// 该修饰键虚拟键码（0xA2 LCtrl / 0xA3 RCtrl / 0xA4 LAlt / 0xA5 RAlt）是否属于本切换键；F1 恒 false。
    pub fn matches_vk(self, vk: u32) -> bool {
        match self {
            SwitchKey::Ctrl => matches!(vk, 0xA2 | 0xA3),
            SwitchKey::LCtrl => vk == 0xA2,
            SwitchKey::RCtrl => vk == 0xA3,
            SwitchKey::Alt => matches!(vk, 0xA4 | 0xA5),
            SwitchKey::LAlt => vk == 0xA4,
            SwitchKey::RAlt => vk == 0xA5,
            SwitchKey::F1 => false,
        }
    }

    pub fn is_ctrl(self) -> bool {
        matches!(self, SwitchKey::Ctrl | SwitchKey::LCtrl | SwitchKey::RCtrl)
    }

    pub fn is_alt(self) -> bool {
        matches!(self, SwitchKey::Alt | SwitchKey::LAlt | SwitchKey::RAlt)
    }
}

/// 切换注释态的方式（值域见 `config_schema::COMMENT_SWITCH_MODE_VALUES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchMode {
    Off,
    #[default]
    Session,
    Persist,
    Hold,
}

/// 命名模板。`id` 只含 `[a-z0-9_]`，`name` 是显示名，`desc` 是选模板时显示的一句说明。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommentTemplateDef {
    pub id: String,
    pub name: String,
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub desc: String,
}

/// 一个作用范围的四格；`None` = 不写（跟随下一层）。格值为模板 id / `none` / `inherit`。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommentCells {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_vertical: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_horizontal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_vertical: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_horizontal: Option<String>,
}

/// 作用范围 -> 四格的分配表。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommentAssign {
    pub global: CommentCells,
    /// 方案类型：`codetable` / `pinyin` / `shuangpin` / `mixed` / `english`。
    #[serde(rename = "type")]
    pub by_type: BTreeMap<String, CommentCells>,
    /// 方案 id。
    pub schema: BTreeMap<String, CommentCells>,
    /// 模式键：`temp_pinyin` / `temp_english` / `url` / `email` / `unicode` /
    /// `mix:<id>` / `special:<方案id>`。
    pub mode: BTreeMap<String, CommentCells>,
}

/// `config.toml` 的 `[ui.comment]`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiCommentConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "crate::tolerant_de::tolerant")]
    pub state: CommentState,
    #[serde(deserialize_with = "crate::tolerant_de::tolerant")]
    pub switch_key: SwitchKey,
    #[serde(deserialize_with = "crate::tolerant_de::tolerant")]
    pub switch_mode: SwitchMode,
    /// 用户模板，同 id 覆盖内置。
    pub templates: Vec<CommentTemplateDef>,
    /// 用户分配，优先于出厂分配。
    pub assign: CommentAssign,
}

impl Default for UiCommentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            state: CommentState::Default,
            switch_key: SwitchKey::F1,
            switch_mode: SwitchMode::Session,
            templates: Vec::new(),
            assign: CommentAssign::default(),
        }
    }
}

/// 内置注释词库条目（`[[dicts]]`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltinCommentDict {
    pub id: String,
    pub label: String,
    /// 相对数据目录。
    pub path: String,
}

/// `data/system.comments.toml`：内置模板、出厂分配、内置注释词库。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemComments {
    pub templates: Vec<CommentTemplateDef>,
    pub assign: CommentAssign,
    pub dicts: Vec<BuiltinCommentDict>,
}

impl SystemComments {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        toml::from_str(text).map_err(|e| anyhow::anyhow!("解析 system.comments.toml 失败: {e}"))
    }

    /// 编译期内置的出厂副本，盘上文件缺失或解析失败时兜底（Android 等不带该文件的包也靠它）。
    pub fn embedded() -> Self {
        Self::parse(include_str!("../../../../data/system.comments.toml"))
            .expect("内置 system.comments.toml 应能解析")
    }

    /// 盘上文件（`path`，已按层序解析）优先；缺失或读/解析失败时用内置副本，第二项是告警原因。
    pub fn load_or_embedded(path: Option<&std::path::Path>) -> (Self, Option<String>) {
        let Some(path) = path else {
            return (
                Self::embedded(),
                Some("system.comments.toml 缺失，使用内置副本".into()),
            );
        };
        match std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|t| Self::parse(&t))
        {
            Ok(s) => (s, None),
            Err(e) => (
                Self::embedded(),
                Some(format!("读取 {} 失败，使用内置副本: {e}", path.display())),
            ),
        }
    }
}

/// 方案类型，对应 `[assign.type.<key>]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaType {
    CodeTable,
    Pinyin,
    Shuangpin,
    Mixed,
    English,
}

impl SchemaType {
    pub fn key(self) -> &'static str {
        match self {
            SchemaType::CodeTable => "codetable",
            SchemaType::Pinyin => "pinyin",
            SchemaType::Shuangpin => "shuangpin",
            SchemaType::Mixed => "mixed",
            SchemaType::English => "english",
        }
    }
}

impl SchemaType {
    pub const ALL: [SchemaType; 5] = [
        SchemaType::CodeTable,
        SchemaType::Pinyin,
        SchemaType::Shuangpin,
        SchemaType::Mixed,
        SchemaType::English,
    ];
}

/// 快捷输入默认的模式键：`mix:<id>` 自己没写的格落到这里，再往下才是当前方案。
pub const MIX_ALL_MODE: &str = "mix";

/// 解析链上的一层，按从具体到一般的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignLayer {
    Mode = 0,
    Schema = 1,
    Type = 2,
    Global = 3,
}

/// 总表一格的实际结果，见 [`CommentLibrary::effective_table`]。
/// `scope` / `from_scope` 形如 `global` / `type:codetable` / `schema:<id>` / `mode:<键>`；
/// `template_id` 为 `None` = 不显示（序列化成格值 `"none"`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveCell {
    pub scope: String,
    pub state: CommentState,
    pub vertical: bool,
    #[serde(serialize_with = "id_or_none")]
    pub template_id: Option<String>,
    pub from_scope: String,
}

fn id_or_none<S: serde::Serializer>(id: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(id.as_deref().unwrap_or("none"))
}

/// 解析注释模板时的作用范围：模式（可无）、方案、方案类型。
#[derive(Debug, Clone, Copy)]
pub struct CommentScope<'a> {
    pub mode: Option<&'a str>,
    pub schema: &'a str,
    pub schema_type: SchemaType,
}

/// 模板来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateOrigin {
    Builtin,
    User,
    UserOverridesBuiltin,
}

/// 一格的值（未知模板 id 在 `build` 时已折成"不写"）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cell {
    Inherit,
    None,
    Id(String),
}

#[derive(Debug, Clone, Default)]
struct ScopeCells {
    default_vertical: Option<Cell>,
    default_horizontal: Option<Cell>,
    detail_vertical: Option<Cell>,
    detail_horizontal: Option<Cell>,
}

impl ScopeCells {
    fn get(&self, state: CommentState, vertical: bool) -> Option<&Cell> {
        match (state, vertical) {
            (CommentState::Default, true) => &self.default_vertical,
            (CommentState::Default, false) => &self.default_horizontal,
            (CommentState::Detail, true) => &self.detail_vertical,
            (CommentState::Detail, false) => &self.detail_horizontal,
        }
        .as_ref()
    }
}

#[derive(Debug, Clone, Default)]
struct MergedAssign {
    global: ScopeCells,
    by_type: BTreeMap<String, ScopeCells>,
    schema: BTreeMap<String, ScopeCells>,
    mode: BTreeMap<String, ScopeCells>,
}

/// 合并后的模板表与分配表，按 模式 → 方案 → 类型 → 全局 逐层解析。
#[derive(Debug, Clone)]
pub struct CommentLibrary {
    templates: Vec<(CommentTemplateDef, TemplateOrigin)>,
    assign: MergedAssign,
    fallback_default: String,
    fallback_detail: String,
}

/// 模板 id 或 `none` 才算有主见；未写与 `inherit` 都继续往下找。
fn adopt(c: Option<&Cell>) -> Option<&Cell> {
    match c {
        Some(Cell::Id(_) | Cell::None) => c,
        _ => None,
    }
}

fn valid_template_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn merge_cell(user: &Option<String>, system: &Option<String>) -> Option<String> {
    user.clone().or_else(|| system.clone())
}

fn merge_cells(user: &CommentCells, system: &CommentCells) -> CommentCells {
    CommentCells {
        default_vertical: merge_cell(&user.default_vertical, &system.default_vertical),
        default_horizontal: merge_cell(&user.default_horizontal, &system.default_horizontal),
        detail_vertical: merge_cell(&user.detail_vertical, &system.detail_vertical),
        detail_horizontal: merge_cell(&user.detail_horizontal, &system.detail_horizontal),
    }
}

fn merge_maps(
    user: &BTreeMap<String, CommentCells>,
    system: &BTreeMap<String, CommentCells>,
) -> BTreeMap<String, CommentCells> {
    let mut out = system.clone();
    for (k, u) in user {
        let merged = match system.get(k) {
            Some(s) => merge_cells(u, s),
            None => u.clone(),
        };
        out.insert(k.clone(), merged);
    }
    out
}

impl CommentLibrary {
    /// 合并出厂与用户配置；返回值的第二项是需要提示用户的问题（每条一次）。
    pub fn build(system: &SystemComments, user: &UiCommentConfig) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();

        let mut templates: Vec<(CommentTemplateDef, TemplateOrigin)> = Vec::new();
        for t in &system.templates {
            if templates.iter().all(|(e, _)| e.id != t.id) {
                templates.push((t.clone(), TemplateOrigin::Builtin));
            }
        }
        let mut seen_user = std::collections::HashSet::new();
        for t in &user.templates {
            if !valid_template_id(&t.id) {
                warnings.push(format!("用户模板 id 不合法，已忽略: {:?}", t.id));
                continue;
            }
            if matches!(t.id.as_str(), "none" | "inherit") {
                warnings.push(format!("用户模板 id 是保留字，已忽略: {}", t.id));
                continue;
            }
            if !seen_user.insert(t.id.as_str()) {
                warnings.push(format!("用户模板 id 重复，已忽略后一个: {}", t.id));
                continue;
            }
            match templates.iter_mut().find(|(e, _)| e.id == t.id) {
                Some(slot) => *slot = (t.clone(), TemplateOrigin::UserOverridesBuiltin),
                None => templates.push((t.clone(), TemplateOrigin::User)),
            }
        }

        let merged = CommentAssign {
            global: merge_cells(&user.assign.global, &system.assign.global),
            by_type: merge_maps(&user.assign.by_type, &system.assign.by_type),
            schema: merge_maps(&user.assign.schema, &system.assign.schema),
            mode: merge_maps(&user.assign.mode, &system.assign.mode),
        };

        let mut unknown: Vec<String> = Vec::new();
        let mut convert = |c: &CommentCells| {
            let mut one = |v: &Option<String>| -> Option<Cell> {
                let v = v.as_deref()?;
                match v {
                    "inherit" => Some(Cell::Inherit),
                    "none" => Some(Cell::None),
                    id if templates.iter().any(|(t, _)| t.id == id) => {
                        Some(Cell::Id(id.to_string()))
                    }
                    id => {
                        if !unknown.iter().any(|u| u == id) {
                            unknown.push(id.to_string());
                        }
                        None
                    }
                }
            };
            ScopeCells {
                default_vertical: one(&c.default_vertical),
                default_horizontal: one(&c.default_horizontal),
                detail_vertical: one(&c.detail_vertical),
                detail_horizontal: one(&c.detail_horizontal),
            }
        };
        let conv_map = |m: &BTreeMap<String, CommentCells>,
                        f: &mut dyn FnMut(&CommentCells) -> ScopeCells| {
            m.iter().map(|(k, v)| (k.clone(), f(v))).collect()
        };
        let assign = MergedAssign {
            global: convert(&merged.global),
            by_type: conv_map(&merged.by_type, &mut convert),
            schema: conv_map(&merged.schema, &mut convert),
            mode: conv_map(&merged.mode, &mut convert),
        };
        for id in unknown {
            warnings.push(format!("注释分配引用了不存在的模板 {id}，已按未设置处理"));
        }

        let builtin_text = |id: &str| {
            system
                .templates
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.text.clone())
                .unwrap_or_default()
        };
        let lib = Self {
            templates,
            assign,
            fallback_default: builtin_text(Self::fallback_id(CommentState::Default)),
            fallback_detail: builtin_text(Self::fallback_id(CommentState::Detail)),
        };
        (lib, warnings)
    }

    fn text_of(&self, cell: &Cell) -> &str {
        match cell {
            Cell::Id(id) => self
                .templates
                .iter()
                .find(|(t, _)| &t.id == id)
                .map(|(t, _)| t.text.as_str())
                .unwrap_or(""),
            _ => "",
        }
    }

    /// 从 `start` 那层起按 模式 → 方案 → 类型 → 全局 找第一个有主见的格，带回它所在的层与
    /// 胜出格的模式键（只有模式层有）。快捷输入 `mix:<id>` 自己没主见时先看通配的 [`MIX_ALL_MODE`]。
    fn pick(
        &self,
        scope: &CommentScope,
        start: AssignLayer,
        state: CommentState,
        vertical: bool,
    ) -> Option<(AssignLayer, Option<&str>, &Cell)> {
        let a = &self.assign;
        let mode_of = |m: &str| {
            a.mode
                .get_key_value(m)
                .map(|(k, l)| (AssignLayer::Mode, Some(k.as_str()), l))
        };
        let mix_all = scope
            .mode
            .filter(|m| m.starts_with("mix:"))
            .and_then(|_| mode_of(MIX_ALL_MODE));
        let layers = [
            scope.mode.and_then(mode_of),
            mix_all,
            a.schema
                .get(scope.schema)
                .map(|l| (AssignLayer::Schema, None, l)),
            a.by_type
                .get(scope.schema_type.key())
                .map(|l| (AssignLayer::Type, None, l)),
            Some((AssignLayer::Global, None, &a.global)),
        ];
        for (at, key, layer) in layers.into_iter().flatten() {
            if (at as usize) < start as usize {
                continue;
            }
            // 横排为主：竖排没写时同本层横排（横排有主见时），否则看下一层竖排。
            let picked = if vertical {
                match layer.get(state, true) {
                    Some(Cell::Inherit) => None,
                    Some(c) => Some(c),
                    None => adopt(layer.get(state, false)),
                }
            } else {
                adopt(layer.get(state, false))
            };
            if let Some(c) = picked {
                return Some((at, key, c));
            }
        }
        None
    }

    fn fallback_id(state: CommentState) -> &'static str {
        match state {
            CommentState::Default => "code",
            CommentState::Detail => "full",
        }
    }

    /// 按作用范围解析当前态与排布对应的模板串；`""` = 不显示。
    pub fn template_text(&self, scope: &CommentScope, state: CommentState, vertical: bool) -> &str {
        match self.pick(scope, AssignLayer::Mode, state, vertical) {
            Some((_, _, c)) => self.text_of(c),
            None => match state {
                CommentState::Default => &self.fallback_default,
                CommentState::Detail => &self.fallback_detail,
            },
        }
    }

    /// 总表一格的实际结果：从 `start` 层起解析，胜出的模板 id（`None` = 不显示）与所在层。
    /// 各层都没写时落到全局的内置兜底。与 [`Self::template_text`] 同一条解析链。
    pub fn resolve_cell(
        &self,
        scope: &CommentScope,
        start: AssignLayer,
        state: CommentState,
        vertical: bool,
    ) -> (Option<&str>, AssignLayer) {
        let (id, at, _) = self.resolve_cell_from(scope, start, state, vertical);
        (id, at)
    }

    /// 同 [`Self::resolve_cell`]，另带胜出格所在的模式键（模式层才有；快捷输入可能落在通配层）。
    fn resolve_cell_from(
        &self,
        scope: &CommentScope,
        start: AssignLayer,
        state: CommentState,
        vertical: bool,
    ) -> (Option<&str>, AssignLayer, Option<&str>) {
        match self.pick(scope, start, state, vertical) {
            Some((at, key, Cell::Id(id))) => (Some(id.as_str()), at, key),
            Some((at, key, _)) => (None, at, key),
            None => (Some(Self::fallback_id(state)), AssignLayer::Global, None),
        }
    }

    /// 设置端总表每格的实际结果：全局、五个类型常驻，加上出厂与用户分配里出现过的方案行、
    /// 模式行。方案行的类型由 `type_of` 给；模式行的下层取 `active_schema`（用户此刻看到的）。
    pub fn effective_table(
        &self,
        active_schema: &str,
        type_of: impl Fn(&str) -> SchemaType,
    ) -> Vec<EffectiveCell> {
        let bare = |schema_type| CommentScope {
            mode: None,
            schema: "",
            schema_type,
        };
        let mut rows: Vec<(String, CommentScope, AssignLayer)> = vec![(
            "global".into(),
            bare(SchemaType::CodeTable),
            AssignLayer::Global,
        )];
        for t in SchemaType::ALL {
            rows.push((format!("type:{}", t.key()), bare(t), AssignLayer::Type));
        }
        for s in self.assign.schema.keys() {
            let scope = CommentScope {
                mode: None,
                schema: s,
                schema_type: type_of(s),
            };
            rows.push((format!("schema:{s}"), scope, AssignLayer::Schema));
        }
        let active_type = type_of(active_schema);
        for m in self.assign.mode.keys() {
            let scope = CommentScope {
                mode: Some(m),
                schema: active_schema,
                schema_type: active_type,
            };
            rows.push((format!("mode:{m}"), scope, AssignLayer::Mode));
        }
        let mut out = Vec::with_capacity(rows.len() * 4);
        for (key, scope, start) in &rows {
            for state in [CommentState::Default, CommentState::Detail] {
                for vertical in [true, false] {
                    let (id, at, hit) = self.resolve_cell_from(scope, *start, state, vertical);
                    let from_scope = match at {
                        AssignLayer::Global => "global".to_string(),
                        AssignLayer::Type => format!("type:{}", scope.schema_type.key()),
                        AssignLayer::Schema => format!("schema:{}", scope.schema),
                        AssignLayer::Mode => format!("mode:{}", hit.unwrap_or_default()),
                    };
                    out.push(EffectiveCell {
                        scope: key.clone(),
                        state,
                        vertical,
                        template_id: id.map(str::to_string),
                        from_scope,
                    });
                }
            }
        }
        out
    }

    /// 被任一格引用的模板串加兜底串，去重，供按需加载数据。
    pub fn referenced_texts(&self) -> Vec<&str> {
        let a = &self.assign;
        let mut out: Vec<&str> = Vec::new();
        let scopes = std::iter::once(&a.global)
            .chain(a.by_type.values())
            .chain(a.schema.values())
            .chain(a.mode.values());
        for sc in scopes {
            for c in [
                &sc.default_vertical,
                &sc.default_horizontal,
                &sc.detail_vertical,
                &sc.detail_horizontal,
            ]
            .into_iter()
            .flatten()
            {
                if let Cell::Id(_) = c {
                    let t = self.text_of(c);
                    if !out.contains(&t) {
                        out.push(t);
                    }
                }
            }
        }
        for t in [
            self.fallback_default.as_str(),
            self.fallback_detail.as_str(),
        ] {
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }

    pub fn templates(&self) -> impl Iterator<Item = (&CommentTemplateDef, TemplateOrigin)> {
        self.templates.iter().map(|(t, o)| (t, *o))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped() -> SystemComments {
        SystemComments::embedded()
    }

    fn code_hint_text() -> String {
        let s = shipped();
        s.templates
            .iter()
            .find(|t| t.id == "code")
            .unwrap()
            .text
            .clone()
    }

    fn global_default_of(sys: &SystemComments) -> String {
        let (lib, _) = CommentLibrary::build(sys, &UiCommentConfig::default());
        lib.template_text(&scope(None), CommentState::Default, false)
            .to_string()
    }

    #[test]
    fn missing_system_comments_falls_back_to_embedded() {
        let (sys, w) = SystemComments::load_or_embedded(None);
        assert!(w.is_some());
        assert_eq!(global_default_of(&sys), code_hint_text());
    }

    #[test]
    fn unparsable_system_comments_falls_back_to_embedded_with_warning() {
        let p = std::env::temp_dir().join(format!("sys_comments_bad_{}.toml", std::process::id()));
        std::fs::write(&p, "templates = [[[").unwrap();
        let (sys, w) = SystemComments::load_or_embedded(Some(&p));
        let _ = std::fs::remove_file(&p);
        assert!(w.unwrap().contains("内置副本"));
        assert_eq!(global_default_of(&sys), code_hint_text());
    }

    #[test]
    fn on_disk_system_comments_overrides_embedded() {
        let p = std::env::temp_dir().join(format!("sys_comments_ok_{}.toml", std::process::id()));
        std::fs::write(
            &p,
            "[[templates]]\nid = \"x\"\ntext = \"X\"\n[assign.global]\ndefault_horizontal = \"x\"\n",
        )
        .unwrap();
        let (sys, w) = SystemComments::load_or_embedded(Some(&p));
        let _ = std::fs::remove_file(&p);
        assert!(w.is_none());
        assert_eq!(global_default_of(&sys), "X");
    }

    #[test]
    fn reserved_user_template_ids_are_rejected() {
        let mut u = UiCommentConfig::default();
        u.templates = vec![tpl("none", "N"), tpl("inherit", "I")];
        let (lib, w) = CommentLibrary::build(&sys(global_only()), &u);
        assert_eq!(
            w.iter().filter(|w| w.contains("保留字")).count(),
            2,
            "{w:?}"
        );
        assert!(
            lib.templates()
                .all(|(t, _)| t.id != "none" && t.id != "inherit")
        );
    }

    #[test]
    fn system_comments_parses_shipped_file() {
        let s = shipped();
        let ids: Vec<&str> = s.templates.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "code",
                "code_hint",
                "code_rev",
                "shuangpin",
                "pinyin",
                "chaizi",
                "chaizi_code",
                "chaizi_pinyin",
                "zh_en",
                "en_zh",
                "pinyin_en",
                "full",
            ],
            "设计 §13.6 的内置模板与顺序"
        );
        assert_eq!(s.assign.global.default_horizontal.as_deref(), Some("code"));
        assert_eq!(s.assign.global.detail_horizontal.as_deref(), Some("full"));
        let dicts: Vec<&str> = s.dicts.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(dicts, ["en_zh", "zh_en"]);
        assert_eq!(s.dicts[0].path, "comments/en_zh.txt");
        assert_eq!(s.dicts[1].path, "comments/zh_en.txt");
    }

    #[test]
    fn shipped_assign_matches_design_table() {
        let a = shipped().assign;
        let cell = |m: &BTreeMap<String, CommentCells>, k: &str| {
            let c = &m[k];
            (c.default_horizontal.clone(), c.detail_horizontal.clone())
        };
        let s = |v: &str| Some(v.to_string());
        // 设计 §13.6 出厂分配表。
        assert_eq!(cell(&a.by_type, "codetable"), (None, s("chaizi_pinyin")));
        assert_eq!(cell(&a.by_type, "pinyin"), (s("none"), s("pinyin_en")));
        assert_eq!(cell(&a.by_type, "shuangpin"), (s("none"), s("pinyin_en")));
        assert_eq!(cell(&a.by_type, "mixed"), (s("code_rev"), None));
        assert_eq!(cell(&a.by_type, "english"), (s("none"), s("en_zh")));
        assert_eq!(cell(&a.mode, "temp_english"), (s("none"), s("en_zh")));
        assert_eq!(cell(&a.mode, "temp_pinyin"), (s("code"), s("chaizi_code")));
        assert_eq!(cell(&a.mode, MIX_ALL_MODE), (s("code"), None));
        let all = std::iter::once(&a.global)
            .chain(a.by_type.values())
            .chain(a.schema.values())
            .chain(a.mode.values());
        for c in all {
            assert!(
                c.default_vertical.is_none() && c.detail_vertical.is_none(),
                "出厂只写横排格: {c:?}"
            );
        }
    }

    #[test]
    fn code_template_matches_legacy_default() {
        let s = shipped();
        let t = s.templates.iter().find(|t| t.id == "code").unwrap();
        assert_eq!(t.text, "${code_hint|code_rev|shuangpin}");
    }

    #[test]
    fn template_ids_are_valid() {
        let s = shipped();
        let mut seen = std::collections::HashSet::new();
        for t in &s.templates {
            assert!(
                !t.id.is_empty()
                    && t.id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "模板 id 不合法: {:?}",
                t.id
            );
            assert!(seen.insert(t.id.clone()), "模板 id 重复: {}", t.id);
        }
    }

    #[test]
    fn ui_comment_defaults() {
        let c = UiCommentConfig::default();
        assert!(c.enabled);
        assert_eq!(c.state, CommentState::Default);
        assert_eq!(c.switch_key, SwitchKey::F1);
        assert_eq!(c.switch_mode, SwitchMode::Session);
        assert!(c.templates.is_empty());
        assert_eq!(c.assign, CommentAssign::default());
    }

    #[test]
    fn switch_key_parses_all_values_and_matches_vk() {
        for (name, key, vks) in [
            ("f1", SwitchKey::F1, vec![]),
            ("ctrl", SwitchKey::Ctrl, vec![0xA2, 0xA3]),
            ("lctrl", SwitchKey::LCtrl, vec![0xA2]),
            ("rctrl", SwitchKey::RCtrl, vec![0xA3]),
            ("alt", SwitchKey::Alt, vec![0xA4, 0xA5]),
            ("lalt", SwitchKey::LAlt, vec![0xA4]),
            ("ralt", SwitchKey::RAlt, vec![0xA5]),
        ] {
            let c: UiCommentConfig = toml::from_str(&format!("switch_key = \"{name}\"")).unwrap();
            assert_eq!(c.switch_key, key, "{name}");
            for vk in 0xA2..=0xA5u32 {
                assert_eq!(key.matches_vk(vk), vks.contains(&vk), "{name} {vk:#x}");
            }
            assert_eq!(key.is_alt(), name.ends_with("alt"));
            assert_eq!(key.is_ctrl(), name.ends_with("ctrl"));
        }
        assert!(!SwitchKey::Ctrl.matches_vk(0x41));
    }

    #[test]
    fn invalid_switch_values_fall_back_to_defaults() {
        let c: UiCommentConfig =
            toml::from_str("switch_key = \"bogus\"\nswitch_mode = \"nope\"").unwrap();
        assert_eq!(c.switch_key, SwitchKey::F1);
        assert_eq!(c.switch_mode, SwitchMode::Session);
    }

    #[test]
    fn user_toml_round_trips_with_type_key() {
        let c: UiCommentConfig = toml::from_str(
            "state = \"detail\"\nswitch_key = \"ralt\"\nswitch_mode = \"hold\"\n[assign.type.codetable]\ndetail_vertical = \"x\"\n[assign.mode.\"mix:q\"]\ndefault_vertical = \"none\"\n",
        )
        .unwrap();
        assert_eq!(c.state, CommentState::Detail);
        assert_eq!(c.switch_key, SwitchKey::RAlt);
        assert_eq!(c.switch_mode, SwitchMode::Hold);
        assert_eq!(
            c.assign.by_type["codetable"].detail_vertical.as_deref(),
            Some("x")
        );
        assert_eq!(
            c.assign.mode["mix:q"].default_vertical.as_deref(),
            Some("none")
        );
    }

    // ---- 解析链 ----

    fn cells(
        dv: Option<&str>,
        dh: Option<&str>,
        tv: Option<&str>,
        th: Option<&str>,
    ) -> CommentCells {
        let o = |s: Option<&str>| s.map(str::to_string);
        CommentCells {
            default_vertical: o(dv),
            default_horizontal: o(dh),
            detail_vertical: o(tv),
            detail_horizontal: o(th),
        }
    }

    fn tpl(id: &str, text: &str) -> CommentTemplateDef {
        CommentTemplateDef {
            id: id.into(),
            name: id.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    /// 内置模板：code / full / a / b / c / d，文本为 `T:<id>`。
    fn sys(assign: CommentAssign) -> SystemComments {
        SystemComments {
            templates: ["code", "full", "a", "b", "c", "d"]
                .iter()
                .map(|id| tpl(id, &format!("T:{id}")))
                .collect(),
            assign,
            dicts: Vec::new(),
        }
    }

    fn scope<'a>(mode: Option<&'a str>) -> CommentScope<'a> {
        CommentScope {
            mode,
            schema: "wubi",
            schema_type: SchemaType::CodeTable,
        }
    }

    fn text(lib: &CommentLibrary, mode: Option<&str>, st: CommentState, v: bool) -> String {
        lib.template_text(&scope(mode), st, v).to_string()
    }

    fn global_only() -> CommentAssign {
        CommentAssign {
            global: cells(None, Some("code"), None, Some("full")),
            ..Default::default()
        }
    }

    #[test]
    fn global_only_resolves_both_states_and_layouts() {
        let (lib, w) = CommentLibrary::build(&sys(global_only()), &UiCommentConfig::default());
        assert!(w.is_empty());
        for v in [true, false] {
            assert_eq!(text(&lib, None, CommentState::Default, v), "T:code");
            assert_eq!(text(&lib, None, CommentState::Detail, v), "T:full");
        }
    }

    #[test]
    fn mode_beats_schema_beats_type_beats_global() {
        let mk = |id: &str| cells(None, None, None, Some(id));
        let mut a = global_only();
        a.global = mk("a");
        a.by_type.insert("codetable".into(), mk("b"));
        a.schema.insert("wubi".into(), mk("c"));
        a.mode.insert("url".into(), mk("d"));
        let u = UiCommentConfig::default();
        let check = |a: &CommentAssign, want: &str| {
            let (lib, _) = CommentLibrary::build(&sys(a.clone()), &u);
            for v in [false, true] {
                assert_eq!(text(&lib, Some("url"), CommentState::Detail, v), want);
            }
        };
        check(&a, "T:d");
        a.mode.clear();
        check(&a, "T:c");
        a.schema.clear();
        check(&a, "T:b");
        a.by_type.clear();
        check(&a, "T:a");
    }

    #[test]
    fn none_is_an_opinion() {
        let mut a = global_only();
        a.mode
            .insert("url".into(), cells(None, Some("none"), None, None));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        for v in [false, true] {
            assert_eq!(text(&lib, Some("url"), CommentState::Default, v), "");
            assert_eq!(text(&lib, None, CommentState::Default, v), "T:code");
        }
    }

    #[test]
    fn user_inherit_skips_system_cell() {
        let mut a = global_only();
        a.mode
            .insert("temp_english".into(), cells(None, Some("none"), None, None));
        let mut u = UiCommentConfig::default();
        u.assign.mode.insert(
            "temp_english".into(),
            cells(None, Some("inherit"), None, None),
        );
        let (lib, _) = CommentLibrary::build(&sys(a), &u);
        for v in [false, true] {
            assert_eq!(
                text(&lib, Some("temp_english"), CommentState::Default, v),
                "T:code"
            );
        }
    }

    #[test]
    fn vertical_same_as_horizontal_when_unset() {
        let mut a = global_only();
        a.global.detail_vertical = Some("b".into());
        a.schema
            .insert("wubi".into(), cells(None, None, None, Some("a")));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:a");
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:a");
    }

    #[test]
    fn vertical_same_as_horizontal_none() {
        let mut a = global_only();
        a.schema
            .insert("wubi".into(), cells(None, Some("none"), None, None));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Default, true), "");
    }

    #[test]
    fn vertical_inherit_goes_down() {
        let mut a = global_only();
        a.global.detail_vertical = Some("b".into());
        a.schema
            .insert("wubi".into(), cells(None, None, Some("inherit"), Some("a")));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:a");
    }

    #[test]
    fn horizontal_inherit_goes_down_and_vertical_follows_next_layer() {
        let mut a = global_only();
        a.global.detail_vertical = Some("b".into());
        a.schema
            .insert("wubi".into(), cells(None, None, None, Some("inherit")));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
    }

    #[test]
    fn vertical_written_does_not_affect_horizontal() {
        let mut a = global_only();
        a.schema
            .insert("wubi".into(), cells(None, None, Some("a"), None));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:a");
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
    }

    #[test]
    fn vertical_unset_and_horizontal_unset_follows_down() {
        let mut a = global_only();
        a.global.detail_vertical = Some("b".into());
        a.schema
            .insert("wubi".into(), cells(None, None, None, None));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
    }

    #[test]
    fn vertical_unset_with_horizontal_unset_applies_rule_at_next_layer() {
        // 方案层两格都不写 → 看类型层：类型层竖排也不写、横排有值 → 同类型层横排。
        let mut a = global_only();
        a.global.detail_vertical = Some("b".into());
        a.by_type
            .insert("codetable".into(), cells(None, None, None, Some("c")));
        a.schema
            .insert("wubi".into(), cells(None, None, None, None));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:c");
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:c");
    }

    #[test]
    fn user_inherit_on_vertical_skips_row_horizontal() {
        let mut a = global_only();
        a.global.detail_vertical = Some("b".into());
        a.by_type
            .insert("codetable".into(), cells(None, None, None, Some("c")));
        let mut u = UiCommentConfig::default();
        u.assign
            .by_type
            .insert("codetable".into(), cells(None, None, Some("inherit"), None));
        let (lib, _) = CommentLibrary::build(&sys(a), &u);
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:c");
    }

    #[test]
    fn unknown_id_follows_and_warns_once() {
        let mut a = global_only();
        a.schema
            .insert("wubi".into(), cells(None, Some("gone"), None, Some("gone")));
        let (lib, w) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("gone"));
        assert_eq!(text(&lib, None, CommentState::Default, true), "T:code");
        assert_eq!(text(&lib, None, CommentState::Detail, true), "T:full");
    }

    #[test]
    fn user_template_overrides_builtin() {
        let mut u = UiCommentConfig::default();
        u.templates.push(tpl("code", "X"));
        let (lib, w) = CommentLibrary::build(&sys(global_only()), &u);
        assert!(w.is_empty());
        assert_eq!(text(&lib, None, CommentState::Default, true), "X");
        let origin = lib
            .templates()
            .find(|(t, _)| t.id == "code")
            .map(|(_, o)| o);
        assert_eq!(origin, Some(TemplateOrigin::UserOverridesBuiltin));
    }

    #[test]
    fn fallback_when_global_empty() {
        let (lib, _) =
            CommentLibrary::build(&sys(CommentAssign::default()), &UiCommentConfig::default());
        assert_eq!(text(&lib, None, CommentState::Default, true), "T:code");
        assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
    }

    #[test]
    fn referenced_texts_covers_detail() {
        let (lib, _) = CommentLibrary::build(&sys(global_only()), &UiCommentConfig::default());
        assert!(lib.referenced_texts().contains(&"T:full"));
        assert!(lib.referenced_texts().contains(&"T:code"));
    }

    #[test]
    fn invalid_user_template_id_skipped_and_warned() {
        let mut u = UiCommentConfig::default();
        u.templates.push(tpl("Bad-Id", "x"));
        u.templates.push(tpl("", "x"));
        u.templates.push(tpl("mine", "first"));
        u.templates.push(tpl("mine", "second"));
        let (lib, w) = CommentLibrary::build(&sys(global_only()), &u);
        assert_eq!(w.len(), 3, "{w:?}");
        let ids: Vec<&str> = lib.templates().map(|(t, _)| t.id.as_str()).collect();
        assert!(!ids.contains(&"Bad-Id") && !ids.contains(&""));
        let mine = lib.templates().find(|(t, _)| t.id == "mine").unwrap();
        assert_eq!(
            (mine.0.text.as_str(), mine.1),
            ("first", TemplateOrigin::User)
        );
    }

    #[test]
    fn resolve_cell_reports_winner_and_layer() {
        let mut a = global_only();
        a.by_type
            .insert("codetable".into(), cells(None, None, None, Some("b")));
        a.mode
            .insert("url".into(), cells(None, Some("none"), None, None));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        let sc = scope(Some("url"));
        assert_eq!(
            lib.resolve_cell(&sc, AssignLayer::Mode, CommentState::Default, true),
            (None, AssignLayer::Mode)
        );
        assert_eq!(
            lib.resolve_cell(&sc, AssignLayer::Schema, CommentState::Default, true),
            (Some("code"), AssignLayer::Global)
        );
        assert_eq!(
            lib.resolve_cell(&sc, AssignLayer::Mode, CommentState::Detail, false),
            (Some("b"), AssignLayer::Type)
        );
        assert_eq!(
            lib.resolve_cell(&sc, AssignLayer::Global, CommentState::Detail, true),
            (Some("full"), AssignLayer::Global)
        );
    }

    /// 快捷输入自己没写的格落到通配 `mode.mix`，写了的覆盖它；别的模式不看通配格。
    #[test]
    fn mix_modes_fall_back_to_mix_all_cell() {
        let mut a = global_only();
        a.mode
            .insert(MIX_ALL_MODE.into(), cells(None, Some("b"), None, None));
        a.mode
            .insert("mix:own".into(), cells(None, None, None, Some("none")));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        for m in ["mix:own", "mix:other"] {
            assert_eq!(
                lib.resolve_cell(
                    &scope(Some(m)),
                    AssignLayer::Mode,
                    CommentState::Default,
                    false
                ),
                (Some("b"), AssignLayer::Mode),
                "{m}"
            );
        }
        assert_eq!(
            lib.resolve_cell(
                &scope(Some("mix:own")),
                AssignLayer::Mode,
                CommentState::Detail,
                false
            ),
            (None, AssignLayer::Mode)
        );
        assert_eq!(
            lib.resolve_cell(
                &scope(Some("url")),
                AssignLayer::Mode,
                CommentState::Default,
                false
            ),
            (Some("code"), AssignLayer::Global)
        );
    }

    #[test]
    fn resolve_cell_falls_back_to_builtin_ids() {
        let (lib, _) =
            CommentLibrary::build(&sys(CommentAssign::default()), &UiCommentConfig::default());
        assert_eq!(
            lib.resolve_cell(&scope(None), AssignLayer::Type, CommentState::Detail, false),
            (Some("full"), AssignLayer::Global)
        );
    }

    #[test]
    fn effective_table_lists_fixed_and_assigned_rows() {
        let mut a = global_only();
        a.schema
            .insert("wubi".into(), cells(None, None, None, Some("a")));
        a.mode
            .insert("url".into(), cells(None, Some("none"), None, None));
        let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
        let t = lib.effective_table("wubi", |_| SchemaType::CodeTable);
        assert_eq!(t.len(), (1 + 5 + 1 + 1) * 4);
        let find = |scope: &str, st: CommentState, v: bool| {
            t.iter()
                .find(|c| c.scope == scope && c.state == st && c.vertical == v)
                .unwrap()
                .clone()
        };
        let c = find("schema:wubi", CommentState::Detail, false);
        assert_eq!(
            (c.template_id.as_deref(), c.from_scope.as_str()),
            (Some("a"), "schema:wubi")
        );
        let c = find("mode:url", CommentState::Detail, true);
        assert_eq!(
            (c.template_id.as_deref(), c.from_scope.as_str()),
            (Some("a"), "schema:wubi"),
            "模式行的下层取当前方案"
        );
        let c = find("mode:url", CommentState::Default, true);
        assert_eq!((c.template_id, c.from_scope.as_str()), (None, "mode:url"));
    }

    /// 出厂分配在两种排布下的实际结果（横排为主改写前后一致）。
    #[test]
    fn shipped_effective_table_snapshot() {
        let (lib, w) = CommentLibrary::build(&shipped(), &UiCommentConfig::default());
        assert!(w.is_empty(), "{w:?}");
        let t = lib.effective_table("wubi86", |_| SchemaType::CodeTable);
        let got = |scope: &str, st: CommentState| {
            let pick = |v: bool| {
                t.iter()
                    .find(|c| c.scope == scope && c.state == st && c.vertical == v)
                    .map(|c| (c.template_id.clone(), c.from_scope.clone()))
                    .unwrap()
            };
            let (h, v) = (pick(false), pick(true));
            assert_eq!(h, v, "{scope} {st:?}: 竖排应同横排");
            (h.0.unwrap_or_else(|| "none".into()), h.1)
        };
        use CommentState::{Default as D, Detail as T};
        for (scope, st, id, from) in [
            ("global", D, "code", "global"),
            ("global", T, "full", "global"),
            ("type:codetable", D, "code", "global"),
            ("type:codetable", T, "chaizi_pinyin", "type:codetable"),
            ("type:pinyin", D, "none", "type:pinyin"),
            ("type:pinyin", T, "pinyin_en", "type:pinyin"),
            ("type:shuangpin", T, "pinyin_en", "type:shuangpin"),
            ("type:mixed", D, "code_rev", "type:mixed"),
            ("type:mixed", T, "full", "global"),
            ("type:english", D, "none", "type:english"),
            ("type:english", T, "en_zh", "type:english"),
            ("mode:temp_english", D, "none", "mode:temp_english"),
            ("mode:temp_english", T, "en_zh", "mode:temp_english"),
            ("mode:temp_pinyin", D, "code", "mode:temp_pinyin"),
            ("mode:temp_pinyin", T, "chaizi_code", "mode:temp_pinyin"),
            ("mode:mix", D, "code", "mode:mix"),
        ] {
            assert_eq!(
                got(scope, st),
                (id.to_string(), from.to_string()),
                "{scope} {st:?}"
            );
        }
    }
}
