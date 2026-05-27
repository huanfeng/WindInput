<!-- Updated: 2026-05-26 -->

# macOS IMKit `.app` 工程开发计划 (PR-A)

> 本文档是 macOS 端开发的**实战工作手册**, 配合 [`macos-port.md`](macos-port.md) (设计) 与 [`../macos-build.md`](../macos-build.md) (Go 服务运行调试) 使用. 阅读顺序: 先读 `macos-port.md` 理解整体架构, 再读 `../wire-protocol-reference.md` 看协议, 然后回到本文按里程碑实施.

## 0. 全景

```
┌──────────────── macOS 用户机 ──────────────────┐
│                                                  │
│  WindInput.app (新工程, 本文档主题)              │
│    Swift / Obj-C                                 │
│    IMKInputController                            │
│    NSPanel 候选框                                │
│         ↕  UDS                                   │
│  wind_input (Go 服务, 已完成)                    │
│    ~/Library/Application Support/WindInput/      │
│      bridge.sock                                 │
│      bridge_push.sock                            │
│                                                  │
└──────────────────────────────────────────────────┘
```

`.app` 必须是**独立进程**(Bundle 形式), 由系统 `imklaunchagent` 在用户激活该输入法时自动拉起.

## 1. 目录结构 (建议)

把 macOS 工程放在仓库根的 `wind_macos/` 下, 与 Win 端的 `wind_tsf/` (C++ DLL) 对位:

```
WindInput/
├── wind_tsf/              # Windows TSF DLL (C++)
├── wind_input/            # Go 服务 (跨平台)
├── wind_setting/          # Wails 设置端 (Win 主)
└── wind_macos/            # ★ 新建: macOS IMKit .app 工程
    ├── AGENTS.md
    ├── README.md
    ├── WindInput.xcodeproj/    # 或用 SwiftPM
    ├── Sources/
    │   ├── App/
    │   │   ├── main.swift              # NSApplication 入口, 注册 IMK server
    │   │   ├── Info.plist              # IMK 元数据
    │   │   └── WindInput.entitlements  # 沙盒/网络/IPC 权限
    │   ├── Controller/
    │   │   ├── InputController.swift   # IMKInputController 子类
    │   │   ├── KeyHandler.swift        # NSEvent → bridge 协议帧
    │   │   └── CompositionState.swift  # 当前 composition / caret 状态
    │   ├── IPC/
    │   │   ├── BridgeClient.swift      # UDS 连接 + 主请求/响应
    │   │   ├── PushClient.swift        # UDS push 订阅
    │   │   ├── BinaryCodec.swift       # ipc 二进制协议编解码
    │   │   ├── UICmdCodec.swift        # uicmd Command/Event 编解码
    │   │   └── ProtocolTypes.swift     # ipc + uicmd 全部 cmd id 与 payload 镜像
    │   ├── UI/
    │   │   ├── CandidatePanel.swift    # NSPanel 候选框
    │   │   ├── CandidateView.swift     # 自绘候选词 (CoreText)
    │   │   ├── ToolbarItem.swift       # NSStatusItem (菜单栏图标)
    │   │   ├── ToastWindow.swift       # Toast 通知 NSPanel
    │   │   └── Theme.swift             # 主题色板 (与 Go 服务推送的 ThemeApplyPayload 对齐)
    │   └── System/
    │       ├── AppearanceWatcher.swift # NSApplication.effectiveAppearance KVO
    │       ├── PasteboardBridge.swift  # cmdbar 复制候选 → NSPasteboard
    │       └── KeyInjector.swift       # 命令直通车键注入 (CGEventCreateKeyboardEvent)
    ├── Tests/
    │   ├── BinaryCodecTests.swift      # 协议帧 roundtrip
    │   └── BridgeClientTests.swift     # 与 Go 服务的端到端
    └── Resources/
        └── Themes/                     # 内置主题资源 (可选, 也可只 Go 端解析后下发)
```

## 2. Info.plist 关键字段

IMKit 框架靠 `Info.plist` 元数据让系统识别这个 `.app` 是输入法:

```xml
<key>InputMethodConnectionName</key>
<string>WindInput_1_Connection</string>

<key>InputMethodServerControllerClass</key>
<string>WindInput.InputController</string>

<key>tsInputMethodCharacterRepertoireKey</key>
<array>
    <string>zh-Hans</string>
    <string>en</string>
</array>

<key>ComponentInputModeDict</key>
<dict>
    <key>tsInputModeListKey</key>
    <dict>
        <key>to.feng.wind_input.mode</key>
        <dict>
            <key>tsInputModeAlternateMenuIconFileKey</key>
            <string>icon.png</string>
            <key>TISInputSourceID</key>
            <string>to.feng.wind_input.mode</string>
            <key>TISIntendedLanguage</key>
            <string>zh-Hans</string>
            <key>tsInputModeCharacterRepertoireKey</key>
            <array>
                <string>zh-Hans</string>
                <string>en</string>
            </array>
            <key>tsInputModeKeyEquivalentModifiersKey</key>
            <integer>0</integer>
            <key>tsInputModeIsVisibleKey</key>
            <true/>
            <key>tsInputModeMenuIconFileKey</key>
            <string>icon.png</string>
            <key>tsInputModePaletteIconFileKey</key>
            <string>icon.png</string>
        </dict>
    </dict>
</dict>

<key>LSBackgroundOnly</key>
<true/>

<key>NSPrincipalClass</key>
<string>NSApplication</string>

<key>CFBundleIdentifier</key>
<string>to.feng.WindInput</string>

<key>CFBundlePackageType</key>
<string>APPL</string>
```

参考: 鼠须管 (Squirrel) 的 [`Info.plist`](https://github.com/rime/squirrel/blob/master/Info.plist).

## 3. 启动入口最小骨架

`Sources/App/main.swift`:

```swift
import Cocoa
import InputMethodKit

let kConnectionName = "WindInput_1_Connection"

// 1. 注册 IMK server (系统通过 Info.plist 中 InputMethodConnectionName 调度)
guard let mainBundleID = Bundle.main.bundleIdentifier else {
    fatalError("missing bundle identifier")
}
let server = IMKServer(name: kConnectionName, bundleIdentifier: mainBundleID)
_ = server  // 保持引用

// 2. NSApplication 主循环 (IMKit 走 Cocoa 消息泵)
let app = NSApplication.shared
app.setActivationPolicy(.accessory)  // 后台代理, 不显示 Dock 图标
app.run()
```

`Sources/Controller/InputController.swift`:

```swift
import Cocoa
import InputMethodKit

class InputController: IMKInputController {
    private var bridge: BridgeClient?
    private var compositionState = CompositionState()

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        // 每个 IMKInputController 实例独立连接 Go 服务 (PR-1 设计 方案 A)
        do {
            bridge = try BridgeClient.connect(socketPath: BridgeClient.defaultSocketPath())
            bridge?.onCommand = { [weak self] cmd in self?.handleCommand(cmd) }
        } catch {
            NSLog("BridgeClient connect failed: \(error)")
        }
    }

    deinit {
        bridge?.disconnect()
    }

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let bridge = bridge, event.type == .keyDown else {
            return false  // 让事件落到下层 (英文模式)
        }
        // KeyHandler 把 NSEvent 翻译成 KeyEventData payload, 走二进制协议
        let frame = KeyHandler.encodeKeyEvent(event)
        let response = bridge.sendAndWait(frame: frame)
        return KeyHandler.applyResponse(response, to: sender, state: compositionState)
    }

    private func handleCommand(_ cmd: UICmd) {
        switch cmd.type {
        case .candidatesShow(let payload):
            CandidatePanel.shared.show(payload)
        case .candidatesHide:
            CandidatePanel.shared.hide()
        case .toastShow(let payload):
            ToastWindow.shared.show(payload)
        // ... 其他命令
        default:
            NSLog("Unhandled UICmd: \(cmd.type)")
        }
    }
}
```

## 4. 协议实现关键点

完整协议参考: [`../wire-protocol-reference.md`](../wire-protocol-reference.md).

### 4.1 IPC 二进制帧 (与 wind_tsf 完全一致)

每帧: `Header(8 bytes) + Payload`

| 偏移 | 长度 | 字段 | 备注 |
|------|------|------|------|
| 0 | 2 | Version | LittleEndian uint16, 当前 0x1001 |
| 2 | 2 | Command | LittleEndian uint16, 见协议速查 |
| 4 | 4 | Length | LittleEndian uint32, payload 字节数 |

Swift 读帧示例:

```swift
class BinaryCodec {
    static func readFrame(from input: InputStream) throws -> (cmd: UInt16, payload: Data) {
        var header = Data(count: 8)
        let n = try input.readFull(into: &header, count: 8)
        guard n == 8 else { throw IPCError.eof }
        let version = header.uint16LE(at: 0)
        let cmd = header.uint16LE(at: 2)
        let length = header.uint32LE(at: 4)
        guard version & 0xF000 == 0x1000 else { throw IPCError.versionMismatch }
        var payload = Data(count: Int(length))
        if length > 0 {
            _ = try input.readFull(into: &payload, count: Int(length))
        }
        return (cmd, payload)
    }
}
```

### 4.2 UICmd 帧 (Go → IMKit 渲染指令)

uicmd Command 二进制布局: `cmdType(2) + session(8) + payload bytes`

每个 payload 字段按 little endian, 字符串用 `uint32 长度 + UTF-8 bytes`, 切片用 `uint32 count + 元素逐个`, nullable 用 `uint8 present + 内容?`. 完整字段表见 `../wire-protocol-reference.md`.

### 4.3 UICmd 事件 (IMKit → Go 反向)

布局: `evtType(2) + payload bytes`. 通常在用户与候选框/工具栏交互后发回 Go.

### 4.4 端点路径

```swift
struct BridgeClient {
    static func defaultSocketPath() -> String {
        let runtime = ProcessInfo.processInfo.environment["WIND_INPUT_RUNTIME_DIR"]
            ?? "\(NSHomeDirectory())/Library/Application Support/WindInput"
        return "\(runtime)/bridge.sock"
    }

    static func defaultPushSocketPath() -> String {
        let runtime = ProcessInfo.processInfo.environment["WIND_INPUT_RUNTIME_DIR"]
            ?? "\(NSHomeDirectory())/Library/Application Support/WindInput"
        return "\(runtime)/bridge_push.sock"
    }
}
```

注意: `<suffix>` 由 Go 端 `pkg/buildvariant` 提供, debug 构建是 `_debug`. 用户安装 release 时路径无后缀.

## 5. 开发里程碑

### M1: "Hello bridge" — 1-2 天

**目标**: `.app` 跑起来, 连接 Go 服务, 收到一个帧, 验证协议通路.

步骤:
1. 创建 Xcode/SwiftPM 工程 + Info.plist
2. 启动 Go 服务 (`./wind_input`)
3. `.app` 内 `IMKServer` 注册成功 (能在系统设置 → 键盘 → 输入法看到)
4. `InputController` init 时 `BridgeClient.connect()` 不报错
5. 给 Go 服务发一个伪造 `CmdToggleMode` 帧 (cmd id 0x0207), 看 Go 端日志确认收到
6. 收 push pipe 帧, 把 cmd id 打 NSLog

验证: 切换到 WindInput 输入法后, 控制台能看到协议帧来回.

### M2: "能打出字" — 3-5 天

**目标**: 拼音字母键击 → 候选框显示 → 选词 → 上屏.

步骤:
1. `KeyHandler.encodeKeyEvent(NSEvent)` 翻译键码 (NSEvent.keyCode → Win VK 兼容映射, 见 `KeyHandler.swift` 中的 keyCodeToVK 表)
2. 发 `CmdKeyEvent` 帧到 Go bridge
3. 解 Go 响应: PassThrough → 让 NSEvent 落到下层; Consumed → 吃掉; CommitText → `client().insertText(...)`; UpdateComposition → `client().setMarkedText(...)`
4. 收 push pipe 的 `CmdCandidatesShow` (uicmd) → 简单 `NSLog` 打印候选词文本 (不画 NSPanel)
5. 数字键 1-9 → 发 `CmdCommitRequest` → 收 commit text → `insertText`

**关键关卡**: caret 位置的协议帧 (`CmdCaretUpdate`)— 用 `IMKInputController.attributes(forCharacterIndex:lineHeightRectangle:)` 拿到屏幕坐标.

验证: 输入 "ni hao" 看到 NSLog 输出"你好 / 那好 / 倪好...", 按数字 1 上屏"你好".

### M3: "候选框可见" — 1 周

**目标**: 自绘 NSPanel 候选框, 替代 NSLog 输出.

实现:
- `CandidatePanel: NSPanel`, level = `kCGPopUpMenuWindowLevel` (101)
- `CandidateView: NSView`, 自绘候选词 + 序号 + 主题色
- 用 CoreText `CTLineCreateWithAttributedString` + `CTLineDraw`
- 主题数据来自 `CmdThemeApply` payload (字号/字色/圆角/padding 等)

布局算法**已在 Go 端做好** (`internal/ui/renderer_layout.go` Win 端用; macOS 端只需用 Go 推送的"已排好序"的候选数据 + 锚点坐标贴像素).

### M4: "工具栏 / Toast / 菜单" — 1 周

- NSStatusItem 显示输入模式图标 (`StatusUpdateData.IconLabel`)
- 右键 NSMenu 调用 `CmdMenuShow` 流程 (Go 端发菜单结构, IMKit 渲染 NSMenu, 点选回发 `EvtMenuItemSelected`)
- Toast 用 NSPanel + NSVisualEffectView

### M5: "可用日常输入" — 1 周

- 全屏检测 (`NSApplication.didChangeOcclusionStateNotification`) 推 Go 服务
- 系统暗色模式监听 (`NSApplication.effectiveAppearance` KVO) 推 Go 服务
- cmdbar "复制候选" 直接调 `NSPasteboard.generalPasteboard.setString(...)` (Go clipboard stub 不参与)
- 焦点应用识别 (`NSWorkspace.shared.frontmostApplication.bundleIdentifier`) 在 `CmdFocusGained` 帧 payload 加 bundleID 字段

### M6: 打包 + 签名 + Notarization — 3-5 天

- `xcodebuild archive`
- 用 Apple Developer 证书签名
- `xcrun notarytool submit` 走 Apple 公证
- `productbuild` 生成 `.pkg` 安装包到 `/Library/Input Methods/WindInput.app`

## 6. 验证步骤 (每个里程碑都应该跑一遍)

### 6.1 Go 服务可达性

```bash
ls -la ~/Library/Application\ Support/WindInput/
# 应看到 bridge.sock / bridge_push.sock / wind_input.pid
```

如缺失: Go 服务没启动, 或权限问题. 启动:
```bash
WIND_INPUT_LOG_LEVEL=debug ./wind_input
```

### 6.2 用 Python 单元测试 IMKit 客户端逻辑

在 macOS 上写 Swift 解码器之前, 先用 Python 验证你的协议理解正确 (参见 [`../macos-build.md`](../macos-build.md) §6 的示例).

### 6.3 把 .app 放到 /Library/Input Methods/

```bash
sudo cp -R WindInput.app /Library/Input\ Methods/
killall -9 WindInput 2>/dev/null || true
# 系统设置 → 键盘 → 输入法 → +号 → 中文 → WindInput
# 切换到 WindInput, 在任意文本框尝试输入
```

调试 IMKit 进程崩溃: `Console.app` 过滤 `WindInput` 看 crash report.

### 6.4 协议互通快速校验

启动 Go 服务后:
```bash
# 终端 1: 看 Go 端日志
tail -F ~/Library/Logs/WindInput/wind_input.log
# 终端 2: 看 .app 端 Console
log stream --predicate 'process == "WindInput"' --info
```

切换 WindInput 输入法, 应看到:
- Go 端: `bridge client connected connID=N` + `push client connected connID=N+1`
- IMKit 端: BridgeClient connected + onCommand received

## 7. 与 Go 服务的版本契约

- Go 端 ipc 协议版本: `ProtocolVersion = 0x1001` (`internal/ipc/binary_protocol.go`)
- uicmd 协议: 命令 cmd id 在 0x06xx 段, 事件 evt id 在 0x07xx 段; 字段顺序见 `internal/uicmd/payload_*.go` 各 marshal/unmarshal 方法
- **任何协议变更必须同步**:
  - Go: `internal/ipc/binary_protocol.go` + `internal/uicmd/*.go`
  - Win: `wind_tsf/include/BinaryProtocol.h` + `wind_tsf/src/IPCClient.cpp`
  - macOS: `wind_macos/Sources/IPC/ProtocolTypes.swift` + `BinaryCodec.swift`

## 8. 不在 PR-A 范围 (留作后续 PR)

- ⏭️ macOS 上跑 Wails 设置端 (`wind_setting/`) — 需要 Wails v2 darwin 配置 + Apple Developer 证书
- ⏭️ 用户字典 / Shadow / 主题等 GUI 配置在 macOS 上的可视化 — 暂时通过 Go 服务 + 命令行工具 / 配置文件直接编辑
- ⏭️ macOS 启动项 (launchd plist) 让 Go 服务在用户登录时自动起 — 简单 stub 用 `launchctl load`
- ⏭️ 安装包 GUI (`wind_portable/` 的对位) — 用 productbuild 默认 UI 即可

## 9. 关键参考

- **Squirrel (鼠须管, 最相近开源参考)**: <https://github.com/rime/squirrel>
  - 同样走 IMKit + 自绘 NSPanel
  - 用 Obj-C 实现, 看 `SquirrelInputController.m` 的 handle 方法
  - 候选框 `SquirrelPanel.m` 是绘制参考
- **Apple IMKit 文档**: <https://developer.apple.com/documentation/inputmethodkit>
- **IMKit sample (Apple)**: TextInputSources `MyInputMethod` 示例
- 本仓库内置参考:
  - Win 端 IPC 客户端 (协议解码参考): `wind_tsf/src/IPCClient.cpp`
  - Go 端协议定义: `wind_input/internal/ipc/binary_protocol.go` + `internal/bridge/protocol.go`
  - uicmd 协议定义: `wind_input/internal/uicmd/`
  - Go 服务架构总览: [`macos-port.md`](macos-port.md)

## 10. 风险与可能的陷阱

| 风险 | 描述 | 缓解 |
|------|------|------|
| sandbox 权限 | IMKit `.app` 默认沙盒, 访问 `~/Library/Application Support/WindInput/` 需要 entitlement | 用 `com.apple.security.files.user-selected.read-write` + `com.apple.security.network.client` (UDS 算 file-based, 一般不需要 network) |
| caret 坐标空间 | macOS 用 bottom-left 原点, Win 是 top-left; Go 服务接收的坐标系约定要文档化 | IMKit 端在 `CmdCaretUpdate` 发送前转换为 top-left (Y 翻转) |
| 字符编码 | Go 端字符串都是 UTF-8; Swift String 内部 UTF-16 但 `.data(using: .utf8)` 正确 | 编解码时统一走 UTF-8, 别用 `NSString.cString` |
| 多 controller 同时使用 | 用户切 tab/窗口, 多个 IMKInputController 实例同时存在, 各自独立 socket | Go 端已设计支持 (connID 索引), IMKit 端只需让每个 controller 独立 BridgeClient |
| 系统输入法切换闪烁 | 切到 WindInput 时 IMKit 默认显示自带候选框, 与我们自绘冲突 | 在 Info.plist 关闭默认候选框, 或让 IMKit 候选框始终为空 |
| Go 服务未启动 | `.app` 启动时 Go 服务可能还没起 | IMKit 端 BridgeClient 做指数退避重连; 用户可看到状态栏图标变灰提示 |

## 11. 后续 PR 大概会怎么排

- **PR-A.1**: M1+M2 (能打字) — 大概 1 周, 包含 Xcode 工程骨架 + 协议解码器 + KeyHandler + commit text
- **PR-A.2**: M3 (候选框可见) — 1 周, NSPanel + CoreText 渲染
- **PR-A.3**: M4 (工具栏/Toast/菜单) + 主题
- **PR-A.4**: M5 系统集成 (剪贴板/暗色模式/全屏)
- **PR-B**: Go 端 stub 替换 (跟随 PR-A 各阶段所需)
- **PR-C**: 安装包 + 签名 + Notarization (M6)

## 12. 踩坑记录 (M2.1 期间)

PR-A M2.1 阶段试图让 `.app` 出现在 系统设置 → 键盘 → 输入法 列表, 走了一系列盘旋的弯路, 记录如下以免后续踩同样坑.

### 12.1 Info.plist 必须项 (踩过对应坑)

- **`LSBackgroundOnly=true` 单独配会让系统拒绝 IMK 注册**. 必须同时设 `LSUIElement=true` (允许后台 .app 创建 NSPanel). Squirrel 与多数主流 IME 都同时有这两项.
- **`InputMethodConnectionName` 必须等于 `$(CFBundleIdentifier)_Connection`**. 任意字符串会被 Big Sur+ 的 XPC handshake 拒. main.swift 应运行时从 bundleID 派生.
- **`tsInputModeCharacterRepertoireKey` 必须用 ISO 15924 四字母脚本码 (`Hans`/`Hant`/`Latn`/...)**, 不是 BCP-47 `zh-Hans`/`en`. 用错值系统视为本输入法没声明任何 repertoire, 直接不显示.
- **ComponentInputModeDict 缺这些字段会出现"装上了系统不认"**: `tsInputModeScriptKey=smUnicodeScript`, `tsInputModeDefaultStateKey=true`, `tsInputModePrimaryInScriptKey=true`, `TISIconLabels`, 顶层 `tsVisibleInputModeOrderedArrayKey`.
- **本地化菜单名走 `Contents/Resources/<lang>.lproj/InfoPlist.strings`** (键 `"<mode-id>" = "清风输入法";`), 不是 `CFBundleDisplayName`.

### 12.2 签名 / hardened runtime

- **install 阶段不能 `sudo codesign --force --sign -`** 把 build 阶段的 hardened runtime + entitlements + 真证书全摘掉. install 应该只 cp + 不重签 (sudo 不需要 codesign, 也找不到 user keychain 里的证书).
- **codesign 报 `errSecInternalComponent`** 通常是: (a) login keychain 没解锁 — `security unlock-keychain` 先; (b) 私钥 partition list 没让 codesign 用 — `security set-key-partition-list -S apple-tool:,apple:,codesign: -s ~/Library/Keychains/login.keychain-db`.
- **`sudo -u user env VAR=X cmd`** 才能正确把环境变量传给 drop-sudo 后的子进程. `sudo -E + 内联 VAR=X` 组合 sudoers 会过滤掉, 表现为 SIGN_IDENTITY 没生效, .app flags 退回 `0x20002(adhoc,linker-signed)`.
- **本机自签 Code Signing 证书** 在 macOS 26 上不够: 必须 `add-trusted-cert -d -r trustRoot -p codeSign -k /Library/Keychains/System.keychain cert.crt` 把它加为 Trust Root, codesign Authority 才不被判 `CSSMERR_TP_NOT_TRUSTED`.

### 12.3 LaunchServices DB 陷阱

- **LS DB 用 bundleID 索引, 早期 build/ 路径的 .app 会"抢占"** /Library/Input Methods/ 路径的同 bundleID — dump 看 `path:` 字段, 如果指向旧位置就必须 `lsregister -u` 两条路径都 unreg, 删 build/ 那个 .app, 再 `lsregister -f -R /Library/Input Methods/WindInput.app` 重读.
- **`lsregister -kill` 在 macOS 26 被移除** ("the option has been removed because it was dangerous"). 替代: 重启 `launchservicesd` 或 `lsregister -f` 强刷单 bundle.
- **LS DB 的 `bundle flags: launch-disabled`** 不是真的禁止启动 — 它只是 LS 对 `LSBackgroundOnly=true` 的内部归类 (Qingg 同样有此 flag 但工作正常). 别花时间清这个 flag.

### 12.4 Bundle ID 必须含 `.inputmethod.` (魔术字符串 filter)

macOS 在第一步扫描 `/Library/Input Methods/` 时, 直接通过 bundleID 是否含 `inputmethod` 子串过滤掉非 IME 应用. **bundleID 不含 `inputmethod` 的 .app, 系统会当成普通应用 skip, 任何 TIS API 调用都返回 success 但 silent no-op**.

证据 — 系统自带 + 主流第三方 IME 全部含 `inputmethod`:
- Apple SCIM: `com.apple.inputmethod.SCIM`
- Squirrel: `im.rime.inputmethod.Squirrel`
- Qingg (能用): `com.aodaren.inputmethod.Qingg`

我们最初用 `to.feng.WindInput` 撞墙, 改成 `to.feng.inputmethod.WindInput` 后, `TISRegisterInputSource` 终于真把我们写进 TIS DB.

### 12.5 register 进程必须常驻 (TIS 注册是进程级 lifecycle)

`TISRegisterInputSource(bundleURL)` 把 IME 写进系统 TIS DB 是 **进程级 lifecycle 绑定** — 调 API 的进程一旦退出, 注册几秒内被系统清掉. 实测 (`PID 61635: launchservicesd attach @ 51.034, invalidated @ 51.050`, 0.016 秒就死亡), 然后 mode 在 10-15s 后从 `TISCreateInputSourceList` 消失.

修复: `WindInput --register-input-source` 子命令调完 API 后不 exit, 用 `RunLoop.current.run()` 永久阻塞, 让外部 SIGTERM 退. install 脚本以后台 fork 调它 (`"$APP_EXEC" --register-input-source > /tmp/wind_register.log 2>&1 &`), 主流程不卡, 后台进程维持注册.

Squirrel postinstall 看似 register-完即返回也能用, 是因为 Squirrel binary 还跑别的常驻服务 (LSReady / IMK server); 我们的 register 子命令只调 API 没别的逻辑, 必须自带 RunLoop 才能保持注册.

### 12.6 系统设置 UI 显示 — macOS 26 真正的硬墙 (ensan 对照实验铁证)

修了 §12.4 / §12.5 之后:
- `TISRegisterInputSource` 真把 mode 持久写进 TIS DB ✓
- `swift list_input_sources.swift` 看到 mode `selectable=true` ✓
- 但 **`TISEnableInputSource(src)` 返回 `OSStatus=0` 但 isEnabled 仍 false (silent no-op)**
- **`TISSelectInputSource(src)` 返回 `OSStatus=-50` paramErr**
- 系统设置 → 键盘 → 文本输入 → 编辑 → + → 简体中文 **看不到本输入法**

为了避免误判是我们项目独有问题, 跑了对照实验:

**Cloned [ensan-hcl/macOS_IMKitSample_2021](https://github.com/ensan-hcl/macOS_IMKitSample_2021)** — 80 行 swift 的最简 Apple IMKit sample, 含 `LSBackgroundOnly` / `NSManualApplication` NSPrincipalClass / `sandbox=true` + `com.apple.security.temporary-exception.mach-register.global-name` entitlement (我们没有), 用同样 ad-hoc 路径 build + cp 到 `/Library/Input Methods/` + lsregister.

**实测结果完全一摸一样**:
```
ID     : com.ensan.inputmethod.IMKitSample
kind   : TISTypeKeyboardInputMethodWithoutModes
enabled: false  selectable: true
```
ensan IMKitSample (Apple-recommended sample) 在 macOS 26 Tahoe 上 **同样系统设置 UI 不显示**.

这彻底排除:
- ❌ sandbox=true + mach-register exception entitlement (ensan 有, 一样失败)
- ❌ NSManualApplication NSPrincipalClass (ensan 用了, 一样失败)
- ❌ Apple-style entitlements / Swift 工程结构

**最终结论**: macOS 26 Tahoe 对 **任何非 Notarized 第三方 IME** 在系统设置 UI 显示层一刀切拒绝, 与 plist / entitlement / 签名身份 (ad-hoc vs Personal Team) 全无关. 这是 Apple Tahoe 新加的统一安全策略.

self-signed / 本机 trust / Personal Team Apple Development 全部不够 (ensan 同样不行). 出路:
- **付费 \$99 Apple Developer Program** → Developer ID Application 证书 → `xcrun notarytool submit` 走 Apple 公证 → 用 notarized .app 重 deploy
- 或者 **降级到 macOS 15 Sequoia / 14 Sonoma** (虚拟机或别的机器, 那里 ad-hoc 据社区反馈仍可用)

### 12.7 当前 PR-A.1 状态

工程层 100% ready, 缺的只是 \$99 + notarize 这一步:

- ✓ Info.plist 全字段 (含 §12.1 + §12.4 的全部约定)
- ✓ bundleID 含 `.inputmethod.` (§12.4 实证)
- ✓ ad-hoc / Personal Team Apple Development 签名 + hardened runtime
- ✓ IME 自身 binary `--register-input-source` / `--enable-input-source` / `--select-input-source` 子命令 (镜像 Squirrel)
- ✓ register 子命令 RunLoop 常驻 (§12.5)
- ✓ install 脚本: cp + lsregister + 后台 fork register 进程
- ✓ TIS DB 注册持久不被 watchdog 清
- ❌ 系统设置 UI 显示 (§12.6 Notarization 硬墙)

M2.2+ 代码层 (composition / candidates / commit / push pipe 解码) 继续用 swift test 覆盖 ± 逻辑, 拿到 notarized 证书后一次性 deploy 验真实 IMKit 端到端流程.

### 12.5 诊断脚本入口

`scripts/list_input_sources.swift` 是 TIS 注册状态的金标准工具 (调 Apple `TISCreateInputSourceList` API), 任何时候都可以用它确认 IME 是否真被系统收录.
