//! 主题明暗（亮 / 暗 / 跟随系统）的类型与系统明暗探测。
//!
//! 用枚举而非裸 `u8`：[`ThemeStyle::resolve_dark`] 是唯一的明暗出口，新增分支必须在此显式回答，
//! 漏处理会编译失败，而不是被 `_` 分支悄悄按亮色跑。

// 两个消费点分别在 `cfg(windows)` 与 `cfg(target_os = "macos")` 的 `system_prefers_dark`
// 里，其余平台（Android/Linux 的 headless 形态）走空实现，此 import 在那里即死代码。
#[cfg(any(windows, target_os = "macos"))]
use tracing::debug;

/// 主题明暗设置（`ui.theme.style` 的运行时形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeStyle {
    /// 跟随系统明暗（默认）
    #[default]
    System,
    /// 恒亮色
    Light,
    /// 恒暗色
    Dark,
}

impl ThemeStyle {
    /// 解析 `ui.theme.style` 配置值；未知值按跟随系统（与配置默认一致）。
    pub fn from_config(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    /// 回写 `ui.theme.style` 的配置值。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// 菜单协议编码（`MenuCmd::ThemeStyle(u8)`：0 跟随 / 1 亮 / 2 暗）。
    /// 该编码跨进程传给 macOS `.app` 的 `NSMenuItem.tag`，取值不可变更。
    pub fn as_menu_id(self) -> u8 {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }

    /// 由菜单协议编码还原；未知值按跟随系统。
    pub fn from_menu_id(id: u8) -> Self {
        match id {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }

    /// 本次渲染该用暗色吗——主题解析的唯一明暗出口。
    ///
    /// `System` 每次调用都实时探测：系统明暗可在运行中被用户改掉，缓存会让
    /// 「切了系统主题但输入法没跟上」重新出现。探测本身是一次注册表读，量级远低于
    /// 随后的主题解析与重绘，不必优化。
    pub fn resolve_dark(self) -> bool {
        self.resolve_dark_with(system_prefers_dark())
    }

    /// 同 [`Self::resolve_dark`]，但由宿主告知系统明暗。
    ///
    /// 移动端必须走这一个：[`system_prefers_dark`] 在非 Windows/macOS 上恒 false，走
    /// [`Self::resolve_dark`] 会让「跟随系统」静默退化成恒亮色。明暗判定的 `match` 只有这一处，
    /// `resolve_dark` 委托给它。
    pub fn resolve_dark_with(self, system_dark: bool) -> bool {
        match self {
            Self::Light => false,
            Self::Dark => true,
            Self::System => system_dark,
        }
    }

    /// 菜单/气泡展示名。
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Light => "亮色",
            Self::Dark => "暗色",
        }
    }
}

/// 系统当前是否为深色模式。
///
/// Windows 读 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` 的
/// `AppsUseLightTheme`（0=深色）；同键下的 `SystemUsesLightTheme` 管任务栏，不是应用明暗。
/// 值缺失（老系统或用户从未设过）与读取失败一律按浅色：猜错深色会在浅底主题上叠深色文本。
#[cfg(windows)]
pub fn system_prefers_dark() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let dark = hkcu
        .open_subkey(KEY)
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
        .map(|v| v == 0)
        .unwrap_or(false);
    debug!("系统明暗探测: AppsUseLightTheme → dark={}", dark);
    dark
}

/// macOS 读全局偏好 `AppleInterfaceStyle`：浅色时该键不存在，深色时值为 `"Dark"`。
///
/// 走 `CFPreferencesCopyAppValue`：直接读 plist 会拿到 cfprefsd 尚未落盘的旧值，`defaults` 子进程
/// 对每次主题解析都 spawn 太重。服务是用户会话内的 LaunchAgent，读到的就是当前用户域。
/// 取不到一律按浅色（同 Windows 分支）。
#[cfg(target_os = "macos")]
pub fn system_prefers_dark() -> bool {
    use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFTypeRef, kCFAllocatorDefault};
    use core_foundation_sys::preferences::{
        CFPreferencesCopyAppValue, kCFPreferencesAnyApplication,
    };
    use core_foundation_sys::string::{
        CFStringCreateWithBytes, CFStringGetCString, CFStringGetTypeID, CFStringRef,
        kCFStringEncodingUTF8,
    };

    const KEY: &[u8] = b"AppleInterfaceStyle";
    let dark = unsafe {
        let key = CFStringCreateWithBytes(
            kCFAllocatorDefault,
            KEY.as_ptr(),
            KEY.len() as isize,
            kCFStringEncodingUTF8,
            false as u8,
        );
        if key.is_null() {
            return false;
        }
        let value = CFPreferencesCopyAppValue(key, kCFPreferencesAnyApplication);
        CFRelease(key as CFTypeRef);
        if value.is_null() {
            // 键不存在 = 浅色（这是浅色模式下的正常状态，不是错误）。
            return false;
        }
        // 必须先验类型再当 CFString 用。
        //
        // 这里原先写的是「拿到别的类型时 CFStringGetCString 会失败并返回 false」——那是错的：
        // CoreFoundation 对入参做类型断言，类型不符是直接 abort 整个进程，不是返回 0。偏好键
        // 理论上只会是字符串，但它是全局可写的（第三方工具、损坏的 plist 都能塞进别的类型），
        // 而代价是输入法服务当场崩掉、用户完全没法打字。一次 CFGetTypeID 换掉这个风险很划算。
        if CFGetTypeID(value) != CFStringGetTypeID() {
            CFRelease(value);
            tracing::warn!("AppleInterfaceStyle 不是字符串类型，按浅色处理");
            return false;
        }
        let mut buf = [0i8; 32];
        let ok = CFStringGetCString(
            value as CFStringRef,
            buf.as_mut_ptr(),
            buf.len() as isize,
            kCFStringEncodingUTF8,
        );
        CFRelease(value);
        ok != 0 && {
            let s = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy();
            // 实测取值恒为 "Dark"；用 eq_ignore_ascii_case 而非全等，容忍未来的大小写变体。
            s.eq_ignore_ascii_case("dark")
        }
    };
    debug!("系统明暗探测: AppleInterfaceStyle → dark={}", dark);
    dark
}

/// 其余平台（Linux 等）恒浅色：无统一的系统明暗来源。
#[cfg(not(any(windows, target_os = "macos")))]
pub fn system_prefers_dark() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip() {
        for s in [ThemeStyle::System, ThemeStyle::Light, ThemeStyle::Dark] {
            assert_eq!(ThemeStyle::from_config(s.as_config()), s);
            assert_eq!(ThemeStyle::from_menu_id(s.as_menu_id()), s);
        }
        // 大小写与空白容错（配置可手写）
        assert_eq!(ThemeStyle::from_config("  Dark "), ThemeStyle::Dark);
        // 未知值与空值回落跟随系统（与 config.toml 默认一致）
        assert_eq!(ThemeStyle::from_config(""), ThemeStyle::System);
        assert_eq!(ThemeStyle::from_config("auto"), ThemeStyle::System);
        // 菜单协议编码固定，跨进程传给 macOS .app，不可变更
        assert_eq!(ThemeStyle::System.as_menu_id(), 0);
        assert_eq!(ThemeStyle::Light.as_menu_id(), 1);
        assert_eq!(ThemeStyle::Dark.as_menu_id(), 2);
    }

    #[test]
    fn resolve_dark_explicit_ignores_system() {
        // 显式明暗不受系统设置影响；System 与实时探测一致。
        assert!(!ThemeStyle::Light.resolve_dark());
        assert!(ThemeStyle::Dark.resolve_dark());
        assert_eq!(ThemeStyle::System.resolve_dark(), system_prefers_dark());
    }
}
