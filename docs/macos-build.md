<!-- Updated: 2026-06-04 -->

# macOS 构建与调试指南

> 实用文档. 设计层面背景见 [`design/macos-port.md`](design/macos-port.md), PR-A 工程计划见 [`design/macos-imkit-plan.md`](design/macos-imkit-plan.md).
>
> 当前状态 (alpha): macOS 端采用 **IMKit `.app` 输入法客户端 + Go 后台服务** 双进程模型,
> 输入 / 候选 / 上屏 / 设置界面均已打通, 以单个 `.pkg` 分发输入法 + 服务 + 设置三件套
> (universal, 同时支持 Apple Silicon 与 Intel). 仍处 alpha: 未做苹果公证, 部分功能与
> Windows 版有差异 (见 [§7 功能实装状态](#7-功能实装状态)).
>
> 本指南覆盖:
> 1. 用 `dev_mac.sh` / `scripts_mac` 构建与安装三件套 (推荐)
> 2. 单独构建 Go 服务端二进制 (用于交叉编译 / 协议调试)
> 3. 用 `socat` / `nc` / Python 模拟客户端做底层 bridge 协议调试

## 1. 环境要求

| 项 | 版本 |
|----|------|
| Go | 1.25+ (与仓库 `wind_input/go.mod` 一致; 构建设置应用需 1.26+) |
| macOS | 12 Monterey 及以上 |
| Xcode | 15+ (含命令行工具, 提供 Swift 5.9 工具链 / `clang` / `xcrun` / 系统 SDK) |
| Node.js + pnpm | 设置应用 (Wails) 前端构建 |
| Wails CLI | v2.12+ (设置应用) |

Go 服务端无需 CGO 即可构建 (当前 darwin 路径不依赖任何 CGO); IME `.app` 与设置应用需 Xcode 工具链。

## 2. 推荐构建方式 (`dev_mac.sh`)

仓库根的 `dev_mac.sh` 提供构建 / 安装 / 部署 / 诊断的一体化交互菜单, 对位 Windows 的 `dev.ps1`:

```bash
./dev_mac.sh            # 显示菜单
./dev_mac.sh 1          # 构建全部: Go 服务 + 词库 + IME .app + 设置应用
./dev_mac.sh 2          # 仅构建 Go 服务 (跳过词库下载)
./dev_mac.sh app        # 仅构建 IME .app bundle
./dev_mac.sh setting    # 仅构建设置应用 (Wails)
./dev_mac.sh pkg --build --universal   # 打 universal 分发安装包 .pkg
./dev_mac.sh clean      # 清 build/ 与 build_debug/
```

前缀 `d` 表示调试版变体 (如 `d1` / `dapp`), 可与正式版并存。底层脚本位于 `scripts_mac/`:

| 目录 | 用途 |
|------|------|
| `scripts_mac/build/` | `build.sh` (Go 服务 + 词库) / `app.sh` (IME .app) / `setting.sh` (Wails) / `pkg.sh` (.pkg 打包) |
| `scripts_mac/deploy/` | `install_service.sh` / `install_app.sh` / `install_setting.sh` (per-user 安装, 无需 sudo) |
| `scripts_mac/vm/` | host→VM 远程部署 |
| `scripts_mac/test/` | TIS 注册检查等诊断脚本 |

### 本机安装 / 卸载

```bash
./dev_mac.sh i      # 安装全部 (Go 服务 LaunchAgent + IME .app + 设置应用)
./dev_mac.sh m setting   # 单模块 构建+安装 (模块: service / app / setting)
./dev_mac.sh u      # 卸载全部
```

> 安装均为 per-user (装到 `~/Library` / `~/Applications`), **不要** 用 sudo。
> 安装后到 **系统设置 → 键盘 → 文本输入 → 输入法** 添加并切换到「清风输入法」。

## 3. 单独构建 Go 服务端二进制

用于交叉编译或脱离 `.app` 做协议调试。

### 在 Windows 上交叉编译 darwin 二进制

```powershell
# arm64 (Apple Silicon)
$env:GOOS = "darwin"; $env:GOARCH = "arm64"
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

### 在 macOS 上本地构建

```bash
cd wind_input
go build ./cmd/service       # 产物: wind_input/wind_input (当前架构)
```

或同时产 universal binary:

```bash
GOARCH=arm64 go build -o build/wind_input.arm64 ./cmd/service
GOARCH=amd64 go build -o build/wind_input.amd64 ./cmd/service
lipo -create -output build/wind_input.universal build/wind_input.arm64 build/wind_input.amd64
```

## 4. 启动与端点检查

直接运行二进制 (或 `./dev_mac.sh r` 前台运行 debug 日志):

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

`<suffix>` 由 build variant 决定 (debug = `_debug`, release = `""`)。

### 改运行时目录 (测试与 portable)

```bash
WIND_INPUT_RUNTIME_DIR=/tmp/wind_test ./wind_input
```

注意: 该环境变量同时影响 `bridge` 和 `pkg/rpcapi` 两侧端点路径。

### 检查 socket 是否监听

```bash
ls -la ~/Library/Application\ Support/WindInput*/      # 期望看到 srwx------ 权限的 .sock
lsof -U | grep wind_input                              # 看哪个进程持有
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

## 6. 协议层验证 (Swift smoke / socat / Python)

IME `.app` 已落地, 日常输入直接用 `.app` 即可。以下手段用于**底层 bridge 协议调试** (脱离 IMKit 验证帧收发):

### Swift smoke 工具

```bash
./dev_mac.sh smoke           # 连真实 bridge 收发帧 (swift run wind-smoke)
# 或:
cd wind_macos && swift run wind-smoke 10
```

### 用 socat dial 双向 socket

```bash
./wind_input &
socat - UNIX-CONNECT:~/Library/Application\ Support/WindInput/bridge.sock
```

### 用 Python 发一个 KeyEvent 帧

```python
import socket, struct, os

sock_path = os.path.expanduser("~/Library/Application Support/WindInput/bridge.sock")
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock_path)

# Header: version(0x1001 LE) + cmd(0x0101 KeyEvent LE) + length(16 LE)
header = struct.pack("<HHI", 0x1001, 0x0101, 16)
# KeyEvent payload: keyCode(4) scanCode(4) modifiers(4) eventType(1) toggles(1) eventSeq(2)
payload = struct.pack("<IIIBBH", 0x41, 0, 0, 0, 0, 1)
s.sendall(header + payload)

resp_header = s.recv(8)
ver, cmd, length = struct.unpack("<HHI", resp_header)
resp_payload = s.recv(length) if length > 0 else b""
print(f"resp cmd=0x{cmd:04x} len={length} payload={resp_payload.hex()}")
```

预期响应是 PassThrough (cmd 0x0002) 或 Consumed (0x0401), 取决于当前模式与按键。

### 订阅 push socket

```python
import socket, struct, os

push = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
push.connect(os.path.expanduser("~/Library/Application Support/WindInput/bridge_push.sock"))
while True:
    hdr = push.recv(8)
    if not hdr: break
    ver, cmd, length = struct.unpack("<HHI", hdr)
    body = push.recv(length) if length else b""
    print(f"push cmd=0x{cmd:04x} len={length} body={body[:64].hex()}")
```

任何状态变更 (模式切换 / 输入提交) 都会在 push socket 出现帧。完整协议字段见
[`wire-protocol-reference.md`](wire-protocol-reference.md)。

## 7. 功能实装状态

下表对照 darwin 与 Windows 的功能落地情况:

| 功能 | Windows 实现 | darwin 当前 |
|------|---------|-------------|
| 剪贴板 (SetText/GetText, 命令直通车 clip.paste) | Win32 Clipboard | ✅ `.app` 调 `NSPasteboard` |
| 候选框 / 工具栏 / Toast / Tooltip 渲染 | `gg` + DirectWrite + LayeredWindow | ✅ `.app` 自绘 NSPanel + CoreText (`CandidatePanel` / `ToastPanel` / `TooltipPanel` / `StatusBubblePanel`) |
| host_render 位图共享 | 共享内存 bitmap (Win11 开始菜单) | ✅ POSIX SHM (Go 写 BGRA → Swift `SharedMemoryReader` 读显示) |
| 命令直通车按键注入 | `SendInput` | ✅ `KeySynthesizer` 用 `CGEvent` 合成 |
| 全局快捷键 | `RegisterHotKey` | ✅ `.app` 端经 uicmd 注册/触发通路 (需辅助功能权限) |
| 前台应用识别 / 密码框抑制 | 管道客户端 PID + 进程名 | ✅ IMKit client 自报 + 敏感字段自动抑制中文 |
| 进程操作 (open/run/shell) | Win32 | ✅ `proc_darwin.go` (`open` / `exec.Command`); `term` flag 暂不支持 |
| 系统暗色模式自动切换 | 注册表轮询 + watcher | ✅ Go 服务端 darwin forwarder 渲染前读 `defaults read -g AppleInterfaceStyle` (2s TTL 缓存) 判定, `theme_style=system` 时跟随系统; 候选框位图按解析后主题在 Go 侧渲染 |

## 8. 调试技巧

### 用 dlv 远程调试

```bash
# 在 macOS 上启动
dlv exec ./wind_input --listen=:2345 --headless --api-version=2
# 在 Windows VSCode 用 remote attach launch.json 连过去
```

### 单元测试

```bash
# Swift 协议层单测 (帧 roundtrip)
cd wind_macos && swift test

# 在 Windows 上验证 darwin 代码可编译
GOOS=darwin GOARCH=arm64 go test -c -o /dev/null ./internal/bridge

# 在 macOS 上跑真测试
go test ./internal/bridge/... ./internal/ui/... ./internal/uicmd/...
```

### 查看 TIS 注册状态

```bash
./dev_mac.sh tis        # 列出 TIS 内 WindInput / 相关条目
```

### 卸载残留 socket

服务异常退出未清理 socket 时, 重启前手动清:

```bash
rm -f ~/Library/Application\ Support/WindInput*/{bridge,bridge_push,rpc,rpc_events}.sock
rm -f ~/Library/Application\ Support/WindInput*/wind_input.pid
```

(正常退出会 `os.Remove` 自动清理; flock 在进程退出时由内核释放。)

## 9. 性能数据 (参考)

在 M2 Pro / macOS 14 上跑 `go test ./internal/bridge/...`:

- 单元测试集 7 个: ~1 秒
- KeyEvent 端到端 latency (socket dial + 帧 roundtrip): < 1 ms

bridge UDS 的 IPC 延迟远低于 macOS IMKit 框架本身的 invocation 延迟 (通常 1-5 ms), 不会成为瓶颈。

## 10. 已知问题与未来工作

- ⚠️ 未做苹果公证, Gatekeeper 在首次安装 / 启用时会拦截 → 需在「系统设置 → 隐私与安全性」中放行; macOS 26 (Tahoe) 对未公证输入法限制更强
- ⚠️ 暗色模式按渲染时机轮询 (≤2s TTL) 而非实时 KVO 推送, 系统切换在下一帧候选渲染时生效 (见 §7)
- ⚠️ 重装前建议先注销或重启, 以清除系统的输入法注册缓存
- ⚠️ 功能与 Windows 版仍有差异, 处于 alpha

详见 [`design/macos-port.md`](design/macos-port.md) 的路线图。
