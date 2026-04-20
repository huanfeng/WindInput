<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# wind_portable/ - 便携版启动器

## 用途

便携版应用启动器（Portable Launcher）。与安装版（`installer/` 使用 NSIS 脚本 + COM 注册）不同，便携版不需要管理员权限、无系统注册表污染、无 COM 注册，所有文件相对于可执行文件存放在本地目录。启动器负责：

1. **检测便携版环境**：根据标记文件（`portable.marker`）判断是否运行于便携版
2. **管理子进程**：启动/停止 `wind_input.exe`（引擎服务）和 `wind_setting.exe`（配置界面）
3. **部署与卸载**：从 ZIP 文件提取组件、注册 TSF DLL（临时）、清理残留
4. **UI 与 CLI 模式**：提供图形界面或命令行界面管理便携版生命周期
5. **Tray 集成**：系统托盘菜单快速访问启动器功能

## 关键文件

| 文件 | 描述 |
|------|------|
| `main.go` | 程序入口：CLI 参数解析、便携版检测、管理器初始化、UI/CLI 模式分发 |
| `manager.go` | 核心管理器（launcherManager）：进程管理、RPC 通信、配置持久化 |
| `deploy.go` | ZIP 部署逻辑：验证 ZIP 完整性、提取文件、处理重复部署 |
| `tray.go` | 系统托盘集成：菜单、快捷操作、窗口通知 |
| `process_windows.go` | Windows 进程管理：启动/停止/查询进程状态 |
| `register_windows.go` | TSF 动态注册：临时注册 DLL、枚举 IM、配置输入法 |
| `winapi_windows.go` | Windows API 封装：FindWindow、PostMessage、Window Band 查询 |
| `go.mod` / `go.sum` | Go 模块依赖 |
| `winres/` | Windows 资源：图标、版本信息 |
| `res/` | 资源文件目录 |

## 便携版工作流

### 1. 检测便携版环境

```go
detectPortableConfig() portableConfig
// 检查同目录下是否存在 portable.marker
// 返回配置：RootDir、AppDataDir、UserDataDir、DLL/EXE 路径等
```

**返回值说明**：
- `RootDir`: 启动器所在目录（便携版根目录）
- `AppDataDir`: 数据目录（`RootDir/data`）
- `UserdataDir`: 用户数据目录（`RootDir/userdata`）
- `PortableMarker`: 标记文件路径（`RootDir/portable.marker`）

### 2. 启动流程

```
main.go:main()
  ├─ detectPortableConfig()  → 检测便携版标记
  ├─ parseCLI()              → 解析 CLI 参数 (--deploy, --start, --stop, --ui)
  ├─ newLauncherManager()    → 初始化管理器
  └─ opts.UI ? showUI() : runCLI()  → 启动 UI 或 CLI 模式
```

### 3. 部署流程（--deploy）

```
deploy.go:validateZip()
  ├─ 验证 ZIP 内必要文件 (wind_input.exe, wind_tsf.dll 等)
  ├─ 检查重复部署 (是否已存在相同版本)
  └─ 提取文件到 RootDir

register_windows.go:registerTSF()
  ├─ 临时注册 DLL (不写 HKLM，仅在当前会话)
  ├─ 调用 InstallLayoutOrTip 注册输入法
  └─ 保存配置到 userdata/
```

### 4. 启动/停止流程

**启动**：
```
manager.go:Start()
  ├─ 检查 wind_input.exe 已启动 (进程互斥锁)
  ├─ RPC 连接失败时启动新进程
  └─ 返回 PID
```

**停止**：
```
manager.go:Stop()
  ├─ 获取 wind_input.exe PID
  ├─ 发送终止信号 / 强制杀死
  └─ 清理临时 DLL 注册
```

## CLI 模式

```bash
# 从 ZIP 部署
wind_portable.exe --deploy path/to/wind-portable.zip

# 启动服务
wind_portable.exe --start

# 停止服务
wind_portable.exe --stop

# 查询状态
wind_portable.exe --status

# 启动 UI 模式（默认）
wind_portable.exe --ui
wind_portable.exe  # 默认行为
```

## 编译与版本号

### 编译标志

编译时通过 `ldflags` 注入版本号：

```bash
go build -ldflags "-X main.version=0.1.0" -o wind_portable.exe
```

### buildvariant 支持

支持多个编译变体（debug/release、32/64 位）：

```go
serviceName = "wind_input" + buildvariant.Suffix() + ".exe"
// 例如：wind_input_debug.exe、wind_input.exe
```

## 依赖关系

### 内部
- `wind_input` - 引擎服务（启动器启动的子进程）
- `wind_setting` - 配置界面
- `wind_tsf` DLL - TSF 输入法核心（需临时注册）
- `pkg/rpcapi` - RPC 通信客户端
- `pkg/buildvariant` - 编译变体管理
- `pkg/config` - 配置管理

### 外部
- `github.com/rodrigocfd/windigo` - Windows GUI 和 API 封装
- `archive/zip` - ZIP 文件处理
- Windows API (user32.dll, kernel32.dll 等)

## 工作指南

### 修改启动器逻辑

1. **新增 CLI 参数**：在 `main.go:parseCLI()` 中添加 flag
2. **修改进程管理**：编辑 `process_windows.go`（进程启动/停止/查询）
3. **修改 DLL 注册**：编辑 `register_windows.go`（TSF 动态注册）
4. **修改部署逻辑**：编辑 `deploy.go`（ZIP 验证和提取）

### 测试便携版

```bash
# 1. 构建启动器
cd wind_portable
go build -o wind_portable.exe

# 2. 准备测试目录
mkdir test_portable
cd test_portable

# 3. 创建便携版标记
touch portable.marker

# 4. 准备组件 ZIP 文件
# (将 wind_input.exe、wind_tsf.dll 等打包为 wind-portable.zip)

# 5. 测试部署
.\wind_portable.exe --deploy wind-portable.zip

# 6. 测试启动
.\wind_portable.exe --start

# 7. 验证输入法可用
# (在文本编辑器中输入)

# 8. 测试停止
.\wind_portable.exe --stop
```

### 调试 Windows API 问题

使用 `check_band.ps1` 诊断 z-order 问题（见 `scripts/AGENTS.md`）：

```powershell
scripts\check_band.ps1 -Loop
```

## 常见问题

### 为什么需要动态注册 TSF DLL？

便携版无法写入 `HKLM` 注册表（不需要管理员权限是设计目标）。解决方案：
- 运行时通过 `CoCreateInstance` 加载 DLL
- 调用 `InstallLayoutOrTip` 临时注册输入法（会话级别）
- 进程退出时自动清理

### 如何处理多个便携版实例？

使用互斥锁（Mutex）防止重复启动：

```go
var mutexName = "Local\\WindPortable" + buildvariant.Suffix() + "Launcher"
```

检查互斥锁可判断已有实例是否运行。

### 便携版与系统已安装版本共存吗？

**完全隔离**：
- 便携版使用 `buildvariant.Suffix()` 区分命名（如 `wind_input_portable.exe`）
- 独立的 userdata 目录（便携版根目录）
- 独立的 COM GUID（配置时选择）

可同时安装和运行。

<!-- MANUAL: -->
