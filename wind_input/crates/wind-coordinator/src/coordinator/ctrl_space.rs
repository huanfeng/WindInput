//! Ctrl+空格 中英切换开关（GH#172，`keys.ctrl_space_toggle`）的服务端两半：
//!
//! 1. **下发**：有效值经 `CONFIG_KEY_CTRL_SPACE_TOGGLE` 推给 DLL。吃不吃 Ctrl+空格 在
//!    `OnTestKeyDown` 就定了（早于 IPC），只能由 DLL 本地判；关掉后它不吃、不做按键侧兜底，
//!    键原样交给宿主（IDEA / Android Studio 的代码提示）。
//! 2. **仲裁**：Windows 系统输入法开关热键在 msctf 层直接翻 OPENCLOSE compartment，DLL
//!    拦不住（见 KeyEventSink.cpp `ctrl_space_intercept` 处的注释），只能事后拒绝——回包带
//!    当前模式，DLL 的 `_ApplyModeSwitch` 见「回包 ≠ 请求」就把 compartment 拉回。与 per-app
//!    `ignore_host_ime_switch` 同一条回路。⚠ 系统热键吃掉的键宿主仍收不到，那只能引导用户
//!    去系统设置里关（设置项 hint / 文档里写了）。
//!
//! 「这次切换是不是用户按出来的」与 t283 的 `CapsCancelSwitch::from_system` **同一判据**，
//! 不另写一份：两处对「Ctrl+空格」的认定一旦分叉，就会出现「拒绝了却仍取消了大写」之类
//! 自相矛盾的组合。

use super::*;

/// 纯判定：关掉开关后，这次系统模式切换是否要拒绝。
///
/// - 开关开着 ⇒ 从不拒绝（出厂行为）；
/// - 请求等于当前模式 ⇒ 不是翻转，无所谓拒绝；
/// - 否则只拒绝**用户按出来的**（`CapsCancelSwitch::System`）：按键侧兜底恒算，compartment
///   来源要 Ctrl 按住；宿主自己写 compartment、功能菜单照办。
pub(crate) fn ctrl_space_switch_rejected(
    enabled: bool,
    requested: bool,
    current: bool,
    source: wind_ipc::protocol::ModeSwitchSource,
    ctrl_held: bool,
) -> bool {
    if enabled || requested == current {
        return false;
    }
    matches!(
        CapsCancelSwitch::from_system(requested, source, ctrl_held),
        CapsCancelSwitch::System(_)
    )
}

impl Coordinator {
    /// Ctrl+空格 切换的有效值（开关 + 「全局自定义按键」里的 `none`），见
    /// `KeysConfig::ctrl_space_toggle_effective`。
    pub(crate) fn ctrl_space_toggle_enabled(&self) -> bool {
        self.rt().config.keys.ctrl_space_toggle_effective()
    }

    /// 本次 `CMD_SYSTEM_MODE_SWITCH` 是否因 Ctrl+空格 被关掉而拒绝。
    pub(crate) fn ctrl_space_switch_rejected(
        &self,
        requested: bool,
        source: wind_ipc::protocol::ModeSwitchSource,
        ctrl_held: bool,
    ) -> bool {
        ctrl_space_switch_rejected(
            self.ctrl_space_toggle_enabled(),
            requested,
            self.is_chinese_mode(),
            source,
            ctrl_held,
        )
    }

    /// 下发 Ctrl+空格 切换开关给 DLL（`CONFIG_KEY_CTRL_SPACE_TOGGLE`）。
    ///
    /// 全局配置、不分进程，`client_token=0` 时广播。握手与配置重载时推——DLL 每次重连都从
    /// 默认值（开）起步，只在重载时推会让关掉的用户在重连后又被吃键。
    pub fn push_ctrl_space_toggle_config(&self, client_token: u64) {
        let value =
            wind_ipc::codec::encode_ctrl_space_toggle_value(self.ctrl_space_toggle_enabled());
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_CTRL_SPACE_TOGGLE,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }
}

#[cfg(test)]
#[path = "ctrl_space_tests.rs"]
mod tests;
