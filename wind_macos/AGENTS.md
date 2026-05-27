# wind_macos

macOS IMKit `.app` 工程 (PR-A). 与 Win 端 `wind_tsf/` DLL 对位, 与跨平台 Go 服务 (`wind_input/`) 通过 Unix Domain Socket 通信.

## 当前阶段

**PR-A M1** — "Hello bridge", 仅有 SwiftPM 骨架:

- 协议层 (`WindInputKit`)
- 命令行 smoke 工具 (`wind-smoke`)
- 单元测试

**未完成**: 真正的 `.app` bundle, IMKServer 注册, 候选框 NSPanel —— 见 `docs/design/macos-imkit-plan.md` 各里程碑.

## 目录

| 路径 | 角色 |
|------|------|
| `Package.swift` | SwiftPM 清单, 三个 target (kit / smoke / tests) |
| `Sources/WindInputKit/IPC/ProtocolTypes.swift` | 协议常量 + payload 类型 + endpoint 路径 |
| `Sources/WindInputKit/IPC/BinaryCodec.swift` | 帧 encode/decode (镜像 Go `internal/ipc/binary_codec.go`) |
| `Sources/WindInputKit/IPC/BridgeClient.swift` | UDS 阻塞客户端 |
| `Sources/WindInputSmoke/main.swift` | `swift run wind-smoke` — 连 bridge + push, 打印帧 |
| `Tests/WindInputKitTests/BinaryCodecTests.swift` | 帧 roundtrip + 边界 |

## 协议同步铁律

修改 cmd id 或帧布局必须三处同步:

- `wind_input/internal/ipc/binary_protocol.go` (Go SSOT)
- `wind_tsf/include/BinaryProtocol.h` (Win)
- `wind_macos/Sources/WindInputKit/IPC/{ProtocolTypes,BinaryCodec}.swift` (本目录)

完整速查: `../docs/wire-protocol-reference.md`.

## 本地开发

需要的工具: Xcode CLT (含 swift 5.9+), Go 1.24+ (跑 Go 服务).

```bash
cd wind_macos

# 跑单测
swift test

# 启动 Go 服务 (另一终端)
cd ../wind_input && go run ./cmd/service

# 跑 smoke (默认监听 push 10 秒)
swift run wind-smoke
# 或自定义时长
swift run wind-smoke 30
```

期望输出:

- 请求通道: `[smoke] <- recv cmd=0x0002 len=0` (PassThrough, 没注册 KeyEvent 'A')
- push 通道: 至少看到 `cmd=0x0207` (ServiceReady) 一帧

## 下一步 (M2)

- `KeyHandler.swift` — `NSEvent.keyCode` → Win VK 映射
- `BridgeClient` 升级: async callback + 重连
- Xcode `.xcodeproj` 工程 + Info.plist + IMK server 注册
- `swift run` 走不动 IMK, 这一步开始必须出 `.app`

参考: `docs/design/macos-imkit-plan.md` §3-§5.
