//! wind-theme: 主题系统（TOML 主题加载、调色板、View 定义）
//!
//! 与 Go 版本 `wind_input/pkg/theme/` 对齐（schema v3）；存储格式为 TOML（扁平人写形态，
//! 经 `normalize` 归一化为内存形态，见 `normalize.rs`）。

pub mod contract;
pub mod fallback;
pub mod normalize;
pub mod palette;
pub mod resolve;
pub mod rvnode;
pub mod schema;
pub mod skin;
pub mod span;
pub mod theme;

pub use palette::{Rgba, composite_over, contrast_ratio};
pub use resolve::{
    LangbarTextColors, Resolved, ResolvedBehavior, load_resolved, load_resolved_dirs, resolve,
};
pub use rvnode::{DEFAULT_ACCENT_BAR_HEIGHT_RATIO, RvGradient, RvImage, RvNode, RvViews};
pub use schema::Meta;
pub use span::{
    Atom, ColorRef, InlineColor, PINYIN_HINT_ROLE, TextState, body_color, pinyin_hint_color,
    span_color,
};
pub use theme::{
    find_theme_dir, is_plain_slug, is_windows_reserved_name, list_theme_ids, load_merged_dirs,
    meta_from_text, read_meta, slug_from_name, validate_text,
};
