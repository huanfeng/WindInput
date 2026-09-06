//! activate_ime → Windows DirectSwitchHotkeys 注册表同步
//!
//! 与 Go 版本 `wind_input/internal/ui/direct_switch_hotkey_windows.go` 对齐（含加固，见下）。
//!
//! 机制：`HKCU\Software\Microsoft\CTF\DirectSwitchHotkeys` 是 Windows TSF「直接切换热键」表，
//! 写入条目后由 ctfmon 监听变更并原生处理按键——按热键把**当前前台应用**的输入法切到指定
//! TIP（per-app，不波及整个会话）。本进程不参与该热键的按键分发。
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
//! 使条目完全可预测、并可据此反认自家残留条目。
//!
//! 相对 Go 版的加固（针对「配置了但不生效」的用户反馈）：
//! 1. 根键为**新建**时记 warn——ctfmon 的注册表监听在其启动时建立，键此前不存在
//!    （用户从未配置过系统输入法热键）则本次写入可能要到注销重登后才生效；
//! 2. 清理旧条目按 CLSID **或** Profile **或** PreservedKeyId 匹配（Go 只匹配 CLSID，
//!    值缺失/损坏的自家旧条目永不清理，会残留脏数据）；
//! 3. slot 取 ≥0x1000 的最小空闲编号（Go 用 max+1，反复保存配置会无限增长）；
//! 4. 成功/清理记 info（Go 只记 debug，生产日志无痕迹，用户反馈不生效时无从排查）。

use tracing::{info, warn};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, RegDisposition};

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
/// - 先删除所有属于本输入法（CLSID 或 Profile 匹配）的旧条目；
/// - `entry = None`（未配置/解析失败）→ 仅清理；
/// - `entry = Some((mods, vk))` → 在最小空闲 slot 创建新条目。
///   mods 为 TF/Win32 位序（Alt=0x01/Ctrl=0x02/Shift=0x04），vk 为虚拟键码。
///
/// 注册表失败仅记日志（best-effort），不影响输入法主流程。
pub fn sync(hotkey_desc: &str, entry: Option<(u32, u32)>) {
    if let Err(e) = sync_inner(hotkey_desc, entry) {
        warn!("DirectSwitchHotkeys 同步失败: {}", e);
    }
}

fn sync_inner(hotkey_desc: &str, entry: Option<(u32, u32)>) -> std::io::Result<()> {
    let (clsid, profile, preserved_key_id) = tip_guid_strings();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (root, disp) = hkcu.create_subkey(KEY_PATH)?;
    if matches!(disp, RegDisposition::REG_CREATED_NEW_KEY) {
        warn!(
            "DirectSwitchHotkeys 注册表键此前不存在（已新建）；ctfmon 可能未监听此键，\
             activate_ime 热键若未即时生效，需注销重新登录后生效"
        );
    }

    // 枚举现有子键：删除本输入法旧条目，收集其余条目占用的 slot 编号
    let names: Vec<String> = root.enum_keys().filter_map(|r| r.ok()).collect();
    let mut used: Vec<u32> = Vec::new();
    for name in names {
        let Ok(sub) = root.open_subkey(&name) else {
            continue;
        };
        // 值不存在/类型不符按空串处理：CLSID 缺失但 Profile 匹配的残缺自家条目也能清掉。
        // PreservedKeyId 同样参与匹配——它是本变体独有常量，可兜住 CLSID/Profile 均损坏的条目。
        let c: String = sub.get_value("CLSID").unwrap_or_default();
        let p: String = sub.get_value("Profile").unwrap_or_default();
        let k: String = sub.get_value("PreservedKeyId").unwrap_or_default();
        drop(sub);
        if c.eq_ignore_ascii_case(clsid)
            || p.eq_ignore_ascii_case(profile)
            || k.eq_ignore_ascii_case(preserved_key_id)
        {
            if let Err(e) = root.delete_subkey_all(&name) {
                warn!("DirectSwitchHotkeys: 清理旧条目 {} 失败: {}", name, e);
            }
            continue;
        }
        if let Ok(n) = u32::from_str_radix(&name, 16) {
            used.push(n);
        }
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
