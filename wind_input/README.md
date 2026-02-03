# wind_input - Go 输入服务

WindInput 输入法的 Go 服务端，负责输入逻辑处理、词库管理和候选窗口显示。

## 功能

- **多引擎支持**: 拼音引擎、五笔引擎
- **词库系统**: 码表加载、通用字过滤、用户词库
- **候选词管理**: 智能排序与过滤
- **二进制 IPC**: 高效的进程间通信协议
- **候选窗口**: 原生 Win32 窗口
- **工具栏**: 可拖动状态显示
- **配置系统**: YAML 格式，支持热更新
- **DPI 支持**: 自动适配高分辨率

## 项目结构

```
wind_input/
├── cmd/service/
│   └── main.go              # 服务入口
└── internal/
    ├── ipc/                 # IPC 通信层（二进制协议）
    │   ├── binary_protocol.go  # 协议定义
    │   ├── binary_codec.go     # 编解码器
    │   └── server.go           # IPC 服务器
    │
    ├── bridge/              # 兼容层（JSON 协议）
    │   ├── protocol.go
    │   └── server.go
    │
    ├── coordinator/         # 输入协调器
    │   └── coordinator.go   # 状态管理、核心逻辑
    │
    ├── engine/              # 多引擎支持
    │   ├── engine.go        # 引擎接口
    │   ├── manager.go       # 引擎管理器
    │   ├── pinyin/          # 拼音引擎
    │   │   ├── pinyin.go
    │   │   └── syllable.go
    │   └── wubi/            # 五笔引擎
    │       └── wubi.go
    │
    ├── dict/                # 词库系统
    │   ├── dict.go          # 词库接口
    │   ├── loader.go        # 加载器
    │   ├── codetable.go     # 码表处理
    │   ├── common_chars.go  # 通用规范汉字
    │   ├── manager.go       # 词库管理
    │   └── user_dict.go     # 用户词库
    │
    ├── candidate/           # 候选词
    │   ├── candidate.go     # 候选词结构
    │   └── filter.go        # 候选词过滤
    │
    ├── state/               # 状态管理
    │   └── manager.go
    │
    ├── hotkey/              # 快捷键处理
    │   └── compiler.go
    │
    ├── transform/           # 文本转换
    │   ├── fullwidth.go     # 全角/半角
    │   └── punctuation.go   # 标点转换
    │
    ├── control/             # 控制接口
    │   └── server.go        # 与设置工具通信
    │
    ├── ui/                  # UI 组件
    │   ├── manager.go       # UI 管理器
    │   ├── window.go        # 候选窗口 (Win32)
    │   ├── renderer.go      # 渲染器
    │   ├── toolbar_window.go   # 工具栏窗口
    │   └── toolbar_renderer.go # 工具栏渲染
    │
    └── config/              # 配置系统
        └── config.go
```

## 构建

需要 Go 1.21+：

```bash
go build -o ../build/wind_input.exe ./cmd/service

# 构建 Windows GUI 应用 (无控制台窗口)
go build -ldflags "-H windowsgui" -o ../build/wind_input.exe ./cmd/service
```

## 运行

```bash
# 默认运行
./wind_input.exe

# 指定词库
./wind_input.exe -dict path/to/dict.txt

# 调试模式
./wind_input.exe -log debug

# 保存默认配置
./wind_input.exe -save-config
```

## 管道架构

Go 服务监听三个命名管道：

| 管道 | 方向 | 用途 |
|------|------|------|
| `\\.\pipe\wind_input` | TSF→Go | 主通道，同步请求/响应 |
| `\\.\pipe\wind_input_push` | Go→TSF | 推送通道，异步状态通知 |
| `\\.\pipe\wind_input_control` | 设置→Go | 控制通道，配置重载 |

**主管道**: 处理按键事件、模式切换等，同步返回响应。

**推送管道**: 向 TSF 推送状态变更、热键更新等通知。

**控制管道**: 接收设置工具的重载命令（`RELOAD_CONFIG` 等）。

## 核心组件

### IPC Server

处理与 C++ TSF 的二进制协议通信：

```go
// 主管道：同步请求/响应
type Server struct {
    handler MessageHandler
}

// 推送管道：异步通知
type PushServer struct {
    clients map[*PushClient]bool
}
```

命名管道：`\\.\pipe\wind_input`

### Coordinator

输入状态协调器，核心业务逻辑：

```go
type Coordinator struct {
    engine    engine.Engine
    uiManager *ui.Manager

    chineseMode bool       // 中/英文模式
    inputBuffer string     // 当前拼音输入
    candidates  []Candidate
    currentPage int
}
```

状态流转：
- 英文模式：直接透传字母
- 中文模式：累积拼音 → 查询候选词 → 显示候选窗口
- Shift：切换中英文

### Engine

输入引擎接口（支持多引擎）：

```go
type Engine interface {
    Convert(input string, maxCandidates int) ([]Candidate, error)
    Reset()
    Type() string
}
```

**拼音引擎**:
- 音节解析: `nihao` → `["ni", "hao"]`
- 词组匹配: 优先匹配长词
- 权重排序: 高频词优先
- 五笔反查提示

**五笔引擎**:
- 码表匹配: 精确匹配编码
- 自动上屏: 四码唯一时可自动上屏
- 五码顶字: 第五码自动顶出首选
- 标点顶字: 标点自动提交首选

### UI Manager

候选窗口管理：

```go
type Manager struct {
    window   *CandidateWindow
    renderer *Renderer
    cmdCh    chan UICommand  // 异步命令队列
}
```

特点：
- 异步更新，不阻塞 IPC
- 使用 Go image 渲染
- 调用 Win32 API 显示窗口
- 自动 DPI 缩放

### Config

配置系统：

```go
type Config struct {
    General    GeneralConfig    `yaml:"general"`
    Dictionary DictionaryConfig `yaml:"dictionary"`
    Hotkeys    HotkeyConfig     `yaml:"hotkeys"`
    UI         UIConfig         `yaml:"ui"`
}
```

配置路径：`%APPDATA%\WindInput\config.yaml`

## IPC 协议（二进制）

使用高效的二进制协议，协议版本 v1.1。

### 上行命令 (C++ → Go)

| 命令 | 代码 | 说明 |
|------|------|------|
| `CMD_KEY_EVENT` | 0x0101 | 按键事件 |
| `CMD_COMMIT_REQUEST` | 0x0104 | 提交请求（barrier） |
| `CMD_FOCUS_GAINED` | 0x0201 | 获得焦点 |
| `CMD_FOCUS_LOST` | 0x0202 | 焦点丢失 |
| `CMD_IME_ACTIVATED` | 0x0203 | 输入法激活 |
| `CMD_CARET_UPDATE` | 0x0301 | 光标位置更新 |

### 下行命令 (Go → C++)

| 命令 | 代码 | 说明 |
|------|------|------|
| `CMD_ACK` | 0x0001 | 简单确认 |
| `CMD_PASS_THROUGH` | 0x0002 | 按键透传 |
| `CMD_COMMIT_TEXT` | 0x0101 | 提交文字 |
| `CMD_UPDATE_COMPOSITION` | 0x0102 | 更新组字 |
| `CMD_MODE_CHANGED` | 0x0201 | 模式变更 |
| `CMD_STATUS_UPDATE` | 0x0202 | 状态更新 |
| `CMD_SYNC_HOTKEYS` | 0x0301 | 同步热键 |

## 词库格式

纯文本格式，每行一个词条：

```
拼音 汉字 权重
```

示例 (`dict/pinyin/base.txt`)：
```
# 这是注释
ni 你 100
ni 泥 20
hao 好 100
nihao 你好 150
zhongguo 中国 150
```

- `#` 开头为注释
- 空行忽略
- 权重越高排序越靠前

## 配置文件

`%APPDATA%\WindInput\config.yaml`：

```yaml
general:
  start_in_chinese_mode: true   # 启动默认中文
  log_level: info               # debug/info/warn/error

dictionary:
  system_dict: dict/pinyin/base.txt
  user_dict: user_dict.txt

hotkeys:
  toggle_mode: shift            # 中英切换键

ui:
  font_size: 18                 # 字体大小
  candidates_per_page: 9        # 每页候选数
  font_path: ""                 # 自定义字体路径
```

## 依赖

```
golang.org/x/sys/windows  # Windows API
golang.org/x/image/font   # 字体渲染
gopkg.in/yaml.v3          # YAML 解析
```

## 调试

启用详细日志：
```bash
./wind_input.exe -log debug
```

日志输出到 stdout，使用 `slog` 结构化日志：
```
2024/01/22 10:00:00 INFO HandleKeyEvent key=a keycode=65 modifiers=0
2024/01/22 10:00:00 INFO Input buffer updated buffer=a
2024/01/22 10:00:00 INFO Got candidates count=5
```

## 单例控制

服务使用命名互斥体确保单例运行：
- 互斥体名称: `Global\WindInputIMEService`
- 如果已有实例运行，新实例会弹出提示并退出

## 扩展

### 添加新引擎

1. 实现 `Engine` 接口：

```go
type WubiEngine struct {
    dict dict.Dict
}

func (e *WubiEngine) Convert(input string, max int) ([]Candidate, error) {
    // 五笔编码逻辑
}

func (e *WubiEngine) Reset() {}
```

2. 在 `main.go` 中注册

### 添加新消息类型

1. 在 `bridge/protocol.go` 定义
2. 在 `bridge/server.go` 处理
3. 在 `MessageHandler` 接口添加方法
4. 在 `Coordinator` 实现

## 注意事项

- 服务需要在 C++ DLL 加载前启动
- 候选窗口渲染使用 Go 原生图像库
- Windows 消息循环运行在独立协程
- IPC 处理和 UI 更新是异步的
