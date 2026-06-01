# wind_macos

macOS IMKit `.app` 工程 (PR-A). 与 Win 端 `wind_tsf/` DLL 对位, 与跨平台 Go 服务 (`wind_input/`) 通过 Unix Domain Socket 通信.

## 当前阶段

**PR-A M1 ✅ + M2.1 ✅** — bridge 协议通路与 `.app` IMKit 骨架就位:

- 协议层 `WindInputKit` (BinaryCodec + BridgeClient + ProtocolTypes)
- 命令行 smoke 工具 `wind-smoke` (`Sources/WindInputSmoke/`)
- IMKit `.app` 进程入口 + InputController + KeyHandler (`Sources/WindInputApp/`)
- IMK server 注册 + 自身 `--register-input-source` 子命令 (镜像 Squirrel/RIME 路径)
- 单元测试 (`Tests/WindInputKitTests/`)

**未完成 (M2.2+)**: composition `setMarkedText` / commit `insertText` / push pipe 候选解码与 NSPanel 渲染, 见 `docs/design/macos-imkit-plan.md` 各里程碑.

## 已知限制 (macOS 26 Tahoe 系统设置 UI 显示 Notarization 硬墙)

`.app` 工程层已全部做对:

- bundleID 含 `.inputmethod.` 字符串 (Apple 第一步 filter, 不含直接 skip)
- Info.plist 全字段 (ComponentInputModeDict + ts* + TISInputSourceID + ISO 15924 脚本码)
- Bundle 结构 (Contents/{Info.plist, MacOS, Resources/lproj, _CodeSignature, PkgInfo})
- Hardened runtime (`codesign --options runtime`)
- 真证书签名 (本机自签 trusted 或 Personal Team Apple Development)
- IME 自身 `--register-input-source` 子命令 + RunLoop 常驻 (TIS 注册是进程级 lifecycle, register API 调完进程退出 mode 会被清掉)
- install 脚本后台 fork register 进程
- `TISRegisterInputSource(bundleURL)` 真把 mode 持久写进 TIS DB, `swift scripts_mac/test/list_input_sources.swift` 能看到 mode `selectable=true`

**但 macOS 26 Tahoe 在系统设置 UI 显示层有 Notarization 硬墙**:
- `TISEnableInputSource` 返回 `OSStatus=0` 但 isEnabled 仍 false (silent no-op)
- `TISSelectInputSource` 返回 `OSStatus=-50` (paramErr) 直接拒绝
- 系统设置 → 键盘 → + → 简体中文 看不到本输入法
- 手动 `defaults write AppleEnabledInputSources -array-add` 硬塞 user pref 也无效

**对照实验铁证**: clone [ensan-hcl/macOS_IMKitSample_2021](https://github.com/ensan-hcl/macOS_IMKitSample_2021) (Apple-recommended 80 行 swift sample, 完整 sandbox+mach-register exception entitlement, 我们都没用), 同样 ad-hoc 路径 build + install, **同样系统设置 UI 不显示**. 印证: 是 Apple Tahoe 对所有非 Notarized IME 一刀切, 与项目代码 / plist / entitlement / 签名 (ad-hoc vs Personal Team) 全无关.

**结论**: 真正端到端 IMKit 测试需要 Apple Developer Program (\$99) + `xcrun notarytool submit` 走完公证 (走 PR-A.5 / PR-C). 或在 macOS 15 Sequoia / 14 Sonoma (虚拟机或别的机器, 据社区反馈那里 ad-hoc 仍可用). 当前 PR 工作重心: 完善代码层 (M2.2+ composition / candidates / commit), 用 swift test 覆盖 ± 逻辑, 不被 IMK 注册门槛阻塞.

详见 `docs/design/macos-imkit-plan.md` §12 "踩坑记录" 章 (§12.4 bundleID filter / §12.5 register 进程必须常驻 / §12.6 系统设置 UI Notarization 硬墙 + ensan 对照).

## 变体共存 (debug/release)

`.app` 无编译期变体标记, 一律从**自身 bundleID 后缀**派生变体: `Bundle.main.bundleIdentifier` 末尾为 `Debug` → debug。`BridgeEndpoints.variantSuffix` (`""`/`"_debug"`) 决定运行时目录 (`…/WindInput[_debug]`, socket/SHM) 与 Go `buildvariant.Suffix()` 对齐; `ModeStatusController` 菜单头读 `CFBundleDisplayName` (release「清风输入法」/ debug「清风输入法开发版」)、`openSettings` 按变体启动对应设置应用 (`com.wails.wind_setting[_debug]`)。构建/安装/部署的变体化在 `scripts_mac/AGENTS.md` 变体共存表。**关键**: 两变体 `.app` 可执行同名 `WindInput`, 进程定位须用 .app 路径; SHM/socket/config 全部变体隔离 (漏一处即冲突, 如曾漏 SHM → 开机后开发版候选框不显示)。

## 目录

| 路径 | 角色 |
|------|------|
| `Package.swift` | SwiftPM 清单, 4 个 target (kit / smoke / app / tests) |
| `Sources/WindInputKit/IPC/ProtocolTypes.swift` | 协议常量 + payload 类型 + endpoint 路径 |
| `Sources/WindInputKit/IPC/BinaryCodec.swift` | 帧 encode/decode (镜像 Go `internal/ipc/binary_codec.go`) |
| `Sources/WindInputKit/IPC/BridgeClient.swift` | UDS 阻塞客户端；`init(socketPath:, ioTimeoutMs:)` 可选 I/O 超时（`SO_RCVTIMEO`/`SO_SNDTIMEO`, 0=不设）。request 连接设 2s——服务卡死/重启时同步 `readFrame` 超时抛错而非在 IMKit 主线程无限 hang（上层 catch → reconnect 自愈）；push 连接 (PushClient) 必须保持 0（长期空闲等服务端推送, 否则被读超时误判断连）。**`connect()` 必设 `SO_NOSIGPIPE`**: 否则对端 (Go 服务) 重启后向死连接 `write` 触发 SIGPIPE → 默认处置直接**杀死 .app 进程**（表现为服务重启后输入法彻底失灵、需强制重启前端）；设此项后 `write` 改返回 EPIPE 由 `send()` 抛错交上层重连。request/push/sendClient 都用此构造, 一处覆盖|
| `Sources/WindInputSmoke/main.swift` | `swift run wind-smoke` — 连 bridge + push, 打印帧 |
| `Sources/WindInputApp/main.swift` | `.app` 入口: 默认启 IMKServer + NSApp.run; 也支持 `--register-input-source` / `--enable-input-source` / `--select-input-source` 子命令 (镜像 Squirrel/RIME 路径)。`--register-input-source` **总是重注册 + RunLoop 常驻**, 不因「已注册」早退 (重新部署 .app 的 cdhash 变, 必须重注册刷新否则无法切换; install 已先杀旧守护, 早退会让注册失去维持进程而失效)。变体隔离: mode-id 检查用 `Bundle.main.bundleIdentifier + "."` 前缀精确匹配 (避免 `WindInput`/`WindInputDebug` 子串互串), 默认 mode-id 由自身 bundleID 拼 |
| `Sources/WindInputApp/Controller/InputController.swift` | `IMKInputController` 子类, 同步 KeyEvent roundtrip, 路由 PassThrough/Consumed/CommitText/UpdateComposition; `activateServer`/`deactivateServer` 发 FocusGained/FocusLost (驱动 Go imeActivated → 工具栏/模式指示器); `deactivateServer` 失焦时若仍有 marked text 先 `setMarkedText("")` 清残留 + 清本端 composition (与 Go `HandleFocusLost` 的 clearState 一致, 切回为全新输入)。`menu()` 重写: 点系统输入源图标弹出统一菜单 (复用 UnifiedMenuBuilder); 选中项经 IMK `doCommandBySelector` → `imkMenuCommand:` 读 NSMenuItem.tag 回发 CmdMenuAction。**自愈重连**: bridge 连接持有在实例字段, `activateServer`/`handle` 入口 `ensureConnected()` 懒重连; `handle` 的 `send`/`readFrame` 经 `sendAndApply` 执行, 失败 catch → `reconnect()` 后**用新连接重试当前键一次** (服务重启后第一个键就自愈, 不丢字、不需手动 `pkill`/切换输入法; 重连或重试仍失败才透传, 下一键再试); 连接用 `ioTimeoutMs=2000` (`requestIOTimeoutMs`) + `SO_NOSIGPIPE` (见 BridgeClient)。**智能配对光标**: `router.moveHostCursor` 闭包 (init 注入) 把 kit 层的 `CursorMove` 意图用 `KeySynthesizer` 合成 ←/→ 方向键 (主线程 async, 排在 insertText 后), 实现自动配对插入后光标回退中间 / 智能跳过右移; 需辅助功能授权, 未授权静默降级 |
| `Sources/WindInputApp/Controller/KeyHandler.swift` | `NSEvent.keyCode` → Win VK 映射 + Modifier 编码 + KeyEvent 帧构造 |
| `Sources/WindInputApp/UI/CandidatePanelHost.swift` | 候选框承载层: 订阅 push, 收 CmdHostRenderFrame→SHM→NSPanel, CmdCandidateRects→hit-test, CmdModeStatus→ModeStatusController, CmdTooltip*/CmdStatus*→气泡, CmdToast*→ToastPanel; 命令直通车按键: CmdKeyTap/Hold/Release/Seq→`KeySynthesizer` CGEvent 合成, CmdKeyType→`activeResponder.applyPushResponse`→router `insertText` 上屏; 鼠标选词回发。导出 `unifiedMenuItems()`/`sendMenuAction(_:)` 供三处菜单 (候选框右键/菜单栏指示器/系统输入菜单) 复用同一 IPC 请求与回发路径 |
| `Sources/WindInputApp/UI/KeySynthesizer.swift` | 命令直通车按键合成 (key.tap/seq/hold/release): canonical 键名→CGKeyCode + 修饰键→CGEventFlags 映射, 经 `CGEvent.post(tap: .cgSessionEventTap)` 向聚焦应用注入; **需「辅助功能」(Accessibility) 授权**, `ensureTrusted()` 未授权时弹一次系统请求并放弃本次 (ad-hoc 签名重部署 cdhash 变会使旧授权失效, 须重授)。key.type / clip.paste 文本上屏不走此处, 走 `client.insertText` 免授权 |
| `Sources/WindInputApp/UI/CandidatePanel.swift` | 候选框 NSPanel (borderless 浮窗) + 自绘 bitmap + 鼠标命中/悬停; 空白处右键经 UnifiedMenuBuilder 弹统一菜单 |
| `Sources/WindInputApp/UI/UnifiedMenuBuilder.swift` | 把 Go 下发的统一菜单树 (MenuItemData) 构建为原生 NSMenu; 候选框右键/菜单栏指示器/系统输入菜单三处共用。两种派发 (Dispatch): `.inProcess` (普通 NSMenu, builder 作 target 回调) 与 `.imkCommand` (系统输入菜单, target=nil + selector, IMK 经 doCommandBySelector 路由); 菜单 id 统一经 NSMenuItem.tag 回传 |
| `Sources/WindInputApp/UI/ModeStatusController.swift` | 菜单栏模式指示器 (NSStatusItem): 收 CmdModeStatus 显示中英/全半角/标点/方案; 下拉菜单 (NSMenuDelegate 动态填充) 复用统一菜单树, 与候选框右键菜单一致, 点击回发 CmdMenuAction, 服务未就绪时回退只读状态 |
| `Sources/WindInputApp/UI/ToastPanel.swift` | 屏幕级 Toast 通知 NSPanel (词库就绪/错误等): 收 CmdToastShow (标题+正文+bg/fg/accent+position+时长) 渲染暗色圆角卡片 + 左侧 accent 条, bottom_right/center 落位, 按 durationMs 自动隐藏 (0=5000, <0 常驻); 点击穿透。区别于锚 caret 的 StatusBubblePanel |
| `Sources/WindInputApp/Resources/Info.plist` | IMK 元数据: ComponentInputModeDict / TISInputSourceID / LSUIElement (不可设 LSBackgroundOnly, 否则候选 NSPanel 无法显示) / InputMethodConnectionName = bundleID_Connection。**不可设 `tsInputModeDefaultStateKey`**: 会让 mode 注册即「已启用」却不落盘 AppleEnabledInputSources, 导致「+ 添加输入法」列表过滤掉它、主列表又没有 → 中英文分组两头都看不见 (Tahoe 实测) |
| `Sources/WindInputApp/Resources/AppIcon.icns` | 应用图标 (Finder/安装器/关于面板), plist `CFBundleIconFile=AppIcon` 引用。由 `wind_setting/build/appicon.png` 经 sips+iconutil 生成 (重生成命令见 `scripts_mac/build/app.sh` 注释)。与菜单栏单色 `menu_icon.pdf` (`tsInputMethodIconFileKey`) 互不相干 |
| `Sources/WindInputApp/Resources/WindInput.entitlements` | App Sandbox 关闭 (IMKit `.app` 与 Go UDS 共享文件路径需要) |
| `Sources/WindInputApp/Resources/{zh-Hans,en}.lproj/InfoPlist.strings` | 本地化菜单名 ("清风输入法" / "WindInput") |
| `Tests/WindInputKitTests/BinaryCodecTests.swift` | 帧 roundtrip + 边界 |

## 协议同步铁律

修改 cmd id 或帧布局必须三处同步:

- `wind_input/internal/ipc/binary_protocol.go` (Go SSOT)
- `wind_tsf/include/BinaryProtocol.h` (Win)
- `wind_macos/Sources/WindInputKit/IPC/{ProtocolTypes,BinaryCodec}.swift` (本目录)

完整速查: `../docs/wire-protocol-reference.md`.

## 本地开发

需要的工具: Xcode (含 swift 5.9+), Go 1.24+ (跑 Go 服务).

```bash
cd wind_macos

# 跑单测
swift test

# 启动 Go 服务 (另一终端)
cd ../wind_input && go run ./cmd/service

# 跑 smoke (默认监听 push 10 秒)
swift run wind-smoke
```

期望输出:

- 请求通道: `[smoke] <- recv cmd=0x0401 len=0` (Consumed) 或 `cmd=0x0002 len=0` (PassThrough)
- push 通道: 至少看到 `cmd=0x0206` (StatePush) 一帧

## 部署到 IME 目录 (M2.1 起)

```bash
# 1. 一次性建本机自签 cert (将来 macOS 15 / 上架后再换 Developer ID)
scripts_mac/deploy/setup_signing.sh

# 2. build + install + 验证 TIS
SIGN_IDENTITY="WindInput Dev" scripts_mac/deploy/redeploy.sh
```

`redeploy.sh` 会自动:
build .app → cp 到 `/Library/Input Methods/` → `lsregister -f` 刷 LS DB → 跑 `.app --register-input-source` 调 TIS API → `--enable-input-source` 启用 mode → 验证 `swift scripts_mac/test/list_input_sources.swift` 里出现 `to.feng.wind_input.mode`.

详细脚本说明: `../scripts_mac/AGENTS.md` 中 build/app.sh / deploy/install_app.sh / deploy/redeploy.sh / deploy/setup_signing.sh / test/list_input_sources.swift.

## 下一步 (M2.2)

- 解码 push pipe 的 `CmdCandidatesShow` (uicmd 0x0601), NSLog 候选词
- 完善 InputController `applyResponse`: 处理 `CmdCommitText`/`CmdUpdateComposition` 的 payload 解码 + `insertText:`/`setMarkedText:` 真实调用
- 数字键 1-9 选词: 发 `CmdCommitRequest` (0x0104) + 等响应 commit
- `IMKInputController.attributes(forCharacterIndex:lineHeightRectangle:)` 拿屏幕坐标推 `CmdCaretUpdate`

参考: `docs/design/macos-imkit-plan.md` §5 M2 / §4 协议.
