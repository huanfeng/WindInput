//! 定制版「设置界面减法」：`data_custom/custom.toml` 的 `[settings] hide` 选择器。
//!
//! # 语义
//!
//! 这一段只决定**设置程序显示什么**，不影响任何功能——被隐藏的设置照常生效，值仍来自
//! 定制层 / 用户层。与同一份清单里的 `[schemas] hide` / `[themes] hide`（「这个东西在本
//! 定制版里**不存在**」）刻意不同：定制者要关掉一个功能，应在 `data_custom/config.toml`
//! 里把值设好，再在这里把它藏起来。把两件事合成一个开关，就会重演 `hidden` 与 `hide`
//! 被混用的那个坑（见 `docs/design/data-custom-layer.md` 契约 5）。
//!
//! # 为什么住在 wind-config
//!
//! 消费者有两个：`wind_input config check --custom`（打包前校验）与设置程序（真正按它
//! 隐藏）。两边各写一份解析，迟早一边认 `key:` 带空格、另一边不认；而设置程序本就依赖
//! 本 crate，放这里是唯一不用复制的位置。
//!
//! # 选择器
//!
//! - `key:<配置键>`：精确匹配设置项的 key；以 `*` 结尾时按前缀匹配（`key:input.temp_english.*`）。
//!   两种写法都连带视图项 `a.b#视图`（`schema.mix_modes` 这类一个配置键多行的设置）：视图与
//!   本体读写的是同一个配置键，只藏一半就留下一行还能改它的控件。`a.b.*` 另命中 `a.b` 本身。
//!   精确写法不命中子键 `a.b.c` 与同前缀的 `a.b_x`。
//!   用配置键而不是界面上的分区名：分区名是文案，随界面调整改名；配置键有迁移保障。
//!   一个分区里的项全被隐藏后，设置程序会把那张卡片连同二级导航一并去掉，故不需要单独的
//!   「隐藏分区」语法。
//! - `feature:<id>`：不由设置清单驱动的界面元素，取值见 [`SETTINGS_FEATURES`]。

/// `feature:` 选择器认识的 id。新增一项 = 设置程序那边多一个消费点，两边同时改。
///
/// - `wizard`：设置向导（不自动弹出，侧栏也不显示入口）。定制版通常已替用户选好方案，
///   向导的「选方案 + 套推荐预设」会写进用户层、盖掉定制层调好的值。
///
/// 其余都是设置清单之外的手写卡 / 控件（没有配置键，`key:` 够不着），一张一个：
///
/// - `theme_transfer`：外观 → 主题卡里的「导入主题 / 导出当前主题 / 主题市场」三行。
/// - `theme_editor`：外观 → 「主题编辑器」卡。
/// - `shuangpin_layout`：方案 → 「双拼」卡里的「双拼布局」下拉（布局属于方案文件，不是配置键）。
/// - `integration`：高级 → 「系统集成」卡（协议关联 / 文件关联）。
/// - `config_import`：配置导入的全部入口——高级 → 「导入」卡、侧栏 / 底栏 / 词库窗的右键
///   导入菜单、`--page import` 深链（含命令直通车 `setting.open("import")`）。
/// - `backup`：高级 → 「备份与还原」卡。
/// - `compat`：高级 → 「应用兼容性」入口卡。
/// - `memory`：高级 → 性能与诊断卡里的「内存占用 / 诊断信息」行。
/// - `update_check`：关于页的「检查更新」按钮，连同打开设置时的自动检查。
/// - `dark_mode`：底栏的浅色 / 深色切换。
/// - `accent`：底栏的主题色下拉。
/// - `subnav`：设置窗中间那一栏二级导航（各页的分块目录），所有页都不显示。
pub const SETTINGS_FEATURES: &[&str] = &[
    "wizard",
    "theme_transfer",
    "theme_editor",
    "shuangpin_layout",
    "integration",
    "config_import",
    "backup",
    "compat",
    "memory",
    "update_check",
    "dark_mode",
    "accent",
    "subnav",
];

/// 一条解析过的 `[settings] hide` 选择器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsSelector {
    /// 设置项 key。`prefix = true` 时 `pattern` 是去掉尾部 `*` 后的前缀。
    Key { pattern: String, prefix: bool },
    /// 界面元素，取值属于 [`SETTINGS_FEATURES`]。
    Feature(String),
}

impl SettingsSelector {
    /// 解析一条选择器。错误文案直接给定制者看（`config check` 原样输出），故写成「哪里错 +
    /// 该怎么写」。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw.trim();
        let Some((kind, body)) = s.split_once(':') else {
            return Err(format!(
                "`{s}` 缺少类型前缀。写成 `key:<配置键>` 或 `feature:<{}>`。",
                SETTINGS_FEATURES.join("|")
            ));
        };
        let body = body.trim();
        match kind.trim() {
            "key" => {
                if body.is_empty() || body == "*" {
                    return Err(format!(
                        "`{s}` 没有给出配置键。隐藏全部设置不受支持——要整体精简，请逐段列出。"
                    ));
                }
                let (pattern, prefix) = match body.strip_suffix('*') {
                    Some(p) => (p, true),
                    None => (body, false),
                };
                if pattern.contains('*') {
                    return Err(format!("`{s}` 里的 `*` 只能出现在末尾（前缀匹配）。"));
                }
                Ok(Self::Key {
                    pattern: pattern.to_string(),
                    prefix,
                })
            }
            "feature" => {
                if SETTINGS_FEATURES.contains(&body) {
                    Ok(Self::Feature(body.to_string()))
                } else {
                    Err(format!(
                        "`{s}` 不是认识的界面元素。可用：{}。",
                        SETTINGS_FEATURES.join(" / ")
                    ))
                }
            }
            other => Err(format!(
                "`{s}` 的类型 `{other}` 不认识。只有 `key:` 与 `feature:` 两种。"
            )),
        }
    }

    /// 是否命中设置项 `key`。`Feature` 永不命中设置项。
    pub fn matches_key(&self, key: &str) -> bool {
        match self {
            Self::Key { pattern, prefix } => {
                if *prefix {
                    // `a.b.*` 也命中 `a.b` 本身与它的视图项 `a.b#视图`：定制者写 `.*` 想的是
                    // 「这一整块」，而 `schema.mix_modes` 这类键自己就是一行、其余行是它的视图。
                    key.starts_with(pattern.as_str())
                        || pattern.strip_suffix('.').is_some_and(|stem| {
                            key.strip_prefix(stem)
                                .is_some_and(|rest| rest.is_empty() || rest.starts_with('#'))
                        })
                } else {
                    // 精确 key 也命中它的视图项 `a.b#视图`，不命中 `a.b.c` / `a.b_x`。
                    key.strip_prefix(pattern.as_str())
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('#'))
                }
            }
            Self::Feature(_) => false,
        }
    }
}

/// 一份解析好的 `[settings] hide`。解析失败的条目被丢弃（设置程序侧不该因为一条写错的
/// 选择器就整段失效），错误另由 [`SettingsHide::parse`] 的第二个返回值交给调用方记日志。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsHide {
    selectors: Vec<SettingsSelector>,
}

impl SettingsHide {
    /// 解析整段。返回（可用的选择器集合，每条坏选择器的错误文案）。
    pub fn parse<S: AsRef<str>>(raw: &[S]) -> (Self, Vec<String>) {
        let mut selectors = Vec::new();
        let mut errors = Vec::new();
        for r in raw {
            if r.as_ref().trim().is_empty() {
                continue;
            }
            match SettingsSelector::parse(r.as_ref()) {
                Ok(sel) => selectors.push(sel),
                Err(e) => errors.push(e),
            }
        }
        (Self { selectors }, errors)
    }

    /// 没有任何有效选择器。
    pub fn is_empty(&self) -> bool {
        self.selectors.is_empty()
    }

    /// 设置项 `key` 是否被隐藏。
    pub fn hides_key(&self, key: &str) -> bool {
        self.selectors.iter().any(|s| s.matches_key(key))
    }

    /// 界面元素 `id` 是否被隐藏。
    pub fn hides_feature(&self, id: &str) -> bool {
        self.selectors
            .iter()
            .any(|s| matches!(s, SettingsSelector::Feature(f) if f == id))
    }

    /// 全部选择器（设置程序用来报「哪条一项都没命中」）。
    pub fn selectors(&self) -> &[SettingsSelector] {
        &self.selectors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_key_matches_only_itself() {
        let sel = SettingsSelector::parse("key:input.temp_english.enabled").unwrap();
        assert!(sel.matches_key("input.temp_english.enabled"));
        assert!(!sel.matches_key("input.temp_english.enabled_x"));
        assert!(!sel.matches_key("input.temp_english"));
    }

    #[test]
    fn trailing_star_is_prefix_match() {
        let sel = SettingsSelector::parse("key:input.temp_english.*").unwrap();
        assert!(sel.matches_key("input.temp_english.enabled"));
        assert!(sel.matches_key("input.temp_english.trigger_keys"));
        assert!(!sel.matches_key("input.temp_pinyin.enabled"));
    }

    #[test]
    fn dot_star_also_matches_the_stem_and_its_views() {
        let sel = SettingsSelector::parse("key:schema.mix_modes.*").unwrap();
        assert!(sel.matches_key("schema.mix_modes"));
        assert!(sel.matches_key("schema.mix_modes#members"));
        assert!(!sel.matches_key("schema.mix_modes_x"));
        assert!(!sel.matches_key("schema.mix"));
    }

    /// 精确 key 连带视图项，但不外溢到子键或同前缀的兄弟键。
    #[test]
    fn exact_key_also_matches_its_views() {
        let sel = SettingsSelector::parse("key:schema.mix_modes").unwrap();
        assert!(sel.matches_key("schema.mix_modes"));
        assert!(sel.matches_key("schema.mix_modes#members"));
        assert!(sel.matches_key("schema.mix_modes#candidate_layout"));
        assert!(!sel.matches_key("schema.mix_modes.members"));
        assert!(!sel.matches_key("schema.mix_modes_x"));
        assert!(!sel.matches_key("schema.mix"));
    }

    /// 精确写视图 key 只藏那一个视图，本体与别的视图不受牵连。
    #[test]
    fn exact_view_key_matches_only_that_view() {
        let sel = SettingsSelector::parse("key:schema.mix_modes#members").unwrap();
        assert!(sel.matches_key("schema.mix_modes#members"));
        assert!(!sel.matches_key("schema.mix_modes"));
        assert!(!sel.matches_key("schema.mix_modes#free_input"));
    }

    #[test]
    fn whitespace_around_parts_is_tolerated() {
        let sel = SettingsSelector::parse("  key : ui.toolbar.* ").unwrap();
        assert!(sel.matches_key("ui.toolbar.visible"));
    }

    #[test]
    fn known_feature_parses() {
        for id in SETTINGS_FEATURES {
            assert_eq!(
                SettingsSelector::parse(&format!("feature:{id}")).unwrap(),
                SettingsSelector::Feature((*id).into())
            );
        }
    }

    /// 报错要把合法值列全：定制者照着改，漏列一个就等于没告诉他有这一项。
    #[test]
    fn unknown_feature_error_lists_every_id() {
        let err = SettingsSelector::parse("feature:nope").unwrap_err();
        for id in SETTINGS_FEATURES {
            assert!(err.contains(id), "报错里缺 `{id}`：{err}");
        }
    }

    #[test]
    fn malformed_selectors_are_rejected() {
        for bad in [
            "input.temp_english.*",
            "key:",
            "key:*",
            "key:input.*.enabled",
            "feature:nope",
            "page:stats",
        ] {
            assert!(SettingsSelector::parse(bad).is_err(), "{bad} 应被拒");
        }
    }

    #[test]
    fn feature_never_matches_a_key() {
        let sel = SettingsSelector::parse("feature:wizard").unwrap();
        assert!(!sel.matches_key("wizard"));
    }

    /// 一条坏选择器不连累其余：设置程序那边宁可少藏一项，也不能整段失效。
    #[test]
    fn bad_entries_are_dropped_good_ones_kept() {
        let (hide, errors) =
            SettingsHide::parse(&["key:ui.toolbar.*", "bogus", "", "feature:wizard"]);
        assert_eq!(errors.len(), 1);
        assert!(hide.hides_key("ui.toolbar.visible"));
        assert!(hide.hides_feature("wizard"));
        assert!(!hide.hides_feature("other"));
    }

    /// 清单里的 `[settings]` 段落进 `CustomManifest::settings`（与 `[schemas]` 同形）。
    #[test]
    fn manifest_settings_section_deserializes() {
        let m: crate::CustomManifest =
            toml::from_str("[settings]\nhide = [\"feature:wizard\"]\n").unwrap();
        assert_eq!(m.settings.hide, vec!["feature:wizard".to_string()]);
        let (hide, errors) = SettingsHide::parse(&m.settings.hide);
        assert!(errors.is_empty());
        assert!(hide.hides_feature("wizard"));
    }
}
