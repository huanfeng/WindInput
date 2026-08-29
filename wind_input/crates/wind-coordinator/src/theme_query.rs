//! 主题的**拉取面**：列主题、切主题、取求值后的调色板。
//!
//! # 为什么单开一个面而不是复用推送通道
//!
//! 桌面的主题是**推**给宿主的（`push_theme` → `UiCommand::SetTheme`，载荷是求值完的
//! `RvNode` 视图树），因为桌面候选窗由 wind-ui 渲染，节点树正是它要的东西。移动端的
//! 键盘是 Android 原生自绘的，那棵桌面视图树对它没有意义——它要的是**语义色表**
//! （`bg`/`surface`/`text`/`accent_soft`…），自己决定哪个控件用哪个色。
//!
//! 所以这里给的是拉取式的 [`Coordinator::theme_palette`]，与 `candidate_pull` 同一种
//! 形状：宿主要的时候自己来取，核心不替它决定渲染。
//!
//! # 明暗必须由宿主注入
//!
//! `theme_palette` 收一个 `system_dark` 参数而不是自己探测——
//! [`crate::theme_style::system_prefers_dark`] 在非 Windows/macOS 上**恒 false**，
//! Android 若走桌面那条 `resolve_dark()`，「跟随系统」会静默退化成恒亮色（不报错、
//! 不崩溃，只是那个选项永远不生效）。系统明暗在 Android 上只有 Java 层的
//! `Configuration.uiMode` 知道。

use std::collections::HashMap;

use tracing::warn;
use wind_theme::Rgba;

use crate::Coordinator;
use crate::theme_style::ThemeStyle;

impl Coordinator {
    /// 可选主题：`(id, 显示名)`，顺序与桌面主题菜单一致（按 `[meta] order` 排）。
    ///
    /// `_` 前缀的基底主题（`_base`/`_qingfeng`）不在其中——它们只供 `base` 继承。
    pub fn theme_entries(&self) -> Vec<(String, String)> {
        self.list_themes()
    }

    /// 当前主题 id。
    pub fn active_theme_id(&self) -> String {
        self.theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 按 id 切主题（持久化到用户配置）。返回是否命中。
    ///
    /// 按 id 而不是按下标：下标是桌面菜单的产物，随主题目录增删漂移，
    /// 宿主存下来下次再用会指向另一个主题。
    ///
    /// ⚠ 刻意**不走** [`Coordinator::select_theme`]（桌面那条路）：它会 `push_theme`，
    /// 即加载整棵桌面视图树并下发 `UiCommand::SetTheme`。移动端不消费该指令
    /// （键盘由 Android 原生自绘，按 [`Self::theme_palette`] 拉色），那棵树纯属白造。
    /// 白造的代价不只是慢——见 [`Self::set_theme_style_name`] 上的栈说明。
    pub fn select_theme_by_id(&self, id: &str) -> bool {
        let Some((_, name)) = self.list_themes().into_iter().find(|(tid, _)| tid == id) else {
            warn!("select_theme_by_id: 未知主题 {}", id);
            return false;
        };
        *self.theme_name.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
        self.persist_theme(id);
        self.show_tip(&format!("主题: {name}"));
        true
    }

    /// 当前明暗设置的配置值（`"system"` / `"light"` / `"dark"`）。
    pub fn theme_style_name(&self) -> &'static str {
        self.theme_style
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_config()
    }

    /// 设置明暗（取值同 [`Self::theme_style_name`]；未知值按跟随系统）。
    ///
    /// ⚠ 同 [`Self::select_theme_by_id`]，刻意不走桌面的 [`Coordinator::set_theme_style`]
    /// ——它尾部会 `push_theme`。除了白造一棵移动端不消费的视图树，那条链在**小栈线程上
    /// 会直接爆栈**：`wind_theme::Resolved` 单个 13,440 字节，debug 构建下沿
    /// `resolve → load_resolved_dirs → load_theme_with_fallback → push_theme` 逐层按值
    /// 返回要复制多次，实测 2 MiB 栈的测试线程稳定 `STATUS_STACK_OVERFLOW`。
    ///
    /// Android 的后台线程默认栈约 1 MiB（核心构造正跑在后台线程上），比测试线程更窄，
    /// 所以这不是「测试环境的怪癖」而是真实崩溃路径。移动端不碰 `push_theme` 即绕开。
    pub fn set_theme_style_name(&self, style: &str) {
        let style = ThemeStyle::from_config(style);
        *self.theme_style.lock().unwrap_or_else(|e| e.into_inner()) = style;
        let _ = wind_config::Config::set_user_string(&["ui", "theme", "style"], style.as_config());
        self.show_tip(style.label());
    }

    /// 本次该用暗色吗——`system_dark` 由宿主给出（见模块文档）。
    pub fn theme_dark_with(&self, system_dark: bool) -> bool {
        self.theme_style
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resolve_dark_with(system_dark)
    }

    /// 求值后的语义色表：`(语义名, ARGB)`，按语义名排序。
    ///
    /// 已完成 `${var}` 递归展开与 `{light, dark}` 变体选择，宿主拿到的就是终值，
    /// **不需要也不该**自己存两套色表——切明暗时重新调本方法即可。
    ///
    /// 排序是稳定性要求：`HashMap` 的遍历序每次进程都不同，宿主若按序号缓存会错位。
    pub fn theme_palette(&self, system_dark: bool) -> Vec<(String, u32)> {
        let is_dark = self.theme_dark_with(system_dark);
        let dirs = self.theme_search_dirs();
        let name = self.active_theme_id();
        // 与桌面 push 链同一裁决：被定制版 `[themes] hide` 删掉的主题按不存在处理。
        // 这条**不能**指望列表侧过滤兜底——`ui.theme.name` 是存量用户配置里带过来的，
        // 列表里没有它照样能被拉取到。
        let name = Self::theme_id_honoring_hide(&name);

        let merged = match wind_theme::load_merged_dirs(&dirs, name, 0) {
            Ok(v) => v,
            Err(e) => {
                warn!("主题 {} 加载失败，调色板为空: {}", name, e);
                return Vec::new();
            }
        };
        let palette: HashMap<String, Rgba> =
            wind_theme::palette::resolve_palette(merged.get("colors"), is_dark);

        let mut out: Vec<(String, u32)> = palette
            .into_iter()
            .map(|(k, rgba)| (k, rgba_to_argb(rgba)))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// `[R, G, B, A]` → `0xAARRGGBB`。
///
/// 用 ARGB 而不是原样透出 `[u8; 4]`：Android 的 `Color` 与 iOS 的 `UIColor(rgb:)`
/// 都吃这个布局，在核心侧转一次，省得每个平台各写一遍位移拼装、各错一次通道顺序。
fn rgba_to_argb(c: Rgba) -> u32 {
    let [r, g, b, a] = c;
    (u32::from(a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_channel_order() {
        // 不透明纯红：R=FF G=00 B=00 A=FF → 0xFFFF0000
        assert_eq!(rgba_to_argb([0xFF, 0x00, 0x00, 0xFF]), 0xFFFF_0000);
        // 半透明纯蓝：A=80 → 0x800000FF
        assert_eq!(rgba_to_argb([0x00, 0x00, 0xFF, 0x80]), 0x8000_00FF);
    }
}
