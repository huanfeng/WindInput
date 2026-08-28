//! 软键盘映射表。
//!
//! 一块持续常驻的符号面板：开启后接管主键区按键，敲什么、点什么，都按当前面的映射表
//! 转换后上屏——与自定义标点是同一种语义，只是范围扩大到整个键盘。
//!
//! ## 定位
//!
//! **它不是方案，是配置文件。** 数据落在单个 `system.softkeyboard.toml`，完全独立于
//! 方案体系；由此软键盘**脱离引擎**——没有方案加载、没有码表查询、没有候选生成，
//! 按键直接查这里展开好的内存表。设计与被否决的两条方案路线见
//! `docs/design/soft-keyboard.md`。
//!
//! ## 三层模型
//!
//! ```text
//! rows / rows_shift（整面画布，位置对位）
//!         ↓ 加载展开
//!    Page.slots: (键位, 层) → 输出        ← 唯一的运行时真相
//!         ↑ keys 单键补丁（同文件内，按键名）
//!         ↑ 用户文件稀疏覆盖（同格式）
//! ```
//!
//! ★ **写侧只允许写覆盖层**：`rows` 只有一个写入者（作者，或用户整份替换文件），
//! 补丁层只做稀疏 diff、永不整表回写。本仓所有覆盖类事故都是同一个根因——
//! 两个写入者对同一份整表各写各的。

pub mod layout;
mod table;

pub use layout::{KEY_ROWS, SLOT_COUNT, all_slots, normalize_slot, parse_patch_key};
pub use table::{HOLE, Page, SoftKeyboardTable};

/// 出厂文件名（相对 `data/`）。
pub const FILE_NAME: &str = "system.softkeyboard.toml";
