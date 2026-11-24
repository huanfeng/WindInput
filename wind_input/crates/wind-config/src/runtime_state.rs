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
    /// 只有纯调试项留在 state.toml；影响用户可见呈现的参数都在 `[ui.langbar]` 配置段，
    /// 避免两个真相源。`None` = 用代码默认，唯一出处是 `wind_ui::langbar_icon`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub langbar_icon_size_marks: Option<bool>,
    /// 工具栏锚点：窗口**右下角**的屏幕坐标，按显示器 key
    /// （`"workRight,workBottom@scalePct"`）独立记录。
    ///
    /// 锚右下角：横竖切换与条目增减会改尺寸，锚左上角时贴右下（出厂默认位置）的工具栏
    /// 会顶出工作区再被拉回。代价是贴左上的工具栏改尺寸时朝左上顶出，越界仍由
    /// `clamp_to_work_area` 兜住。
    ///
    /// 坐标是物理像素；key 带缩放，是因为只改 DPI 缩放时 `rcWork` 物理边界不变，记录仍会
    /// 命中而落点已不对。缩放一变即失配、落回默认位置。
    #[serde(default)]
    pub toolbar_anchors: HashMap<String, (i32, i32)>,
    /// 软键盘锚点：面板**右下角**的屏幕坐标，按显示器 key 独立记录。
    ///
    /// 与 [`Self::toolbar_anchors`] 同构；软键盘的尺寸变化来自切面。
    #[serde(default)]
    pub softkeyboard_anchors: HashMap<String, (i32, i32)>,
    /// 软键盘上次停在哪一面（面 **id**）。空 = 没记录过，开在第一面。
    ///
    /// 存 id 不存下标：跨重启期间面表可能增删，下标会指到别的面；按 id 找不到就当没记录。
    ///
    /// 不受 `input.default.remember_last_state` 门控：那个开关管输入态，这是界面便利。
    #[serde(default)]
    pub last_softkeyboard_page: String,
    /// 软键盘钉住（焦点怎么变都不自动关闭）。跨开合、跨重启保留，直到用户再点一次图钉。
    ///
    /// 只记偏好，不记面板开着：重启后面板不会自己弹出。同样不受 `remember_last_state` 门控。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub softkeyboard_pinned: bool,
    /// 按应用方案的记忆表：进程名（小写；macOS 为 bundle id）→ 方案 id。
    ///
    /// 只服务 compat.toml 里 `schema = "@remember"` 的应用：手切方案时记下，焦点再切入时恢复。
    /// 协调器持内存镜像，经 `StateWriter` 整表写回。
    ///
    /// 记录的 id 已不在 `schema.available` 时当没记过，但不主动清理（方案可能被重新启用）。
    /// 不受 `remember_last_state` 门控：配 `@remember` 本身就是要求记住。
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
