//! 符号通配键作**首码**（论坛 t302）。
//!
//! 现场：通配键配成反引号，空缓冲按它先被出厂的「临拼触发键」（反引号）抢走，再按一次退出
//! 临拼才轮到通配——用户看到的是「要按两次」。通配出厂关闭，开了并选了这个键，它就是码元，
//! 码元理应能出现在第一位。
//!
//! 仲裁口径与 `leading_chars` 的跨层仲裁相同：全局层绑定让位，方案级 `[key_actions]` 显式
//! 绑定仍优先（同层冲突，显式优先）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_OEM_3: u32 = 0xC0; // 反引号

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

fn key(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 五笔 86，`wildcard` 按参数开关、通配键反引号；全局反引号仍是出厂的临拼触发键。
/// `schema_key_actions` 非空时写进方案级 override。
fn coord(tag: &str, wildcard: bool, schema_key_actions: &str) -> (Arc<Coordinator>, PathBuf) {
    coord_key(tag, wildcard, "`", schema_key_actions)
}

fn coord_key(
    tag: &str,
    wildcard: bool,
    wildcard_key: &str,
    schema_key_actions: &str,
) -> (Arc<Coordinator>, PathBuf) {
    let ov = std::env::temp_dir().join(format!("wind_wc_lead_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    if !schema_key_actions.is_empty() {
        std::fs::write(
            ov.join("wubi86.toml"),
            format!("[key_actions]\n{schema_key_actions}\n"),
        )
        .unwrap();
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.wildcard = wildcard;
    cfg.schema.codetable.wildcard_key = wildcard_key.into();
    let c = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));
    (c, ov)
}

#[test]
fn backtick_wildcard_leads_over_global_temp_pinyin_trigger() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    // ★ 前提对照：通配关闭时反引号确实是临拼触发键——否则下面的「没进临拼」不说明任何事。
    let (off, ov0) = coord("off", false, "");
    off.handle_key_event(&key(VK_OEM_3));
    assert_eq!(off.debug_active_mode(), Some("temp_pinyin"));

    let (c, ov) = coord("on", true, "");
    c.handle_key_event(&key(VK_OEM_3));
    assert_eq!(
        c.debug_active_mode(),
        None,
        "首位反引号应作通配码元，不进临拼"
    );
    assert_eq!(c.debug_input_buffer(), "`");
    c.handle_key_event(&key(u32::from(b'G')));
    assert!(
        !c.debug_page_texts().is_empty(),
        "`g 应按通配查出候选（?g 两码）"
    );
    let _ = std::fs::remove_dir_all(&ov0);
    let _ = std::fs::remove_dir_all(&ov);
}

/// 方案级 `[key_actions]` 显式绑了反引号 ⇒ 同层冲突，显式绑定优先，首位仍让位。
#[test]
fn schema_level_binding_still_wins_at_lead() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (c, ov) = coord("schema", true, "backtick = \"temp_pinyin\"");
    c.handle_key_event(&key(VK_OEM_3));
    assert_eq!(c.debug_active_mode(), Some("temp_pinyin"));
    let _ = std::fs::remove_dir_all(&ov);
}

/// 首位能作通配，前提是按键**到得了 core**：中文标点表里产物就是原样 ASCII 的符号（如 `/`）
/// 会被推进「空闲时透传」集合，DLL 在 `OnTestKeyDown` 直接放给宿主。通配键须从中扣掉。
///
/// `should_handle_key` 读的正是推给 DLL 的同一份集合，断言它即断言 DLL 侧的吃键判定。
#[test]
fn symbol_wildcard_key_is_not_passed_through_when_idle() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    const VK_OEM_2: u32 = 0xBF; // `/`
    // ★ 前提对照：通配关时 `/` 确实在透传集里。
    let (off, ov0) = coord_key("pt_off", false, "/", "");
    assert!(!off.should_handle_key(&wind_host::KeyProbe::new(VK_OEM_2)));

    let (on, ov) = coord_key("pt_on", true, "/", "");
    assert!(
        on.should_handle_key(&wind_host::KeyProbe::new(VK_OEM_2)),
        "`/` 是通配键时空闲也必须吃，否则首位通配到不了 core"
    );
    let _ = std::fs::remove_dir_all(&ov0);
    let _ = std::fs::remove_dir_all(&ov);
}

/// 满码后按符号通配键：通配按字面（`wildcard_past_full`），须与关闭通配时**逐键相同**——
/// 出厂下是「顶字 + 进临拼」。曾因首位仲裁不看缓冲，组码中那条路也让全局临拼让位，
/// 引导键与通配两头落空，键被白吞（审查实测）。
#[test]
fn full_code_then_wildcard_key_matches_wildcard_off() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let run = |tag: &str, on: bool| {
        let (c, ov) = coord(tag, on, "");
        for ch in "aaaa".chars() {
            c.handle_key_event(&key(u32::from(ch.to_ascii_uppercase())));
        }
        let act = format!("{:?}", c.handle_key_event(&key(VK_OEM_3)));
        let out = (act, c.debug_active_mode(), c.debug_input_buffer());
        let _ = std::fs::remove_dir_all(&ov);
        out
    };
    let off = run("full_off", false);
    assert_eq!(
        off.1,
        Some("temp_pinyin"),
        "前提：关闭时满码 + 反引号进临拼"
    );
    assert_eq!(run("full_on", true), off);
}

/// 五笔拼音混输：符号通配键（`?`、`/`）首位也作通配（符号不是拼音字母，夺不走拼音输入）；
/// 字母通配键（`z`）首位仍按拼音字面。
#[test]
fn mixed_symbol_wildcard_leads_but_letter_stays_literal() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let run = |wk: &str, vk: u32, mods: u32| {
        let ov = std::env::temp_dir().join(format!("wind_wc_mixed_{}_{}", vk, std::process::id()));
        let _ = std::fs::remove_dir_all(&ov);
        std::fs::create_dir_all(&ov).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
        cfg.schema.active = "wubi86_pinyin".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.wildcard = true;
        cfg.schema.codetable.wildcard_key = wk.into();
        let c = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));
        let mut k = key(vk);
        k.modifiers = mods;
        c.handle_key_event(&k);
        let buf = c.debug_input_buffer();
        c.handle_key_event(&key(u32::from(b'G')));
        let out = (buf, c.debug_page_texts());
        let _ = std::fs::remove_dir_all(&ov);
        out
    };
    // `?` = Shift+/
    let (buf, cands) = run("?", 0xBF, 1);
    assert_eq!(buf, "?", "混输首位 ? 应作通配码元，不是直接上屏标点");
    assert!(!cands.is_empty(), "?g 应按通配查出候选");
    let (buf, cands) = run("/", 0xBF, 0);
    assert_eq!(buf, "/");
    assert!(!cands.is_empty());
    // 字母 z：仍按拼音，`zg` 不成通配。
    let (buf, cands) = run("z", 0x5A, 0);
    assert_eq!(buf, "z");
    assert!(cands.is_empty(), "z 首位按拼音字面：{cands:?}");
}
