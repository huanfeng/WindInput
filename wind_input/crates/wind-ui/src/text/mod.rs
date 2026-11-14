//! 文本渲染后端
//!
//! 与 Go 版本 `wind_input/internal/ui/text_drawer*.go` 对齐。

pub mod dwrite;

// 按脚本切字体段的纯逻辑（不依赖任何平台 API），已迁入 wind-scene，原路径再导出。
pub use wind_scene::text::script;

// 配置字体名 → 渲染端 family 的判定顺序（纯逻辑，平台能力以闭包注入），同上理由。
pub mod font_resolve;

// macOS：CoreText 真字形后端，提供与 dwrite 同契约的 TextRenderer（dwrite.rs 在
// target_os="macos" 下 re-export 它），让候选窗在 mac 上渲染真实汉字（非 mock 桩）。
#[cfg(target_os = "macos")]
pub mod coretext;

// Linux 外部宿主形态（linux-host）：fontconfig + ttf-parser 的真字形后端，同上由 dwrite.rs
// re-export。不开该 feature 时 Linux 仍是 mock（`mock_text`），开发机测试数值确定。
#[cfg(all(target_os = "linux", feature = "linux-host"))]
pub mod linux;

// 唯一要 wind-config 的那条 script 测试留在这里（wind-scene 不准依赖 wind-config）。
#[cfg(test)]
mod script_tests {
    use super::script::ScriptClass;

    /// 类别名有**两份**：这里的 [`ScriptClass::key`]，和 wind-config 注册表里的
    /// `FONT_SCRIPT_KEYS`（`ui.font.scripts` 的键名值域，经 capability 传给设置端做预填）。
    ///
    /// ★ 断言**顺序也一致**而不只是集合相等：设置端的预填是按这个顺序逐行铺的，
    /// 顺序漂了用户每次打开对话框都看到行序变化，会被当成「配置被改了」。
    ///
    /// ⚠️ 只能在 wind-ui 侧测——wind-config 依赖不到 wind-ui，wind-scene 也不准依赖 wind-config。加类别时两处一起改，
    /// 漏了 wind-config 那份的表现是「core 认这个类，但设置页永远不列出来」。
    #[test]
    fn script_class_keys_match_config_registry() {
        let mine: Vec<&str> = ScriptClass::ALL.iter().map(|c| c.key()).collect();
        assert_eq!(
            mine,
            wind_config::config_schema::FONT_SCRIPT_KEYS,
            "ScriptClass::ALL 与 wind-config 的 FONT_SCRIPT_KEYS 漂移了"
        );
        // 反向：注册表里的每个名字都解析得回来（防「两边各写了同样多但不同的名字」）。
        for k in wind_config::config_schema::FONT_SCRIPT_KEYS {
            assert!(
                ScriptClass::from_key(k).is_some(),
                "注册表键 {k} 解析不出类别"
            );
        }
    }
}
