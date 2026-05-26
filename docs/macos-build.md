<!-- Updated: 2026-05-26 -->

# macOS 构建与调试指南

> 实用文档. 设计层面背景见 [`design/macos-port.md`](design/macos-port.md).
>
> 当前状态: Go 服务端可在 macOS 上独立构建并运行 (产出 Mach-O 二进制), 但没有 macOS IMKit `.app` 客户端 → 实际不能输入. 本指南用于:
> 1. 在 macOS 上构建 wind_input 二进制
> 2. 用 `socat` / `nc` 模拟 IMKit 客户端测试 bridge IPC
> 3. 为未来 PR-A (IMKit `.app` 工程) 提供测试基础

## 1. 环境要求

| 项 | 版本 |
|----|------|
| Go | 1.24+ (toolchain 1.24.2 与仓库 go.mod 一致) |
| macOS | 12 Monterey 及以上 (与 NSApplication.effectiveAppearance KVO 行为对齐) |
| Xcode Command Line Tools | 至少包含 `clang` / `xcrun` / 系统 SDK |

无需 CGO 即可构建 Go 服务端 (当前 darwin 路径不依赖任何 CGO).

## 2. 在 Windows 上交叉编译 darwin 二进制

仓库主开发环境是 Windows; 任何 Windows 上的 Go 安装都可直接跨平台编译 darwin:

```powershell
# arm64 (Apple Silicon)
$env:GOOS = "darwin"
$env:GOARCH = "arm64"
go build -o build/wind_input_darwin_arm64 ./wind_input/cmd/service

# amd64 (Intel Mac)
$env:GOARCH = "amd64"
go build -o build/wind_input_darwin_amd64 ./wind_input/cmd/service
```

Bash (Git Bash / WSL):

```bash
GOOS=darwin GOARCH=arm64 go build -o build/wind_input_darwin_arm64 ./wind_input/cmd/service
GOOS=darwin GOARCH=amd64 go build -o build/wind_input_darwin_amd64 ./wind_input/cmd/service
```

产物预期: 约 11.5 MB (arm64) / 12.2 MB (amd64), `file` 显示
`Mach-O 64-bit ... executable, flags:<|DYLDLINK|PIE>`.

## 3. 在 macOS 上本地构建

```bash
cd wind_input
go build ./cmd/service
# 产物: wind_input/wind_input (当前架构)
```

或同时产 universal binary:
```bash
GOARCH=arm64 go build -o build/wind_input.arm64 ./cmd/service
GOARCH=amd64 go build -o build/wind_input.amd64 ./cmd/service
lipo -create -output build/wind_input.universal \
    build/wind_input.arm64 build/wind_input.amd64
```

## 4. 启动与端点检查

直接运行二进制:
```bash
./wind_input
```

预期日志:
```
[INFO ] [...] WindInput IME Service starting version=X.X.X
[INFO ] [...] Starting Bridge IPC server (darwin UDS) socket=/Users/.../bridge.sock
[INFO ] [...] Starting Push pipe listener (darwin UDS) socket=/Users/.../bridge_push.sock
```

### 默认运行时目录

```
~/Library/Application Support/WindInput<suffix>/
├── bridge.sock          # bridge 主请求-响应
├── bridge_push.sock     # bridge 推送
├── rpc.sock             # Wails RPC
├── rpc_events.sock      # Wails RPC 事件
└── wind_input.pid       # 单例 flock + PID
```

`<suffix>` 由 build variant 决定 (debug = `_debug`, release = `""`).

### 改运行时目录 (测试与 portable)

```bash
WIND_INPUT_RUNTIME_DIR=/tmp/wind_test ./wind_input
```

注意: 该环境变量同时影响 `bridge` 和 `pkg/rpcapi` 两侧端点路径.

### 检查 socket 是否监听

```bash
ls -la ~/Library/Application\ Support/WindInput*/
# 期望看到 srwx------ 权限的 .sock 文件

# 用 lsof 看哪个进程持有
lsof -U | grep wind_input
```

## 5. 日志位置

| 类别 | 路径 |
|------|------|
| 服务日志 | `~/Library/Logs/WindInput/wind_input.log` (按 `pkg/config.GetLogsDir` 解析) |
| 运行时崩溃 | `~/Library/Logs/WindInput/crash.log` (Go runtime fatal 通过 `syscall.Dup2(fd, 2)` 重定向) |

调试时升级日志级别:
```bash
WIND_INPUT_LOG_LEVEL=debug ./wind_input
```

## 6. 模拟 IMKit 客户端测试 bridge

在 PR-A (macOS IMKit `.app` 工程) 落地前, 可用 socat/nc 手工验证 bridge 协议:

### 用 socat dial 双向 socket

```bash
# 启动服务
./wind_input &

# 用 socat 连接主 bridge socket, 输入 hex bytes
socat - UNIX-CONNECT:~/Library/Application\ Support/WindInput/bridge.sock
```

### 用 Python 发一个 KeyEvent 帧

```python
import socket, struct, os

sock_path = os.path.expanduser(
    "~/Library/Application Support/WindInput/bridge.sock"
)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock_path)

# Header: version(0x1001 LE) + cmd(0x0101 KeyEvent LE) + length(16 LE)
header = struct.pack("<HHI", 0x1001, 0x0101, 16)

# KeyEvent payload: keyCode(4) scanCode(4) modifiers(4) eventType(1) toggles(1) eventSeq(2)
payload = struct.pack("<IIIBBH",
    0x41,   # 'A'
    0,      # scanCode
    0,      # modifiers
    0,      # eventType: down
    0,      # toggles
    1,      # eventSeq
)

s.sendall(header + payload)

# 读响应 (header + payload)
resp_header = s.recv(8)
ver, cmd, length = struct.unpack("<HHI", resp_header)
resp_payload = s.recv(length) if length > 0 else b""
print(f"resp cmd=0x{cmd:04x} len={length} payload={resp_payload.hex()}")
```

预期响应是 PassThrough (cmd 0x0002) 或 Consumed (0x0401), 取决于当前模式与按键.

### 订阅 push socket

```python
import socket, struct, os

push = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
push.connect(os.path.expanduser(
    "~/Library/Application Support/WindInput/bridge_push.sock"
))
while True:
    hdr = push.recv(8)
    if not hdr: break
    ver, cmd, length = struct.unpack("<HHI", hdr)
    body = push.recv(length) if length else b""
    print(f"push cmd=0x{cmd:04x} len={length} body={body[:64].hex()}")
```

任何状态变更 (模式切换/输入提交) 都会在 push socket 出现帧.

## 7. 当前 darwin stub 限制

下表列出 darwin 上未实装的功能, 行为对照:

| 功能 | Win 行为 | darwin 当前 | 实装计划 |
|------|---------|-------------|---------|
| 剪贴板 (SetText/GetText) | OpenClipboard 等 Win32 | `ErrNotImplemented` | IMKit `.app` 端直接调 `NSPasteboard` |
| 系统暗色模式检测 | 注册表轮询 | 始终 `false` | IMKit `.app` 监听 `NSApplication.effectiveAppearance` KVO 推送 |
| 全局快捷键注册 | `RegisterHotKey` | 无效 | IMKit `.app` 端 `CGEventTap` (需 Accessibility 权限) |
| 命令直通车按键注入 | `SendInput` | `ErrUnsupportedPlatform` | IMKit `.app` `CGEventCreateKeyboardEvent` 或 NSPasteboard+Cmd+V |
| 候选框/工具栏/Toast 渲染 | `gg` + DirectWrite + LayeredWindow | Manager stub (命令投到 cmdCh 等订阅) | IMKit `.app` 自绘 NSPanel + CoreText |
| 前台应用识别 | `GetNamedPipeClientProcessId` + 进程名 | `IsActivelyFocusedPID` 始终 false | IMKit 通过 attach 帧自报 `bundleIdentifier` |
| host_render (Win11 开始菜单) | 共享内存 bitmap | 不需要 | NSPanel level 浮动天然解决 |

## 8. 调试技巧

### 用 dlv 远程调试

```bash
# 在 macOS 上启动
dlv exec ./wind_input --listen=:2345 --headless --api-version=2

# 在 Windows VSCode 用 remote attach launch.json 连过去
```

### 单元测试

```bash
# 全平台测试 (Win 上跑 darwin 测试用 GOOS=darwin)
GOOS=darwin GOARCH=arm64 go test -c -o /dev/null ./internal/bridge
# 输出 EXIT=0 表示 darwin 测试可编译

# 在 macOS 上跑真测试
go test ./internal/bridge/... ./internal/ui/... ./internal/uicmd/...
```

### 卸载残留 socket

如果服务异常退出未清理 socket, 重启前手动清:
```bash
rm -f ~/Library/Application\ Support/WindInput*/{bridge,bridge_push,rpc,rpc_events}.sock
rm -f ~/Library/Application\ Support/WindInput*/wind_input.pid
```

(正常退出会 `os.Remove` 自动清理; flock 在进程退出时由内核释放.)

## 9. 性能数据 (参考)

在 M2 Pro / macOS 14 上跑 `go test ./internal/bridge/...`:
- 单元测试集 7 个: ~1 秒
- KeyEvent 端到端 latency (socket dial + 帧 roundtrip): < 1 ms

bridge UDS 的 IPC 延迟远低于 macOS IMKit 框架本身的 invocation 延迟 (通常 1-5 ms), 不会成为瓶颈.

## 10. 已知问题与未来工作

- ⚠️ 缺 macOS IMKit `.app` 客户端 → 实际输入流程未跑通
- ⚠️ Wails 设置端 (`wind_setting/`) 没做 darwin 构建; Wails v2 支持 macOS, 但需 Apple Developer 证书 + 处理沙盒
- ⚠️ 安装包/卸载脚本 (`installer/nsis/`) 全部 Win-only; macOS 需要 `productbuild` 或 `pkgbuild`
- ⚠️ darwin 二进制未签名, Gatekeeper 在首次启动会拦截 → 用户需在系统设置中"允许打开"

详见 [`design/macos-port.md`](design/macos-port.md) 的路线图.
