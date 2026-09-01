//! 运行时状态持久化（state.toml，存于本机状态目录）。
//!
//! 与 Go 版本 `wind_input/pkg/config/runtime_state.go` 对齐。

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// 运行时状态（进程退出时保存，启动时恢复）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeState {
    /// 上次中文/英文模式。缺字段（旧 state.toml 从未写过）默认 true（中文，与配置默认一致）。
    #[serde(default = "default_true")]
    pub last_chinese_mode: bool,
    /// 上次全角/半角。
    #[serde(default)]
    pub last_full_width: bool,
    /// 上次中/英标点。缺字段（旧 state.toml）默认 true（中文标点，与配置默认一致）。
    #[serde(default = "default_true")]
    pub last_chinese_punct: bool,
    /// 语言栏图标：是否在各尺寸档位图上烧尺寸标记（调试用，见设计文档「验证设计」）。
    ///
    /// ⚠ **只剩这一项留在 state.toml。** 形状、配色、大小、透明度等会影响用户可见呈现的
    /// 参数已全部移到 `[ui.langbar]` 配置段——两处都能改同一个量就等于有两个真相源，
    /// 重启后谁赢取决于加载顺序。留在这里的判据是「它是不是纯调试项」：烧尺寸档标记
    /// 只为回答"系统实际取用了哪一档"，不是任何人想长期看到的样子。
    ///
    /// `Option` 且 `None` = 用代码默认：本 crate 不重复声明默认值，唯一出处是
    /// `wind_ui::langbar_icon`。在这里再写一份，改默认时必然漏掉一处，
    /// 而症状是「新装的机器和用过的机器表现不一样」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub langbar_icon_size_marks: Option<bool>,
    /// 工具栏位置，按显示器 key（"workRight,workBottom"）独立记录。
    #[serde(default)]
    pub toolbar_positions: HashMap<String, (i32, i32)>,
    /// 软键盘上次停在哪一面（面 **id**）。空 = 没记录过，开在第一面。
    ///
    /// # 为什么存 id 不存下标
    ///
    /// 这份状态跨重启，而面表来自配置（`softkeyboard.toml` 与定制层），用户在两次运行
    /// 之间增删一面，下标就**必然**指到别的面上。`SoftKeyboardTable::index_of` 按 id
    /// 找不到就当没记录，比默默开到一个陌生的面好。
    ///
    /// ⚠️ 与 `ToolbarAction::Custom(u8)` 那个**刻意用下标**的载荷不是一回事：那条是同一
    /// 进程内的一次回指，两端之间最多隔着一瞬的配置重载；这条两端之间隔着一次重启。
    ///
    /// # 为什么不受 `input.default.remember_last_state` 门控
    ///
    /// 那个开关管的是**输入态**（中英 / 全半角 / 标点）——它会改变用户下一次开始打字的
    /// 行为，确实有人想每次都从中文半角起步。而「面板上次停在哪一页」不是输入态，是
    /// 界面便利，与 [`Self::toolbar_positions`] 同类：没有人会想要「每次都跳回第一面」。
    #[serde(default)]
    pub last_softkeyboard_page: String,
    /// 候选框固定位置（pin_candidate_position 启用时）。
    /// 外层 key = 进程名（小写），内层 key = 显示器 key。
    #[serde(default)]
    pub candidate_pin_positions: HashMap<String, HashMap<String, (i32, i32)>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            last_chinese_mode: true,
            last_full_width: false,
            last_chinese_punct: true,
            langbar_icon_size_marks: None,
            toolbar_positions: HashMap::new(),
            last_softkeyboard_page: String::new(),
            candidate_pin_positions: HashMap::new(),
        }
    }
}

impl RuntimeState {
    /// 从 `state_dir/state.toml` 加载，文件不存在或解析失败时返回默认值。
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join("state.toml");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 原子写入 `state_dir/state.toml`（tmp + rename）。
    pub fn save(&self, state_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let content = toml::to_string_pretty(self)?;
        let tmp = state_dir.join("state.toml.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, state_dir.join("state.toml"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧 state.toml（无 last_* 三字段）反序列化应落到语义默认：中文/半角/中文标点。
    #[test]
    fn old_state_toml_defaults_to_chinese() {
        let rs: RuntimeState = toml::from_str("[toolbar_positions]\n").unwrap();
        assert!(rs.last_chinese_mode);
        assert!(!rs.last_full_width);
        assert!(rs.last_chinese_punct);
    }

    /// Default 与 serde 缺字段默认一致（load 失败回退 unwrap_or_default 的语义相同）。
    #[test]
    fn default_matches_serde_defaults() {
        let d = RuntimeState::default();
        assert!(d.last_chinese_mode);
        assert!(!d.last_full_width);
        assert!(d.last_chinese_punct);
    }

    /// 语言栏图标三项 roundtrip，且**未设置时不得出现在文件里**。
    ///
    /// 后半条是要害：`None` 的语义是「用代码默认」，一旦被序列化成某个具体值写进
    /// state.toml，这台机器就此被钉死在写入当天的默认上——之后改代码默认对它无效，
    /// 表现为「新机器和老机器不一样」。toml 也确实不能表达 None，漏掉
    /// skip_serializing_if 会直接让整个 save 失败（连工具栏位置一起丢）。
    #[test]
    fn langbar_icon_prefs_roundtrip_and_omit_when_unset() {
        let empty = toml::to_string_pretty(&RuntimeState::default()).unwrap();
        assert!(
            !empty.contains("langbar_icon"),
            "未设置的语言栏图标偏好被写进了文件:\n{empty}"
        );

        let rs = RuntimeState {
            langbar_icon_size_marks: Some(true),
            ..Default::default()
        };
        let s = toml::to_string_pretty(&rs).unwrap();
        let back: RuntimeState = toml::from_str(&s).unwrap();
        assert_eq!(back.langbar_icon_size_marks, Some(true));
    }

    /// 已移到 `[ui.langbar]` 的那几项**不能**再被 state.toml 读回。
    ///
    /// 老机器的 state.toml 里还留着 `langbar_icon_shape` 等键。serde 默认忽略未知字段，
    /// 所以它们只是被静默丢弃——这正是想要的（配置段才是真相源）。本条钉住这个行为：
    /// 若哪天有人"顺手"把字段加回来，两个真相源就复活了，而症状（重启后形状变回旧值）
    /// 要等到下次重启才看得见。
    #[test]
    fn migrated_keys_are_ignored_not_resurrected() {
        let legacy = r#"
last_chinese_mode = true
langbar_icon_shape = "outer_ring"
langbar_icon_colored = false
langbar_icon_size_marks = true
"#;
        let back: RuntimeState = toml::from_str(legacy).expect("旧文件必须仍能解析");
        // 未知键被忽略，且不影响其余字段的读回。
        assert_eq!(back.langbar_icon_size_marks, Some(true));
        assert!(back.last_chinese_mode);
        // 再写出去时不该把它们带回来。
        let out = toml::to_string_pretty(&back).unwrap();
        assert!(
            !out.contains("langbar_icon_shape") && !out.contains("langbar_icon_colored"),
            "已迁移的键又被写回 state.toml:\n{out}"
        );
    }

    /// 三字段 roundtrip。
    #[test]
    fn last_state_roundtrip() {
        let rs = RuntimeState {
            last_chinese_mode: false,
            last_full_width: true,
            last_chinese_punct: false,
            ..Default::default()
        };
        let s = toml::to_string_pretty(&rs).unwrap();
        let back: RuntimeState = toml::from_str(&s).unwrap();
        assert!(!back.last_chinese_mode);
        assert!(back.last_full_width);
        assert!(!back.last_chinese_punct);
    }
}
