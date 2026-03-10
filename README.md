# 清风输入法 (WindInput) - Windows 输入法

基于 Windows TSF (Text Services Framework) 框架的中文输入法，采用 C++ 实现 TSF 核心层，Go 实现输入引擎和候选窗口。

## 特性

- **TSF 原生集成**: 完全兼容 Windows 10/11 的输入法框架
- **C++/Go 混合架构**: C++ 处理系统接口，Go 处理输入逻辑
- **二进制协议通信**: 高效的进程间通信
- **多引擎支持**: 支持拼音和五笔输入
- **中英文切换**: 可配置切换键（Shift/Ctrl/CapsLock）
- **临时英文模式**: Shift+字母快速输入英文
- **候选窗口**: 跟随光标的原生 Windows 候选窗口
- **工具栏**: 可拖动的状态显示工具栏
- **高 DPI 支持**: 自动适配高分辨率显示器
- **配置文件**: YAML 格式配置，支持热更新
- **设置工具**: 基于 Wails 的图形化设置界面

## 系统要求

- Windows 10 或 Windows 11
- Visual Studio 2017 或更高版本 (含 C++ 桌面开发工具)
- Go 1.21 或更高版本
- CMake 3.15 或更高版本

## 快速开始

### 构建项目

使用一键构建脚本：
```batch
build_all.bat
```

或手动分步构建：

```batch
# 构建 C++ TSF DLL
cd wind_tsf
mkdir build && cd build
cmake ..
cmake --build . --config Release

# 构建 Go 服务
cd wind_input
go build -o ../build/wind_input.exe ./cmd/service
```

### 安装

以**管理员权限**运行：
```batch
installer\install.bat
```

### 卸载

以**管理员权限**运行：
```batch
installer\uninstall.bat
```

### 生成正式安装包 (NSIS)

面向最终用户发布时，使用 NSIS 生成单文件安装包：

```batch
installer\build_nsis.bat 0.1.0
```

输出文件：

```text
build\installer\清风输入法-0.1.0-Setup.exe
```

可选参数（跳过构建，仅打包现有 `build\` 产物）：

```batch
installer\build_nsis.bat 0.1.0 --skip-build
```

静默安装/卸载（用于脚本化部署）：

```batch
build\installer\清风输入法-0.1.0-Setup.exe /S
"%ProgramFiles%\WindInput\uninstall.exe" /S
```

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
│   ├── cmd/service/       # 服务入口
│   └── internal/
│       ├── ipc/           # 二进制协议通信
│       ├── coordinator/   # 输入协调器
│       ├── engine/        # 多引擎支持
│       │   ├── pinyin/    # 拼音引擎
│       │   └── wubi/      # 五笔引擎
│       ├── dict/          # 词库管理（含码表过滤）
│       ├── ui/            # 候选窗口和工具栏
│       ├── transform/     # 文本转换（全角/标点）
│       └── config/        # 配置管理
│
├── wind_setting/          # 设置工具 (Wails + Vue 3)
│   ├── frontend/          # Vue 3 前端
│   └── *.go               # Go 后端
│
├── dict/                  # 词库文件
│   ├── pinyin/            # 拼音词库
│   ├── wubi/              # 五笔词库
│   └── common_chars.txt   # 通用规范汉字表
│
├── installer/             # 安装脚本
│   ├── install.bat
│   └── uninstall.bat
│
├── build/                 # 构建输出
└── docs/                  # 开发文档
```

## 技术架构

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
                  │ \\.\pipe\wind_input (主管道)
                  │ \\.\pipe\wind_input_push (推送管道)
┌─────────────────┼────────────────────────────────────────┐
│  wind_input.exe │           Go                           │
│   ┌─────────────▼─────────────────────────┐             │
│   │   IPC Server (二进制协议)              │             │
│   └─────────────┬─────────────────────────┘             │
│   ┌─────────────▼─────────────────────────┐             │
│   │          Coordinator                   │             │
│   │     输入协调器 (权威状态源)            │             │
│   └─────┬───────────────────┬─────────────┘             │
│         │                   │                            │
│   ┌─────▼─────┐       ┌─────▼──────────┐                │
│   │  Engine   │       │   UI Manager   │                │
│   │拼音/五笔  │       │  候选窗/工具栏  │                │
│   └─────┬─────┘       └────────────────┘                │
│         │                                                │
│   ┌─────▼─────┐   ┌────────────────────┐                │
│   │   Dict    │   │  Control Server    │◄── 设置工具    │
│   │   词库    │   │ \\.\pipe\wind_     │   通知重载     │
│   └───────────┘   │   input_control    │                │
│                   └────────────────────┘                │
└──────────────────────────────────────────────────────────┘
```

### 管道说明

| 管道 | 用途 |
|------|------|
| `\\.\pipe\wind_input` | TSF→Go 请求/响应（同步） |
| `\\.\pipe\wind_input_push` | Go→TSF 状态推送（异步） |
| `\\.\pipe\wind_input_control` | 设置工具→Go 配置重载 |

## 配置

配置文件位于 `%APPDATA%\WindInput\config.yaml`:

```yaml
startup:
  remember_last_state: false   # 记忆前次状态
  default_chinese_mode: true   # 启动默认中文模式

engine:
  type: pinyin                 # pinyin / wubi
  filter_mode: smart           # smart / general / gb18030
  wubi:
    auto_commit_at_4: false    # 四码唯一自动上屏
    top_code_commit: true      # 五码顶字上屏

hotkeys:
  toggle_mode_keys: [lshift, rshift]  # 中英切换键
  commit_on_switch: true       # 切换时编码上屏
  switch_engine: "ctrl+`"      # 切换引擎

input:
  select_key_groups: [semicolon_quote]  # 2/3候选键
  page_keys: [pageupdown, minus_equal]  # 翻页键
  shift_temp_english:
    enabled: true              # 临时英文模式

ui:
  font_size: 18                # 字体大小
  candidates_per_page: 9       # 每页候选数

advanced:
  log_level: info              # 日志级别
```

## 使用方法

1. 安装后，使用 `Win + Space` 或 `Ctrl + Shift` 切换到 **清风输入法**
2. 输入拼音，候选窗口自动显示
3. 使用数字键 1-9 选择候选词，或空格选择第一个
4. 按 `Shift` 切换中英文模式
5. 按 `Esc` 取消当前输入
6. 按 `Enter` 输出原始拼音

## 开发状态

### 核心功能
- [x] TSF 框架集成与 Windows 10/11 兼容
- [x] 二进制协议 IPC 通信
- [x] 拼音引擎
- [x] 五笔引擎
- [x] 候选窗口与工具栏 UI
- [x] 中英文切换（多键位支持）
- [x] 临时英文模式
- [x] 语言栏图标
- [x] DPI 缩放支持

### 输入特性
- [x] 自动上屏策略
- [x] 五码顶字/标点顶字
- [x] 候选翻页配置
- [x] 2/3候选快捷键
- [x] 全角/半角切换
- [x] 中英文标点切换
- [x] 码表过滤（通用规范汉字）

### 待开发
- [ ] 用户词库学习
- [ ] 模糊音支持
- [ ] 自定义短语
- [ ] 云同步

## 文档

详细开发文档请参阅：

- [docs/architecture.md](docs/architecture.md) - 架构设计文档
- [docs/wubi_requirements.md](docs/wubi_requirements.md) - 基于用户功能汇总整理的五笔需求文档

## 许可证

MIT License

## 致谢

- [Windows TSF 官方文档](https://docs.microsoft.com/en-us/windows/win32/tsf/text-services-framework)
- [Windows Classic Samples](https://github.com/microsoft/Windows-classic-samples)
