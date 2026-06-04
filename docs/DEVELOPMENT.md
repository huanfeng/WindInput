# 开发文档

本文档面向希望参与清风输入法 (WindInput) 开发的贡献者，介绍项目架构、开发环境搭建和构建流程。

清风输入法以 **Windows** 为主平台，同时提供 **macOS** 端（alpha）。Go 输入服务、设置工具的后端与前端为跨平台共享代码，平台差异集中在系统接口客户端（Windows 为 C++ TSF DLL，macOS 为 Swift IMKit `.app`）。

## 系统要求

### 通用

- Go —— 构建 Go 服务端 (`wind_input`) 需 **1.25+**；构建完整套件（含设置工具 `wind_setting`）需 **1.26+**（`wind_setting/go.mod` 要求 `go 1.26.2`）。
  > 建议直接使用 Go 1.26+：在 Go 1.25 下执行 `go install github.com/wailsapp/wails/v2/cmd/wails@latest` 可能拉取到与 `go.mod` 不匹配的旧版 Wails CLI，导致设置工具编译失败。
- Wails **v2.12+** CLI（构建设置工具）
- Node.js + pnpm（设置工具前端构建）

### Windows

- Windows 10 或 Windows 11
- Visual Studio 2017 或更高版本，安装时勾选「使用 C++ 的桌面开发」（自带 MSVC 编译器与 CMake）和「.NET 桌面开发」（用于便携启动器）这两个组件
- CMake 3.15+（通常无需单独安装，见下文「CMake 说明」）
- PowerShell **7+**（构建/安装脚本，见下文「PowerShell 说明」）
- .NET SDK + .NET Framework 4.8 开发包（构建便携启动器 `wind_portable`，VS 的「.NET 桌面开发」负载已包含）

#### CMake 说明

随 Visual Studio「使用 C++ 的桌面开发」组件一起安装的 CMake 为微软定制版，默认查找 VS 生成器与 MSVC 编译器，与 CMake 官方版（默认 NMake/Ninja）行为不同。**建议从「Developer PowerShell for VS」运行构建脚本**，以便直接使用 VS 自带的 CMake 与 MSVC 工具链。

注意系统 `PATH` 环境变量中靠前的 CMake 会被优先使用，可能覆盖 VS 终端期望的 CMake。可用以下命令确认当前 CMake 版本与默认生成器（开头带 `*` 者为默认）：

```powershell
cmake --version
cmake -G          # 列出可用生成器；VS 终端因自动设置环境变量，输出可能与普通终端不同
```

#### PowerShell 说明

Windows 自带的通常是 PowerShell 5，而本项目脚本（如 `installer/uninstall.ps1`）使用了 PowerShell 7 引入的运算符（如 `?.`），**必须安装 PowerShell 7+**。请从 [微软官网](https://learn.microsoft.com/zh-cn/powershell/scripting/install/install-powershell-on-windows) 下载安装。

### macOS（alpha）

- macOS 12 (Monterey) 及以上
- Xcode 15+（含命令行工具，提供 Swift 5.9 工具链、`clang`/`xcrun`/系统 SDK）
- 支持 Apple Silicon 与 Intel（可产 universal 二进制）

> macOS 端构建/调试的完整说明见 [macos-build.md](macos-build.md)，设计背景见 [design/macos-port.md](design/macos-port.md)。

## 项目结构

```
WindInput/
├── wind_tsf/              # C++ TSF 核心 (Windows DLL)
│   ├── src/               # 源代码
│   │   ├── dllmain.cpp    # DLL 入口点
│   │   ├── TextService.cpp # TSF 主服务
│   │   ├── KeyEventSink.cpp # 按键处理
│   │   ├── HotkeyManager.cpp # 快捷键管理
│   │   ├── IPCClient.cpp  # 命名管道客户端
│   │   └── ...            # 其他组件
│   └── include/           # 头文件 (含 BinaryProtocol.h)
│
├── wind_macos/            # macOS Swift IMKit 客户端 (SwiftPM)
│   ├── Package.swift      # WindInputKit / Smoke / App / Demo target
│   └── Sources/           # 协议 codec + UDS 客户端 + IMKit 输入法主体
│
├── wind_input/            # Go 输入服务（跨平台）
│   ├── cmd/service/       # 服务入口（含版本注入）
│   ├── cmd/               # 数据生成工具 (gen_unigram / gen_opencc_dict / ...)
│   ├── tools/dictgen/     # 五笔主词库按词频重排工具
│   └── internal/
│       ├── bridge/        # IPC 桥接层（Win 命名管道 / macOS UDS，二进制协议）
│       ├── coordinator/   # 输入协调器（权威状态源）
│       ├── engine/        # 多引擎支持
│       │   ├── pinyin/    # 拼音引擎（全拼 + 双拼）
│       │   │   └── shuangpin/ # 双拼转换器（小鹤/自然码/搜狗/微软）
│       │   ├── wubi/      # 五笔码表引擎
│       │   └── mixed/     # 五笔拼音混输引擎
│       ├── dict/          # 词库管理（分层架构）
│       ├── schema/        # Schema 方案驱动（加载/工厂）
│       ├── ui/            # 候选窗口、工具栏、指示器
│       ├── transform/     # 文本转换（全角/标点/简繁）
│       └── control/       # 控制管道（设置工具通信）
│
├── wind_setting/          # 设置工具 (Wails v2 + Vue 3，Win / macOS)
│   ├── frontend/          # Vue 3 前端
│   └── *.go               # Go 后端（编辑器：配置/方案/Shadow/短语/用户词库）
│
├── wind_portable/         # 便携启动器 (.NET Framework 4.8 WinForms，Windows)
│
├── data/                  # 数据文件
│   ├── schemas/           # 输入方案定义 (*.schema.yaml)
│   ├── dict/              # 词库源数据
│   └── examples/          # 示例配置
│
├── installer/             # Windows 安装/卸载脚本 + NSIS 打包
├── scripts/               # 通用辅助脚本 (bump-version 等)
├── scripts_mac/           # macOS 构建/部署/打包脚本 (build / deploy / vm / test)
├── build_all.ps1          # Windows 一键构建脚本
├── dev.ps1 / dev.bat      # Windows 开发交互菜单（构建 + 安装 + 部署）
├── dev_mac.sh             # macOS 开发交互菜单
├── build/                 # 构建输出（release 变体）
├── build_debug/           # 构建输出（debug 变体）
└── docs/                  # 开发文档
```

## 技术架构

WindInput 采用 **系统接口层 / Go 业务层** 分离的设计，以 **Schema（输入方案）驱动** 为核心理念：

- **系统接口层**：负责与操作系统输入法框架交互、捕获键盘事件
  - Windows：`wind_tsf.dll`（C++，对接 TSF 框架）
  - macOS：`wind_macos` IMKit `.app`（Swift，对接 InputMethodKit）
- **Go 服务层 (`wind_input`)**：Schema 驱动的输入引擎、候选词管理、UI 渲染（权威状态源，跨平台共享）
- **设置工具 (`wind_setting`)**：基于 Wails v2 的桌面应用（Go + Vue 3）

系统接口层与 Go 服务层通过本机 IPC 使用自定义二进制协议通信：Windows 用命名管道 (Named Pipe)，macOS 用 Unix Domain Socket (UDS)。

### 架构图（Windows）

```
┌──────────────────────────────────────────────────────────┐
│                    Windows 应用程序                       │
│                (记事本、浏览器、Office 等)                 │
└────────────────────────┬─────────────────────────────────┘
                         │ TSF 接口
┌────────────────────────┼─────────────────────────────────┐
│    wind_tsf.dll        │           C++                   │
│   ┌────────────────────▼───────────────────────┐         │
│   │              TextService                   │         │
│   │     ITfTextInputProcessor 实现             │         │
│   └─────────────┬───────────────────┬──────────┘         │
│                 │                   │                    │
│   ┌─────────────▼─────┐   ┌────────▼────────┐           │
│   │  KeyEventSink     │   │ LangBarItemButton│           │
│   │  按键事件处理      │   │   语言栏图标    │           │
│   └─────────────┬─────┘   └─────────────────┘           │
│   ┌─────────────▼─────────────────────────┐             │
│   │   IPCClient (双管道)                   │             │
│   │   主管道: 请求/响应  推送管道: 接收通知 │             │
│   └─────────────┬─────────────────────────┘             │
└─────────────────┼────────────────────────────────────────┘
                  │ Named Pipe (二进制协议)
┌─────────────────┼────────────────────────────────────────┐
│  wind_input.exe │           Go                           │
│   ┌─────────────▼─────────────────────────┐             │
│   │         Bridge (IPC Server)           │             │
│   └─────────────┬─────────────────────────┘             │
│   ┌─────────────▼─────────────────────────┐             │
│   │          Coordinator                   │             │
│   │     输入协调器 (权威状态源)            │             │
│   └──┬──────────┬──────────┬──────────────┘             │
│      │          │          │                             │
│  ┌───▼───┐  ┌──▼───┐  ┌──▼──────────┐                  │
│  │Schema │  │Engine│  │ UI Manager  │                  │
│  │Manager│  │Mgr   │  │候选窗/工具栏│                  │
│  └───┬───┘  └──┬───┘  └─────────────┘                  │
│      │         │                                        │
│      │    ┌────┴────────────────────┐                   │
│      │    │  Pinyin / Wubi / Mixed  │                   │
│      │    └────┬────────────────────┘                   │
│      │    ┌────▼────────────────────┐                   │
│      │    │     DictManager         │                   │
│      │    │  CompositeDict (分层)   │                   │
│      │    └─────────────────────────┘                   │
│      │                                                  │
│  ┌───▼──────────────────────────┐                       │
│  │  Control Server              │◄── wind_setting       │
│  │  \\.\pipe\wind_input_control │    (配置重载通知)     │
│  └──────────────────────────────┘                       │
└──────────────────────────────────────────────────────────┘
```

> macOS 端将上图的 `wind_tsf.dll` 替换为 Swift IMKit `.app`，命名管道替换为 UDS；Go 服务层不变。详见 [design/macos-port.md](design/macos-port.md)。

### IPC 端点

| 平台 | 用途 | 端点 |
|------|------|------|
| Windows | TSF→Go 请求/响应（同步） | `\\.\pipe\wind_input` |
| Windows | Go→TSF 状态推送（异步） | `\\.\pipe\wind_input_push` |
| Windows | 设置工具→Go 配置重载 | `\\.\pipe\wind_input_control` |
| macOS | bridge 请求/响应 | `~/Library/Application Support/WindInput<suffix>/bridge.sock` |
| macOS | bridge 状态推送 | `~/Library/Application Support/WindInput<suffix>/bridge_push.sock` |

> `<suffix>` 由构建变体决定（debug = `_debug`，release = 空）。协议字段速查见 [wire-protocol-reference.md](wire-protocol-reference.md)。

### Schema 驱动架构

每个输入方案通过 YAML 文件定义，位于 `data/schemas/`，包含引擎类型、词库配置、用户数据路径和学习策略：

```
data/schemas/*.schema.yaml → SchemaManager (loader) → SchemaFactory → Engine + Dict
```

内置方案：

| 方案文件 | 引擎类型 | 说明 |
|----------|----------|------|
| `pinyin.schema.yaml` | `pinyin` | 全拼输入 |
| `shuangpin.schema.yaml` | `pinyin`（双拼模式） | 双拼输入（支持小鹤/自然码/搜狗/微软） |
| `wubi86.schema.yaml` | `codetable` | 五笔 86 |
| `wubi86_pinyin.schema.yaml` | `mixed` | 五笔拼音混输 |

### 引擎系统

项目支持三种引擎类型：

**拼音引擎 (`pinyin`)**
- 全拼和双拼共用同一引擎，双拼通过转换器（`shuangpin/converter`）将双键映射为全拼后复用整套算法
- 核心流程：Parser 解析音节 → Lexicon 词库查询 → Viterbi 智能组句 → Ranker 排序
- 支持 Unigram 语言模型

**码表引擎 (`codetable`)**
- 用于五笔等编码类输入法
- 支持四码唯一自动上屏、五码顶字、标点顶字
- 基于 mmap 的二进制码表格式（WDB），快速加载

**混输引擎 (`mixed`)**
- 五笔优先、拼音补充的并行查询策略
- 短编码（<2 字符）仅查五笔，长编码（>4 字符）回退拼音
- 中间范围并行查询，通过权重调整合并候选
- Shadow 规则（置顶/删除）在混输引擎合并后统一应用

### 词库分层架构

```
CompositeDict (聚合词库)
├── PhraseLayer      — 特殊短语（全局）
├── CodeTableLayer   — 五笔码表（按方案）
├── PinyinDictLayer  — 拼音词库（按方案）
├── UserDict         — 用户词库（按方案）
└── ShadowLayer      — 置顶/删除规则（按方案）
```

词库数据支持 WDB 二进制缓存格式，首次加载后自动生成缓存文件加速后续启动。

### 文件路径

| 平台 | 路径 | 用途 |
|------|------|------|
| Windows | `%APPDATA%\WindInput\config.yaml` | 用户配置文件 |
| Windows | `%APPDATA%\WindInput\schemas\` | 方案文件 |
| Windows | `%LOCALAPPDATA%\WindInput\logs\` | 日志文件 |
| macOS | `~/Library/Application Support/WindInput<suffix>/` | 运行时目录（socket / pid / 数据） |
| macOS | `~/Library/Logs/WindInput/wind_input.log` | 服务日志 |

> 便携版（Windows）的数据保存在程序所在目录而非 `%APPDATA%`。

## 构建

### 版本管理

版本号统一维护在项目根目录的 `VERSION` 文件中。构建时通过 Go ldflags 注入到二进制文件，构建号基于 git 提交数自动生成：

```powershell
# 更新版本号
.\scripts\bump-version.ps1 -Version 0.2.0-alpha
```

### Windows 一键构建

```powershell
.\build_all.ps1
```

常用参数：

| 参数 | 说明 |
|------|------|
| `-Module all` | 构建全部（默认）：DLL + 服务 + 设置 + 便携 + 词库数据 |
| `-Module dll\|service\|setting\|portable` | 仅构建指定模块（可组合，如 `-Module dll,service`） |
| `-WailsMode debug` | 设置工具调试模式（默认，可按 F12 打开 DevTools） |
| `-WailsMode release` | 设置工具发布模式 |
| `-WailsMode skip` | 跳过设置工具构建 |
| `-DebugVariant` | 构建可与正式版并存的调试版变体（输出到 `build_debug/`） |
| `-Brief` | 精简输出 |

> 全量构建会自动下载所需词库（白霜拼音、极点五笔、OpenCC 简繁词典等）到 `.cache/`，并生成 Unigram 语言模型与五笔主词库。`.wdb` 二进制词库由运行时按需生成缓存。

### Windows 开发交互菜单

`dev.ps1`（或 `dev.bat`）提供构建、安装、部署的一体化交互菜单，自动按需提权：

```powershell
.\dev.ps1            # 显示菜单
.\dev.ps1 1          # 构建(Release) + 卸载 + 安装
.\dev.ps1 2          # 仅构建(Release)
.\dev.ps1 m123       # 构建并部署 模块 1=DLL 2=服务 3=设置（4=便携）
.\dev.ps1 p          # 构建并部署到便携目录
```

前缀 `d` 表示调试版变体（如 `d1`、`dm12`），详见菜单内说明。

### Windows 手动分步构建

```powershell
# 1. 构建 Go 服务
cd wind_input
go build -ldflags "-H windowsgui -X main.version=0.2.0-alpha" -o ../build/wind_input.exe ./cmd/service

# 2. 构建 C++ TSF DLL（需 x64 + x86 两个架构）
cd wind_tsf
mkdir build; cd build
cmake ..
cmake --build . --config Release

# 3. 构建设置工具
cd wind_setting
wails build

# 4. 构建便携启动器（.NET Framework 4.8）
cd wind_portable
dotnet build -c Release -o ../build /p:AssemblyName=wind_portable
```

### macOS 构建

macOS 端通过 `dev_mac.sh` 交互菜单或 `scripts_mac/` 下脚本构建：

```bash
./dev_mac.sh 1          # 构建全部：Go 服务 + 词库 + IME .app + 设置应用
./dev_mac.sh app        # 仅构建 IME .app bundle
./dev_mac.sh setting    # 仅构建设置应用 (Wails)
./dev_mac.sh pkg --build --universal   # 打 universal 分发安装包 .pkg
```

也可仅交叉编译/本机构建 Go 服务端二进制用于协议调试，完整流程（含 `socat`/Python 模拟客户端、`dlv` 远程调试、TIS 注册检查）见 [macos-build.md](macos-build.md)。

### 生成分发包

```powershell
# Windows：NSIS 安装包
installer\build_nsis.ps1 -Version 0.2.0-alpha
```

```bash
# macOS：.pkg 安装包（含输入法 + 服务 + 设置三件套，universal）
./dev_mac.sh pkg --build --universal
```

## 安装与调试

### Windows

以**管理员权限**运行：

```powershell
installer\install.ps1        # 安装
installer\uninstall.ps1      # 卸载
```

> 卸载脚本会自动处理输入法注销，无需手动关闭已打开的应用或注销系统。

### macOS

per-user 安装（装到 `~/Library` / `~/Applications`，无需 sudo）：

```bash
./dev_mac.sh i      # 安装全部（Go 服务 LaunchAgent + IME .app + 设置应用）
./dev_mac.sh u      # 卸载全部
./dev_mac.sh r      # 前台运行 Go 服务（debug 日志）
```

> macOS 当前版本未做苹果公证，首次启用可能需在「系统设置 → 隐私与安全性」中放行；macOS 26 (Tahoe) 对未公证输入法限制更强。重装前建议先注销或重启以清除输入法注册缓存。

## 测试

```powershell
# Go 单元测试（跨平台逻辑）
cd wind_input
go test ./...

# 前端测试（如有）
cd wind_setting/frontend
pnpm test
```

```bash
# macOS 协议层单测（Swift）
cd wind_macos
swift test

# 在 Windows 上验证 darwin 代码可编译
$env:GOOS = "darwin"; $env:GOARCH = "arm64"
go test -c -o $null ./internal/bridge
```

## 代码规范

- Go 代码修改后运行 `go fmt`
- 前端代码修改后运行格式化工具
- 修改某目录的对外接口、导出常量或文件结构时，需同步更新该目录的 `AGENTS.md`（模板见 [AGENTS-TEMPLATE.md](AGENTS-TEMPLATE.md)）
- 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/) 规范

## 更多文档

- [ARCHITECTURE.md](ARCHITECTURE.md) — 详细架构设计文档
- [requirements/wubi-requirements.md](requirements/wubi-requirements.md) — 五笔需求文档
- [requirements/features-todo.md](requirements/features-todo.md) — 功能开发计划
- [macos-build.md](macos-build.md) — macOS 构建与调试指南
- [design/macos-port.md](design/macos-port.md) — macOS 移植设计
- [wire-protocol-reference.md](wire-protocol-reference.md) — IPC 二进制协议速查
