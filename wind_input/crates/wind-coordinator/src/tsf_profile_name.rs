//! TSF 语言配置文件显示名（HKLM）的读写——`system.dota2_compat` 的落地动作。
//!
//! Dota 2 的 `imemanager.dll` 按 TSF Profile Description 全等匹配一张硬编码的输入法白名单，
//! 未命中就取不到候选。这是游戏侧写死的，只能改名绕过；证据与证伪过的方向见
//! `docs/design/game-compat-tsf-uielement.md` §1.1。
//!
//! 写的是 HKLM，须管理员权限（否则 `ERROR_ACCESS_DENIED`）；设置程序负责用 `runas` 拉起
//! `wind_input system dota2-compat on|off`。名字由调用方显式传入、本模块不读配置：提权到
//! 另一个管理员账户时读到的 `%APPDATA%` 未必是拧开关那个用户的。游戏启动时读名字，改完须重启游戏。

use std::io;

use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE};

use crate::direct_switch::tip_guid_strings;

/// Dota 2 白名单里选定的默认别名。
///
/// 只是出厂值：用户可在 `system.dota2_compat_name` 里改成表里的任何一条。
/// 真值定义在 `wind-config`，与 serde 的 default 同源。
pub use wind_config::DEFAULT_DOTA2_ALIAS as DOTA2_ALIAS;

/// 别名清洗与校验。定义在 `wind-config`（纯逻辑、与注册表无关，且要跟出厂值同源），
/// 在此重导出，免得调用方为一个校验去依赖两个 crate。
pub use wind_config::sanitize_dota2_alias as sanitize_alias;

/// 「当前登记的这个名字是我们自己写上去的」的记录值名，落在本应用自己的 HKLM 键下。
///
/// `RegisterProfile()` 无条件覆盖 `Description`，安装/升级会冲掉用户的别名；C++ 侧
/// （`wind_tsf/src/Register.cpp` 的 `_ProfileNameToRegister()`）读这条记录，当前值等于它才保留。
/// 别把那侧放宽成「保留任何非真名的值」，那会把误写与脏值也固化下来。
const ALIAS_VALUE: &str = "Dota2CompatAlias";

/// 本应用在 HKLM 下的自有键。与 `wind_tsf/include/Globals.h` 的 `WIND_APP_REGKEY`
/// 同一个键（`Software\<app_dir_name>`，dev 变体带 `Dev` 后缀），`InstallDir` 也在这儿。
fn app_key_path() -> String {
    format!(r"Software\{}", wind_config::variant::app_dir_name())
}

/// TSF 语言配置文件在注册表里的语言子键。与 `wind_tsf` 的 `TEXTSERVICE_LANGID`（0x0804）一致。
const LANGID_SUBKEY: &str = "0x00000804";

/// 本输入法的真实显示名。
///
/// 须与 `wind_tsf/include/Globals.h` 的 `TEXTSERVICE_NAME` 逐字一致（跨语言、无编译期约束），
/// 否则关闭开关时名字会被还原成错的字符串。
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

/// 把显示名切到 `alias`（`enabled=true`）或还原为真实名（`enabled=false`）。
///
/// `alias` 先过 [`sanitize_alias`]；`enabled=false` 时它被忽略（连清洗都不做，
/// 免得一个坏名字挡住「关闭」这条退路——用户改坏了总得关得掉）。
///
/// 幂等：已经是目标值时直接回 `Ok(false)`（未改动），便于调用方少弹一次 UAC 之外的噪音。
/// 返回 `Ok(true)` 表示确实写了。
///
/// 记录值（[`ALIAS_VALUE`]）与 `Description` 不是原子写，故先写 `Description` 再写记录值：
/// 中途失败时记录值滞后，最坏只是重装冲掉别名，不会让 `RegisterProfile` 保留一个不存在的名字。
pub fn set_dota2_compat(enabled: bool, alias: &str) -> io::Result<bool> {
    let target = if enabled {
        let t =
            sanitize_alias(alias).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // 别名不能等于真实名：否则记录值存的是真实名，将来 `TEXTSERVICE_NAME` 一改，
        // profile 会被 `Register.cpp` 永久钉在旧名上。登记成真名本就是「关闭」的语义。
        if t == real_name() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "「{t}」是本输入法的真实名称，它不在游戏名单里；\
                     要还原成它请直接关闭本开关"
                ),
            ));
        }
        t
    } else {
        real_name().to_string()
    };
    let path = profile_key_path();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    // 只开已存在的键，不创建：键不在说明 TSF 组件根本没注册，此时凭空造一个
    // 半截的 LanguageProfile 只会让系统多出一个点不开的输入法条目。
    let key = hklm.open_subkey_with_flags(&path, KEY_READ | KEY_SET_VALUE)?;
    let unchanged = key
        .get_value::<String, _>("Description")
        .is_ok_and(|cur| cur == target);
    if !unchanged {
        key.set_value("Description", &target)?;
    }
    // Description 没变也要补记录值：旧版本改过名的机器没有这个值，不补就永远补不上。
    record_alias(enabled.then_some(target.as_str()))?;
    Ok(!unchanged)
}

/// 系统侧的当前状态 —— 「注册表里到底是什么」的单一出口。
///
/// 设置程序以它而不是 config.toml 为基线，注册表被升级重注册、别的输入法或手工改动
/// 漂移后才能判脏并重新落地。它经 `system dota2-compat status --json`（读 HKLM 无需提权）
/// 拿这份快照；别让它自己拼注册表路径，那要把 CLSID/profile GUID 再抄一份。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AliasStatus {
    /// TSF LanguageProfile 键在不在。反注册之后（升级过程中）它会整个消失。
    pub registered: bool,
    /// 当前登记的显示名。键不在或值读不出时为 `None`。
    pub description: Option<String>,
    /// 我们记下的别名。没开兼容、或老版本装机时为 `None`。
    pub recorded: Option<String>,
    /// 本输入法的真实显示名（随变体不同）。
    pub real: String,
    /// 出厂别名。
    pub factory: String,
}

impl AliasStatus {
    /// 从系统里取一份当前状态。读失败按「读不到」处理，不向上抛 —— 调用方要的是
    /// 「系统现在是什么样」，而读不到本身就是一种回答（视同未启用）。
    pub fn probe() -> Self {
        Self {
            registered: current_description().ok().flatten().is_some(),
            description: current_description().ok().flatten(),
            recorded: recorded_alias().ok().flatten(),
            real: real_name().to_string(),
            factory: DOTA2_ALIAS.to_string(),
        }
    }

    /// 系统里此刻是不是「开着兼容」。判据是登记名不等于真实名 —— 与开关的语义一致。
    pub fn enabled_now(&self) -> bool {
        self.description
            .as_deref()
            .is_some_and(|d| d != self.real.as_str())
    }

    /// 系统里此刻用的是哪个别名。未启用时回 `None`。
    ///
    /// 优先取记录值（跨重注册也在）；`description` 是给还没有记录值的老装机的回退。
    pub fn alias_now(&self) -> Option<&str> {
        if !self.enabled_now() {
            return None;
        }
        self.recorded.as_deref().or(self.description.as_deref())
    }
}

/// 读回已登记的别名。没开兼容（或老版本装机）时回 `Ok(None)`。
///
/// 供 `system dota2-compat status` 展示：`Register.cpp` 重装后能否认出用户的别名全靠它。
pub fn recorded_alias() -> io::Result<Option<String>> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    match hklm.open_subkey_with_flags(app_key_path(), KEY_READ) {
        Ok(key) => match key.get_value::<String, _>(ALIAS_VALUE) {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 写下（或清掉）「我们登记的别名」这条记录。
///
/// 键不存在时创建（与 LanguageProfile 相反）：这是本应用自己的键，便携/手工部署下未必已建好，
/// 不该因此让整个开关失败。
fn record_alias(alias: Option<&str>) -> io::Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (key, _) = hklm.create_subkey(app_key_path())?;
    match alias {
        Some(a) => key.set_value(ALIAS_VALUE, &a),
        // 关闭时删掉而不是写空串：C++ 侧「值不存在」与「值为空」要分别处理就多一条
        // 分支，而两者语义完全相同。不存在时的 NotFound 不是错误。
        None => match key.delete_value(ALIAS_VALUE) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_key_is_the_same_one_the_dll_reads() {
        // 与 wind_tsf/include/Globals.h 的 WIND_APP_REGKEY 同一个键。写错了不会报错，
        // 只是 C++ 那边读不到记录值 —— 表现为「重装之后别名被冲回真名」。
        let p = app_key_path();
        assert!(
            p == r"Software\WindInput" || p == r"Software\WindInputDev",
            "{p}"
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
