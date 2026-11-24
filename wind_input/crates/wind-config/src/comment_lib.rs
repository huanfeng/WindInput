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
    /// 不用切换键（只靠按键绑定、命令栏切换）。
    None,
    /// 功能键 F1（出厂）–F12。不走修饰键单击判定：生效时补进会话按键表（见
    /// [`crate::Config::effective_session_actions`]），有候选时按下即切换。
    #[default]
    #[serde(rename = "f1")]
    F1,
    #[serde(rename = "f2")]
    F2,
    #[serde(rename = "f3")]
    F3,
    #[serde(rename = "f4")]
    F4,
    #[serde(rename = "f5")]
    F5,
    #[serde(rename = "f6")]
    F6,
    #[serde(rename = "f7")]
    F7,
    #[serde(rename = "f8")]
    F8,
    #[serde(rename = "f9")]
    F9,
    #[serde(rename = "f10")]
    F10,
    #[serde(rename = "f11")]
    F11,
    #[serde(rename = "f12")]
    F12,
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
    /// 该修饰键虚拟键码（0xA2 LCtrl / 0xA3 RCtrl / 0xA4 LAlt / 0xA5 RAlt）是否属于本切换键；功能键与 `None` 恒 false。
    pub fn matches_vk(self, vk: u32) -> bool {
        match self {
            SwitchKey::Ctrl => matches!(vk, 0xA2 | 0xA3),
            SwitchKey::LCtrl => vk == 0xA2,
            SwitchKey::RCtrl => vk == 0xA3,
            SwitchKey::Alt => matches!(vk, 0xA4 | 0xA5),
            SwitchKey::LAlt => vk == 0xA4,
            SwitchKey::RAlt => vk == 0xA5,
            _ => false,
        }
    }

    /// 功能键在会话按键表里的键名（`f1`..`f12`）；不是功能键为 `None`。
    pub fn function_key_name(self) -> Option<&'static str> {
        const NAMES: [&str; 12] = [
            "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
        ];
        let i = match self {
            SwitchKey::F1 => 0,
            SwitchKey::F2 => 1,
            SwitchKey::F3 => 2,
            SwitchKey::F4 => 3,
            SwitchKey::F5 => 4,
            SwitchKey::F6 => 5,
            SwitchKey::F7 => 6,
            SwitchKey::F8 => 7,
            SwitchKey::F9 => 8,
            SwitchKey::F10 => 9,
            SwitchKey::F11 => 10,
            SwitchKey::F12 => 11,
            _ => return None,
        };
        Some(NAMES[i])
    }

    /// 功能键的虚拟键码（F1 = 0x70 … F12 = 0x7B）；不是功能键为 `None`。
    pub fn function_vk(self) -> Option<u32> {
        let name = self.function_key_name()?;
        name[1..].parse::<u32>().ok().map(|n| 0x6F + n)
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
    /// 只在内部用：注释关着、或切换键是「无」时的生效档。不是配置值（不用切换键就把
    /// `switch_key` 设成 `none`），配置里写了也解析不出、按出厂处理。
    #[serde(skip)]
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

/// 用户模板 id：合 `[a-z0-9_]+` 且不是保留字。
pub(crate) fn valid_user_template_id(id: &str) -> bool {
    valid_template_id(id) && !matches!(id, "none" | "inherit")
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
#[path = "comment_lib_tests.rs"]
mod tests;
