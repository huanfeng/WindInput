//! 主题的拉取面：列主题、切主题、取求值后的调色板。
//!
//! 桌面主题是推给宿主的（`push_theme` → `UiCommand::SetTheme`，载荷是 `RvNode` 视图树）；移动端
//! 键盘原生自绘，要的是语义色表（`bg`/`surface`/`text`/`accent_soft`…）。故这里给拉取式的
//! [`Coordinator::theme_palette`]，与 `candidate_pull` 同形。
//!
//! 明暗必须由宿主注入（`system_dark` 参数）：[`crate::theme_style::system_prefers_dark`] 在
//! 非 Windows/macOS 上恒 false，Android 的系统明暗只有 Java 层 `Configuration.uiMode` 知道。

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

    /// 按 id 切主题（持久化到用户配置）。返回是否命中。按 id 而不是按下标：下标随主题目录
    /// 增删漂移。
    ///
    /// 刻意不走 [`Coordinator::select_theme`]：它会 `push_theme` 造整棵桌面视图树，移动端不消费，
    /// 且有爆栈风险（见 [`Self::set_theme_style_name`]）。
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
    /// 刻意不走桌面的 [`Coordinator::set_theme_style`]：它尾部会 `push_theme`，除了白造视图树，
    /// 那条链逐层按值返回大结构体 `wind_theme::Resolved`，在小栈线程（Android 后台线程约 1 MiB）
    /// 上会爆栈。
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
    /// 不需要也不该自己存两套色表——切明暗时重新调本方法即可。
    ///
    /// 排序是稳定性要求：`HashMap` 的遍历序每次进程都不同，宿主若按序号缓存会错位。
    pub fn theme_palette(&self, system_dark: bool) -> Vec<(String, u32)> {
        let is_dark = self.theme_dark_with(system_dark);
        let dirs = self.theme_search_dirs();
        let name = self.active_theme_id();
        // 与桌面 push 链同一裁决：被定制版 `[themes] hide` 删掉的主题按不存在处理。
        // 这条不能指望列表侧过滤兜底——`ui.theme.name` 是存量用户配置里带过来的，
        // 列表里没有它照样能被拉取到。
        let name = Self::theme_id_honoring_hide(&name);

        let merged = match wind_theme::load_merged_dirs(&dirs, name, 0) {
            Ok(v) => v,
            Err(e) => {
                warn!("主题 {} 加载失败，调色板为空: {}", name, e);
                return Vec::new();
            }
        };
        let mut palette: HashMap<String, Rgba> =
            wind_theme::palette::resolve_palette(merged.get("colors"), is_dark);
        // 与桌面 `wind_theme::resolve` 同一口径：补主题没写的标准色契约名。
        wind_theme::contract::fill_missing(merged.get("colors"), &mut palette, is_dark);

        let mut out: Vec<(String, u32)> = palette
            .into_iter()
            .map(|(k, rgba)| (k, rgba_to_argb(rgba)))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// 主题对「外观可覆盖键」给出的值——设置端「跟随主题（值）」的数据源（R8.1）。
///
/// 只抽几个字段就把 `Resolved` 丢掉：它约 13 KB，按值传递在 debug 构建下曾把栈撑爆
/// （见 [`Coordinator::set_theme_style_name`] 上的说明），而 RPC 的 ctrl 线程没有
/// 另设栈大小。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeFollowValues {
    pub theme_id: String,
    pub font_size: i32,
    pub font_family: Option<String>,
    pub font_weight: i32,
    pub pager_bar_display: &'static str,
    pub page_number_display: &'static str,
    pub langbar_text: [Option<String>; 4],
}

/// 与 `wind_ui::candidate_window::pager_visible` 的「跟随主题」分支同一裁决。
fn follow_values_of(theme_id: &str, r: &wind_theme::resolve::Resolved) -> ThemeFollowValues {
    let b = &r.behavior;
    let hex = |c: Option<Rgba>| c.map(|[r, g, b, _]| format!("#{r:02X}{g:02X}{b:02X}"));
    let lt = &r.langbar_text;
    ThemeFollowValues {
        theme_id: theme_id.to_string(),
        font_size: b.font_size,
        font_family: r.views.text.font_family.clone(),
        font_weight: r.views.text.font_weight,
        pager_bar_display: if b.hide_pager {
            "hide"
        } else if b.always_show_pager {
            "always"
        } else {
            "auto"
        },
        page_number_display: if b.show_page_number { "show" } else { "hide" },
        langbar_text: [
            hex(lt.cn_light),
            hex(lt.cn_dark),
            hex(lt.en_light),
            hex(lt.en_dark),
        ],
    }
}

impl Coordinator {
    /// 当前生效主题的跟随值。与桌面推送链同一裁决：先过定制版 hide，再回落
    /// `FALLBACK_THEME`（`load_theme_with_fallback`）；两级都失败返回 None。
    pub fn theme_follow_values(&self) -> Option<ThemeFollowValues> {
        let dirs = self.theme_search_dirs();
        let name = <Self as crate::web_host::WebDataHost>::current_theme_name(self);
        let is_dark = <Self as crate::web_host::WebDataHost>::current_theme_is_dark(self);
        let (id, resolved) = Self::load_theme_with_fallback(
            |n| wind_theme::load_resolved_dirs(&dirs, n, is_dark).map(Box::new),
            &name,
        )?;
        Some(follow_values_of(&id, &resolved))
    }
}

/// 主题强调色的亮 / 暗两档——设置窗「主题色跟随输入法」的取色源。
///
/// 两档都给：设置窗的明暗是它自己的（`wind_setting.toml` 的 `ui.dark`），与输入法的
/// `ui.theme.style` 无关，按哪档上色只有设置端知道。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeAccent {
    /// 实际取色的主题 id：请求的主题被 hide 或加载失败时是降级后的 `default`。
    pub theme_id: String,
    pub light: ThemeAccentPair,
    pub dark: ThemeAccentPair,
}

/// 一档的强调色。`#RRGGBB`，带透明度时 `#RRGGBBAA`；`None` = 主题写了却解析不出。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeAccentPair {
    /// 填充用强调色（候选序号、强调条、菜单高亮底）。
    pub accent: Option<String>,
    /// 当文字用的强调色，作者按该档底色调过可读性——标准色契约名，所有主题都有。
    pub accent_text: Option<String>,
}

fn accent_pair_of(colors: Option<&toml::Value>, is_dark: bool) -> ThemeAccentPair {
    let mut palette = wind_theme::palette::resolve_palette(colors, is_dark);
    // 不继承 _base 的第三方主题也要有 accent / accent_text：与渲染同一口径补契约名。
    wind_theme::contract::fill_missing(colors, &mut palette, is_dark);
    let hex = |n: &str| palette.get(n).map(|&c| rgba_hex(c));
    ThemeAccentPair {
        accent: hex("accent"),
        accent_text: hex("accent_text"),
    }
}

fn rgba_hex([r, g, b, a]: Rgba) -> String {
    if a == 0xFF {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

impl Coordinator {
    /// 主题的强调色两档；`name` 缺省取当前主题。降级规则同 [`Self::theme_follow_values`]，
    /// 两级都失败返回 None。
    ///
    /// 只合并 colors 不求整棵 `Resolved`：省 13 KB 的按值搬运（栈风险见上），也省两次视图树求值。
    pub fn theme_accent(&self, name: Option<&str>) -> Option<ThemeAccent> {
        let dirs = self.theme_search_dirs();
        let name = match name {
            Some(n) => n.to_string(),
            None => <Self as crate::web_host::WebDataHost>::current_theme_name(self),
        };
        let (id, merged) =
            Self::load_theme_with_fallback(|n| wind_theme::load_merged_dirs(&dirs, n, 0), &name)?;
        let colors = merged.get("colors");
        Some(ThemeAccent {
            theme_id: id,
            light: accent_pair_of(colors, false),
            dark: accent_pair_of(colors, true),
        })
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

#[cfg(test)]
mod follow_values_tests {
    use super::*;
    use wind_theme::resolve::{Resolved, ResolvedBehavior};

    #[test]
    fn pager_display_maps_theme_behavior() {
        let mut r = Resolved {
            behavior: ResolvedBehavior {
                hide_pager: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(follow_values_of("default", &r).pager_bar_display, "hide");
        r.behavior = ResolvedBehavior {
            always_show_pager: true,
            ..Default::default()
        };
        assert_eq!(follow_values_of("default", &r).pager_bar_display, "always");
        r.behavior = ResolvedBehavior::default();
        assert_eq!(follow_values_of("default", &r).pager_bar_display, "auto");
    }

    #[test]
    fn page_number_and_font_fields_copied() {
        let mut r = Resolved::default();
        r.behavior.show_page_number = false;
        r.behavior.font_size = 20;
        r.views.text.font_weight = 500;
        let v = follow_values_of("x", &r);
        assert_eq!(v.page_number_display, "hide");
        assert_eq!(v.font_size, 20);
        assert_eq!(v.font_weight, 500);
        assert_eq!(v.font_family, None);
    }

    #[test]
    fn langbar_text_formats_rgb_hex_and_keeps_none() {
        let mut r = Resolved::default();
        r.langbar_text.cn_light = Some([0x11, 0x22, 0x33, 0xFF]);
        let v = follow_values_of("x", &r);
        assert_eq!(v.langbar_text[0].as_deref(), Some("#112233"));
        assert_eq!(v.langbar_text[1], None);
    }
}

#[cfg(test)]
mod accent_tests {
    use super::*;

    fn colors(src: &str) -> toml::Value {
        toml::from_str::<toml::Value>(src).unwrap()["colors"].clone()
    }

    #[test]
    fn picks_variant_per_band_and_follows_refs() {
        let c = colors(
            r##"[colors]
primary = "#10b981"
accent = "${primary}"
accent_text = { light = "#059669", dark = "#34d399" }"##,
        );
        let light = accent_pair_of(Some(&c), false);
        let dark = accent_pair_of(Some(&c), true);
        assert_eq!(light.accent.as_deref(), Some("#10B981"));
        assert_eq!(light.accent_text.as_deref(), Some("#059669"));
        assert_eq!(dark.accent_text.as_deref(), Some("#34D399"));
    }

    #[test]
    fn contract_fills_accent_text_when_theme_omits_it() {
        // 不写 accent_text 的主题：契约兜底，不能是 None（设置端会误判成主题坏了）。
        let c = colors("[colors]\naccent = \"#123456\"");
        assert!(accent_pair_of(Some(&c), false).accent_text.is_some());
    }

    #[test]
    fn hex_keeps_alpha_only_when_translucent() {
        assert_eq!(rgba_hex([0x12, 0x34, 0x56, 0xFF]), "#123456");
        assert_eq!(rgba_hex([0x12, 0x34, 0x56, 0x80]), "#12345680");
    }
}
