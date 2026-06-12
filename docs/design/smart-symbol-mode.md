# 智能符号模式（Smart Symbol Mode）

## 概述

新增一个可选输入行为：**智能符号模式**，默认关闭。开启后，在中文标点模式下，
当用户提交一个「单体可转标点」（默认 `。，？！：；`）后，若在时限内（默认 500ms）
再次按下产生**同一中文标点**的键，则删除刚上屏的中文标点，替换为单个英文标点。

净效果（纠错语义）：

```
按键序列： 。 → 。（0.5s 内重复，同一符号键）
─────────────────────────────────────────
屏幕最终：  .          （净 1 个英文字符）
```

第二次按键不额外产字；它的语义是「我其实想要英文符号」，因此把前一个中文符号
原地纠正为英文。

## 行为定义

- 选项 `智能符号模式`，默认 **关闭**。
- 仅在中文标点模式下生效（英文标点模式 / CapsLock 视为英文时不产生中文标点，特性自然不触发）。
- 参与转换的符号默认为单体标点 `。，？！：；`，可通过内部配置扩展。
- 时限默认 500ms，可通过内部配置调整。
- 触发判定为「同一符号连按两次」：第二次产生的中文标点必须与第一次相同。
  不同符号（如 `。` 后 `，`）不触发。

## 配置项（`input.` 段）

| key | 类型 | 默认 | 暴露方式 |
|---|---|---|---|
| `input.smart_symbol_mode` | bool | `false` | Web UI「启用」勾选（「字符与标点」卡片，手写控件） |
| `input.smart_symbol_timeout_ms` | int | `500` | Web UI 数字输入（依赖开关启用） |
| `input.smart_symbol_chars` | string | `。，？！：；、～￥·……——` | Web UI「配置」按钮 → 对话框内可换行文本框（含「恢复默认」+ 说明）|

默认集合涵盖非成对的常用中文标点 + 双字符省略号 `……` / 破折号 `——`；成对括号/引号
与 auto-pair 冲突，默认不纳入（用户可在对话框中自行增删）。

**参与集合与替换规则**：`smart_symbol_chars` 是中文标点字符串，按子串包含匹配
（`strings.Contains`），**支持多字符标点**（省略号 `……`、破折号 `——`）与**引号**
（`“”‘’`）。每个标点对应一个 ASCII 触发键；替换文本 = 该键在「英文标点模式」下的产物
（`computePunctStrPure(r, false)`），即半角原键、全角时全角英文、自定义英文全角列；删除
数 = 已武装中文标点串的 rune 数（`……` 删 2）。

中文/英文产物均由 `computePunctStrPure(r, chinese)` 计算，**镜像 `convertPunct` 优先级**
（自定义映射列 > 中文/英文转换 > 全角），其中中文产物经 `PunctuationConverter.PeekChineseStr`
**无副作用地**预测（覆盖多字符与引号左右交替，不修改转换器状态）。

## 触发状态机（coordinator）

新增状态：

```go
smartSymbolArmed bool      // 是否处于「刚提交一个参与中文标点」的待命态
smartSymbolKey   rune      // 触发该标点的 ASCII 按键
smartSymbolStr   string    // 待命的中文标点串（可多字符；引号为单字符）
smartSymbolAt    time.Time // 提交时刻
```

- **置位（arm）**：`handlePunctuation` 入口（普通处理之前），按本次按键的中文产物
  （`smartSymbolArmStr`）武装。引号经 peek 预测 press1 的产物（`“` 或 `”`）。
- **press2 触发判定**：当且仅当以下全部满足才替换，并随即 disarm：
  1. `smartSymbolArmed == true`
  2. `r == smartSymbolKey`（**同一按键**，而非"同一产物"——引号 press1=`“`/press2=`”`
     产物不同，故按 press1 的已武装产物匹配）
  3. 仍处于中文标点模式（`isEffectiveChinesePunct()`）
  4. `now - smartSymbolAt < timeout`
  5. **`PrevChar == 已武装中文标点串的末位 rune`** —— 复用 ITfTextEditSink 的「光标前
     一个字符」（macOS 经 `selectedRange` / `replacementRange` 兜底）。
  - 替换吃掉一个引号后调 `PunctuationConverter.RevertLastQuote(r)` 回退引号交替状态。
- **失效（disarm）**：焦点变化（FocusGained / 焦点丢失）、超时、本次按键非参与标点、
  `PrevChar` 校验不过 —— 任一即放弃，**绝不误删**。

PrevChar 校验是稳健性核心：把"盲删"改为"确认光标前确实是刚提交的中文标点之后再删"，
在用户挪光标 / 点别处 / 应用插入了别的字符等场景下安全退化为普通行为。

## 上屏 / 替换协议（跨平台）

新增通用响应类型 `ResponseTypeReplaceBackward{ count, text }`（v1 固定 `count = 1`，
设计为通用以便复用）：

- **Windows**：`wind_tsf` C++ 在单一**同步** TSF `EditSession` 内完成「取光标选区 →
  起点回退 count 字符 → `SetText(text)` 覆盖」（`CReplaceBackwardEditSession`）。
  同步、原子、不受输入队列时序与修饰键状态影响。TSF 路径失败（非 TSF 控件等）时
  回退到 `SendInput`（count 次 Backspace + Unicode 注入 text，两者同入输入队列，
  顺序一致）。
  > 设计取舍：最初拟用「合成退格 + 提交」，但实现中发现两个正确性缺陷——(1) TSF
  > `CommitText` 同步立即生效，而合成退格经 `SendInput` 入队列稍后处理，二者混用会
  > 导致「先插入、再退格删掉插入」的时序倒错；(2) 默认参与集中 `？！：` 为 Shift+键，
  > 而 `_SimulatePairKey` 在修饰键按住时会推迟发键。故改用 TSF 范围替换（与 macOS
  > `replacementRange` 同思路），合成退格仅作兜底。
- **macOS**：`.app` 使用 IMKit `insertText:replacementRange:`
  （`NSMakeRange(caret-1, 1)`）一步原子替换，无需模拟退格。

协议层新增 `CMD_REPLACE_BACKWARD`（0x0109）命令号与 `EncodeReplaceBackward(count, text)`。

## 关键边界

- **顶字上屏共存**：`PunctCommit` 顶字时提交「顶字 + 。」，尾字仍是「。」，替换
  只动尾部的中文标点，顶字不受影响。
- **三连击 `。。。`**：第三次按键时，光标前是英文 `.`（非记录的中文 rune），
  `PrevChar` 校验不过 → 第三个正常提交为中文「。」，净结果 `.。`。属可接受边界。
- **引号（`“”‘’`）**：默认不在集合，可手动添加。同一按键交替产生左/右引号，故触发
  按「同一按键 + press1 已武装产物」匹配（见状态机）；替换后回退引号交替状态。
- **自动配对冲突**：开启「自动配对」（`auto_pair.chinese` / `auto_pair.english`）后，被
  配对的符号 press1 会插入配对并回退光标，破坏本特性的 PrevChar 假设，故**对被配对的
  符号不生效**（实测确认）。设置对话框已说明。成对括号 `（）【】` 等同理，默认不纳入。
- **多字符标点**：`……`（`^` 键）、`——`（`_` 键）参与时，删除数取实际 rune 数（2），
  `PrevChar` 校验取标点串的末位 rune。C++ / macOS 替换均已按 `count` 参数化，无需改原生层。
- **全角模式**：检测「中文产物」与替换「英文产物」都经 `computePunctStrPure` 跟随全角——
  全角下 `。。` 替换为全角英文 `．`（而非半角 `.`）。
- **自定义符号映射**：自定义映射的键**也参与**（不再排除）。`computePunctStrPure` 镜像
  `convertPunct` 的列优先级（中文半角/英文全角/中文全角），并复用已记录的**原始按键**消歧：
  如把 `/` 自定义为 `、`、而 `\` 默认也是 `、`，双击 `//` 还原为 `/`、`\\` 还原为 `\`。
- **英文标点模式 / CapsLock**：本就不产生中文标点，特性不触发。

## 待办：模式激活键的特殊处理（未实现）

部分符号键同时被配置为「模式激活键」：临时拼音触发键（反引号 `` ` ``）、临时英文触发键、
特殊模式入口键、二三候选选词键（`;` `'`）等。这些键在按键管线里**先于** `handlePunctuation`
被消费，因此智能符号当前对它们**自然不触发**（安全让位，不会破坏模式键）。

用户期望进一步：对这类键，"输出中文符号后再次快速按键"应做**符号切换（转英文）**而非重新进入
模式。由于各模式键的"重复按"行为不一（多数是第二次才输出原符号，故切换发生在第三次按键），
需逐模式追踪其符号输出路径后接入智能符号判定。此项**风险较高、需单独实现**，留作后续。

## 改动文件

### Go

- `pkg/config/config.go`：`InputConfig` 新增 3 字段 + `DefaultConfig` 默认值
- `pkg/config/accessor.go`：新增 `FieldDesc` 条目（命令直通车可调）
- 运行 `go test ./pkg/config -run TestExportKeyPaths -update`
  生成 `configkey/keys_gen.go` + `wind_setting/frontend/src/generated/config-keys.json`
- `internal/coordinator/coordinator.go`：状态字段 + arm/disarm
- `internal/coordinator/handle_punctuation.go`：press2 判定 + 发替换响应
- `internal/bridge/protocol.go`：`ResponseTypeReplaceBackward` + 数据结构
- `internal/ipc/binary_codec.go`：`EncodeReplaceBackward` + 命令号
- `internal/bridge/server_handler.go`（Win）/ `server_darwin.go`（mac）：处理新响应类型

### C++ / 原生

- `wind_tsf/include/BinaryProtocol.h`：新命令号
- `wind_tsf`：处理 replace-backward（退格 ×N + 插入）
- macOS `.app`：通过 `replacementRange` 实现

### 前端

- `wind_setting/frontend/src/api/settings.ts`：`InputConfig` 接口 + `getDefaultConfig`
- `wind_setting/frontend/src/pages/InputPage.vue`：「字符与标点」卡片手写控件——「启用」勾选 +
  「配置」按钮 + 时限数字输入；配置对话框含可换行文本框、说明与「恢复默认」（仿自定义标点映射）
- `wind_setting/frontend/src/pages/input.search.ts`：手写搜索项（`smart_symbol_mode` / `smart_symbol_timeout_ms`）

## 测试

- **Go 单元测试**：状态机表驱动用例 —— armed/超时/同符号/PrevChar 校验/各 disarm
  路径/顶字共存/三连击，仿 `internal/coordinator` 现有 `mixed.go` 多变体测试风格。
- **真机手测**：WPS、EverEdit、浏览器、终端等验证退格替换的实际表现（跨应用稳健性，
  单测无法覆盖）。
