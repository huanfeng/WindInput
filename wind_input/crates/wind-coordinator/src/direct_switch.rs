//! activate_ime → Windows DirectSwitchHotkeys 注册表同步
//!
//! 与 Go 版本 `wind_input/internal/ui/direct_switch_hotkey_windows.go` 对齐（含加固，见下）。
//!
//! 机制：`HKCU\Software\Microsoft\CTF\DirectSwitchHotkeys` 是 Windows TSF「直接切换热键」表，
//! 写入条目后由 ctfmon 监听变更并原生处理按键——按热键把**当前前台应用**的输入法切到指定
//! TIP（per-app，不波及整个会话）。本进程不参与该热键的按键分发。
//!
//! **Windows 设置优先**：用户在「高级键盘设置 → 输入语言热键」里手动分配的条目，其
//! `PreservedKeyId` 是系统现场生成的随机 GUID；本变体自己写入的条目用固定常量。
//! 因此归属判定**只认 `PreservedKeyId`**：标记等于本变体常量的条目才允许清理/改写；
//! 只要存在 CLSID/Profile 指向本输入法、但标记不匹配的条目，就视为用户在 Windows 里
//! 的手动配置，**保留且本次不写入 `keys.activate_ime`**。只有系统侧没有任何条目时，
//! 才在启动/配置重载时写入配置值（出厂 `ctrl+shift+[`）。
//!
//! 值格式（全部 REG_SZ 字符串，与系统设置 UI 写入一致；DWORD 不被识别）：
//!   CLSID          = "{...}"      本输入法 TIP CLSID（随构建变体）
//!   Profile        = "{...}"      本输入法 guidProfile（随构建变体）
//!   PreservedKeyId = "{...}"      条目标识 GUID，**Win10 必需**（见下）
//!   LangId         = "00000804"   简体中文
//!   Modifiers      = "0000c0XX"   0xC000（固定高位）| TF 修饰位（Alt=0x01/Ctrl=0x02/Shift=0x04）
//!   VirtualKey     = "000000XX"   Windows 虚拟键码
//!
//! ⚠️ `PreservedKeyId` 是 Win10/Win11 的行为分水岭，缺了它 Win10 上整条无效：
//!   - Win10 (msctf.dll 10.0.19041)：msctf.dll **和** input.dll 都含该值名 → 读取它。
//!     不写则热键完全不响应，且系统「高级键设置」里该行显示「无」。
//!   - Win11 (msctf.dll 10.0.26100)：两个 DLL 都**不含**该字符串 → 已废弃，写了也只是被忽略。
//!
//! 因此无条件写入：Win10 必需，Win11 无害。切勿因为「Win11 上不写也能用」而删掉它。
//!
//! 取值语义：系统 UI 每次配置都用 `UuidCreate()` 现场生成一个全新 v1 GUID（实测同一
//! profile 反复禁用/启用得到的值各不相同），可见它只是条目的唯一标识，不承载语义，
//! 也不要求 TIP 通过 `ITfKeystrokeMgr::PreserveKey` 注册过对应的保留键——系统能为我们
//! 这个从未注册过任何 preserved key 的 TIP 凭空生成一个。故此处用每变体一个固定常量，
//! 使条目完全可预测、并可据此反认自家残留条目；系统 UI 改写后标记会变成新 GUID，
//! 同步逻辑自然把它当成「用户的设置」而不再覆盖。
//!
//! 相对 Go 版的加固（针对「配置了但不生效」的用户反馈）：
//! 1. 根键为**新建**时记 warn——ctfmon 的注册表监听在其启动时建立，键此前不存在
//!    （用户从未配置过系统输入法热键）则本次写入可能要到注销重登后才生效；
//! 2. 清理旧条目**只按 PreservedKeyId**：CLSID/Profile 指向本输入法但标记不匹配的条目
//!    是用户/系统写的，删掉就等于覆盖 Windows 手动设置（历史缺陷的根因）；
//! 3. slot 取 ≥0x1000 的最小空闲编号（Go 用 max+1，反复保存配置会无限增长）；
//! 4. 成功/清理/保留系统设置都记 info（Go 只记 debug，生产日志无痕迹，用户反馈不生效时无从排查）。

use tracing::{info, warn};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, RegDisposition};

const KEY_PATH: &str = r"Software\Microsoft\CTF\DirectSwitchHotkeys";
/// 子键名起始值（与系统约定一致，0x1000 起）。
const SLOT_BASE: u32 = 0x1000;
/// Modifiers 高位固定标志（系统样本恒为 0xC000）。
const MOD_BASE: u32 = 0xC000;

/// 当前构建变体的 (CLSID, guidProfile, PreservedKeyId) 字符串
/// （大写带花括号，前两者与 wind_tsf Globals.cpp 一致）。
///
/// PreservedKeyId 取本变体 GUID 系列的 `x5`——`x0..x4` 已被 Globals.cpp 占用
/// （CLSID / Profile / LangBarItemButton / DisplayAttributeInput / DisplayAttributeConverted）。
pub(crate) fn tip_guid_strings() -> (&'static str, &'static str, &'static str) {
    if wind_config::variant::is_dev() {
        (
            "{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}",
            "{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}",
            "{99C2DEB5-5C57-45A2-9C63-FB54B34FD90A}",
        )
    } else {
        (
            "{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}",
            "{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}",
            "{99C2EE35-5C57-45A2-9C63-FB54B34FD90A}",
        )
    }
}

/// 把 activate_ime 热键同步到 DirectSwitchHotkeys 表（幂等，可反复调用）：
/// - 先清理带本变体固定 `PreservedKeyId` 的自家旧条目；
/// - 若存在 Windows 手动配置的条目（CLSID/Profile 指向本输入法、标记不匹配）→
///   保留系统条目，本次不写入；
/// - `entry = None`（未配置/解析失败）→ 仅清理自家条目；
/// - `entry = Some((mods, vk))` 且系统侧没有条目 → 在最小空闲 slot 创建新条目。
///   mods 为 TF/Win32 位序（Alt=0x01/Ctrl=0x02/Shift=0x04），vk 为虚拟键码。
///
/// 注册表失败仅记日志（best-effort），不影响输入法主流程。
pub fn sync(hotkey_desc: &str, entry: Option<(u32, u32)>) {
    if let Err(e) = sync_inner(hotkey_desc, entry) {
        warn!("DirectSwitchHotkeys 同步失败: {}", e);
    }
}

/// 一条既有条目与当前变体的关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    /// 本变体自己写的（`PreservedKeyId` 等于本变体固定常量）。
    Ours,
    /// 指向本输入法，但不是本变体写的（Windows 设置 UI 手动配置 / 其它来源）。
    Foreign,
    /// 与本输入法无关。
    Unrelated,
}

/// 归属判定只认 `PreservedKeyId`。
///
/// 系统设置 UI 每次配置都会生成新的 GUID，所以「标记等于本变体常量」是
/// 「这一条是我们写的」的可靠判据；CLSID/Profile 相同但标记不同，说明
/// 用户在 Windows 里手动改过，必须保留。
fn entry_kind(
    entry_clsid: &str,
    entry_profile: &str,
    entry_preserved_key_id: &str,
    our_clsid: &str,
    our_profile: &str,
    our_preserved_key_id: &str,
) -> EntryKind {
    if !our_preserved_key_id.is_empty()
        && entry_preserved_key_id.eq_ignore_ascii_case(our_preserved_key_id)
    {
        return EntryKind::Ours;
    }
    if entry_clsid.eq_ignore_ascii_case(our_clsid)
        || entry_profile.eq_ignore_ascii_case(our_profile)
    {
        return EntryKind::Foreign;
    }
    EntryKind::Unrelated
}

fn sync_inner(hotkey_desc: &str, entry: Option<(u32, u32)>) -> std::io::Result<()> {
    let (clsid, profile, preserved_key_id) = tip_guid_strings();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // 先读写打开；键不存在时，只有确实要写才创建，避免无意义地碰系统设置。
    let root = match hkcu.open_subkey_with_flags(KEY_PATH, KEY_READ | KEY_WRITE) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if entry.is_none() {
                info!("DirectSwitchHotkeys: 键不存在且 activate_ime 未配置，无需同步");
                return Ok(());
            }
            let (root, disp) = hkcu.create_subkey(KEY_PATH)?;
            if matches!(disp, RegDisposition::REG_CREATED_NEW_KEY) {
                warn!(
                    "DirectSwitchHotkeys 注册表键此前不存在（已新建）；ctfmon 可能未监听此键，\
                     activate_ime 热键若未即时生效，需注销重新登录后生效"
                );
            }
            root
        }
        Err(e) => return Err(e),
    };

    // 枚举现有子键：只清理带本变体固定标记的自家条目；其余条目占用 slot。
    let names: Vec<String> = root.enum_keys().filter_map(|r| r.ok()).collect();
    let mut used: Vec<u32> = Vec::new();
    let mut our_entries: Vec<String> = Vec::new();
    let mut has_foreign = false;
    for name in names {
        let Ok(sub) = root.open_subkey(&name) else {
            continue;
        };
        let c: String = sub.get_value("CLSID").unwrap_or_default();
        let p: String = sub.get_value("Profile").unwrap_or_default();
        let k: String = sub.get_value("PreservedKeyId").unwrap_or_default();
        drop(sub);
        match entry_kind(&c, &p, &k, clsid, profile, preserved_key_id) {
            EntryKind::Ours => our_entries.push(name),
            EntryKind::Foreign => {
                has_foreign = true;
                if let Ok(n) = u32::from_str_radix(name.as_str(), 16) {
                    used.push(n);
                }
            }
            EntryKind::Unrelated => {
                if let Ok(n) = u32::from_str_radix(name.as_str(), 16) {
                    used.push(n);
                }
            }
        }
    }

    for name in &our_entries {
        if let Err(e) = root.delete_subkey_all(name.as_str()) {
            warn!("DirectSwitchHotkeys: 清理旧条目 {} 失败: {}", name, e);
            if let Ok(n) = u32::from_str_radix(name.as_str(), 16) {
                used.push(n);
            }
        }
    }

    // Windows 手动配置优先：只要系统侧有本输入法的条目，就不再写配置值。
    if has_foreign {
        info!(
            "DirectSwitchHotkeys: 检测到 Windows 手动配置的热键，保留系统设置，不写入 activate_ime={}",
            hotkey_desc
        );
        return Ok(());
    }

    let Some((mods, vk)) = entry else {
        info!("DirectSwitchHotkeys: activate_ime 未配置，已仅清理本输入法旧条目");
        return Ok(());
    };

    // 最小空闲 slot（避免 Go 版 max+1 的编号无限增长）
    let mut slot = SLOT_BASE;
    while used.contains(&slot) {
        slot += 1;
    }
    let slot_name = format!("{:08X}", slot);
    let (sub, _) = root.create_subkey(&slot_name)?;
    sub.set_value("CLSID", &clsid)?;
    sub.set_value("Profile", &profile)?;
    // Win10 必需、Win11 忽略——详见文件头说明，勿删
    sub.set_value("PreservedKeyId", &preserved_key_id)?;
    sub.set_value("LangId", &"00000804")?;
    sub.set_value("Modifiers", &format!("{:08x}", MOD_BASE | mods))?;
    sub.set_value("VirtualKey", &format!("{:08x}", vk))?;
    info!(
        "DirectSwitchHotkeys: activate_ime={} 已注册（slot={} modifiers={:08x} vk={:08x}）",
        hotkey_desc,
        slot_name,
        MOD_BASE | mods,
        vk
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLSID: &str = "{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}";
    const PROFILE: &str = "{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}";
    const KEY_ID: &str = "{99C2EE35-5C57-45A2-9C63-FB54B34FD90A}";

    fn kind(c: &str, p: &str, k: &str) -> EntryKind {
        entry_kind(c, p, k, CLSID, PROFILE, KEY_ID)
    }

    #[test]
    fn classifies_our_entry_by_marker() {
        assert_eq!(kind(CLSID, PROFILE, KEY_ID), EntryKind::Ours);
        assert_eq!(
            kind(CLSID, PROFILE, &KEY_ID.to_ascii_lowercase()),
            EntryKind::Ours
        );
        // 标记是归属的唯一依据：CLSID/Profile 损坏也仍然认得出自己。
        assert_eq!(kind("", "", KEY_ID), EntryKind::Ours);
    }

    #[test]
    fn classifies_windows_manual_entry_as_foreign() {
        // Windows 设置 UI 每次配置都会生成新的随机 GUID，CLSID/Profile 仍指向本输入法。
        assert_eq!(
            kind(CLSID, PROFILE, "{11111111-1111-1111-1111-111111111111}"),
            EntryKind::Foreign
        );
        // 值缺失/损坏的条目也不能当成自己的删掉：宁可不碰系统设置。
        assert_eq!(kind(CLSID, "", ""), EntryKind::Foreign);
        assert_eq!(kind("", PROFILE, ""), EntryKind::Foreign);
    }

    #[test]
    fn classifies_unrelated_entry() {
        assert_eq!(kind("", "", ""), EntryKind::Unrelated);
        assert_eq!(
            kind(
                "{AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA}",
                "{BBBBBBBB-BBBB-BBBB-BBBB-BBBBBBBBBBBB}",
                "{CCCCCCCC-CCCC-CCCC-CCCC-CCCCCCCCCCCC}"
            ),
            EntryKind::Unrelated
        );
    }
}
