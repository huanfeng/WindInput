//! macOS 软键盘面板的**冒烟验证**：裸可执行文件到底开不开得出窗口。
//!
//! # 它验证的是哪一条假设
//!
//! 服务是 LaunchAgent 拉起的**非 .app bundle 的裸可执行文件**。它能注册 Carbon 全局
//! 热键（说明有窗口服务器连接），但本仓此前**从未在服务进程里开过窗**——AppKit 的
//! NSPanel 在这种进程形态下是否真的能上屏，是「服务端自绘软键盘」这条路唯一无法靠
//! 编译和单测证伪的假设。本例就为它而写。
//!
//! # 判据不是「没报错」
//!
//! `orderFront:` 对一个拿不到窗口服务器的进程同样不报错——这正是 `global_hotkey_macos`
//! 模块头列的那三个坑的共同特征（返回码全部正常，功能全部不生效）。所以判据取
//! **窗口服务器自己的说法**：`CGWindowListCopyWindowInfo` 里有没有一个属于本进程、
//! 且 `kCGWindowIsOnscreen` 为真的窗口。
//!
//! 用它而不是截图，是因为窗口**元数据**不需要授权，而 `CGWindowListCreateImage` 自
//! macOS 14 起要「屏幕录制」——本输入法申请的是「辅助功能」，为一次冒烟去要一项更
//! 敏感的授权不成比例（与 `PanelCapture` 不走截图那条路是同一个判断）。
//!
//! # 三项判定
//!
//! 判据一律**不取「调用没报错」**——这条路上的坑清一色是*返回码正常但功能不生效*
//! （`global_hotkey_macos` 模块头列了三个同型的）。
//!
//! 1. **窗口在不在屏**：问窗口服务器（`CGWindowListCopyWindowInfo`），不是问我们自己。
//! 2. **鼠标事件到不到得了视图**：往 `NSApp` 队列投一个鼠标 NSEvent，看上行事件通道
//!    收不收得到 `SoftKeyboardKey`。这一条钉的是 2026-09-05 那个缺陷：窗口画得出来、
//!    CFRunLoop 的 source 与 timer 也在跑，鼠标却一次都不来——因为当时主线程跑的是
//!    Carbon 的 `RunApplicationEventLoop()`，它**从不让 NSApplication 跑起来**
//!    （实测 `NSApp.isRunning == false`），于是没有人调 `NSApp.sendEvent:`。
//! 3. **Carbon 全局热键有没有被换循环带坏**：往 Carbon 主事件队列投一个
//!    `kEventHotKeyPressed`，看 handler 还在不在被派发。换事件循环动的是热键**唯一**
//!    的宿主，这一条是它的回归门。
//! 4. **「重启服务」还退不退得出来**：调 `stop_main_loop()` 看 `run_main_loop()` 会不会
//!    返回。`NSApp.stop:` 与旧的 `QuitApplicationEventLoop()` 语义不同——它只设一个标志，
//!    **要等下一个事件处理完**才生效，服务空闲时可能很久没有事件。本例若卡住不退，
//!    说明那个补投的唤醒事件没起作用。
//!
//! ⚠️ 二、三两项都用「往队列里投事件」而不是合成真实输入：`CGEventPost` 在现代 macOS
//! 要「辅助功能」授权，非交互会话拿不到——实测光标纹丝不动，于是**任何**事件循环下都
//! 得到「没反应」，是个假阴性。这个坑本例踩过一次，别再踩回去。
//!
//! # 跑法
//!
//! ```text
//! cargo run -p wind-ui --example mac_panel_smoke
//! ```
//!
//! 走的是服务 `main.rs` macOS 分支的**同一条路**（`install_on_main` + `run_main_loop`）。
//! 面板会在屏幕上出现几秒，随后自行退出并打印判定结果。

fn main() {
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("本例仅适用于 macOS");
    }
    #[cfg(target_os = "macos")]
    imp::run();
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use wind_ui::softkeyboard_host_macos::{self as sk, SkCmd};
    use wind_ui_types::SoftKeyCap;

    /// 与 `soft_keyboard::ROW_SLOTS`（13/13/11/10）对齐的 47 个键位名。
    /// 名字取真实布局，好让键帽上的角标看起来就是一块键盘。
    const SLOTS: [&str; 47] = [
        "grave",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "0",
        "minus",
        "equal",
        "q",
        "w",
        "e",
        "r",
        "t",
        "y",
        "u",
        "i",
        "o",
        "p",
        "lbracket",
        "rbracket",
        "backslash",
        "a",
        "s",
        "d",
        "f",
        "g",
        "h",
        "j",
        "k",
        "l",
        "semicolon",
        "quote",
        "z",
        "x",
        "c",
        "v",
        "b",
        "n",
        "m",
        "comma",
        "period",
        "slash",
    ];

    pub fn run() {
        // 上行事件通道：面板的点击/翻页/关闭经它回协调器。这里计数 + 打印。
        let (tx, rx) = std::sync::mpsc::channel();
        let clicks = Arc::new(AtomicUsize::new(0));
        let hotkeys = Arc::new(AtomicUsize::new(0));
        let (c2, h2) = (clicks.clone(), hotkeys.clone());
        std::thread::spawn(move || {
            for ev in rx {
                println!("↑ 上行事件: {ev:?}");
                match ev {
                    wind_ui_types::UiEvent::GlobalHotkey(_) => &h2,
                    _ => &c2,
                }
                .fetch_add(1, Ordering::SeqCst);
            }
        });

        // 模拟 forwarder 工作线程：推一条 Show，等面板画出来，再合成点击，最后判定。
        std::thread::spawn(move || {
            // 判定三要用的热键：注册一条，随后往 Carbon 队列投它的 id。
            wind_ui::global_hotkey_macos::apply(
                vec![wind_ui_types::GlobalHotkeyEntry {
                    id: 1,
                    // ⚠️ 故意取一个**没人会占**的冷门组合（ctrl+alt+shift+F12）：注册成功是
                    // 判定三的前提（`ACTIONS` 只登记注册成功的条目），而本例又会把它真的
                    // 注册成系统级热键几秒钟——占用常用键既失礼，也会让判定假失败。
                    modifiers: 0x1 | 0x2 | 0x4, // MOD_ALT | MOD_CONTROL | MOD_SHIFT
                    vk: 0x7B,                   // VK_F12
                    action: "smoke_probe".into(),
                }],
                tx.clone(),
            );

            std::thread::sleep(Duration::from_millis(600));
            let keys: Vec<SoftKeyCap> = SLOTS
                .iter()
                .map(|s| SoftKeyCap {
                    slot: (*s).to_string(),
                    base: "·".into(),
                    shift: "•".into(),
                })
                .collect();
            sk::apply(
                SkCmd::Show {
                    pages: vec!["标点".into(), "数字".into(), "数学".into()],
                    current: 0,
                    keys,
                    send_keys: false,
                },
                &tx,
            );

            std::thread::sleep(Duration::from_millis(800));
            if let Some((num, b)) = report_onscreen() {
                // 投递必须在主线程。借协调器那条现成的路：本例没有，故直接用
                // dispatch_async 到主队列。
                run_on_main(move || post_synthetic_click(num, b));
                std::thread::sleep(Duration::from_millis(600));
            }
            let n = clicks.load(Ordering::SeqCst);
            if n > 0 {
                println!("✅ 鼠标判定通过：投递的点击产出了 {n} 条上行事件");
            } else {
                println!(
                    "❌ 鼠标判定失败：投递的点击一条上行事件都没产出。\n\
                     窗口在屏但 NSView 收不到鼠标 ⇒ 没有人调 NSApp.sendEvent:。"
                );
            }

            // ── 判定三：换事件循环有没有把 Carbon 全局热键带坏 ──
            if !wind_ui::global_hotkey_macos::post_test_hotkey(1) {
                println!("⚠️ 热键事件投递失败，判定三无法进行");
            }
            std::thread::sleep(Duration::from_millis(400));
            let hk = hotkeys.load(Ordering::SeqCst);
            if hk > 0 {
                println!("✅ 热键判定通过：Carbon handler 在 AppKit 事件循环下仍被派发");
            } else {
                println!(
                    "❌ 热键判定失败：投进主事件队列的 kEventHotKeyPressed 没被派发。\n\
                     两种可能：换事件循环把全局热键带坏了；或探针热键没注册上\n\
                     （`ACTIONS` 只登记注册成功的条目，看上面有没有「注册 … 失败」的 warn）。"
                );
            }

            // ── 判定四：走「重启服务」那条退出路径 ──
            //
            // 刻意**不用 `process::exit`**：那样测不到 `stop_main_loop`。看门狗兜底，
            // 卡住就是判定失败，而不是让 CI/开发者对着一个挂死的进程干等。
            println!("请求退出事件循环…");
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_secs(5));
                println!("❌ 退出判定失败：5 秒内没退出，NSApp.stop 的唤醒事件没起作用");
                std::process::exit(1);
            });
            wind_ui::global_hotkey_macos::stop_main_loop();
        });

        // 与服务 `main.rs` 里 macOS 分支的收尾**完全同构**。
        sk::install_on_main();
        wind_ui::global_hotkey_macos::run_main_loop();
        println!("✅ 退出判定通过：stop_main_loop() 让事件循环正常返回");
    }

    /// 把闭包丢到主队列执行（`postEvent:` 要主线程）。
    fn run_on_main(f: impl FnOnce() + Send + 'static) {
        use dispatch2::DispatchQueue;
        DispatchQueue::main().exec_async(f);
    }

    /// 把一个鼠标 NSEvent **直接投进 `NSApp` 的事件队列**，看事件循环派不派发它。
    ///
    /// ⚠️ 刻意不用 `CGEventPost` 合成真实鼠标：那条路在现代 macOS 要「辅助功能」授权，
    /// 非交互会话拿不到，实测光标纹丝不动 —— 用它做判据会在**任何**事件循环下都得到
    /// 「没反应」，是个假阴性。`postEvent:atStart:` 不需要任何授权，而且它测的正好是
    /// 本次要问的那件事：**有没有人在 `sendEvent:`**。
    fn post_synthetic_click(win_number: isize, b: (f64, f64, f64, f64)) {
        use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
        use objc2_foundation::{MainThreadMarker, NSPoint};

        let Some(mtm) = MainThreadMarker::new() else {
            println!("⚠️ 不在主线程，跳过 NSEvent 合成");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        println!("NSApp.isRunning = {}", app.isRunning());

        let (_, _, w, h) = b;
        // 窗口坐标：AppKit 的窗口坐标原点在**左下**，键盘区在面板下半部分 ⇒ 取窗口坐标
        // 的偏下位置，也就是 y 偏小。
        for (fx, fy) in [(0.30, 0.30), (0.50, 0.30), (0.70, 0.30), (0.50, 0.15)] {
            let loc = NSPoint::new(w * fx, h * fy);
            for ty in [
                NSEventType::MouseMoved,
                NSEventType::LeftMouseDown,
                NSEventType::LeftMouseUp,
            ] {
                let ev =
                    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                        ty,
                        loc,
                        NSEventModifierFlags::empty(),
                        0.0,
                        win_number,
                        None,
                        0,
                        1,
                        1.0,
                    );
                match ev {
                    Some(e) => app.postEvent_atStart(&e, false),
                    None => println!("⚠️ NSEvent 构造失败 ({ty:?})"),
                }
            }
        }
        println!("已向 NSApp 队列投递 4 组鼠标事件");
    }

    /// 问窗口服务器要本进程的在屏窗口，并返回它的 bounds（CG 坐标：点、左上原点）。
    fn report_onscreen() -> Option<(isize, (f64, f64, f64, f64))> {
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::number::CFNumber;
        use core_foundation::string::CFString;
        use core_graphics::window::{
            copy_window_info, kCGWindowBounds, kCGWindowIsOnscreen,
            kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowNumber,
            kCGWindowOwnerPID,
        };

        let me = std::process::id() as i64;
        let Some(list) = copy_window_info(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            0,
        ) else {
            println!("❌ 判定失败：CGWindowListCopyWindowInfo 返回空");
            return None;
        };

        let key = |name: core_foundation::string::CFStringRef| unsafe {
            CFString::wrap_under_get_rule(name)
        };
        let mut found = 0usize;
        let mut bounds = None;
        let mut win_num = 0isize;
        for i in 0..list.len() {
            let d: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(*list.get(i).unwrap() as _) };
            let pid = d
                .find(key(unsafe { kCGWindowOwnerPID }))
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64());
            if pid != Some(me) {
                continue;
            }
            let onscreen = d
                .find(key(unsafe { kCGWindowIsOnscreen }))
                .is_some_and(|v| v.instance_of::<core_foundation::boolean::CFBoolean>());
            let num = d
                .find(key(unsafe { kCGWindowNumber }))
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
                .unwrap_or(-1);
            found += 1;
            // bounds 是个 {X,Y,Width,Height} 的字典，CG 坐标（点、左上原点）——
            // 与 CGEvent 合成鼠标用的坐标系恰好一致，不需要再翻一次。
            if let Some(d2) = d
                .find(key(unsafe { kCGWindowBounds }))
                .and_then(|v| v.downcast::<CFDictionary>())
            {
                let g = |k: &str| -> Option<f64> {
                    d2.find(CFString::new(k).as_CFTypeRef().cast())
                        .and_then(|v| {
                            unsafe { CFType::wrap_under_get_rule(*v) }.downcast::<CFNumber>()
                        })
                        .and_then(|n| n.to_f64())
                };
                if let (Some(x), Some(y), Some(w), Some(h)) =
                    (g("X"), g("Y"), g("Width"), g("Height"))
                {
                    bounds = Some((x, y, w, h));
                }
            }
            win_num = num as isize;
            println!("  · 窗口 #{num} onscreen={onscreen} bounds={bounds:?}");
        }

        if found > 0 {
            println!("✅ 在屏判定通过：窗口服务器确认本进程（pid={me}）有 {found} 个在屏窗口");
            return bounds.map(|b| (win_num, b));
        } else {
            println!(
                "❌ 判定失败：窗口服务器认为本进程（pid={me}）没有任何在屏窗口。\n\
                 这说明裸可执行文件开不出 AppKit 窗口，服务端自绘的前提不成立。"
            );
        }
        None
    }
}
