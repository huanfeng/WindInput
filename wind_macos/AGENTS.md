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
- `TISRegisterInputSource(bundleURL)` 真把 mode 持久写进 TIS DB, `swift scripts/list_input_sources.swift` 能看到 mode `selectable=true`

**但 macOS 26 Tahoe 在系统设置 UI 显示层有 Notarization 硬墙**:
- `TISEnableInputSource` 返回 `OSStatus=0` 但 isEnabled 仍 false (silent no-op)
- `TISSelectInputSource` 返回 `OSStatus=-50` (paramErr) 直接拒绝
- 系统设置 → 键盘 → + → 简体中文 看不到本输入法
- 手动 `defaults write AppleEnabledInputSources -array-add` 硬塞 user pref 也无效

**对照实验铁证**: clone [ensan-hcl/macOS_IMKitSample_2021](https://github.com/ensan-hcl/macOS_IMKitSample_2021) (Apple-recommended 80 行 swift sample, 完整 sandbox+mach-register exception entitlement, 我们都没用), 同样 ad-hoc 路径 build + install, **同样系统设置 UI 不显示**. 印证: 是 Apple Tahoe 对所有非 Notarized IME 一刀切, 与项目代码 / plist / entitlement / 签名 (ad-hoc vs Personal Team) 全无关.

**结论**: 真正端到端 IMKit 测试需要 Apple Developer Program (\$99) + `xcrun notarytool submit` 走完公证 (走 PR-A.5 / PR-C). 或在 macOS 15 Sequoia / 14 Sonoma (虚拟机或别的机器, 据社区反馈那里 ad-hoc 仍可用). 当前 PR 工作重心: 完善代码层 (M2.2+ composition / candidates / commit), 用 swift test 覆盖 ± 逻辑, 不被 IMK 注册门槛阻塞.

详见 `docs/design/macos-imkit-plan.md` §12 "踩坑记录" 章 (§12.4 bundleID filter / §12.5 register 进程必须常驻 / §12.6 系统设置 UI Notarization 硬墙 + ensan 对照).

## 目录

| 路径 | 角色 |
|------|------|
| `Package.swift` | SwiftPM 清单, 4 个 target (kit / smoke / app / tests) |
| `Sources/WindInputKit/IPC/ProtocolTypes.swift` | 协议常量 + payload 类型 + endpoint 路径 |
| `Sources/WindInputKit/IPC/BinaryCodec.swift` | 帧 encode/decode (镜像 Go `internal/ipc/binary_codec.go`) |
| `Sources/WindInputKit/IPC/BridgeClient.swift` | UDS 阻塞客户端 |
| `Sources/WindInputSmoke/main.swift` | `swift run wind-smoke` — 连 bridge + push, 打印帧 |
| `Sources/WindInputApp/main.swift` | `.app` 入口: 默认启 IMKServer + NSApp.run; 也支持 `--register-input-source` / `--enable-input-source` / `--select-input-source` 子命令 (镜像 Squirrel/RIME 路径) |
| `Sources/WindInputApp/Controller/InputController.swift` | `IMKInputController` 子类, 同步 KeyEvent roundtrip, 路由 PassThrough/Consumed/CommitText/UpdateComposition; `activateServer`/`deactivateServer` 发 FocusGained/FocusLost (驱动 Go imeActivated → 工具栏/模式指示器) |
| `Sources/WindInputApp/Controller/KeyHandler.swift` | `NSEvent.keyCode` → Win VK 映射 + Modifier 编码 + KeyEvent 帧构造 |
| `Sources/WindInputApp/UI/CandidatePanelHost.swift` | 候选框承载层: 订阅 push, 收 CmdHostRenderFrame→SHM→NSPanel, CmdCandidateRects→hit-test, CmdModeStatus→ModeStatusController; 鼠标选词回发 |
| `Sources/WindInputApp/UI/CandidatePanel.swift` | 候选框 NSPanel (borderless 浮窗) + 自绘 bitmap + 鼠标命中/悬停 |
| `Sources/WindInputApp/UI/ModeStatusController.swift` | 菜单栏模式指示器 (NSStatusItem): 收 CmdModeStatus 显示中英/全半角/标点/方案; 下拉菜单展示状态 |
| `Sources/WindInputApp/Resources/Info.plist` | IMK 元数据: ComponentInputModeDict / TISInputSourceID / LSUIElement (不可设 LSBackgroundOnly, 否则候选 NSPanel 无法显示) / InputMethodConnectionName = bundleID_Connection |
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
scripts/setup_signing.sh

# 2. build + install + 验证 TIS
SIGN_IDENTITY="WindInput Dev" scripts/redeploy.sh
```

`redeploy.sh` 会自动:
build .app → cp 到 `/Library/Input Methods/` → `lsregister -f` 刷 LS DB → 跑 `.app --register-input-source` 调 TIS API → `--enable-input-source` 启用 mode → 验证 `swift scripts/list_input_sources.swift` 里出现 `to.feng.wind_input.mode`.

详细脚本说明: `../scripts/AGENTS.md` 中 build_macos_app.sh / install_macos_app.sh / redeploy.sh / setup_signing.sh / list_input_sources.swift.

## 下一步 (M2.2)

- 解码 push pipe 的 `CmdCandidatesShow` (uicmd 0x0601), NSLog 候选词
- 完善 InputController `applyResponse`: 处理 `CmdCommitText`/`CmdUpdateComposition` 的 payload 解码 + `insertText:`/`setMarkedText:` 真实调用
- 数字键 1-9 选词: 发 `CmdCommitRequest` (0x0104) + 等响应 commit
- `IMKInputController.attributes(forCharacterIndex:lineHeightRectangle:)` 拿屏幕坐标推 `CmdCaretUpdate`

参考: `docs/design/macos-imkit-plan.md` §5 M2 / §4 协议.
