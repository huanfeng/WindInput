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
    /// 工具栏锚点：窗口**右下角**的屏幕坐标，按显示器 key
    /// （`"workRight,workBottom@scalePct"`）独立记录。
    ///
    /// # 为什么锚右下角而不是左上角
    ///
    /// 工具栏的尺寸会在**位置不变的前提下**改变：横/纵排切换是宽高对调
    /// （`bar_layout` 转置），显示哪些格（`ui.toolbar.items`）也直接改条长。锚左上角时
    /// 这些变化全部朝右下方向长出去——贴着屏幕右下角摆放的工具栏（**出厂默认位置**）
    /// 一换向就顶出工作区，再被 `clamp_to_work_area` 拉回来，于是"横竖切一次挪一次"。
    /// 锚右下角则让这些尺寸变化朝屏幕内侧展开，位置在视觉上纹丝不动。
    ///
    /// ⚠️ 换来的代价是对称的：贴**左上角**摆放的工具栏改尺寸时会朝左上顶出去。这是
    /// 有意接受的取舍——工具栏默认落在右下角（`corner_in_work_area`），贴右下是主流
    /// 用法；越界仍由 `clamp_to_work_area` 兜住，不会丢失。
    ///
    /// # 为什么 key 里带缩放
    ///
    /// 坐标存的是**物理像素**（与 `clamp_to_work_area`、`SetToolbarAnchor` 全链路同口径）。
    /// 同一块屏改了 DPI 缩放而分辨率不变时，`rcWork` 的物理像素边界**不变** ⇒ 只用
    /// `"right,bottom"` 做 key 的话记录仍然命中，可窗口尺寸已按新 scale 变了，锚点算出的
    /// 落点不再是用户当初摆的地方。把 scale 并进 key，缩放一变即失配、落回默认位置——
    /// 与"改分辨率则 key 变"同一套语义，也就不必在坐标里另引一套 dp 单位（那会让这
    /// 一族窗口里只有它一个不用物理像素）。
    #[serde(default)]
    pub toolbar_anchors: HashMap<String, (i32, i32)>,
    /// 软键盘锚点：面板**右下角**的屏幕坐标，按显示器 key 独立记录。
    ///
    /// 与 [`Self::toolbar_anchors`] 完全同构（同一套 key、同一个右下角口径、同样的
    /// 缩放失配语义）。软键盘吃到的尺寸变化来自**切面**：各面键数不同，面板宽高随之变，
    /// 锚左上角时切面会让面板朝右下长出去。
    #[serde(default)]
    pub softkeyboard_anchors: HashMap<String, (i32, i32)>,
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
    /// 界面便利，与 [`Self::toolbar_anchors`] 同类：没有人会想要「每次都跳回第一面」。
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
            toolbar_anchors: HashMap::new(),
            softkeyboard_anchors: HashMap::new(),
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

    /// 老的 `toolbar_positions`（**左上角**坐标）绝不能被当成新的右下角锚点读回。
    ///
    /// 这是本次语义反转唯一的危险面：两个字段的类型完全相同
    /// （`HashMap<String,(i32,i32)>`），把老键改名读进来编译得过、测试也不会红，
    /// 但每一台老机器的工具栏都会在升级后跳到「右下角落在原左上角处」——横条约 132px
    /// 的偏移，且用户无从得知为什么。故**必须靠改字段名让老数据失配**，
    /// 落回默认位置（右下角）比错位可解释得多。
    ///
    /// 同理老 key 不带 `@scalePct` 后缀，即便有人把字段名改回去也匹配不上——两道
    /// 保险各自独立。若哪天有人为"平滑升级"补一段迁移，这条会红，那正是要挡的改动。
    #[test]
    fn legacy_toolbar_positions_must_not_be_read_as_anchors() {
        let legacy = r#"
last_chinese_mode = true

[toolbar_positions]
"1920,1040" = [1776, 998]
"#;
        let back: RuntimeState = toml::from_str(legacy).expect("旧文件必须仍能解析");
        assert!(
            back.toolbar_anchors.is_empty(),
            "老的左上角坐标被当成右下角锚点读回了: {:?}",
            back.toolbar_anchors
        );
        assert!(back.last_chinese_mode, "同文件其余字段仍须正常读回");
        // 再写出去时不得把老键带回来（否则两个真相源并存）。
        let out = toml::to_string_pretty(&back).unwrap();
        assert!(
            !out.contains("toolbar_positions"),
            "已改名的键又被写回 state.toml:\n{out}"
        );
    }

    /// 两族锚点 roundtrip，且 key 的 `@scalePct` 后缀能原样存活。
    ///
    /// 后半条不是多余的：key 里带 `@` 与逗号，TOML 表名必须被正确引号包裹，
    /// 写出来再读回去必须还是同一个 key——否则重启后一律失配，位置记忆整体失效，
    /// 而症状（"还是记不住"）与压根没实现完全一样。
    #[test]
    fn anchors_roundtrip_with_scaled_monitor_key() {
        let key = "2560,1392@150".to_string();
        let mut rs = RuntimeState::default();
        rs.toolbar_anchors.insert(key.clone(), (2548, 1380));
        rs.softkeyboard_anchors.insert(key.clone(), (1800, 1380));

        let s = toml::to_string_pretty(&rs).unwrap();
        let back: RuntimeState = toml::from_str(&s).unwrap();
        assert_eq!(back.toolbar_anchors.get(&key), Some(&(2548, 1380)));
        assert_eq!(back.softkeyboard_anchors.get(&key), Some(&(1800, 1380)));
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
