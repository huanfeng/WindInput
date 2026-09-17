//! `KeyPayload.toggles` 位的**跨语言对账**：C++ 写值、Rust 读值，漂了就静默失效。
//!
//! # 为什么单给这几个位配对账，而不是所有协议常量
//!
//! 多数协议常量漂移会**立刻可见**：命令码对不上 ⇒ 消息根本处理不了，日志里就是一条
//! "unknown command"。而 `toggles` 是位域——DLL 置 0x08、core 读 0x10，两端都不报错，
//! 表现只是「智能符号在英文半角下偶尔还是会删字」，和修复前一模一样，零日志可查。
//! 这正是本仓反复吃亏的形态（同一个事实写在两处、没有任何编译期约束把它们钉在一起）。
//!
//! 找不到 C++ 头文件（只检出 Rust 子树、或在别的工作区跑）时整条跳过，不制造假红。

use std::path::PathBuf;

fn header() -> Option<String> {
    // 三级：crates/wind-ipc → crates → wind_input → 仓库根。
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../wind_tsf/include/BinaryProtocol.h");
    std::fs::read_to_string(p).ok()
}

/// 从 `constexpr uint8_t <name> = 0xNN;` 里抠出值。写成容错解析（允许空格随意），
/// 但**抠不到就返回 None 让调用方报红**——头文件改了结构却没人发现，与漂移同样危险。
fn constexpr_u8(src: &str, name: &str) -> Option<u8> {
    let line = src
        .lines()
        .find(|l| l.contains(name) && l.contains("constexpr") && l.contains('='))?;
    let rhs = line.split('=').nth(1)?;
    let rhs = rhs.trim();
    // 必须是 `0x` 字面量：`trim_start_matches` 对十进制 `= 16;` 会静默按 16 进制读成 0x16，
    // 于是「两端一致」的结论建立在一次误解析上——对账测试自己假绿，比没有还糟。
    if !rhs.starts_with("0x") {
        return None;
    }
    let hex = rhs.trim_start_matches("0x");
    let hex = hex.split(|c: char| !c.is_ascii_hexdigit()).next()?;
    u8::from_str_radix(hex, 16).ok()
}

#[test]
fn toggle_bits_agree_with_cpp_header() {
    let Some(src) = header() else {
        eprintln!("跳过 toggles 位对账：找不到 wind_tsf/include/BinaryProtocol.h");
        return;
    };
    let cases: &[(&str, u8)] = &[
        ("TOGGLE_CAPSLOCK", wind_ipc::protocol::TOGGLE_CAPSLOCK),
        (
            "TOGGLE_PASSTHROUGH_KEY",
            wind_ipc::protocol::TOGGLE_PASSTHROUGH_KEY,
        ),
    ];
    for (name, rust_val) in cases {
        let cpp_val = constexpr_u8(&src, name)
            .unwrap_or_else(|| panic!("BinaryProtocol.h 里找不到 `constexpr uint8_t {name}`"));
        assert_eq!(
            cpp_val, *rust_val,
            "{name} 两端不一致：C++ = {cpp_val:#04x}，Rust = {rust_val:#04x}。\
             位域漂移不会报错，只会让依赖它的功能静默失效"
        );
    }
}

/// 透传位不得与任何锁定态位重叠——它是搭空闲位的车，撞上就等于「CapsLock 一开就解除
/// 智能符号武装」这类无从归因的串扰。
#[test]
fn passthrough_bit_does_not_collide_with_lock_bits() {
    let Some(src) = header() else {
        eprintln!("跳过 toggles 位互斥检查：找不到 BinaryProtocol.h");
        return;
    };
    let pass = constexpr_u8(&src, "TOGGLE_PASSTHROUGH_KEY").expect("缺 TOGGLE_PASSTHROUGH_KEY");
    for lock in ["TOGGLE_CAPSLOCK", "TOGGLE_NUMLOCK", "TOGGLE_SCROLLLOCK"] {
        let v = constexpr_u8(&src, lock).unwrap_or_else(|| panic!("缺 {lock}"));
        assert_eq!(pass & v, 0, "TOGGLE_PASSTHROUGH_KEY 与 {lock} 位重叠");
    }
}
