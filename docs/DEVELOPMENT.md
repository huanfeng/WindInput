# 开发文档

本文档面向希望参与清风输入法 (WindInput) 开发的贡献者，介绍项目架构、开发环境搭建和构建流程。

## 系统要求

- Windows 10 或 Windows 11
- Go 1.24+
- Visual Studio 2017 或更高版本（含 C++ 桌面开发工具）
- CMake 3.15+
- Wails v2 CLI
- Node.js + pnpm（前端构建）
- PowerShell（构建脚本）

## 项目结构

```
WindInput/
├── wind_tsf/              # C++ TSF 核心 (DLL)
│   ├── src/               # 源代码
│   │   ├── dllmain.cpp    # DLL 入口点
│   │   ├── TextService.cpp # TSF 主服务
│   │   ├── KeyEventSink.cpp # 按键处理
│   │   ├── HotkeyManager.cpp # 快捷键管理
│   │   ├── IPCClient.cpp  # 命名管道客户端
│   │   └── ...            # 其他组件
│   └── include/           # 头文件 (含 BinaryProtocol.h)
│
├── wind_input/            # Go 输入服务
│   ├── cmd/service/       # 服务入口（含版本注入）
│   └── internal/
│       ├── bridge/        # IPC 桥接层（二进制协议通信）
│       ├── coordinator/   # 输入协调器（权威状态源）
│       ├── engine/        # 多引擎支持
│       │   ├── pinyin/    # 拼音引擎（全拼 + 双拼）
│       │   │   └── shuangpin/ # 双拼转换器（小鹤/自然码/搜狗/微软）
│       │   ├── wubi/      # 五笔码表引擎
│       │   └── mixed/     # 五笔拼音混输引擎
│       ├── dict/          # 词库管理（分层架构）
│       ├── schema/        # Schema 方案驱动（加载/工厂）
│       ├── ui/            # 候选窗口、工具栏、指示器
│       ├── transform/     # 文本转换（全角/标点）
│       └── control/       # 控制管道（设置工具通信）
│
├── wind_setting/          # 设置工具 (Wails + Vue 3)
│   ├── frontend/          # Vue 3 前端
│   └── *.go               # Go 后端（编辑器：配置/方案/Shadow/短语/用户词库）
│
├── data/                  # 数据文件
│   ├── schemas/           # 输入方案定义 (*.schema.yaml)
│   ├── dict/              # 词库源数据
│   └── examples/          # 示例配置
│
├── dict/                  # 运行时词库数据
│   └── pinyin/            # 拼音词库 (unigram.txt)
│
├── installer/             # 安装/卸载脚本 + NSIS 打包
├── build/                 # 构建输出
└── docs/                  # 开发文档
```

## 技术架构

WindInput 采用 C++/Go 分层架构，以 **Schema（输入方案）驱动** 为核心设计理念：

- **C++ TSF 层 (`wind_tsf.dll`)**: 与 Windows TSF 框架交互，处理系统级输入法注册和键盘事件
- **Go 服务层 (`wind_input.exe`)**: Schema 驱动的输入引擎，候选词管理，UI 渲染
- **设置工具 (`wind_setting`)**: 基于 Wails v2 的桌面应用（Go + Vue 3）

两层通过命名管道 (Named Pipe) 使用自定义二进制协议进行进程间通信。

### 架构图

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

### 管道说明

| 管道 | 用途 |
|------|------|
| `\\.\pipe\wind_input` | TSF→Go 请求/响应（同步） |
| `\\.\pipe\wind_input_push` | Go→TSF 状态推送（异步） |
| `\\.\pipe\wind_input_control` | 设置工具→Go 配置重载 |

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

| 路径 | 用途 |
|------|------|
| `%APPDATA%\WindInput\config.yaml` | 用户配置文件 |
| `%APPDATA%\WindInput\schemas\` | 方案文件 |
| `%LOCALAPPDATA%\WindInput\logs\` | 日志文件 |

## 构建

### 版本管理

版本号统一维护在项目根目录的 `VERSION` 文件中。构建时通过 Go ldflags 注入到二进制文件：

```powershell
# 更新版本号
.\scripts\bump-version.ps1 -Version 0.2.0-alpha
```

### 一键构建

```powershell
.\build_all.ps1
```

支持参数：
- `-WailsMode debug` — 调试模式（默认）
- `-WailsMode release` — 发布模式
- `-WailsMode skip` — 跳过设置工具构建

### 手动分步构建

```powershell
# 1. 构建 Go 服务
cd wind_input
go build -ldflags "-H windowsgui" -o ../build/wind_input.exe ./cmd/service

# 2. 构建 C++ TSF DLL
cd wind_tsf
mkdir build; cd build
cmake ..
cmake --build . --config Release

# 3. 构建设置工具
cd wind_setting
wails build
```

### 生成安装包 (NSIS)

```powershell
installer\build_nsis.ps1 -Version 0.1.0-alpha
```

## 安装与调试

以**管理员权限**运行：

```powershell
# 安装
installer\install.ps1

# 卸载
installer\uninstall.ps1
```

开发调试：

```powershell
.\dev.ps1
```

## 测试

```powershell
# Go 单元测试
cd wind_input
go test ./...

# 前端测试（如有）
cd wind_setting/frontend
pnpm test
```

## 代码规范

- Go 代码修改后运行 `go fmt`
- 前端代码修改后运行格式化工具
- 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/) 规范

## 更多文档

- [architecture.md](architecture.md) — 详细架构设计文档
- [wubi_requirements.md](wubi_requirements.md) — 五笔需求文档
- [FEATURES_TODO.md](FEATURES_TODO.md) — 功能开发计划
