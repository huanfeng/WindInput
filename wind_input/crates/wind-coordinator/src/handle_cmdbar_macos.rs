//! 命令栏（cmdbar）宿主集成的 macOS 平台差异部分
//!
//! 从 [`handle_cmdbar`](crate::handle_cmdbar) 抽出，集中放置 darwin 专属实现，使主文件保持
//! 平台无关的流程清晰。全模块 `#[cfg(ext_presenter)]`（macOS，或 Linux 的 `linux-host` 形态）。
//!
//! 核心差异：服务进程（LaunchAgent）**无 GUI 事件上下文 / 辅助功能授权**，故：
//! - `open` / `proc.run`：进程内直接经 `open` CLI / `Command::spawn`（app 侧无 shell_exec 下行分支）；
//! - `clip.paste`：不合成 ⌘V，改经 IMKit `insertText`（commit 通道）把剪贴板文本上屏（免授权、纯文本）；
//! - `key.tap/seq/hold/release/type`：推 IPC 帧给 `.app` 侧 `KeySynthesizer` 合成 CGEvent（`.app` 有授权）。

use crate::coordinator::Coordinator;
use std::process::Command;
use std::sync::{Arc, Weak};

/// 服务进程内打开 URL / 文件 / .app。经 `open` CLI（Windows ShellExecute open 语义的 macOS
/// 等价），能正确拉起并激活浏览器 / 目标 app。与 `proc.shell`（`sh -c`）、剪贴板（pbcopy/pbpaste）
/// 一致走进程内子进程——app 侧无 shell_exec 下行分支（0x020E 已被上行 candidateHover 占用），
/// 走 push_shell_exec 会被丢弃，故 macOS 不经 IPC 直接执行。
pub(crate) fn open_native(target: &str) -> anyhow::Result<()> {
    // Linux 的等价物是 xdg-open（同样是「按默认应用打开」，且不阻塞）。
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    crate::handle_cmdbar::spawn_reaped(Command::new(opener).arg(target))?;
    Ok(())
}

/// 服务进程内启动外部程序（带参数），直接 spawn。若需以 .app 名启动并激活，用户可改用
/// `open("...")` 或 `proc.shell("open -a ...")`。
pub(crate) fn run_native(cmd: &str, args: &[String], cwd: &str) -> anyhow::Result<()> {
    let mut c = Command::new(cmd);
    c.args(args);
    // 空串 = 继承服务进程的当前目录。服务由 launchd 拉起时那通常是 `/`，
    // 同样是不确定的，故调用方应先经 resolve_workdir 定好目录。
    if !cwd.is_empty() {
        c.current_dir(cwd);
    }
    crate::handle_cmdbar::spawn_reaped(&mut c)?;
    Ok(())
}

/// `clip.paste` 的 macOS 实现：不合成 ⌘V，输入法直接经 IMKit `insertText`（commit 上屏通道）
/// 把剪贴板文本落到当前输入框——输入法插入文本的正道 API：免辅助功能授权、无焦点/时序竞争、
/// 更可靠。代价：仅纯文本（⌘V 才能粘富文本/图片并触发目标 app 原生粘贴），但对输入法的
/// 「粘贴」命令纯文本即所需。等价于 `type(clip())`。
pub(crate) fn paste_via_ime(weak: &Weak<Coordinator>) {
    let Some(c) = weak.upgrade() else {
        return;
    };
    let text = c.host_services().clipboard_get_text().unwrap_or_default();
    if text.is_empty() {
        return;
    }
    c.push_commit_text_from(crate::session_exit::ActionOrigin::Command, &text);
}

/// 构造 macOS 的按键注入服务（[`CoordKeys`]）。
pub(crate) fn make_keys(weak: Weak<Coordinator>) -> Arc<dyn wind_cmdbar::KeyInjector> {
    Arc::new(CoordKeys(weak))
}

/// 命令直通车按键合成经 IPC 推给 `.app`（服务进程无辅助功能授权无法 post CGEvent）。
/// 把 combo 串（"Ctrl+C" / "Cmd+v" / "Enter"）拆成 `.app` KeySynthesizer 期望的 (key, mods)。
struct CoordKeys(Weak<Coordinator>);

impl CoordKeys {
    fn push(&self, encoded: Vec<u8>) {
        if let Some(c) = self.0.upgrade() {
            c.push_cmdbar_key_frame(&encoded);
        }
    }
}

impl wind_cmdbar::KeyInjector for CoordKeys {
    fn tap(&self, combo: &str) -> anyhow::Result<()> {
        let (key, mods) = split_combo(combo);
        self.push(wind_ipc::codec::encode_key_tap(&key, &mods));
        Ok(())
    }
    fn sequence(&self, combos: &[String]) -> anyhow::Result<()> {
        let list: Vec<(String, Vec<String>)> = combos.iter().map(|c| split_combo(c)).collect();
        self.push(wind_ipc::codec::encode_key_seq(&list));
        Ok(())
    }
    fn hold(&self, combo: &str) -> anyhow::Result<()> {
        let (key, mods) = split_combo(combo);
        self.push(wind_ipc::codec::encode_key_hold(&key, &mods));
        Ok(())
    }
    fn release(&self, combo: &str) -> anyhow::Result<()> {
        let (key, mods) = split_combo(combo);
        self.push(wind_ipc::codec::encode_key_release(&key, &mods));
        Ok(())
    }
    fn type_text(&self, text: &str) -> anyhow::Result<()> {
        self.push(wind_ipc::codec::encode_key_type(text));
        Ok(())
    }
}

/// 拆 combo 串为 `.app` KeySynthesizer 规范 (key, mods)：key 为小写 canonical 名，
/// mods ⊆ {"ctrl","shift","alt","win"}（cmd/command→win、control→ctrl、menu/option→alt）。
///
/// `pub(crate)`：软键盘的功能键点击（`handle_softkeyboard::softkeyboard_tap`）与命令
/// 直通车共用同一条下行帧，键名归一自然也只能有一份。
pub(crate) fn split_combo(combo: &str) -> (String, Vec<String>) {
    let parts: Vec<&str> = combo
        .split('+')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let Some((key, mods)) = parts.split_last() else {
        return (String::new(), Vec::new());
    };
    let mods = mods
        .iter()
        .map(|m| match m.to_lowercase().as_str() {
            "control" | "ctrl" => "ctrl".to_string(),
            "shift" => "shift".to_string(),
            "menu" | "alt" | "option" => "alt".to_string(),
            "win" | "cmd" | "command" | "super" | "meta" => "win".to_string(),
            other => other.to_string(),
        })
        .collect();
    (normalize_key(&key.to_lowercase()), mods)
}

/// 键名归一到 `.app` `KeySynthesizer.keyCodeMap` 认得的那一份。
///
/// ⚠️ 两侧的键名表**不是同一份**，这里是它们唯一的接缝，别指望「大小写一致就通了」：
///
/// - `del`：Rust 侧 `key_inject::parse_key` 收 `"delete" | "del"` 两种写法，Swift 侧只有
///   `delete`。软键盘的功能键表用的正好是 `del` ⇒ 不归一就解析不出键码，**静默丢弃**。
/// - `vk:0xNN`：那是 **Windows** 虚拟键码。Swift 侧的 `resolveKeyCode` 会把 `vk:` 后面的
///   数字当成 **mac CGKeyCode** 原样透传 ⇒ 软键盘的 Caps（`vk:0x14`）会变成 CGKeyCode 20，
///   也就是数字键 `2`——点一下 Caps 往文档里打个 2，比没反应更糟。这里按名字接住它。
///
/// 加新条目前先确认 Swift 那张表里有没有对应键名，没有就得两边一起加。
fn normalize_key(key: &str) -> String {
    match key {
        "del" => "delete".to_string(),
        // VK_CAPITAL。⚠️ 归一之后 `.app` 能解析出 kVK_CapsLock(57) 了，但**大写锁定
        // 本身仍改不动**——它的状态由 HID 层维护，CGEvent 碰不到；实测
        // `IOHIDSetModifierLockState` 也是「返回成功、状态不变」。见
        // `docs/design/soft-keyboard.md` §11.9。归一仍然要做：至少别再打出个 2。
        "vk:0x14" | "vk:20" => "capslock".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[path = "handle_cmdbar_macos_tests.rs"]
mod tests;
