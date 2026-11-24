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
    /// 软键盘钉住（焦点怎么变都不自动关闭）。跨开合、跨重启保留，直到用户再点一次图钉。
    ///
    /// 只记偏好，**不记面板开着**：重启后面板不会自己弹出来，所以「忘了钉的面板一直挡在
    /// 屏上」这种担心落不到实处——它挡着时用户刚亲手打开过它，Esc / 关闭按钮照样能收。
    /// 与 [`Self::last_softkeyboard_page`] 同理不受 `remember_last_state` 门控：界面偏好，不是输入态。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub softkeyboard_pinned: bool,
    /// 按应用方案的记忆表：进程名（小写；macOS 为 bundle id）→ 方案 id。
    ///
    /// 只服务 compat.toml 里 `schema = "@remember"` 的应用（C0-7 / C3-3，GH#80）：
    /// 在这类应用里手切方案时记下，焦点再切入时恢复。协调器持内存镜像，经 `StateWriter`
    /// 整表写回（闭包自带完整目标值）。
    ///
    /// 读取时记录的 id 已不在 `schema.available` ⇒ 当没记过（**不主动清理**：方案可能被
    /// 重新启用）。与 `last_*` 三态不同，**不受** `remember_last_state` 门控——用户配
    /// `@remember` 本身就是在要求记住。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub app_schemas: HashMap<String, String>,
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
            softkeyboard_pinned: false,
            app_schemas: HashMap::new(),
        }
    }
}

impl RuntimeState {
    /// 从 `state_dir/state.toml` 加载，文件不存在或解析失败时返回默认值。
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join("state.toml");
        // 一次性迁移：此前 state.toml 直接躺在本机根下（`local_dir()`），归位到 state/
        if !path.exists()
            && let Some(parent) = state_dir.parent()
            && parent.join("state.toml").is_file()
        {
            let _ = std::fs::create_dir_all(state_dir);
            if let Err(e) = std::fs::rename(parent.join("state.toml"), &path) {
                tracing::warn!("迁移 state.toml 到 state/ 失败: {e}");
            }
        }
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
#[path = "runtime_state_tests.rs"]
mod tests;
