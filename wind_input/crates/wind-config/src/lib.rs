//! wind-config: 配置系统（TOML 四层合并、Schema YAML 定义、热键编译）
//!
//! 与 Go 版本 `wind_input/pkg/config/` 和 `wind_input/internal/schema/` 对齐。
//! 层序 `L1 默认 < L2 data < L2.5 data_custom < L3 用户层`，见 `docs/design/data-custom-layer.md`。

pub mod app_compat;
pub mod by_layout;
pub mod change_hook;
pub mod charset_def;
pub mod code_charset;
pub mod comment_lib;
pub mod compat_admin;
/// 兼容规则表的性能基准（`#[ignore]`，见模块头部）。
#[cfg(test)]
mod compat_bench;
pub(crate) mod compat_overlay;
pub mod compat_schema;
pub mod config;
pub mod config_schema;
pub mod custom_settings;
pub mod dir_var;
pub mod font_name;
pub mod hotkey;
pub mod patch;
pub mod runtime_state;
pub mod schema;
pub mod section_fallback;
pub mod startup_trace;
pub mod tolerant_de;
pub mod user_meta;
/// 值域守门元测试（见模块头部）。放在 `src/` 而非 `tests/`：它要遍历
/// `AppCompatFile` 等 crate 私有类型，集成测试只看得见 pub API。
#[cfg(test)]
mod value_domain_guard;
pub mod variant;

pub use code_charset::{CodeCharSet, CodeCharSetError};
pub use config::{
    AssociationConfig, AuxCodeShare, BoundAction, CUSTOM_DATA_DIR_NAME, CUSTOM_MANIFEST_NAME,
    CodetableEnglishMerge, CodetableGlobal, CommentDictSpec, Config, CorrectionConfig,
    CorrectionRuleToggles, CustomHideList, CustomIdentity, CustomManifest, DEFAULT_DOTA2_ALIAS,
    DEFAULT_LABEL_CAPS, DEFAULT_LABEL_ENGLISH, DOTA2_ALIAS_MAX_LEN, EMOJI_MODULES, EMOJI_USER_DIR,
    EmojiModule, EmojiModuleEntry, HintScope, KeyOrigin, LabelsConfig, LangBarConfig, LayerOrigin,
    LayoutIntent, MixGlobal, MobileAssociationConfig, MobileConfig, ModeIndicatorStyle,
    Orientation, PinyinEnglishMerge, PinyinFuzzy, PinyinGlobalConfig, PinyinHintConfig,
    PreeditDisplay, ResourceLayer, SessionAction, SingleCharAction, TOOLBAR_ITEM_KEYS,
    TOOLBAR_LABEL_MAX_WIDTH, TextOrientation, ToolbarButtonSpec, TopCommitMode,
    sanitize_dota2_alias, toolbar_label_trunc,
};
pub use custom_settings::{SETTINGS_FEATURES, SettingsHide, SettingsSelector};
pub use dir_var::{dir_var, dir_var_help, dir_var_names, dir_var_str, is_dir_var};
pub use runtime_state::RuntimeState;
pub use schema::{
    CandidateSpec, ICON_LABEL_MAX_WIDTH, OverlaySpec, PhrasesSpec, PunctIntent, PunctSpec, Schema,
    SchemaBehavior, icon_label_or, icon_label_trunc,
};
