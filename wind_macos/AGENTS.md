# wind_macos

macOS IMKit `.app` 工程。与 Win 端 `wind_tsf/` DLL 对位，与跨平台 Rust 服务
（`wind_input/`，cargo workspace）通过 Unix Domain Socket 通信。

`.app` 只是 IMKit 壳：渲染、定位、上屏决策的真身都在 Rust 服务里。对应关系见
`scripts/mac/dev.sh` 头部注释（Win 的 `m1`=TSF DLL ↔ mac 的 `m1`=`.app`，
Win 的 `m2`=核心 exe ↔ mac 的 `m2`=Rust 服务）。

## 当前阶段

协议通路、IMKit 骨架、候选/菜单/提示的原生呈现均已就位：

- 协议层 `WindInputKit`（BinaryCodec + BridgeClient + PushClient + ProtocolTypes）
- IMKit `.app`：InputController（composition / commit / 焦点 / 密码框 / 自愈重连）
  + KeyHandler（NSEvent → Win VK）
- 呈现层：候选框 NSPanel（SHM 取帧）、统一菜单、菜单栏模式指示器、tooltip、
  状态气泡、Toast、命令直通车按键合成
- IMK server 注册 + 自身 `--register-input-source` 子命令（镜像 Squirrel/RIME 路径）
- 单元测试 `Tests/WindInputKitTests/`（`swift test`）

### 与 Windows 的功能差距（待补）

Rust 核心跨平台，引擎/词库/候选/词频类改动 macOS 自动受益；差距集中在**宿主层**。
`wind-ui` 的 `UiCommand` 有 51 个变体，`manager_macos.rs` 的 Forwarder 目前接了 37 个
（数字随 core 加变体而变，改动时顺手核一下：
`awk '/^pub enum UiCommand/,/^}/' wind-ui-types/src/command.rs | grep -cE '^    [A-Z]'`）。
未接的：

| UiCommand | 影响 | 备注 |
|---|---|---|
| `ShowInputDiag` / `HideInputDiag` / `CopyInputDiagText` | 输入诊断 HUD 整套缺失 | 设计见 `docs/design/input-diagnostics-hud.md`。**菜单入口已按平台摘掉**（`build_main_menu_items` 的 `advanced_children`）——留一个点了没反应的项比没有更糟 |
| `ShowCandidateMenu` / `HideMenu` / `MenuKey` | N/A（**不是缺失**） | macOS 弹的是原生 NSMenu，方向键/回车/Esc 由 AppKit 自己消费。协调器**刻意不转发**菜单键（见 `handle_key_event` 里那段 `cfg(not(target_os = "macos"))`）：一旦吞键而 `menu_open` 没复位就会永久卡死输入 |
| `SetToolbarPos` / `SetToolbarAutoHide` / `SetToolbarVertical` / `SetToolbarLayout` | N/A（mac 用菜单栏指示器，无浮动工具栏） | 对应配置项已在设置清单里按平台隐藏（`platform = "windows"`），不再是"无处落地"。`SetToolbarLayout`（`ui.toolbar.items`，格的显隐与顺序）同理——菜单栏指示器只有一个主字，没有"哪几格" |
| `SetHostRender` | Windows 专有（宿主进程内 Band 窗口） | mac 无对应概念 |

### 软键盘：唯一一个由**服务进程自己开窗**的浮层

macOS 侧的通例是「服务进程只光栅化，窗口一律归 `.app`」——候选窗的像素在服务进程画好，
经 SHM 推给 `.app` 的 NSPanel 呈现。**软键盘是唯一的例外**，它的窗口
（`wind-ui/src/mac_panel.rs` 的 `MacPanel`，一个 `NSPanel`）就开在服务进程里。

判据是「这个浮层要不要跟随 caret」：

- 候选窗/状态气泡要跟随 caret，而 caret 只有 `.app` 从 IMKit 拿得到 ⇒ 窗口归 `.app`；
- 软键盘**不跟随任何东西**，是用户自己拖到某处的常驻浮层，与 IMKit 无关 ⇒ 没有理由
  为它再实现一份 Swift 面板。`soft_keyboard.rs` 那 1900 行布局/绘制/命中/交互本就是
  跨平台的 tiny-skia + View 树，服务端开窗即可原样复用，两平台共一份实现。

**前提早就具备**，只是此前没人用：服务虽是 LaunchAgent 拉起的裸可执行文件，
`global_hotkey_macos::run_main_loop` 里的 `TransformProcessType(TRANSFORM_TO_UI_ELEMENT)`
已经把它提升为 UIElement 应用（有窗口服务器连接）。冒烟验证见
`cargo run -p wind-ui --example mac_panel_smoke`，它跑四项判定，判据一律取**系统自己的
说法**而不是「调用没报错」——这条路上的坑清一色是*返回码正常但功能不生效*。

三条约定，改这块前先读：

1. **AppKit 只能在主线程碰**，而下发命令的 forwarder 是工作线程。转运由
   `softkeyboard_host_macos` 负责（「入队 + CFRunLoopSource 唤醒」，形制照抄
   `global_hotkey_macos`）。面板可见期间另挂一个 `CFRunLoopTimer` 驱动 `tick()`，
   **面板一关就撤掉**。
2. **`window::LayeredWindow` 在 macOS 上仍是纯像素缓冲，别把它改成真窗口**——
   `candidate_window.rs` 正靠它光栅化后写 SHM，一改就会给候选窗凭空开出一个 NSPanel。
   软键盘用的是独立的 `MacPanel`，两者方法集同形但用途不同。
3. **主线程必须跑 `[NSApp run]`，不能是 Carbon 的 `RunApplicationEventLoop()`**。后者
   从不让 NSApplication 跑起来（实测 `NSApp.isRunning == false`），于是没有人调
   `NSApp.sendEvent:`，面板一个鼠标事件都收不到——窗口画得出来、CFRunLoop 的 source
   与 timer 也照常转，唯独点不动。连带地 `stop_main_loop` 也从
   `QuitApplicationEventLoop()` 换成了 `NSApp.stop:` + 补投一个唤醒事件（`stop:` 只设
   标志，要等下一个事件才生效，不补投的话「重启服务」会挂死）。见
   `docs/design/soft-keyboard.md` §11.2。

非 `UiCommand` 的一项差距，同样待补：

| 能力 | 影响 | 备注 |
|---|---|---|
| `CMD_INPUT_STATS`（英文输入统计） | 英文模式下的输入不计入统计 | Win 侧由 TSF DLL 在英文模式下上报，`.app` 未采集。**已知并暂缓**（2026-08-12 决定：当前 macOS 用户量少，优先级低）。对应设置项 `stats.track_english` 已按平台隐藏，故当前不表现为「开关无效」，只表现为统计数字偏低 |

### 已按平台屏蔽的设置项

无落点的配置**在设置清单里门控掉**（`platform = "windows"`，见 `wind-setting`
`settings_manifest.toml`），而不是留着让用户配一个不生效的开关：

| 配置项 | macOS 上为何无落点 |
|---|---|
| `ui.toolbar.hide_in_fullscreen` / `auto_hide` / `auto_hide_delay` / `vertical` | 菜单栏指示器不是浮动窗口，无从隐藏 / 排列 / 自动淡出。`ui.toolbar.visible` **不在此列**——它在 mac 上控制指示器显隐，是有落点的 |
| `input.capslock.cancel_on_mode_switch` | 实现手段是合成一次 CapsLock 敲击（`key_inject::tap_caps_lock`），而 macOS 的大写锁定态由 HID 层维护，CGEvent 改不动它。补 VK→CGKeyCode 映射也没用。**`IOHIDSetModifierLockState` 那条路后来也实测过：连接开得出、返回 `KERN_SUCCESS`、状态纹丝不动（Darwin 25.5）；`kIOHIDServerConnectType` 则 `IOServiceOpen` 直接失败，需特权。** 详见 `docs/design/soft-keyboard.md` §11.9 —— 软键盘面板上的 Caps 键同样受这条限制，已按此**在 macOS 上画成禁用态**（`soft_keyboard.rs` 的 `CAPS_CLICKABLE`）：键位留着、不进命中表，但 `caps_on` 的高亮照旧——改不动不等于读不到 |
| `stats.track_english` | 英文模式下的输入由 Windows 侧 TSF DLL 经 `CMD_INPUT_STATS` 上报，`.app` 没有对应采集点 |

`keys.session_actions` 里的 **CapsLock** 同理无落点（`capslock_hook` 在非 Windows 是
`bail!` 的空壳），但它是对话框内的一个**取值**而非独立配置项，清单门控管不到，故在
`wind-setting` 的 `session_actions.rs::key_options` 里按平台剔除。已有配置（从 Windows
同步过来的）不丢——词表外的键会被当「当前值」保留，并显示「本平台不支持」。

**仍然保留的两个容易被误判为「该屏蔽」的**：

- **「候选窗首显」**（应用独立配置菜单）：`fast` 档的加速判据依赖 `CMD_CARET_PROBE`，
  那只有 Windows DLL 发；但三档在 mac 上仍有可观测差别（`wait` 兜底 150ms、`fast` 25ms、
  `instant` 不等），故保留。只是菜单文案描述的 TSF reflow 背景在 mac 上不成立。
- **`ui.toolbar.visible`**：形态不同（浮动条 vs 菜单栏指示器）但语义一致，文案已改写成
  平台中立的「常驻显示输入法当前状态」。

已接：`RegisterGlobalHotkeys`（见下）、`OpenPath` / `OpenApp`（`/usr/bin/open`，
`.app` 包走 `open -a … --args`）、`TakeScreenshot` / `ScreenshotCandidateToClipboard` /
`CopyTooltipText`（见下）、`ReportCandidatePos` / `ReportStatusTipPos`（见下「浮窗拖动
与位置固定」）、`ScreenshotStatusTip` / `ScreenshotTooltip`（见下「原生浮窗截图」）。

### 原生浮窗截图（状态气泡 / 悬停提示）

候选窗的像素在服务进程（本进程光栅化后经 SHM 推下去），直接截自己的 buffer 即可；
**这两者相反**——是 `.app` 侧的原生 NSPanel，服务端只下发文本与配色，只能请那边动手：

下行扩展信封 `shot.panel`（body 带 `target` 与服务端定好的 `path`）→ `.app` 截图存盘 +
复制剪贴板 → 上行 `shot.result` 回报 → 协调器据此弹 Toast。**文件名与文案留在服务端**，
与 Windows 侧 `manager.rs` 的对应分支逐字一致，两平台同一操作不该有不同措辞。

「截图所有窗口」（高级菜单）同理：候选窗在服务进程就地截，其余三个（状态气泡 /
悬停提示 / Toast）经同一请求交给 `.app`。**右键菜单不截**——Windows 上它是我们自绘的
窗口（`popup_menu.rs`），macOS 上却是原生 NSMenu，截它同样只能走屏幕录制授权那条路。
两侧的成功数由 `already` 字段（服务端放进请求、`.app` 原样带回）相加，合成**一条**
Toast；分开弹会在四个浮窗都可见时连弹四条。

两个实现细节：

- **不用 `CGWindowListCreateImage`**：那条路自 macOS 14 起要「屏幕录制」授权，而本输入法
  申请的是「辅助功能」。为一个截图菜单项再要一项更敏感的授权不成比例，且用户拒授权后
  只会得到一张黑图——比功能不存在更糟。自己的视图自己渲染不需要任何授权。
- **走 `layer.render(in:)` 而非 `cacheDisplay(in:to:)`**：这两个浮窗的背景是**图层属性**
  （`layer.backgroundColor` + `cornerRadius`）而不是 `draw(_:)` 画出来的，而 `cacheDisplay`
  只走视图绘制路径——对这种视图会截出一张只有文字、没有底色和圆角的透明图。

未接的变体落在 `Forwarder::handle` 的 `other =>` 兜底臂，只打一条 debug 日志。
**新接一个变体时同步更新本表**。

### 按应用配置 / 焦点重型段

`.app` 的 `FocusGained` 载荷此前只有 12 字节（pid 占位 + inputScopeMask），而 Rust
`FocusGainedPayload::from_bytes` 的下限是 36 —— **解码恒失败，整个焦点重型段从未在
macOS 上跑过**。载荷现已补齐为与 Windows 同构的 39 字节 + 尾部 `bundleIdLen + bundleId`。

宿主名的来源两平台不同：Windows 由服务进程 `OpenProcess` 反查进程名，macOS 反查不到
（服务里的 `process_name` 恒返回空串），只能由 `.app` 取 IMKit `client.bundleIdentifier()`
随焦点事件送上来。落点是既有的 `pid_names` 缓存，`update_active_compat` 已改为
**缓存优先于反查** —— 缓存之后的两平台路径完全一致。

`compat.toml` 的 `process` 字段是小写全等匹配，故 `wechat.exe` 与
`com.tencent.xinwechat` 可并存于同一份规则表，不需要分平台的规则文件。

caret 段发全 0 是安全的：`apply_focus_caret` 见 `height == 0` 即返回，坐标另经
`CmdCaretUpdate` 上报。

### 输入源切换（activate_ime）

`keys.activate_ime` 在 Windows 上由 ctfmon 从 `DirectSwitchHotkeys` 注册表接管；macOS
无对应机制，改由**服务进程**注册 Carbon 全局热键 + `TISSelectInputSource`
（`wind-ui/src/input_source_macos.rs`）。放在服务进程是必须的：该热键的语义是「本输入法
没激活时也生效」，那时 `.app` 通常没在跑。

**语义差异不可消除**：Windows 是 per-app 切换（只改当前前台应用），macOS 的
`TISSelectInputSource` 是全局切换 —— 系统没有「只改这个 app」的公开 API。设置界面的
hint 已如实写明两平台差异。

### 候选窗截图 / 提示复制

macOS 的候选窗**像素本来就在服务进程里**（我们光栅化后经 SHM 推给 `.app`），故截图不需要
`.app` 参与：`Forwarder::capture_candidate` 直接拿 `render_frame()` 的 buffer 编码存盘。
悬停提示的文本同理（随帧下发，服务侧有 `last_tip`），复制也不需要 `.app`。

反过来，**状态气泡与悬停提示的截图做不了**：那两者是 `.app` 侧原生 NSPanel，像素不在本进程。

两处踩过的坑：

- `screenshot::timestamp()` 在非 Windows 曾恒返回 `"00000000_000000"`，截图文件名不含真实
  时间 → 第二张起静默覆盖第一张，而操作本身"成功"。现用 `chrono::Local`。
- `copy_bgra_to_clipboard` 在非 Windows 曾是 `Ok(())` 空实现——**报告成功却什么都没做**，
  上层据此弹「已截图到剪贴板」，用户粘贴才发现是空的。现走 PNG 临时文件 + `osascript`
  的 `«class PNGf»`（与文本剪贴板走 `pbcopy` 同一路数，服务进程不必引入 AppKit）；
  其它平台改为如实报错而非谎报成功。

### 系统明暗

`theme_style::system_prefers_dark` 的 macOS 分支读全局偏好 `AppleInterfaceStyle`
（浅色时该键**不存在**，深色为 `"Dark"`）。走 `CFPreferencesCopyAppValue` 而非读
`.GlobalPreferences.plist`：偏好由 cfprefsd 托管并带写回延迟，直接读文件会拿到用户刚改过、
尚未落盘的旧值。

### 全局热键（Carbon）

实现在 `wind-ui/src/global_hotkey_macos.rs`，选 Carbon `RegisterEventHotKey` 而非
CGEventTap / NSEvent 全局监听：后两者要「辅助功能」授权（ad-hoc 重部署 cdhash 一变就
失效），而全局热键的语义恰恰是「本输入法没激活时也得生效」，授权掉了就是静默失灵。

**线程约定不可省**：热键事件只投递到**主线程**的 Carbon 事件队列。因此服务在 macOS 上
主线程跑 `run_main_loop()`（CFRunLoop），「重启服务」的等待挪到辅助线程、收到信号后
`stop_main_loop()`；forwarder 线程调 `apply()` 时只入队 + 唤醒，真正的注册/撤销一律在
主线程的 perform 回调里做。把注册直接放到 forwarder 线程能编过也不报错，但 Carbon Event
Manager 是主线程亲和的，症状会是热键**时灵时不灵**。

修饰键按**物理键直译**：Alt→Option、Win→Command，不做「Ctrl 自动换 Command」的翻译
——否则设置界面显示的与实际生效的不是一回事。VK→CGKeyCode 复用 `wind_keys::key_inject::
vk_to_cgkeycode`（与按键注入同源，禁止另起一张表）。该表已覆盖修饰键 / 功能键 / F1-F12 /
字母 / 数字 / OEM 符号键（`[` `]` `;` `'` `,` `.` `/` `\` `` ` `` `-` `=`）——出厂默认
`keys.activate_ime = "ctrl+shift+["` 正落在 `VK_OEM_4` 上，漏了它该功能就开箱即哑。
仍未覆盖的 VK 跳过并 warn，**不能**当成 keycode 0（那会注册出一个按 `a` 就触发的热键）。

另有一处协议侧的既有约束，不是"漏做"而是设计所限：

- **`CMD_INPUT_STATS`（输入统计）无上报端**：Win 侧由 TSF DLL 在英文模式下上报，
  mac 的 `.app` 未采集，故英文输入不计入统计。

（此处曾记有「`CMD_CANDIDATE_SCROLL` 滚轮翻页 macOS 用不了」——那是 0x0211 码位被
`CMD_FRONT_CONTEXT` 同方向复用所致，该复用已消除，`FRONT_CONTEXT` 迁到 0x0215。
滚轮**现已实现**，且是跨平台的一份实现：`Coordinator::handle_candidate_scroll` 把它
解释成「上下键调整高亮项」，到页边界翻到相邻页。此前它是 trait 上的空实现，Windows 的
host-render DLL 一直在发这个帧、服务端收下什么也不做——两个平台都无效。
`.app` 侧在 `CandidateContentView.scrollWheel` 采集，触控板须攒够一格再发：
`hasPreciseScrollingDeltas` 的一次轻扫会来几十个极小 delta，逐个上报会让高亮飞过整页。）

## 安装与 TIS 注册

`.app` 工程层的必要条件（缺一项就注册不上或切不过去）：

- bundleID 含 `.inputmethod.` 字符串（Apple 第一步 filter，不含直接 skip）
- Info.plist 全字段（ComponentInputModeDict + ts* + TISInputSourceID + ISO 15924 脚本码）
- Bundle 结构（Contents/{Info.plist, MacOS, Resources/lproj, _CodeSignature, PkgInfo}）
- Hardened runtime（`codesign --options runtime`）
- 真证书签名（本机自签 trusted，`scripts/mac/dev.sh sign-setup` 建）
- IME 自身 `--register-input-source` 子命令 + RunLoop 常驻（TIS 注册是进程级
  lifecycle，register API 调完进程退出 mode 会被清掉）
- `TISRegisterInputSource(bundleURL)` 真把 mode 持久写进 TIS DB

本机自签路径（`sign-setup` + `p1`/`pm1`）可以端到端跑起来，无需 Apple Developer
Program 公证——已多次实测。

**但签名身份的选择另有一层与 TCC（辅助功能授权）相关的讲究**，`dev.sh` 因此默认
**优先挑带 Team ID 的 Apple 签发证书**（Apple Development / Developer ID Application），
没有才回落自签的 `WindInput Dev`：

| 签名身份 | TCC 存的授权要求 | 重新部署后 |
|---|---|---|
| 带 Team ID | `anchor apple generic` + `subject.OU=<TeamID>`，与具体构建无关 | **继续有效** |
| 自签 / 无 OU | 一条裸 `cdhash H"…"`，钉死在当次构建 | **失效**，但系统设置里开关仍显示为开 |

后者的表现是：命令直通车的按键合成、智能配对的宿主光标回退**静默不工作**，而用户在
系统设置里看到的是「已授权」。2026-08-12 实测确认（TCC.db 里我们的 csreq 就是裸 cdhash，
与当时安装的 .app cdhash 已对不上）。

`install_app` 装完会自动比对一次并在失配时给出解法；`.app` 启动也会记一行授权状态。
临时解法是 `tccutil reset Accessibility <bundleID>` 后重新授权一次。

**重装后偶发不稳定**：重新部署 `.app` 后有时切不过去 / 系统设置里状态不对。
**注销重登**（Log Out）即恢复——TIS 数据库与输入源列表是登录会话级的缓存，
`lsregister -f` 与重注册刷不动它。碰到这种情况先注销，不要往「代码坏了」方向排查。

> 早期文档曾记有一条「macOS 26 Tahoe 对非 Notarized IME 有系统设置 UI 硬墙、必须公证」
> 的结论，并附 ensan-hcl sample 对照实验。**该结论已被实测推翻**，多半是当时撞上了上面
> 这个需要注销的缓存问题。勿据此认为本机测不了。

## 变体共存（release/dev）

`.app` 无编译期变体标记，一律从**自身 bundleID 后缀**派生变体：
`Bundle.main.bundleIdentifier` 末尾为 `Dev` → dev。`BridgeEndpoints.variantSuffix`
（`""` / `"_debug"`）决定运行时目录（`…/WindInput[Dev]`、socket / SHM），与 Rust 侧
`wind-config/src/variant.rs` 对齐；`ModeStatusController` 菜单头读 `CFBundleDisplayName`
（release「清风输入法」/ dev「清风输入法开发版」）、`openSettings` 按变体启动对应设置应用
（`com.wails.wind_setting[_debug]`）。

变体化的完整对照表在 `scripts/mac/dev.sh` 头部注释「变体身份」一节。

（`com.wails.` 前缀是历史包袱——设置程序早已是 Rust + windui，不是 Wails。改它要同步
`scripts/mac/dev.sh` 与所有已安装机器，**不值当**。）

### 设置深链（扩展信封 `settings.open`）

两处约束，都踩过：

1. **argv 必须由 Rust 侧切好**。曾经的做法是发「页名 + 参数」的空格串，让 Swift 按 shell
   风格切词——等于让另一门语言去猜 `build_settings_args` 的引号规则，含空格的值一切就散。
   现在 `handle_menu.rs::settings_argv` 在 Rust 侧切完，经信封发 `{"args":[…]}`，Swift 只做
   一次 JSON 取值。（更早还有一版直接把整串塞进 `--page=`，设置端解析不出页 id、只开默认
   页——**冷启动即复现**。）
2. **设置程序已在运行时 LaunchServices 不重传 arguments**，只激活窗口。深链要在已开着的
   窗口上生效，靠的是 windui 的单实例转发（`wind-ui-rust/src/single_instance/unix.rs`，
   Unix domain socket）。那一层在 macOS 上一度是空实现，深链因此"时灵时不灵"。

排查入口：设置程序日志里的 `二次实例 argv: [...]`。有这行 = argv 送到了，问题在 cli 解析；
没有 = 转发层没通。

### 浮窗拖动与位置固定

候选窗与状态气泡都能拖，落点是否**持久化**取决于当前定位方式（两平台同一套语义，判定在
`handle_menu.rs::save_candidate_pos` / `save_status_tip_pos`）：

- `fixed`（固定位置）→ 写回 `custom_x/custom_y`，永久生效；
- `follow_caret`（跟随光标）→ **不落盘**，拖动只是把窗口临时挪开。

三处平台差异：

1. **拖动手势在 `.app`**（NSPanel 在那边），落点经上行扩展信封 `pos.candidate` /
   `pos.status_tip` 回报，body 是 `{"x":…,"y":…}`。走信封而非专用码位：拖动是低频动作，
   见 `ext_kind` 的两档划分。
2. **候选窗的固定坐标由服务进程算定**（`candidate_window.rs` macOS 分支的 `place_fixed`），
   帧里带 `FLAG_ABSOLUTE_POS`(0x8)。这一位不能省：`.app` 收普通帧会自作主张做「下方放不下
   就翻到光标上方」，而固定位置的窗口本来就不跟光标走，固定点一靠近屏幕底边就被弹到顶上。
3. **`ReportStatusTipPos` 要多一次往返**。协调器在用户点「固定位置」时问「你现在在哪」，
   好以当前实际位置落盘（否则气泡会跳到上次保存的坐标）。候选窗的答案服务进程自己有
   （`Forwarder::last_pos`，即最后一帧下发的坐标）；气泡的落点是 `.app` 按屏幕边界钳出来
   的，只能下发 `pos.status_tip.query` 去问，答案走上行 `pos.status_tip` 回来。

**坐标换算收口在 `WireGeometry`**（`WindInputKit/IPC/`）。wire 是「主屏左上为原点、y 向下」，
Cocoa 是「主屏左下为原点、y 向上」。以前只有「服务端算好 → 摆窗」一个方向，换算散在各
panel 的 `show()` 里；拖动引入了反方向，两边一旦不互逆，每轮「拖动 → 落盘 → 重新显示」都
累计一次偏移，窗口逐次漂走——`WireGeometryTests.testWireAndCocoaRoundTrip` 就是钉这个的。
参照屏必须取 `NSScreen.screens.first`（带菜单栏的主屏）而非 `NSScreen.main`（随 key window
变），两者在单屏机器上恰好相同，所以用错了在开发机上永远显不出来。

`.app` 侧还有个会话内的落位冻结（`CandidatePanel.dragPin`）：拖过之后窗口就钉在那儿，候选
内容刷新（翻页/继续输入）不会把它拽回光标处，`hidePanel()` 清除。对应 Windows 的 `drag_pin`
——那边在服务进程里，macOS 的鼠标事件不经过服务进程，只能记在 `.app`。

**关键**：两变体 `.app` 可执行同名 `WindInput`，进程定位须用 `.app` 路径；
SHM / socket / config 全部变体隔离（漏一处即冲突，如曾漏 SHM → 开机后开发版候选框不显示）。

## 目录

| 路径 | 角色 |
|------|------|
| `Package.swift` | SwiftPM 清单，4 个 target（kit / smoke / app / tests） |
| `Sources/WindInputKit/IPC/ProtocolTypes.swift` | 协议常量 + payload 类型 + endpoint 路径 |
| `Sources/WindInputKit/IPC/WireGeometry.swift` | 浮窗落位的 wire ↔ Cocoa 坐标换算（双向，互逆性有 round-trip 测试兜底）|
| `Sources/WindInputKit/IPC/BinaryCodec.swift` | 帧 encode/decode。`encodeFocusGainedFrame(inputScopeMask:)`: FocusGained 帧布局 `pid:u32(0占位) + inputScopeMask:u64`（bit31=IS_PASSWORD 标记密码框；旧版空帧=mask 0） |
| `Sources/WindInputKit/IPC/BridgeClient.swift` | UDS 阻塞客户端；`init(socketPath:, ioTimeoutMs:)` 可选 I/O 超时（`SO_RCVTIMEO`/`SO_SNDTIMEO`，0=不设）。request 连接设 2s——服务卡死/重启时同步 `readFrame` 超时抛错而非在 IMKit 主线程无限 hang（上层 catch → reconnect 自愈）；push 连接（PushClient）必须保持 0（长期空闲等服务端推送，否则被读超时误判断连）。**`connect()` 必设 `SO_NOSIGPIPE`**: 否则对端（Rust 服务）重启后向死连接 `write` 触发 SIGPIPE → 默认处置直接**杀死 .app 进程**（表现为服务重启后输入法彻底失灵、需强制重启前端）；设此项后 `write` 改返回 EPIPE 由 `send()` 抛错交上层重连。request/push/sendClient 都用此构造，一处覆盖 |
| `Sources/WindInputKit/IPC/BridgeResponseRouter.swift` | 把响应帧路由到 `TextInputClient` 调用。**新增 cmd 必须在此显式接一臂**——`default` 是"消费按键但不出字"，漏接的表现是按键被吃掉、屏幕上什么都没有（历史案例：`commitThenDefer` 漏接导致码表顶码上屏丢字） |
| `Sources/WindInputSmoke/main.swift` | `swift run wind-smoke` — 连 bridge + push，打印帧 |
| `Sources/WindInputApp/main.swift` | `.app` 入口：默认启 IMKServer + NSApp.run; 也支持 `--register-input-source` / `--enable-input-source` / `--select-input-source` 子命令。`--register-input-source` **总是重注册 + RunLoop 常驻**，不因「已注册」早退（重新部署 .app 的 cdhash 变，必须重注册刷新否则无法切换；install 已先杀旧守护，早退会让注册失去维持进程而失效）。变体隔离：mode-id 检查用 `Bundle.main.bundleIdentifier + "."` 前缀精确匹配（避免 `WindInput`/`WindInputDev` 子串互串） |
| `Sources/WindInputApp/Controller/InputController.swift` | `IMKInputController` 子类，同步 KeyEvent roundtrip，路由 PassThrough/Consumed/CommitText/UpdateComposition; `activateServer`/`deactivateServer` 发 FocusGained/FocusLost（驱动协调器 imeActivated → 指示器）; **密码框适配**（对齐 Win 36614ae）: `activateServer` 用 `IsSecureEventInputEnabled()` 探测，命中则在帧 payload 携带 InputScope bitmask 的 IS_PASSWORD 位（bit31），协调器据此对密码框强制英文半角直通（模式图标不变）; `deactivateServer` 失焦时若仍有 marked text 先 `setMarkedText("")` 清残留 + 清本端 composition。`menu()` 重写：点系统输入源图标弹出统一菜单（复用 UnifiedMenuBuilder）; 选中项经 IMK `doCommandBySelector` → `imkMenuCommand:` 读 NSMenuItem.tag 回发 CmdMenuAction。**自愈重连**: bridge 连接持有在实例字段，`activateServer`/`handle` 入口 `ensureConnected()` 懒重连; `handle` 的 `send`/`readFrame` 经 `sendAndApply` 执行，失败 catch → `reconnect()` 后**用新连接重试当前键一次**（服务重启后第一个键就自愈，不丢字）; 连接用 `ioTimeoutMs=2000` + `SO_NOSIGPIPE`。**智能配对光标**: `router.moveHostCursor` 闭包把 kit 层的 `CursorMove` 意图用 `KeySynthesizer` 合成 ←/→ 方向键（主线程 async，排在 insertText 后）; 需辅助功能授权，未授权静默降级 |
| `Sources/WindInputApp/Controller/KeyHandler.swift` | `NSEvent.keyCode` → Win VK 映射 + Modifier 编码 + KeyEvent 帧构造 |
| `Sources/WindInputApp/UI/CandidatePanelHost.swift` | 候选框承载层：订阅 push，收 CmdHostRenderFrame→SHM→NSPanel、CmdCandidateRects→hit-test、CmdModeStatus→ModeStatusController、CmdTooltip*/CmdStatus*→气泡、CmdToast*→ToastPanel; 命令直通车按键：CmdKeyTap/Hold/Release/Seq→`KeySynthesizer` CGEvent 合成，CmdKeyType→`activeResponder.applyPushResponse`→router `insertText` 上屏; 鼠标选词回发。导出 `unifiedMenuItems()`/`sendMenuAction(_:)` 供三处菜单（候选框右键/菜单栏指示器/系统输入菜单）复用同一 IPC 请求与回发路径 |
| `Sources/WindInputApp/UI/KeySynthesizer.swift` | 命令直通车按键合成（key.tap/seq/hold/release）: canonical 键名→CGKeyCode + 修饰键→CGEventFlags 映射，经 `CGEvent.post(tap: .cgSessionEventTap)` 向聚焦应用注入; **需「辅助功能」授权**，`ensureTrusted()` 未授权时弹一次系统请求并放弃本次（ad-hoc 签名重部署 cdhash 变会使旧授权失效，须重授）。key.type / clip.paste 文本上屏不走此处，走 `client.insertText` 免授权 |
| `Sources/WindInputApp/UI/CandidatePanel.swift` | 候选框 NSPanel（borderless 浮窗）+ 自绘 bitmap + 鼠标命中/悬停; 空白处右键经 UnifiedMenuBuilder 弹统一菜单; 空白处左键**拖动**整窗，松手回报 `pos.candidate`（见「浮窗拖动与位置固定」）|
| `Sources/WindInputApp/UI/UnifiedMenuBuilder.swift` | 把服务下发的统一菜单树（MenuItemData）构建为原生 NSMenu; 三处共用。两种派发：`.inProcess`（普通 NSMenu，builder 作 target 回调）与 `.imkCommand`（系统输入菜单，target=nil + selector，IMK 经 doCommandBySelector 路由）; 菜单 id 统一经 NSMenuItem.tag 回传 |
| `Sources/WindInputApp/UI/ModeStatusController.swift` | 菜单栏模式指示器（NSStatusItem）: 收 CmdModeStatus 显示中英/全半角/标点/方案; 下拉菜单（NSMenuDelegate 动态填充）复用统一菜单树，点击回发 CmdMenuAction，服务未就绪时回退只读状态 |
| `Sources/WindInputApp/UI/TooltipPanel.swift` | 候选悬停 tooltip NSPanel。配色与拆字字根字体路径随 `CmdTooltipShow` 下发（服务侧 `manager_macos.rs` 从主题求值成 `#RRGGBBAA`）; 空串则用内置深色默认 |
| `Sources/WindInputApp/UI/StatusBubblePanel.swift` | 锚 caret 的模式状态气泡（收 CmdStatusShow）; 可拖动，松手回报 `pos.status_tip`，并应答服务端的 `pos.status_tip.query` |
| `Sources/WindInputApp/UI/PanelGeometry.swift` | 把 `WireGeometry` 的纯几何接到真实 NSScreen（只决定「哪块屏」这一件事，其余在 kit 里可测）|
| `Sources/WindInputApp/UI/ToastPanel.swift` | 屏幕级 Toast 通知 NSPanel（词库就绪/错误等）: 收 CmdToastShow（标题+正文+bg/fg/accent+position+时长）渲染暗色圆角卡片 + 左侧 accent 条，按 durationMs 自动隐藏（0=5000，<0 常驻）; 点击穿透 |
| `Sources/WindInputApp/Resources/Info.plist` | IMK 元数据：ComponentInputModeDict / TISInputSourceID / LSUIElement（**不可**设 LSBackgroundOnly，否则候选 NSPanel 无法显示）/ InputMethodConnectionName = bundleID_Connection。**不可设 `tsInputModeDefaultStateKey`**: 会让 mode 注册即「已启用」却不落盘 AppleEnabledInputSources，导致「+ 添加输入法」列表过滤掉它、主列表又没有 → 中英文分组两头都看不见（Tahoe 实测） |
| `Sources/WindInputApp/Resources/AppIcon.icns` | 应用图标（Finder/安装器/关于面板），plist `CFBundleIconFile=AppIcon` 引用。与菜单栏单色 `menu_icon.pdf`（`tsInputMethodIconFileKey`）互不相干 |
| `Sources/WindInputApp/Resources/WindInput.entitlements` | App Sandbox 关闭（IMKit `.app` 与服务 UDS 共享文件路径需要） |
| `Sources/WindInputApp/Resources/{zh-Hans,en}.lproj/InfoPlist.strings` | 本地化菜单名（"清风输入法" / "WindInput"） |

## 协议同步铁律

修改 cmd id 或帧布局必须三处同步：

- `wind_input/crates/wind-ipc/src/protocol.rs` + `codec.rs`（**Rust SSOT**）
- `wind_tsf/include/BinaryProtocol.h`（Win）
- `wind_macos/Sources/WindInputKit/IPC/{ProtocolTypes,BinaryCodec}.swift`（本目录）

⚠️ **别按"名字像 Windows 的"来判断要不要接**：`CommitThenDefer`/`CommitAndHold`
的名字来自 TSF 的「吃了再吐」机制，但产出它们的是跨平台协调器逻辑
（码表顶码 direct_commit 在 `handle_candidate.rs`），macOS 一样会收到。
判断依据只看**谁产出**，不看名字。

## 本地开发

需要的工具：Xcode（含 swift 5.9+）、Rust toolchain。

```bash
cd wind_macos
swift test          # 单测
swift build         # 编 kit / smoke / app

# 另一终端：起 Rust 服务
../scripts/mac/dev.sh m2 && ../scripts/mac/dev.sh pm2

# smoke（默认监听 push 10 秒）
swift run wind-smoke
```

期望输出：

- 请求通道：`[smoke] <- recv cmd=0x0401 len=0`（Consumed）或 `cmd=0x0002 len=0`（PassThrough）
- push 通道：至少看到 `cmd=0x0206`（StatePush）一帧

## 构建 / 部署

统一入口 `scripts/mac/dev.sh`（命令面对齐 Windows 的 `scripts/dev.ps1`）：

```bash
scripts/mac/dev.sh sign-setup   # 一次性：建自签证书 "WindInput Dev"
scripts/mac/dev.sh gd           # 下载词库 + 生成 + 组装 data/ → build_mac/data
scripts/mac/dev.sh 1            # release 全构建（service + app + 数据校验）
scripts/mac/dev.sh p1           # 系统安装全部（service LaunchAgent + IME app + 设置 app）
scripts/mac/dev.sh m1           # 仅构建 .app        pm1  仅安装 .app
scripts/mac/dev.sh status       # 诊断：service pid / socket / 签名 / 进程
scripts/mac/dev.sh logs         # 跟踪 service + IME 日志
scripts/mac/dev.sh 8            # 生成 .pkg 安装包
scripts/mac/dev.sh hooks        # 激活 pre-commit（提交前 cargo fmt --check）
```

`d` 前缀为 dev 变体（`d1` / `pd1` / `dm1` / `pdm1` / `d8`）。
完整命令表与变体身份说明见 `scripts/mac/dev.sh -h`（或脚本头部注释）。

**固定用自签证书签名**：macOS 26 的 IME 必须真 Authority（纯 ad-hoc 装上能切但 IMK 不拉起
控制器 → 无法输入）；且证书签名 cdhash 稳定，重装不掉 TIS 注册，不用每次去系统设置重加。
