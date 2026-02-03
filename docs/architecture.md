# WindInput 架构设计文档

## 1. 架构概述

WindInput 采用分层架构，将系统接口层和业务逻辑层分离：

- **C++ TSF 层 (wind_tsf.dll)**: 负责与 Windows TSF 框架交互，处理底层系统调用
- **Go 服务层 (wind_input.exe)**: 负责输入逻辑、词库管理、候选窗口渲染

两层通过**命名管道 (Named Pipe)** 进行进程间通信。

### 1.1 为什么采用这种架构？

| 设计决策 | 原因 |
|---------|------|
| C++ 实现 TSF 层 | TSF 是 COM 接口，C++ 是与 COM 交互的最自然选择 |
| Go 实现业务逻辑 | Go 开发效率高，内存安全，适合复杂业务逻辑 |
| 命名管道通信 | Windows 原生支持，延迟低，无需额外依赖 |
| 候选窗口在 Go 层 | 便于自定义渲染，独立于 TSF 生命周期 |

### 1.2 整体数据流

```
用户按键
    │
    ▼
┌─────────────────────────────────────────────┐
│  Windows 应用程序 (记事本/浏览器/...)        │
└────────────────────┬────────────────────────┘
                     │ WM_KEYDOWN
                     ▼
┌─────────────────────────────────────────────┐
│  TSF 框架 (msctf.dll)                        │
└────────────────────┬────────────────────────┘
                     │ ITfKeyEventSink::OnKeyDown
                     ▼
┌─────────────────────────────────────────────┐
│  wind_tsf.dll                                │
│  ┌──────────────────────────────────────┐   │
│  │ KeyEventSink::OnKeyDown              │   │
│  │   1. 判断是否需要处理该按键           │   │
│  │   2. 发送 key_event 到 Go 服务       │   │
│  │   3. 接收响应并执行相应操作           │   │
│  └──────────────────────────────────────┘   │
└────────────────────┬────────────────────────┘
                     │ Named Pipe
                     ▼
┌─────────────────────────────────────────────┐
│  wind_input.exe                              │
│  ┌──────────────────────────────────────┐   │
│  │ Coordinator::HandleKeyEvent          │   │
│  │   1. 更新输入缓冲区                   │   │
│  │   2. 调用拼音引擎获取候选词           │   │
│  │   3. 更新候选窗口                     │   │
│  │   4. 返回响应 (insert_text/ack/...)  │   │
│  └──────────────────────────────────────┘   │
└─────────────────────────────────────────────┘
```

---

## 2. C++ TSF 层 (wind_tsf)

### 2.1 组件概览

| 文件 | 类/功能 | 职责 |
|------|---------|------|
| `dllmain.cpp` | DLL 入口 | DllMain, DllGetClassObject, DllRegisterServer 等 |
| `ClassFactory.cpp` | `CClassFactory` | COM 类工厂，创建 CTextService 实例 |
| `TextService.cpp` | `CTextService` | TSF 主服务，实现 ITfTextInputProcessor |
| `KeyEventSink.cpp` | `CKeyEventSink` | 按键事件处理，实现 ITfKeyEventSink |
| `LangBarItemButton.cpp` | `CLangBarItemButton` | 语言栏图标，实现 ITfLangBarItemButton |
| `IPCClient.cpp` | `CIPCClient` | 命名管道客户端，与 Go 服务通信 |
| `HotkeyManager.cpp` | `CHotkeyManager` | 快捷键管理，热键白名单匹配 |
| `CaretEditSession.cpp` | `CCaretEditSession` | 编辑会话管理，光标位置获取 |
| `DisplayAttributeInfo.cpp` | `CDisplayAttributeInfo` | 显示属性，组字状态样式 |
| `Register.cpp` | 注册函数 | TSF 组件注册/卸载 |
| `Globals.cpp` | 全局变量 | GUID 定义、DLL 引用计数 |
| `BinaryProtocol.h` | 协议定义 | 二进制 IPC 协议结构和命令常量 |

### 2.2 核心类详解

#### 2.2.1 CTextService

TSF 输入法的核心类，实现以下接口：

```cpp
class CTextService : public ITfTextInputProcessor,
                     public ITfThreadMgrEventSink
{
    // ITfTextInputProcessor - 输入法生命周期
    STDMETHODIMP Activate(ITfThreadMgr*, TfClientId);   // 激活输入法
    STDMETHODIMP Deactivate();                          // 停用输入法

    // ITfThreadMgrEventSink - 线程管理器事件
    STDMETHODIMP OnSetFocus(ITfDocumentMgr*, ITfDocumentMgr*);  // 焦点变化

    // 自定义方法
    BOOL InsertText(const std::wstring& text);   // 插入文本
    BOOL GetCaretPosition(LONG*, LONG*, LONG*);  // 获取光标位置
    void ToggleInputMode();                       // 切换中英文模式
    void SetInputMode(BOOL bChineseMode);         // 设置模式（无IPC）
};
```

**生命周期流程**:

```
用户切换到输入法
        │
        ▼
    Activate()
        │
        ├─→ _InitThreadMgrEventSink()  // 注册焦点事件
        ├─→ _InitIPCClient()           // 连接 Go 服务
        ├─→ _InitKeyEventSink()        // 注册按键事件
        └─→ _InitLangBarButton()       // 创建语言栏图标
        │
        ▼
    (输入法运行中...)
        │
        ▼
用户切换离开输入法
        │
        ▼
    Deactivate()
        │
        ├─→ _UninitLangBarButton()
        ├─→ _UninitKeyEventSink()
        ├─→ _UninitIPCClient()
        └─→ _UninitThreadMgrEventSink()
```

#### 2.2.2 CKeyEventSink

处理按键事件的核心类：

```cpp
class CKeyEventSink : public ITfKeyEventSink
{
    // ITfKeyEventSink 接口
    STDMETHODIMP OnTestKeyDown(ITfContext*, WPARAM, LPARAM, BOOL*);  // 预判断
    STDMETHODIMP OnKeyDown(ITfContext*, WPARAM, LPARAM, BOOL*);     // 实际处理
    STDMETHODIMP OnTestKeyUp(ITfContext*, WPARAM, LPARAM, BOOL*);
    STDMETHODIMP OnKeyUp(ITfContext*, WPARAM, LPARAM, BOOL*);

private:
    BOOL _IsKeyWeShouldHandle(WPARAM wParam);  // 判断是否处理该按键
    BOOL _SendKeyToService(WPARAM wParam);      // 发送给 Go 服务
    void _HandleServiceResponse();               // 处理响应
    int _GetModifierState();                     // 获取修饰键状态
};
```

**按键处理逻辑**:

```cpp
BOOL CKeyEventSink::_IsKeyWeShouldHandle(WPARAM wParam)
{
    int modifiers = _GetModifierState();

    // Shift 单独按下 → 切换中英文
    if (wParam == VK_SHIFT) {
        if (modifiers & (MOD_CTRL | MOD_ALT))
            return FALSE;  // Shift+Ctrl/Alt 交给系统
        return TRUE;
    }

    // Ctrl/Alt 组合键 → 不拦截
    if (modifiers & (MOD_CTRL | MOD_ALT))
        return FALSE;

    // 组字状态下处理更多按键
    if (_isComposing) {
        // A-Z, 1-9, Backspace, Enter, Escape, Space
        return TRUE;
    }

    // 非组字状态仅处理字母键
    if (wParam >= 'A' && wParam <= 'Z')
        return TRUE;

    return FALSE;
}
```

#### 2.2.3 CLangBarItemButton

语言栏图标实现，用于显示中/英文状态：

```cpp
class CLangBarItemButton : public ITfLangBarItemButton,
                           public ITfSource
{
    // ITfLangBarItem
    STDMETHODIMP GetInfo(TF_LANGBARITEMINFO*);
    STDMETHODIMP GetIcon(HICON*);

    // ITfLangBarItemButton
    STDMETHODIMP OnClick(TfLBIClick, POINT, const RECT*);

    // 自定义
    void UpdateLangBarButton(BOOL bChineseMode);  // 更新图标
};
```

**Windows 11 兼容性关键点**:

```cpp
// 必须使用 GUID_LBI_INPUTMODE 才能在 Windows 11 输入指示器中显示
DEFINE_GUID(GUID_LBI_INPUTMODE,
    0x2C77A81E, 0x41CC, 0x4178, 0xA3, 0xA7, 0x5F, 0x8A, 0x98, 0x75, 0x68, 0xE1);

const GUID CLangBarItemButton::_guidLangBarItemButton = GUID_LBI_INPUTMODE;
```

#### 2.2.4 CIPCClient

命名管道客户端，负责与 Go 服务通信：

```cpp
class CIPCClient
{
    BOOL Connect();                                      // 连接服务
    void Disconnect();                                   // 断开连接
    BOOL SendKeyEvent(const std::wstring& key, int keyCode, int modifiers);
    BOOL SendCaretUpdate(int x, int y, int height);      // 发送光标位置
    BOOL SendFocusLost();                                // 焦点丢失通知
    BOOL SendToggleMode();                               // 请求切换模式
    BOOL ReceiveResponse(ServiceResponse& response);     // 接收响应

private:
    BOOL _SendMessage(const std::wstring& message);      // 发送 JSON
    BOOL _ReceiveMessage(std::wstring& message);         // 接收 JSON
    BOOL _StartService();                                // 自动启动服务
};
```

### 2.3 TSF 注册

在 Windows 10/11 上正确注册输入法需要注册特定的分类：

```cpp
BOOL RegisterCategories()
{
    // 必须注册的 GUID
    const GUID categories[] = {
        GUID_TFCAT_TIP_KEYBOARD,           // 键盘类输入法
        GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, // UWP 应用支持
        GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,   // 系统托盘/输入指示器支持
        GUID_TFCAT_TIPCAP_UIELEMENTENABLED, // UI 元素支持
    };

    // 使用 ITfCategoryMgr 注册
    for (auto& guid : categories) {
        pCategoryMgr->RegisterCategory(c_clsidTextService, guid, c_clsidTextService);
    }
}
```

**Windows 8+ 注册要求**:

```cpp
// 使用 ITfInputProcessorProfileMgr (Windows 8+) 而非旧的 ITfInputProcessorProfiles
hr = pProfileMgr->RegisterProfile(
    c_clsidTextService,    // CLSID
    TEXTSERVICE_LANGID,    // 语言 (0x0804 = 简体中文)
    c_guidProfile,         // Profile GUID
    szDescription,         // 显示名称
    (ULONG)wcslen(szDescription),
    szIconPath,            // 图标路径
    (ULONG)wcslen(szIconPath),
    IDI_WINDINPUT,         // 图标资源 ID
    NULL,                  // HKL (NULL for TIP)
    0,                     //
    TRUE,                  // 启用
    0                      // 标志
);
```

---

## 3. Go 服务层 (wind_input)

### 3.1 包结构

```
wind_input/
├── cmd/service/main.go          # 服务入口
└── internal/
    ├── bridge/                  # C++ 通信层（兼容层）
    │   ├── protocol.go          # 协议定义
    │   └── server.go            # 命名管道服务端
    │
    ├── ipc/                     # 新 IPC 通信层（二进制协议）
    │   ├── binary_protocol.go   # 二进制协议定义
    │   ├── binary_codec.go      # 二进制编解码器
    │   ├── protocol.go          # 协议常量
    │   └── server.go            # IPC 服务器
    │
    ├── coordinator/             # 输入协调器
    │   └── coordinator.go       # 状态管理、业务逻辑
    │
    ├── engine/                  # 输入引擎（多引擎支持）
    │   ├── engine.go            # 接口定义
    │   ├── manager.go           # 引擎管理器
    │   ├── pinyin/              # 拼音引擎
    │   │   ├── pinyin.go        # 拼音引擎实现
    │   │   └── syllable.go      # 音节解析
    │   └── wubi/                # 五笔引擎
    │       └── wubi.go          # 五笔引擎实现
    │
    ├── dict/                    # 词库（高级功能）
    │   ├── dict.go              # 词库接口
    │   ├── loader.go            # 词库加载器
    │   ├── codetable.go         # 码表处理
    │   ├── common_chars.go      # 通用规范汉字表
    │   ├── manager.go           # 词库管理器
    │   ├── user_dict.go         # 用户词库
    │   └── shadow.go            # 影子词库
    │
    ├── candidate/               # 候选词管理
    │   ├── candidate.go         # 候选词结构
    │   └── filter.go            # 候选词过滤
    │
    ├── state/                   # 状态管理
    │   └── manager.go           # 状态管理器
    │
    ├── control/                 # 控制接口
    │   └── server.go            # 控制服务器（设置工具通信）
    │
    ├── hotkey/                  # 快捷键处理
    │   └── compiler.go          # 快捷键编译器
    │
    ├── transform/               # 文本转换
    │   ├── fullwidth.go         # 全角/半角转换
    │   └── punctuation.go       # 中英文标点转换
    │
    ├── ui/                      # 候选窗口 UI
    │   ├── manager.go           # UI 管理器
    │   ├── window.go            # 窗口操作
    │   ├── renderer.go          # 渲染器
    │   ├── protocol.go          # UI 数据结构
    │   ├── toolbar_window.go    # 工具栏窗口
    │   └── toolbar_renderer.go  # 工具栏渲染器
    │
    └── config/                  # 配置
        └── config.go            # 配置加载/保存
```

### 3.2 核心组件详解

#### 3.2.1 Bridge Server

处理与 C++ 的 IPC 通信：

```go
type Server struct {
    handler       MessageHandler  // 消息处理接口
    logger        *slog.Logger
    activeHandles map[windows.Handle]bool
}

type MessageHandler interface {
    HandleKeyEvent(data KeyEventData) *KeyEventResult
    HandleCaretUpdate(data CaretData) error
    HandleFocusLost()
    HandleToggleMode() bool
}

func (s *Server) Start() error {
    for {
        // 创建命名管道实例
        handle := windows.CreateNamedPipe(`\\.\pipe\wind_input`, ...)

        // 等待客户端连接
        windows.ConnectNamedPipe(handle, nil)

        // 在协程中处理客户端
        go s.handleClient(handle, clientID)
    }
}
```

**消息协议 (长度前缀 + JSON)**:

```
┌──────────────┬─────────────────────────┐
│  4 bytes     │     N bytes             │
│  (uint32)    │     (JSON)              │
│  Length = N  │     Message Body        │
└──────────────┴─────────────────────────┘
```

#### 3.2.2 Coordinator

输入协调器，管理整体输入状态：

```go
type Coordinator struct {
    engine    engine.Engine
    uiManager *ui.Manager
    logger    *slog.Logger
    config    *config.Config

    // 状态
    chineseMode       bool       // 中/英文模式
    inputBuffer       string     // 当前输入的拼音
    candidates        []Candidate
    currentPage       int
    totalPages        int
    candidatesPerPage int

    // 光标位置
    caretX, caretY, caretHeight int
}

func (c *Coordinator) HandleKeyEvent(data bridge.KeyEventData) *bridge.KeyEventResult {
    // 1. 处理修饰键
    if hasCtrl || hasAlt {
        return nil  // 交给系统处理
    }

    // 2. 处理 Shift 切换
    if data.KeyCode == 16 {
        c.chineseMode = !c.chineseMode
        return &KeyEventResult{Type: ResponseTypeModeChanged, ChineseMode: c.chineseMode}
    }

    // 3. 英文模式直接透传
    if !c.chineseMode {
        return &KeyEventResult{Type: ResponseTypeInsertText, Text: key}
    }

    // 4. 中文模式处理
    switch {
    case key is letter:
        c.inputBuffer += key
        c.updateCandidates()
        c.showUI()
    case key is number:
        return c.selectCandidate(num - 1)
    case key == "space":
        return c.selectCandidate(0)
    // ...
    }
}
```

**状态机**:

```
                    ┌──────────────┐
       Shift        │   英文模式    │
    ┌──────────────►│  (透传字母)   │◄─────────────┐
    │               └──────────────┘              │ Shift
    │                                              │
    │               ┌──────────────┐              │
    └───────────────│   中文模式    │──────────────┘
                    │  (空闲状态)   │
                    └───────┬──────┘
                            │ 输入字母
                            ▼
                    ┌──────────────┐
                    │   组字状态    │──── Esc ────► 清空，回到空闲
                    │ (显示候选框)  │
                    └───────┬──────┘
                            │ 选择候选词
                            ▼
                    ┌──────────────┐
                    │   输出文字    │──────────────► 回到空闲
                    └──────────────┘
```

#### 3.2.3 Engine (拼音引擎)

```go
// 引擎接口
type Engine interface {
    Convert(input string, maxCandidates int) ([]candidate.Candidate, error)
    Reset()
}

// 拼音引擎实现
type PinyinEngine struct {
    dict dict.Dict
}

func (e *PinyinEngine) Convert(input string, max int) ([]Candidate, error) {
    // 1. 解析音节
    syllables := parseSyllables(input)  // "nihao" → ["ni", "hao"]

    // 2. 查找词组
    phrases := e.dict.FindPhrases(syllables)

    // 3. 查找单字
    for _, syl := range syllables {
        chars := e.dict.Find(syl)
        // ...
    }

    // 4. 排序并返回
    return sortByWeight(candidates), nil
}
```

#### 3.2.4 UI Manager

管理候选窗口的显示：

```go
type Manager struct {
    window   *CandidateWindow
    renderer *Renderer
    cmdCh    chan UICommand  // 异步命令队列
}

func (m *Manager) Start() error {
    // 创建窗口
    m.window.Create()

    // 启动命令处理协程
    go m.processCommands()

    // 运行消息循环 (阻塞)
    m.window.Run()
}

func (m *Manager) ShowCandidates(candidates []Candidate, input string, x, y, page, totalPages int) {
    // 非阻塞发送命令
    select {
    case m.cmdCh <- UICommand{Type: "show", ...}:
    default:
        // 队列满，丢弃命令
    }
}
```

**异步更新架构**:

```
IPC 协程                    UI 命令协程                 Windows 消息循环
    │                           │                           │
    │  ShowCandidates()         │                           │
    ├─────────────────────────►│                           │
    │  (发送到 cmdCh)           │                           │
    │                           │  读取命令                 │
    │                           ├─────────────────────────►│
    │                           │  渲染 + UpdateWindow      │
    │                           │                           │
```

### 3.3 配置系统

```go
type Config struct {
    General    GeneralConfig    `yaml:"general"`
    Dictionary DictionaryConfig `yaml:"dictionary"`
    Hotkeys    HotkeyConfig     `yaml:"hotkeys"`
    UI         UIConfig         `yaml:"ui"`
}

// 配置路径: %APPDATA%\WindInput\config.yaml
func GetConfigPath() (string, error) {
    configDir, _ := os.UserConfigDir()  // %APPDATA%
    return filepath.Join(configDir, "WindInput", "config.yaml"), nil
}

func Load() (*Config, error) {
    data, err := os.ReadFile(configPath)
    if os.IsNotExist(err) {
        return DefaultConfig(), nil  // 使用默认配置
    }
    // 解析 YAML
}
```

---

## 4. IPC 通信协议

WindInput 使用**二进制协议**进行 C++ 与 Go 之间的高效通信，采用**双管道架构**。

### 4.1 管道架构

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           Go 服务 (wind_input.exe)                        │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                        Coordinator (权威状态源)                   │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│         ↑ 接收请求                              ↓ 推送通知              │
│  ┌──────┴──────┐                         ┌─────┴──────┐              │
│  │  主管道服务  │                         │ 推送管道服务 │              │
│  │ (同步响应)  │                         │ (异步通知)  │              │
│  └──────┬──────┘                         └─────┬──────┘              │
└─────────┼────────────────────────────────────────┼────────────────────┘
          │ \\.\pipe\wind_input                    │ \\.\pipe\wind_input_push
          │ (请求/响应)                            │ (推送)
┌─────────┼────────────────────────────────────────┼────────────────────┐
│  TSF DLL│                                        │                    │
│  ┌──────┴──────┐                         ┌─────┴──────┐              │
│  │ IPCClient   │                         │ AsyncReader │              │
│  │ (发送请求)  │                         │ (接收推送)  │              │
│  └─────────────┘                         └─────────────┘              │
└─────────────────────────────────────────────────────────────────────────┘
```

**主管道 (`\\.\pipe\wind_input`)**:
- TSF → Go 发送请求，等待同步响应
- 用于：按键事件、模式切换请求、焦点事件等
- 同步调用模式，阻塞等待响应

**推送管道 (`\\.\pipe\wind_input_push`)**:
- Go → TSF 单向推送通知
- 用于：状态变更广播、热键更新等
- 独立线程异步读取，不阻塞主流程

### 4.2 协议格式

```
┌──────────────┬──────────────┬──────────────┬─────────────────────────┐
│  2 bytes     │  2 bytes     │  4 bytes     │     N bytes             │
│  Version     │  Command     │  Length = N  │     Payload             │
└──────────────┴──────────────┴──────────────┴─────────────────────────┘
```

**协议版本**: `0x1001` (v1.1，支持 barrier 机制)

### 4.2 上行命令 (C++ → Go)

| 命令 | 值 | 说明 |
|------|-----|------|
| `CMD_KEY_EVENT` | 0x0101 | 按键事件 (KeyDown/KeyUp) |
| `CMD_COMMIT_REQUEST` | 0x0104 | 提交请求（带 barrier 序号） |
| `CMD_FOCUS_GAINED` | 0x0201 | 获得焦点 |
| `CMD_FOCUS_LOST` | 0x0202 | 焦点丢失 |
| `CMD_IME_ACTIVATED` | 0x0203 | 输入法激活 |
| `CMD_MODE_NOTIFY` | 0x0205 | 模式变更通知（本地切换） |
| `CMD_TOGGLE_MODE` | 0x0207 | 请求切换模式（UI 点击） |
| `CMD_CARET_UPDATE` | 0x0301 | 光标位置更新 |

### 4.3 下行命令 (Go → C++)

| 命令 | 值 | 说明 |
|------|-----|------|
| `CMD_ACK` | 0x0001 | 简单确认 |
| `CMD_PASS_THROUGH` | 0x0002 | 按键不处理，透传系统 |
| `CMD_COMMIT_TEXT` | 0x0101 | 提交文字到应用 |
| `CMD_UPDATE_COMPOSITION` | 0x0102 | 更新组字状态 |
| `CMD_CLEAR_COMPOSITION` | 0x0103 | 清除组字 |
| `CMD_COMMIT_RESULT` | 0x0105 | 提交结果（barrier 响应） |
| `CMD_MODE_CHANGED` | 0x0201 | 模式已变更 |
| `CMD_STATUS_UPDATE` | 0x0202 | 完整状态更新 |
| `CMD_SYNC_HOTKEYS` | 0x0301 | 同步热键白名单 |

### 4.4 KeyPayload 结构 (16 字节)

```cpp
struct KeyPayload {
    uint32_t keyCode;      // Virtual key code
    uint32_t scanCode;     // Scan code
    uint32_t modifiers;    // 修饰键状态
    uint8_t  eventType;    // 0=KeyDown, 1=KeyUp
    uint8_t  toggles;      // Toggle 键状态 (CapsLock/NumLock/ScrollLock)
    uint16_t eventSeq;     // 事件序号
};
```

### 4.5 修饰键定义

```go
const (
    ModShift    = 0x0001  // 通用 Shift
    ModCtrl     = 0x0002  // 通用 Ctrl
    ModAlt      = 0x0004  // Alt
    ModWin      = 0x0008  // Windows 键
    ModLShift   = 0x0010  // 左 Shift
    ModRShift   = 0x0020  // 右 Shift
    ModLCtrl    = 0x0040  // 左 Ctrl
    ModRCtrl    = 0x0080  // 右 Ctrl
    ModCapsLock = 0x0100  // CapsLock 标记
)
```

### 4.6 状态标志

```go
const (
    StatusChineseMode    = 0x0001  // 中文模式
    StatusFullWidth      = 0x0002  // 全角模式
    StatusChinesePunct   = 0x0004  // 中文标点
    StatusToolbarVisible = 0x0008  // 工具栏可见
    StatusModeChanged    = 0x0010  // 模式刚变更
    StatusCapsLock       = 0x0020  // CapsLock 开启
)
```

---

## 5. 关键流程

### 5.1 输入法激活流程

```
1. 用户切换到 WindInput
2. Windows 加载 wind_tsf.dll
3. CTextService::Activate() 被调用
4. C++ 连接到 Go 服务 (如果服务未运行，自动启动)
5. 注册按键事件监听
6. 显示语言栏图标
```

### 5.2 输入处理流程

```
1. 用户按下 'n' 键
2. Windows 发送 WM_KEYDOWN 到应用
3. TSF 截获并调用 OnTestKeyDown() → 返回 TRUE (要处理)
4. TSF 调用 OnKeyDown()
5. C++ 发送 {"type":"key_event","data":{"key":"n",...}} 到 Go
6. Go Coordinator 更新 inputBuffer = "n"
7. Go 调用拼音引擎获取候选词
8. Go 更新候选窗口显示
9. Go 返回 {"type":"ack"}
10. (用户继续输入 "i")
11. Go inputBuffer = "ni", 显示 "你 泥 尼..."
12. (用户按空格选择第一个)
13. Go 返回 {"type":"insert_text","data":{"text":"你"}}
14. C++ 调用 SendInput() 模拟输入 "你"
15. Go 隐藏候选窗口
```

### 5.3 中英文切换流程

**方式1: 按 Shift 键**
```
1. 用户按下 Shift
2. C++ 发送 key_event (keycode=16, key="shift")
3. Go Coordinator 切换 chineseMode
4. Go 返回 {"type":"mode_changed","data":{"chinese_mode":false}}
5. C++ 调用 SetInputMode(FALSE)
6. C++ 更新语言栏图标为 "En"
```

**方式2: 点击语言栏图标**
```
1. 用户点击语言栏图标
2. CLangBarItemButton::OnClick() 被调用
3. 调用 CTextService::ToggleInputMode()
4. C++ 发送 {"type":"toggle_mode"}
5. Go 切换 chineseMode
6. Go 返回 {"type":"mode_changed","data":{"chinese_mode":true}}
7. C++ 更新本地状态和图标
```

---

## 6. 调试指南

### 6.1 C++ TSF 日志系统

TSF 层使用分级日志系统，定义在 `Globals.h` 中：

**日志级别**:
| 级别 | 值 | 说明 |
|------|-----|------|
| `OFF` | 0 | 禁用所有日志 |
| `ERROR` | 1 | 仅严重错误 |
| `WARN` | 2 | 警告和错误 |
| `INFO` | 3 | 重要信息（默认） |
| `DEBUG` | 4 | 调试信息 |
| `TRACE` | 5 | 追踪信息（非常详细） |

**使用方法**:

```cpp
// 错误日志（始终输出）
WIND_LOG_ERROR(L"Critical error occurred");
WIND_LOG_ERROR_FMT(L"Error code: %d", errorCode);

// 警告日志
WIND_LOG_WARN(L"Something unexpected");

// 信息日志
WIND_LOG_INFO(L"Operation completed");
WIND_LOG_INFO_FMT(L"Connected to pipe: %s", pipeName);

// 调试日志（需启用 WIND_DEBUG_LOG）
WIND_LOG_DEBUG(L"Detailed debug info");
WIND_LOG_DEBUG_FMT(L"Key: 0x%X, Mods: 0x%X", keyCode, modifiers);

// 追踪日志（非常详细）
WIND_LOG_TRACE(L"Function entry/exit");
```

**配置方式**:

```cpp
// 在 Globals.h 中取消注释以启用详细日志
#define WIND_DEBUG_LOG

// 或编译时指定日志级别
#define WIND_LOG_LEVEL WIND_LOG_LEVEL_DEBUG
```

**查看日志**:
- 使用 DebugView（Sysinternals 工具）
- Visual Studio 输出窗口
- 日志前缀：`[WindInput][LEVEL]`

### 6.2 Go 调试

```go
// 启用 debug 日志
./wind_input.exe -log debug

// 日志会输出到 stdout
// 2024/01/22 10:00:00 INFO HandleKeyEvent key=a keycode=65
```

### 6.3 常见问题

| 问题 | 可能原因 | 解决方案 |
|------|---------|---------|
| 输入法不在列表中 | 注册失败 | 检查是否以管理员权限运行 regsvr32 |
| 语言栏图标不显示 | 未注册 SYSTRAYSUPPORT | 确保注册了所有必需的分类 |
| 候选窗口不显示 | Go 服务未运行 | 检查 wind_input.exe 进程 |
| 按键无响应 | IPC 连接失败 | 检查命名管道权限 |

---

## 7. 扩展指南

### 7.1 添加新的输入引擎

1. 实现 `Engine` 接口:

```go
// internal/engine/wubi/wubi.go
type WubiEngine struct {
    dict dict.Dict
}

func (e *WubiEngine) Convert(input string, max int) ([]Candidate, error) {
    // 五笔编码解析逻辑
}

func (e *WubiEngine) Reset() {
    // 重置状态
}
```

2. 在 main.go 中根据配置选择引擎:

```go
var eng engine.Engine
if cfg.Engine == "wubi" {
    eng = wubi.NewEngine(dict)
} else {
    eng = pinyin.NewEngine(dict)
}
```

### 7.2 添加新的 IPC 消息类型

1. 在 `bridge/protocol.go` 添加类型定义:

```go
const RequestTypeNewFeature RequestType = "new_feature"

type NewFeatureData struct {
    Param1 string `json:"param1"`
}
```

2. 在 `bridge/server.go` 添加处理:

```go
case RequestTypeNewFeature:
    var data NewFeatureData
    json.Unmarshal(request.Data, &data)
    s.handler.HandleNewFeature(data)
```

3. 在 `MessageHandler` 接口添加方法

4. 在 `Coordinator` 实现方法

5. 在 C++ `IPCClient` 添加发送方法

---

## 8. 设置工具架构

### 8.1 概述

WindInput 设置工具 (`wind_setting`) 是独立的 GUI 应用，通过**控制管道**与输入服务通信。

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    wind_setting (设置工具)                               │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │  Vue 3 前端 (Wails)                                              │   │
│  │  - 配置界面                                                       │   │
│  │  - 词库管理                                                       │   │
│  └───────────────────────────┬─────────────────────────────────────┘   │
│                               │ Wails 绑定                              │
│  ┌───────────────────────────┴─────────────────────────────────────┐   │
│  │  Go 后端                                                          │   │
│  │  - 读写配置文件                                                   │   │
│  │  - 发送重载命令                                                   │   │
│  └───────────────────────────┬─────────────────────────────────────┘   │
└───────────────────────────────┼─────────────────────────────────────────┘
                                │
            ┌───────────────────┴───────────────────┐
            │                                       │
            ▼ 直接读写                              ▼ 控制管道
    ┌───────────────────┐               ┌───────────────────────┐
    │ %APPDATA%\WindInput│               │ \\.\pipe\wind_input_  │
    │   config.yaml     │               │      control          │
    │   state.yaml      │               └───────────┬───────────┘
    └───────────────────┘                           │
                                                    ▼
                                    ┌───────────────────────────┐
                                    │   wind_input.exe          │
                                    │   (重新加载配置/词库)      │
                                    └───────────────────────────┘
```

### 8.2 控制管道协议

**管道名称**: `\\.\pipe\wind_input_control`

**支持的命令**:

| 命令 | 说明 |
|------|------|
| `PING` | 心跳检测，验证服务是否运行 |
| `RELOAD_CONFIG` | 重新加载配置文件 |
| `RELOAD_PHRASES` | 重新加载短语定义 |
| `RELOAD_SHADOW` | 重新加载 Shadow 规则 |
| `RELOAD_USERDICT` | 重新加载用户词库 |
| `RELOAD_ALL` | 重新加载所有配置和词库 |
| `GET_STATUS` | 获取服务状态 |

**协议格式**:
```
请求: COMMAND [JSON_ARGS]\n
响应: STATUS [JSON_DATA/MESSAGE]\n

状态: OK | ERROR message | DATA json
```

### 8.3 工作流程

1. **修改配置**：
   - 用户在设置界面修改选项
   - wind_setting 直接写入 `config.yaml`

2. **通知服务**：
   - 发送 `RELOAD_CONFIG` 到控制管道
   - 服务重新加载配置文件并应用

3. **实时生效**：
   - 大部分配置支持热更新
   - 无需重启输入法

---

## 9. 参考资料

- [TSF 官方文档](https://docs.microsoft.com/en-us/windows/win32/tsf/text-services-framework)
- [TSF ITfTextInputProcessor](https://docs.microsoft.com/en-us/windows/win32/api/msctf/nn-msctf-itftextinputprocessor)
- [TSF ITfKeyEventSink](https://docs.microsoft.com/en-us/windows/win32/api/msctf/nn-msctf-itfkeyeventsink)
- [Named Pipes](https://docs.microsoft.com/en-us/windows/win32/ipc/named-pipes)
- [Windows Classic Samples - TSF](https://github.com/microsoft/Windows-classic-samples/tree/main/Samples/Win7Samples/winui/tsf)
