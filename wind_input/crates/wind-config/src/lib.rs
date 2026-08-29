//! wind-config: 配置系统（TOML 四层合并、Schema YAML 定义、热键编译）
//!
//! 与 Go 版本 `wind_input/pkg/config/` 和 `wind_input/internal/schema/` 对齐。
//! 层序 `L1 默认 < L2 data < L2.5 data_custom < L3 用户层`，见 `docs/design/data-custom-layer.md`。

pub mod app_compat;
pub mod change_hook;
pub mod code_charset;
pub mod config;
pub mod config_schema;
pub mod dir_var;
pub mod hotkey;
pub mod patch;
pub mod runtime_state;
pub mod schema;
pub mod startup_trace;
pub mod variant;

pub use code_charset::{CodeCharSet, CodeCharSetError};
pub use config::{
    AssociationConfig, AuxCodeShare, BoundAction, CUSTOM_DATA_DIR_NAME, CUSTOM_MANIFEST_NAME,
    CodetableGlobal, Config, CustomHideList, CustomIdentity, CustomManifest, DEFAULT_LABEL_CAPS,
    DEFAULT_LABEL_ENGLISH, LabelsConfig, LangBarConfig, LayoutIntent, MixGlobal,
    MobileAssociationConfig, MobileConfig, ModeIndicatorStyle, Orientation, PinyinFuzzy,
    PinyinGlobalConfig, PreeditDisplay, SessionAction, TOOLBAR_ITEM_KEYS, TOOLBAR_LABEL_MAX_WIDTH,
    TextOrientation, ToolbarButtonSpec, TopCommitMode, toolbar_label_trunc,
};
pub use dir_var::{dir_var, dir_var_help, dir_var_names, dir_var_str, is_dir_var};
pub use runtime_state::RuntimeState;
pub use schema::{
    CandidateSpec, ICON_LABEL_MAX_WIDTH, OverlaySpec, PhrasesSpec, PunctIntent, PunctSpec, Schema,
    SchemaBehavior, icon_label_or, icon_label_trunc,
};
