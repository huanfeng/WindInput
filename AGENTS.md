<!-- Generated: 2026-04-08 | Updated: 2026-05-01 -->

# WindInput - 清风输入法

## Purpose

中文输入法，支持拼音和五笔双模式。采用平台特定 IME 框架 + Go 输入引擎 + Vue 3 设置界面的混合架构。核心采用 **Schema（输入方案）驱动架构**，通过 YAML 方案文件定义引擎类型、词库配置和学习策略。

### 平台支持
- **Windows** (主线, 全功能): C++ TSF DLL + Go 服务 + Wails 设置端
- **macOS** (推进中, Go 服务端可编译): Go 服务端已完成 darwin 全包编译, 产出 Mach-O 二进制; **macOS IMKit `.app` 工程未启动**, 当前仅可作为 IPC server 运行, 不能实际输入. 详见 [`docs/design/macos-port.md`](docs/design/macos-port.md) 与 [`docs/macos-build.md`](docs/macos-build.md).

### 三大模块
- **wind_tsf** (Windows 专属): C++ TSF 桥接层 DLL
- **wind_input** (跨平台): Go 输入引擎服务 (Win/macOS 共用代码 + 平台分支文件)
- **wind_setting** (Windows 主, macOS 未做): Wails 设置界面应用
- **(规划中) WindInput.app**: macOS IMKit `.app` 客户端, 对应 `wind_tsf` 的 macOS 等价物 (PR-A 未启动)

## Architecture

```
┌──────────────┐     IPC (Named Pipe)     ┌──────────────────┐
│  wind_tsf    │ ◄───────────────────────► │   wind_input     │
│  C++ DLL     │     Binary Protocol      │   Go Service     │
│  TSF Bridge  │                          │   Input Engine   │
└──────────────┘                          └──────────────────┘
                                                   ▲
                                                   │ Control IPC
                                                   ▼
                                          ┌──────────────────┐
                                          │  wind_setting    │
                                          │  Wails GUI       │
                                          │  Vue 3 Frontend  │
                                          └──────────────────┘

Schema 驱动流程:
  data/schemas/*.schema.yaml → SchemaManager → EngineFactory → Engine + Dict
```

- **wind_tsf**: C++17 DLL，实现 Windows TSF (Text Services Framework) 接口，负责系统级输入法注册和键盘事件捕获；采用 HostWindow 机制解决 Win11 开始菜单候选框 z-order 问题
- **wind_input**: Go 服务进程，Schema 驱动的核心输入引擎（拼音连续评分 + 五笔码表），候选词管理，UI 渲染；通过 CGO 直接调用系统 dwrite.dll
- **wind_setting**: Wails v2 桌面应用，Go 后端 + Vue 3 前端，提供用户设置和方案管理界面

## Key Files

| File | Description |
|------|-------------|
| `build_all.ps1` | PowerShell 一键构建脚本（Go 服务 + C++ DLL + Wails 设置界面 + 词库下载），支持 debug/release/skip 参数 |
| `dev.ps1` | 开发调试启动脚本 |
| `dev.bat` | dev.ps1 的 bat 包装 |
| `CLAUDE.md` | AI Agent 工作指南 |

## Subdirectories

| Directory | Purpose |
|-----------|---------|
| `wind_tsf/` | C++ TSF 桥接层 DLL (see `wind_tsf/AGENTS.md`) |
| `wind_input/` | Go 输入引擎服务 (see `wind_input/AGENTS.md`) |
| `wind_setting/` | Wails 设置界面应用 (see `wind_setting/AGENTS.md`) |
| `data/` | Schema 方案定义、词库源数据、默认配置文件 (see `data/AGENTS.md`) |
| `docs/` | 项目文档：design/ 设计方案、requirements/ 需求规划、testing/ 测试指南、archive/ 历史文档 (see `docs/AGENTS.md`) |
| `dict/` | 运行时词库数据（unigram 等） |
| `installer/` | 安装/卸载脚本 (see `installer/AGENTS.md`) |
| `scripts/` | 构建辅助和工具脚本（Windows 版本管理、IME/窗口诊断）(see `scripts/AGENTS.md`) |
| `scripts_mac/` | macOS 构建/部署/诊断工具链（build/deploy/test/vm）(see `scripts_mac/AGENTS.md`) |
| `wind_portable/` | 便携版启动器工具（部署、进程管理、TSF 动态注册）(see `wind_portable/AGENTS.md`) |
| `pic/` | 项目截图和图片资源 |

## For AI Agents

### Working In This Directory
- 构建命令: `.\build_all.ps1` (PowerShell，支持 `-WailsMode debug/release/skip` 参数)
- 构建产物输出到 `build/` 目录
- 不要主动进行 git commit（功能未测试前）和 git push
- 每次修改完 Go 代码需运行 `go fmt`
- 前端代码修改完需格式化
- 不需要提醒输入法卸载相关事项

### 枚举与"魔法字符串"约束（强制）
**红线**：任何有限取值字符串（行为/模式/按键名/组合键群/Wails 事件名）必须通过具名常量引用，前后端互为镜像；YAML/JSON 协议字面量不可单边修改。
完整规则、SSOT 文件清单、Go/前端样板、PR 自检命令见 [`docs/design/enum-constraint.md`](docs/design/enum-constraint.md)。

### Build Steps
1. `[1/6]` Go 服务: `cd wind_input && go build -ldflags "-H windowsgui" -o ../build/wind_input.exe ./cmd/service`
2. `[2/6]` C++ DLL: `cd wind_tsf/build && cmake .. && cmake --build . --config Release`（仅输出 wind_tsf.dll；wind_dwrite.dll 已移除，Go 侧通过 CGO 直接调用系统 dwrite.dll）
3. `[3/6]` 设置界面: `cd wind_setting && wails build [-debug]`
4. `[4/6]` 下载白霜拼音 rime-frost 词库到 `.cache/rime-frost/`
5. `[5/6]` 复制词库、Schema 配置和默认配置（config.toml）到 `build/`
6. `[6/6]` 验证构建产物

### Testing Requirements
- Go 测试: `cd wind_input && go test ./...`
- 前端: `cd wind_setting/frontend && pnpm test`（如有）

### IPC Protocol
- wind_tsf ↔ wind_input: Named Pipe (`\\.\pipe\wind_input`) 使用自定义二进制协议
- wind_tsf ← wind_input: Push Pipe (`\\.\pipe\wind_input_push`) 异步状态推送
- wind_setting → wind_input: Control IPC 进行配置管理和热重载通知

## Dependencies

### External
- Go 1.24+ with toolchain go1.24.2
- CMake 3.15+ / MSVC (C++17)
- Wails v2 CLI
- pnpm (前端包管理)
- Node.js (前端构建)
- PowerShell (构建脚本)

### Data Sources
- 拼音词库: [白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)
- 五笔词库: Rime 生态格式（自描述加载）

## AGENTS.md 索引（ToC）

> 本仓库每个有意义的目录下都放有 AGENTS.md。新增/重构模块时使用 [`docs/AGENTS-TEMPLATE.md`](docs/AGENTS-TEMPLATE.md) 模板。

### 跨模块全局文档

| 路径 | 用途 |
|------|------|
| [`docs/AGENTS-TEMPLATE.md`](docs/AGENTS-TEMPLATE.md) | AGENTS.md 写作模板与字段规范 |
| [`docs/design/enum-constraint.md`](docs/design/enum-constraint.md) | 枚举与魔法字符串约束 SSOT |
| [`docs/design/macos-port.md`](docs/design/macos-port.md) | macOS 移植设计 (架构、协议、目录约定、stub 边界) |
| [`docs/design/macos-imkit-plan.md`](docs/design/macos-imkit-plan.md) | macOS IMKit `.app` 工程详细开发计划 (PR-A 实战手册) |
| [`docs/wire-protocol-reference.md`](docs/wire-protocol-reference.md) | bridge + uicmd 协议字节布局速查 |
| [`docs/macos-build.md`](docs/macos-build.md) | macOS 构建/调试指南 (实用文档) |
| [`scripts/lint_agents_md.ps1`](scripts/lint_agents_md.ps1) | AGENTS.md 引用路径有效性扫描脚本 |

### wind_input/（Go 服务）

| 路径 | 用途 |
|------|------|
| [`wind_input/AGENTS.md`](wind_input/AGENTS.md) | Go 模块根：架构分层、构建命令 |
| [`wind_input/cmd/AGENTS.md`](wind_input/cmd/AGENTS.md) | service 主入口、词库生成工具入口 |
| [`wind_input/internal/AGENTS.md`](wind_input/internal/AGENTS.md) | internal 包总索引 |
| [`wind_input/internal/coordinator/AGENTS.md`](wind_input/internal/coordinator/AGENTS.md) | 输入流程编排（key action、加词、候选操作） |
| [`wind_input/internal/engine/AGENTS.md`](wind_input/internal/engine/AGENTS.md) | Schema 驱动的引擎工厂（拼音/码表/混合） |
| [`wind_input/internal/dict/AGENTS.md`](wind_input/internal/dict/AGENTS.md) | 词库分层架构、Shadow pin/delete、CompositeDict |
| [`wind_input/internal/schema/AGENTS.md`](wind_input/internal/schema/AGENTS.md) | Schema 类型与 Manager |
| [`wind_input/internal/ipc/AGENTS.md`](wind_input/internal/ipc/AGENTS.md) | Go 端二进制协议（与 wind_tsf 镜像） |
| [`wind_input/internal/bridge/AGENTS.md`](wind_input/internal/bridge/AGENTS.md) | 命名管道桥接业务层 |
| [`wind_input/internal/uicmd/AGENTS.md`](wind_input/internal/uicmd/AGENTS.md) | 平台无关的 UI 命令/事件数据模型（Win/macOS 共享） |
| [`wind_input/internal/rpc/AGENTS.md`](wind_input/internal/rpc/AGENTS.md) | 控制 IPC（与 wind_setting 通信） |
| [`wind_input/pkg/AGENTS.md`](wind_input/pkg/AGENTS.md) | pkg 子包总索引 |
| [`wind_input/pkg/config/AGENTS.md`](wind_input/pkg/config/AGENTS.md) | 配置加载与枚举常量（SSOT） |
| [`wind_input/pkg/keys/AGENTS.md`](wind_input/pkg/keys/AGENTS.md) | 按键名/修饰键/组合键群（SSOT） |
| [`wind_input/pkg/rpcapi/AGENTS.md`](wind_input/pkg/rpcapi/AGENTS.md) | Wails 事件名常量 |
| [`wind_input/themes/AGENTS.md`](wind_input/themes/AGENTS.md) | 主题 YAML |

### wind_setting/（Wails 设置界面）

| 路径 | 用途 |
|------|------|
| [`wind_setting/AGENTS.md`](wind_setting/AGENTS.md) | Wails 应用根 |
| [`wind_setting/internal/AGENTS.md`](wind_setting/internal/AGENTS.md) | Go 后端逻辑 |
| [`wind_setting/frontend/AGENTS.md`](wind_setting/frontend/AGENTS.md) | Vue 3 前端根 |
| [`wind_setting/frontend/src/AGENTS.md`](wind_setting/frontend/src/AGENTS.md) | 前端源码总入口 |
| [`wind_setting/frontend/src/lib/AGENTS.md`](wind_setting/frontend/src/lib/AGENTS.md) | 前端枚举常量清单（SSOT 镜像） |
| [`wind_setting/frontend/src/api/AGENTS.md`](wind_setting/frontend/src/api/AGENTS.md) | HTTP/Wails 双 API 封装 |
| [`wind_setting/frontend/src/pages/AGENTS.md`](wind_setting/frontend/src/pages/AGENTS.md) | 各设置页面组件 |
| [`wind_setting/frontend/src/components/AGENTS.md`](wind_setting/frontend/src/components/AGENTS.md) | 可复用组件 |
| [`wind_setting/frontend/src/components/dict/AGENTS.md`](wind_setting/frontend/src/components/dict/AGENTS.md) | 词库管理组件 |
| [`wind_setting/frontend/src/composables/AGENTS.md`](wind_setting/frontend/src/composables/AGENTS.md) | Vue composables |

### wind_tsf/（C++ TSF 桥接）

| 路径 | 用途 |
|------|------|
| [`wind_tsf/AGENTS.md`](wind_tsf/AGENTS.md) | C++ DLL 架构与 IPC 协议（与 wind_input 镜像） |
| [`wind_tsf/include/AGENTS.md`](wind_tsf/include/AGENTS.md) | 头文件（含 BinaryProtocol.h） |
| [`wind_tsf/src/AGENTS.md`](wind_tsf/src/AGENTS.md) | 实现文件（TextService、IPCClient、HostWindow…） |
| [`wind_tsf/res/AGENTS.md`](wind_tsf/res/AGENTS.md) | 图标与版本资源模板 |

### docs/、data/、installer/、scripts/

| 路径 | 用途 |
|------|------|
| [`docs/AGENTS.md`](docs/AGENTS.md) | 文档总索引 |
| [`docs/design/AGENTS.md`](docs/design/AGENTS.md) | 设计方案文档 |
| [`docs/requirements/AGENTS.md`](docs/requirements/AGENTS.md) | 需求规划文档 |
| [`docs/testing/AGENTS.md`](docs/testing/AGENTS.md) | 测试指南 |
| [`docs/release-notes/AGENTS.md`](docs/release-notes/AGENTS.md) | 发版记录 |
| [`docs/archive/AGENTS.md`](docs/archive/AGENTS.md) | 历史文档 |
| [`data/AGENTS.md`](data/AGENTS.md) | Schema 方案、词库源数据、默认配置 |
| [`data/schemas/AGENTS.md`](data/schemas/AGENTS.md) | Schema YAML 定义 |
| [`installer/AGENTS.md`](installer/AGENTS.md) | 安装器总览 |
| [`installer/nsis/AGENTS.md`](installer/nsis/AGENTS.md) | NSIS 安装脚本 |
| [`scripts/AGENTS.md`](scripts/AGENTS.md) | Windows 构建辅助与诊断脚本 |
| [`scripts_mac/AGENTS.md`](scripts_mac/AGENTS.md) | macOS 构建/部署/诊断工具链 |

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
