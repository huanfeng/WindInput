# 英文标点配对功能设计文档

> 日期：2026-04-08
> 状态：已确认

## 概述

扩展自动标点配对功能，支持英文标点配对。覆盖三种场景：
1. 中文模式 + 中文标点 → Go 处理，使用 ChinesePairs（已实现）
2. 中文模式 + 英文标点 → Go 处理，使用 EnglishPairs（新增）
3. 英文模式 → C++ 处理，使用 EnglishPairs（新增）

## Go 侧改动（中文模式 + 英文标点）

### 新增英文配对追踪器

在 Coordinator 中新增 `pairTrackerEn *transform.PairTracker`，使用 `EnglishPairs` 初始化。

### shouldAutoPair 扩展

返回对应的 tracker：
- 中文标点 + `Chinese` 开关 → `pairTracker`（ChinesePairs）
- 英文标点 + `English` 开关 → `pairTrackerEn`（EnglishPairs）
- 都关 → nil

### 热更新

`UpdateInputConfig` 中同步更新 `pairTrackerEn`。

## C++ 侧改动（英文模式）

### PairEngine 类

新建轻量级配对引擎（在 KeyEventSink 中使用）：

```cpp
struct PairEntry { wchar_t left; wchar_t right; };

class PairEngine {
    std::map<wchar_t, wchar_t> _pairMap;  // 左→右
    std::set<wchar_t> _rightSet;          // 右标点集合
    std::vector<PairEntry> _stack;         // LIFO 栈
    bool _enabled = false;
public:
    void SetEnabled(bool enabled);
    void SetPairs(const std::vector<std::pair<wchar_t, wchar_t>>& pairs);
    bool IsLeft(wchar_t ch);
    bool IsRight(wchar_t ch);
    wchar_t GetRight(wchar_t left);
    void Push(wchar_t left, wchar_t right);
    bool Peek(PairEntry& entry);
    bool Pop(PairEntry& entry);
    void Clear();
    bool IsEnabled() { return _enabled; }
    bool IsEmpty() { return _stack.empty(); }
};
```

### OnTestKeyDown 拦截

英文模式下，将 VK code + Shift 映射到字符，检查是否在 pairMap 中：

| VK + Modifier | 字符 |
|--------------|------|
| VK_9 + Shift | `(` |
| VK_0 + Shift | `)` |
| VK_OEM_4 | `[` |
| VK_OEM_4 + Shift | `{` |
| VK_OEM_6 | `]` |
| VK_OEM_6 + Shift | `}` |

匹配时 `*pfEaten = TRUE`。

### OnKeyDown 处理

- 左标点 → `CommitText("()")` + `_SimulatePairKey(VK_LEFT)` + Push
- 右标点且栈顶匹配 → `_SimulatePairKey(VK_RIGHT)` 智能跳过
- 右标点不匹配 → 清栈，直通

### 栈失效

- `ResetComposingState`（焦点丢失等）
- 输入非配对字符
- 模式切换

## 通用配置同步协议 CMD_SYNC_CONFIG

### 动机

避免每个配置同步需求都增加新的 CMD，使用通用的 key/value 配置同步命令。

### 协议格式

```
CMD_SYNC_CONFIG (0x0303)
Payload: keyLen(2, LE) + valueLen(4, LE) + key(UTF-8) + value(bytes)
```

### Key 定义（两侧同步）

| Key | Value 格式 | 用途 |
|-----|-----------|------|
| `en_pairs` | enabled(1) + count(1) + pairs(N × 4bytes: left_u16 + right_u16) | 英文配对表 |

### 推送时机

- IME 激活时（`HandleIMEActivated` / `HandleFocusGained`）
- 配置热更新时
- StatusUpdate 推送时（附带推送）

## 设置界面

已完成（前一个提交）。移除英文配对选项的 `disabled` 属性。

## 文件变更清单

### Go 层
| 文件 | 变更 |
|------|------|
| `coordinator.go` | 新增 `pairTrackerEn` 字段 |
| `handle_punctuation.go` | `shouldAutoPair` 返回对应 tracker |
| `handle_config.go` | 热更新 `pairTrackerEn` |
| `handle_lifecycle.go` | 激活/焦点时推送英文配对配置 |
| `bridge/protocol.go` | 无需新增（复用 StatusUpdate 推送时机） |
| `ipc/binary_protocol.go` | 新增 `CmdSyncConfig` |
| `ipc/binary_codec.go` | 新增 `EncodeSyncConfig` |
| `bridge/server_push.go` | 新增推送英文配对配置方法 |

### C++ 层
| 文件 | 变更 |
|------|------|
| `BinaryProtocol.h` | 新增 `CMD_SYNC_CONFIG` |
| `IPCClient.cpp` | 解析 `CMD_SYNC_CONFIG` |
| `KeyEventSink.h` | 新增 `PairEngine` 和英文配对处理 |
| `KeyEventSink.cpp` | OnTestKeyDown/OnKeyDown 英文配对逻辑 |

### 前端
| 文件 | 变更 |
|------|------|
| `InputPage.vue` | 移除英文配对 disabled |
