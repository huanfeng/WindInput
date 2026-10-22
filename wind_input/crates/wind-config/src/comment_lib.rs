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

/// Alt 切换态的方式（值域见 `config_schema::COMMENT_ALT_SWITCH_VALUES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AltSwitch {
    Off,
    #[default]
    Session,
    Persist,
    Hold,
}

/// 命名模板。`id` 只含 `[a-z0-9_]`，`name` 是显示名。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommentTemplateDef {
    pub id: String,
    pub name: String,
    pub text: String,
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
    pub alt_switch: AltSwitch,
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
            alt_switch: AltSwitch::Session,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped() -> SystemComments {
        SystemComments::parse(include_str!("../../../../data/system.comments.toml"))
            .expect("出厂 system.comments.toml 应能解析")
    }

    #[test]
    fn system_comments_parses_shipped_file() {
        let s = shipped();
        let ids: Vec<&str> = s.templates.iter().map(|t| t.id.as_str()).collect();
        for want in [
            "code_hint",
            "pinyin",
            "chaizi",
            "chaizi_code",
            "chaizi_pinyin",
            "en_meaning",
            "zh_english",
            "pinyin_english",
            "detail_full",
        ] {
            assert!(ids.contains(&want), "缺内置模板 {want}");
        }
        assert_eq!(
            s.assign.global.default_vertical.as_deref(),
            Some("code_hint")
        );
        assert_eq!(
            s.assign.global.detail_vertical.as_deref(),
            Some("detail_full")
        );
        let dicts: Vec<&str> = s.dicts.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(dicts, ["en_zh", "zh_en"]);
        assert_eq!(s.dicts[0].path, "comments/en_zh.txt");
        assert_eq!(s.dicts[1].path, "comments/zh_en.txt");
    }

    #[test]
    fn shipped_assign_matches_design_table() {
        let a = shipped().assign;
        let detail = |m: &BTreeMap<String, CommentCells>, k: &str| {
            m.get(k).and_then(|c| c.detail_vertical.clone())
        };
        assert_eq!(
            detail(&a.by_type, "codetable").as_deref(),
            Some("chaizi_pinyin")
        );
        assert_eq!(
            detail(&a.by_type, "pinyin").as_deref(),
            Some("pinyin_english")
        );
        assert_eq!(
            detail(&a.by_type, "shuangpin").as_deref(),
            Some("pinyin_english")
        );
        assert_eq!(detail(&a.by_type, "english").as_deref(), Some("en_meaning"));
        assert_eq!(
            a.by_type["english"].default_vertical.as_deref(),
            Some("none")
        );
        assert_eq!(
            detail(&a.mode, "temp_english").as_deref(),
            Some("en_meaning")
        );
        assert_eq!(
            a.mode["temp_english"].default_vertical.as_deref(),
            Some("none")
        );
        assert_eq!(
            detail(&a.mode, "temp_pinyin").as_deref(),
            Some("chaizi_code")
        );
    }

    #[test]
    fn code_hint_template_matches_legacy_default() {
        let s = shipped();
        let t = s.templates.iter().find(|t| t.id == "code_hint").unwrap();
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
        assert_eq!(c.alt_switch, AltSwitch::Session);
        assert!(c.templates.is_empty());
        assert_eq!(c.assign, CommentAssign::default());
    }

    #[test]
    fn user_toml_round_trips_with_type_key() {
        let c: UiCommentConfig = toml::from_str(
            "state = \"detail\"\nalt_switch = \"hold\"\n[assign.type.codetable]\ndetail_vertical = \"x\"\n[assign.mode.\"mix:q\"]\ndefault_vertical = \"none\"\n",
        )
        .unwrap();
        assert_eq!(c.state, CommentState::Detail);
        assert_eq!(c.alt_switch, AltSwitch::Hold);
        assert_eq!(
            c.assign.by_type["codetable"].detail_vertical.as_deref(),
            Some("x")
        );
        assert_eq!(
            c.assign.mode["mix:q"].default_vertical.as_deref(),
            Some("none")
        );
    }
}
