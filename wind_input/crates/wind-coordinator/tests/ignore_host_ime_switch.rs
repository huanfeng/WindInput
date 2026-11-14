//! `ignore_host_ime_switch`：宿主自己写 compartment 引起的中英切换，开和关两个方向都忽略
//! （GH#312：SSMS 内部焦点切换把 OPENCLOSE 写成开，英文态被切成中文、第一键被吃）。
//!
//! 拒绝 = 模式不变、回包是当前模式（DLL 的 `_ApplyModeSwitch` 据此把 compartment 拉回）。
//! Ctrl 按住（系统热键 Ctrl+空格）、按键兜底、功能菜单照常放行。
//! 这里走全局开关（按应用覆盖由协调器单测与 wind-config 的叠加测试钉住）。
//! 不依赖词库（`new_headless(.., None)`）。

use wind_bridge::handler::MessageHandler;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::ModeSwitchSource as Src;

fn coord(chinese: bool, ignore: bool) -> std::sync::Arc<Coordinator> {
    let mut c = Config::default();
    c.input.default.chinese_mode = chinese;
    c.input.ignore_host_ime_switch = ignore;
    let coord = Coordinator::new_headless(c, None);
    assert_eq!(coord.is_chinese_mode(), chinese, "前提：起步模式");
    coord
}

/// 两个方向的宿主写入都被拒：模式不变、回包是当前模式。
#[test]
fn host_switch_is_rejected_in_both_directions() {
    for start_chinese in [true, false] {
        for src in [Src::CompartmentOpenClose, Src::CompartmentConversion] {
            let c = coord(start_chinese, true);
            let (status, commit) = c.handle_system_mode_switch(!start_chinese, src, false);
            assert_eq!(
                c.is_chinese_mode(),
                start_chinese,
                "{src:?} 请求切到 {} 应被拒",
                if start_chinese { "英" } else { "中" }
            );
            assert_eq!(
                status
                    .expect("拒绝也要回包，DLL 靠它把 compartment 拉回")
                    .chinese_mode,
                start_chinese,
                "{src:?} 回包须是当前模式"
            );
            assert!(commit.is_empty());
        }
    }
}

/// Ctrl 按住（系统热键）、按键兜底、菜单：不是宿主自作主张，两个方向都照办。
#[test]
fn user_initiated_switch_still_goes_through() {
    for start_chinese in [true, false] {
        for (src, ctrl) in [
            (Src::CompartmentOpenClose, true),
            (Src::CompartmentConversion, true),
            (Src::CtrlSpaceKey, false),
            (Src::Menu, false),
        ] {
            let c = coord(start_chinese, true);
            c.handle_system_mode_switch(!start_chinese, src, ctrl);
            assert_eq!(
                c.is_chinese_mode(),
                !start_chinese,
                "{src:?} ctrl={ctrl} 应照办"
            );
        }
    }
}

/// 出厂关：宿主写入照常采纳（两个方向）。
#[test]
fn factory_default_honours_host_switch() {
    assert!(!Config::default().input.ignore_host_ime_switch);
    for start_chinese in [true, false] {
        let c = coord(start_chinese, false);
        c.handle_system_mode_switch(!start_chinese, Src::CompartmentOpenClose, false);
        assert_eq!(c.is_chinese_mode(), !start_chinese);
    }
}
