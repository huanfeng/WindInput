<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-05-27 -->

# scripts/ - 构建辅助和工具脚本

## Purpose

项目构建辅助脚本和诊断工具目录。这些脚本不参与主构建流程，供开发者手动调用，用于版本管理、系统诊断、macOS 端构建与冒烟等任务。

仓库根另有 `dev.ps1`（Win 开发菜单）与 `dev_mac.sh`（macOS 开发菜单），后者通过 `BUILD_SCRIPT="$REPO_DIR/scripts/build_macos.sh"` 调用本目录的 macOS 构建脚本。

## Key Files

| File | Description |
|------|-------------|
| `bump-version.ps1` | 版本号管理脚本：读取 VERSION 文件，按 major/minor/patch/prerelease 规则递增版本号，同步更新所有版本号引用文件（VERSION、go.mod、CMakeLists.txt 等） |
| `check_band.ps1` | DWM Window Band 诊断工具：枚举系统窗口并显示各窗口的 Band 等级，用于调试 Win11 开始菜单候选框 z-order 问题和验证 HostWindow 机制 |
| `probe_ime_mode.ps1` | IME 中/英文模式外部探针（IMM32 视角）：模拟 KBLSwitch 等第三方工具的探测路径，用 `WM_IME_CONTROL/IMC_GETCONVERSIONMODE` 跨线程查询前台窗口的 IMM32 桥接状态。`NO-IMEWND` 表示前台是 TSF-only 客户端（Win11 新版记事本 / Edge / 部分 UWP），CUAS 未建 IMM HIMC，物理上无法外部读取，需要靠功能行为验证 |
| `lint_agents_md.ps1` | 检查 AGENTS.md 文档引用是否悬空 |
| `build_macos.sh` | macOS 端构建脚本（对位 `build_all.ps1` 的可裁剪子集）：下载 rime-frost / rime-wubi / OpenCC 词库源到 `.cache/`，跑 `gen_unigram` + `dictgen` + `gen_opencc_dict`，产出 `build/{wind_input, data/}`（debug variant 产出 `build_debug/{wind_input_debug, data/}`）。子命令：`all` / `service` / `data` / `clean`，开关：`--debug` |
| `smoke_bridge.py` | bridge IPC 冒烟脚本（macOS 端开发期）：连 `bridge.sock` 发一帧 `CmdKeyEvent` 验证 roundtrip，再订阅 `bridge_push.sock` 打印推送帧；用于在 Swift IMKit 客户端落地前快速验证 Go 服务协议通路。`scripts/smoke_bridge.py [push_wait_seconds]`，默认 5 秒 |

## Usage

### bump-version.ps1

```powershell
# 升级补丁版本（0.1.0 → 0.1.1）
scripts\bump-version.ps1 -Version patch

# 升级次版本（0.1.0 → 0.2.0）
scripts\bump-version.ps1 -Version minor

# 升级主版本（0.1.0 → 1.0.0）
scripts\bump-version.ps1 -Version major

# 设置预发布标识（0.1.0 → 0.1.0-alpha）
scripts\bump-version.ps1 -Version patch -Preid alpha
```

### check_band.ps1

```powershell
# 倒计时 8 秒后扫描（先打开开始菜单再运行）
scripts\check_band.ps1

# 立即扫描
scripts\check_band.ps1 -Now

# 持续监控（Ctrl+C 停止）
scripts\check_band.ps1 -Loop

# 显示所有窗口（含 Band=0/1 的普通窗口）
scripts\check_band.ps1 -All
```

### build_macos.sh

```bash
# 全量：下载词库 + 构建 Go 服务 + 准备 data
scripts/build_macos.sh

# 仅 Go 服务（不动词库）
scripts/build_macos.sh service

# 仅词库下载 + 准备 data
scripts/build_macos.sh data

# debug variant（产出 build_debug/wind_input_debug）
scripts/build_macos.sh --debug

# 清 build/ 与 build_debug/
scripts/build_macos.sh clean
```

输出：

- release：`build/{wind_input, data/}`
- debug ：`build_debug/{wind_input_debug, data/}`

Win 端 `build_all.ps1` 还会构建 TSF DLL / Wails 设置端 / 便携启动器，macOS 端这些尚未实现（IMKit `.app` 走 PR-A 后续里程碑）。

### smoke_bridge.py

```bash
# 默认监听 push 5 秒
scripts/smoke_bridge.py

# 自定义 push 监听时长
scripts/smoke_bridge.py 10

# 也可改运行时目录
WIND_INPUT_RUNTIME_DIR=/tmp/wind_test scripts/smoke_bridge.py
```

预期输出（Go 服务运行中）：

```
[smoke] === KeyEvent roundtrip ===
[smoke] -> KeyEvent bytes=26 hex=0110010112000000...
[smoke] <- cmd=0x0401 ver=0x1001 len=0
[smoke] === Push channel (5s) ===
[smoke] push cmd=0x0206 len=15 body=0c000000...
[smoke] done
```

### probe_ime_mode.ps1

```powershell
# 默认 200ms 轮询，状态变化时输出
pwsh -File scripts\probe_ime_mode.ps1

# 自定义轮询间隔
pwsh -File scripts\probe_ime_mode.ps1 -IntervalMs 100
```

输出形如：

```
13:45:01.234  CN        open=1 conv=0x0001 imeWnd=0x000A0188 pid=12345  proc=notepad++          win=[xxx.txt - Notepad++]
13:45:02.451  EN        open=1 conv=0x0000 imeWnd=0x000A0188 pid=12345  proc=notepad++          win=[xxx.txt - Notepad++]
13:45:05.012  NO-IMEWND open=0 conv=0x0000 imeWnd=0x0       pid=23456  proc=Notepad             win=[文档 1 - 记事本]
```

- `Mode` 取值：
  - `CN`：IME_CMODE_NATIVE 置位（中文）
  - `EN`：NATIVE 清零（英文）
  - `OFF`：IME 未打开
  - `NO-IMEWND`：`ImmGetDefaultIMEWnd` 返回 0，前台是 TSF-only 客户端
- 验证方法：
  - 传统 IMM32 应用（cmd / Notepad++ / WPS / Chrome）：切换中英文时 `Mode` 应立即翻转，外部第三方工具（KBLSwitch）也能正确读到。
  - TSF-only 应用（Win11 新版记事本 / Edge / 部分 UWP）：通常显示 `NO-IMEWND`，**任何外部 probe 都读不到**（compartment 是进程内状态，CUAS 也没建 IMM HIMC），这种应用 KBLSwitch 的锁定功能受系统限制无法工作 —— 此时只能靠功能行为（实际锁定是否生效）验证。

## For AI Agents

### Working In This Directory

- `bump-version.ps1` 会修改多个文件中的版本号，运行前确认当前工作区干净
- `check_band.ps1` 是只读诊断工具，不修改系统状态，可随时运行
- PowerShell 脚本均不需要管理员权限
- 新增 PowerShell 脚本时保持与现有文件相同的编码风格（UTF-8 with BOM，`$ErrorActionPreference = "Stop"`）
- 新增 bash 脚本（macOS）以 `set -euo pipefail` 开头；调用 `go run` 前用 `cd "$REPO_DIR/wind_input"` 进入 module 根（仓库根本身不是 Go module）
- `build_macos.sh` 词库下载到 `.cache/`（已被 `.gitignore` 排除），dictgen / unigram 输出到 `build/data/schemas/`，仓库 `data/` 目录里的预制文件除 `AGENTS.md` 外全部复制到 `build/data/`

## Dependencies

### Internal
- `bump-version.ps1` 依赖项目根目录的 `VERSION` 文件
- `check_band.ps1` 通过 P/Invoke 调用 `user32.dll` 的 `GetWindowBand`（与 HostWindow 使用相同的非公开 API）
- `build_macos.sh` 依赖 `wind_input/cmd/{gen_unigram,gen_opencc_dict}`、`wind_input/tools/dictgen`、仓库根 `data/`、`VERSION`
- `smoke_bridge.py` 依赖正在运行的 Go 服务（`bridge.sock` + `bridge_push.sock`，路径见 `internal/bridge/endpoint_darwin.go`）

### External
- PowerShell 5.1+ 或 PowerShell 7+（PowerShell 脚本）
- `check_band.ps1` 需要 Windows 10/11（GetWindowBand API 仅在 Win10+ 存在）
- `build_macos.sh` 需要 macOS + Go 1.24+ + `curl`
- `smoke_bridge.py` 需要 Python 3（仅用 stdlib：`socket`/`struct`/`threading`）

<!-- MANUAL: -->
