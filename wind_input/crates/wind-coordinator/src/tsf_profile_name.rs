//! TSF 语言配置文件显示名（HKLM）的读写——`system.dota2_compat` 的落地动作。
//!
//! **为什么要改这个名字**：Dota 2（起源2 引擎）的 `imemanager.dll` 内置一张硬编码的
//! 输入法白名单，按注册表里该输入法的 TSF Profile Description 做**全等**比对
//! （`V_stricmp_fast`/`V_wcsicmp`，非子串）。命中的走「取候选 → 游戏自己画」；
//! 未命中的在 `WM_IME_NOTIFY` 第一道闸门就被打发到 `DefWindowProc`，于是候选永远
//! 取不到，还会多出一个系统默认 IME 小窗。这是游戏那侧写死的，数据侧绕不过去——
//! 完整证据与九条已实测证伪的方向见 `docs/design/game-compat-tsf-uielement.md` §1.1。
//!
//! ⚠️ 写的是 **HKLM**，必须以管理员权限运行；非提权进程会得到 `ERROR_ACCESS_DENIED`（5）。
//! 调用方（设置程序）负责用 `runas` 提权拉起 `wind_input system dota2-compat on|off`。
//!
//! ⚠️ 生效时机：宿主是在**自己启动时**读这个名字的，改完必须重启游戏。

use std::io;

use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE};

use crate::direct_switch::tip_guid_strings;

/// Dota 2 白名单里选定的别名。
///
/// ⛔ **必须与游戏内表逐字一致**（半角括号、括号前后各一个半角空格、连字符前后各一个）。
/// 差一个空格就不命中，而且没有任何报错——表现为「开了开关也还是没候选」。
///
/// 表里可选的条目都是别家输入法的名字；取「郑码」是因为它是**码表方案名**而不是
/// 任何在世产品的品牌名（其余候选要么是竞品，要么是已停更产品的产品名）。
pub const DOTA2_ALIAS: &str = "中文 (简体) - 郑码";

/// TSF 语言配置文件在注册表里的语言子键。与 `wind_tsf` 的 `TEXTSERVICE_LANGID`（0x0804）一致。
const LANGID_SUBKEY: &str = "0x00000804";

/// 本输入法的真实显示名。
///
/// ⚠️ **跨语言重复，无编译期约束**：必须与 `wind_tsf/include/Globals.h` 的
/// `TEXTSERVICE_NAME` 逐字一致。改那边要同步改这里，否则关闭开关后名字会被还原成
/// 一个错的字符串（且只在用户关开关时才暴露）。与本文件复用的 `tip_guid_strings()`
/// 同一性质的约定——那边也是照抄 `Globals.cpp` 的 GUID。
fn real_name() -> &'static str {
    if wind_config::variant::is_dev() {
        "清风输入法 (开发版)"
    } else {
        "清风输入法"
    }
}

/// 本输入法的 LanguageProfile 键路径（HKLM 下）。
fn profile_key_path() -> String {
    let (clsid, profile, _) = tip_guid_strings();
    format!(r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\{LANGID_SUBKEY}\{profile}")
}

/// 读当前登记的显示名。键不存在（未注册 TSF 组件）时回 `Ok(None)`。
pub fn current_description() -> io::Result<Option<String>> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    match hklm.open_subkey_with_flags(profile_key_path(), KEY_READ) {
        Ok(key) => match key.get_value::<String, _>("Description") {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 把显示名切到别名（`enabled=true`）或还原为真实名（`enabled=false`）。
///
/// 幂等：已经是目标值时直接回 `Ok(false)`（未改动），便于调用方少弹一次 UAC 之外的噪音。
/// 返回 `Ok(true)` 表示确实写了。
pub fn set_dota2_compat(enabled: bool) -> io::Result<bool> {
    let target = if enabled { DOTA2_ALIAS } else { real_name() };
    let path = profile_key_path();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    // 只开已存在的键，**不创建**：键不在说明 TSF 组件根本没注册，此时凭空造一个
    // 半截的 LanguageProfile 只会让系统多出一个点不开的输入法条目。
    let key = hklm.open_subkey_with_flags(&path, KEY_READ | KEY_SET_VALUE)?;
    if let Ok(cur) = key.get_value::<String, _>("Description")
        && cur == target
    {
        return Ok(false);
    }
    key.set_value("Description", &target)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_matches_the_hardcoded_table_byte_for_byte() {
        // 从 Dota 2 的 imemanager.dll 里原样提取的 UTF-8 字节（2026-09-06）。
        // 这条断言的作用不是「测代码」，而是把那串字节钉在仓里：别名一旦被人
        // 顺手「整理」成全角括号或改了空格，开关就会静默失效，而现象只在游戏里看得到。
        const FROM_BINARY: &[u8] = &[
            0xE4, 0xB8, 0xAD, 0xE6, 0x96, 0x87, 0x20, 0x28, 0xE7, 0xAE, 0x80, 0xE4, 0xBD, 0x93,
            0x29, 0x20, 0x2D, 0x20, 0xE9, 0x83, 0x91, 0xE7, 0xA0, 0x81,
        ];
        assert_eq!(
            DOTA2_ALIAS.as_bytes(),
            FROM_BINARY,
            "别名与 Dota 2 白名单里的字节不一致，开关会静默失效"
        );
    }

    #[test]
    fn profile_path_targets_the_language_profile_key() {
        let p = profile_key_path();
        assert!(p.starts_with(r"SOFTWARE\Microsoft\CTF\TIP\{"), "{p}");
        assert!(
            p.ends_with(r"\LanguageProfile\0x00000804\{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}")
                || p.ends_with(
                    r"\LanguageProfile\0x00000804\{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}"
                ),
            "profile 子键必须是本变体的 guidProfile，不是 CLSID: {p}"
        );
    }
}
