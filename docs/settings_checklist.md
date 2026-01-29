# WindInput 设置功能检查列表

> 本文档用于追踪设置界面中每个选项的实现状态
>
> **最后更新**: 2026-01-29

## 状态说明

| 标记 | 含义 |
|------|------|
| ✅ | **测试通过** - 代码实现完整，功能正常工作 |
| ⚠️ | **代码已实现测试异常** - Go 端有实现但 C++ 端未完全支持 |
| ❌ | **代码未实现** - 设置界面有选项但后端代码未实现 |
| ❓ | **待验证** - 需要运行时测试确认 |
| 🔧 | **已修复** - 问题已修复，待验证 |

---

## 一、常用设置 (general)

### 1.1 输入模式切换

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 1 | 拼音/五笔切换 | `engine.type` | ✅ | | API + Ctrl+` 切换 |

### 1.2 默认状态

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 2 | 记忆前次状态 | `startup.remember_last_state` | ✅ | | RuntimeState 保存/加载 |
| 3 | 初始语言模式 | `startup.default_chinese_mode` | ✅ | | 中文/英文 |
| 4 | 初始字符宽度 | `startup.default_full_width` | ✅ | | 半角/全角 |
| 5 | 初始标点模式 | `startup.default_chinese_punct` | ✅ | | 中文标点/英文标点 |

---

## 二、输入习惯 (input)

### 2.1 字符与标点

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 6 | 候选字符范围 | `engine.filter_mode` | ❓ | | smart/general/gb18030 |
| 7 | 标点随中英文切换 | `input.punct_follow_mode` | ✅ | | 切换模式时同步标点 |

### 2.2 五笔设置

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 8 | 自动上屏 | `engine.wubi.auto_commit` | 🔧 | | 已添加热更新支持和调试日志 |
| 9 | 空码处理 | `engine.wubi.empty_code` | ❓ | | none/clear/clear_at_4/to_english |
| 10 | 五码顶字 | `engine.wubi.top_code_commit` | 🔧 | | 已添加热更新支持和调试日志 |
| 11 | 标点顶字 | `engine.wubi.punct_commit` | ✅ | | 标点上屏首选 |

### 2.3 拼音设置

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 12 | 五笔反查提示 | `engine.pinyin.show_wubi_hint` | ❓ | | 候选词旁显示五笔编码 |

---

## 三、外观设置 (appearance)

### 3.1 候选窗口

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 13 | 字体大小 | `ui.font_size` | ❓ | | 12-36px |
| 14 | 每页候选数 | `ui.candidates_per_page` | ✅ | | 3-9 个 |
| 15 | 自定义字体 | `ui.font_path` | ❓ | | 字体文件路径 |

### 3.2 编码显示

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 16 | 嵌入式编码行 | `ui.inline_preedit` | ✅ | | 编码显示在光标处 |

### 3.3 状态栏

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 17 | 显示工具栏 | `toolbar.visible` | ✅ | | 可拖动状态栏 |

---

## 四、按键设置 (hotkey)

### 4.1 中英文切换

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 18 | 切换按键 | `hotkeys.toggle_mode_keys` | 🔧 | | 已实现 HotkeyManager，支持 lshift/rshift/lctrl/rctrl/capslock |
| 19 | 切换时编码上屏 | `hotkeys.commit_on_switch` | 🔧 | | 已修复 CommitOnSwitch 功能 |

### 4.2 功能快捷键

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 20 | 切换拼音/五笔 | `hotkeys.switch_engine` | 🔧 | | 已通过 HotkeyManager 实现动态配置 |
| 21 | 切换全角/半角 | `hotkeys.toggle_full_width` | 🔧 | | 已通过 HotkeyManager 实现动态配置 |
| 22 | 切换中/英文标点 | `hotkeys.toggle_punct` | 🔧 | | 已通过 HotkeyManager 实现动态配置 |

### 4.3 候选选择键

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 23 | 2/3候选快捷键组 | `input.select_key_groups` | 🔧 | | 已通过 HotkeyManager 实现动态配置 |

**可选键组**:
- `semicolon_quote`: ; ' 键
- `comma_period`: , . 键
- `lrshift`: L/R Shift
- `lrctrl`: L/R Ctrl

### 4.4 翻页键

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 24 | 翻页快捷键 | `input.page_keys` | 🔧 | | 已通过 HotkeyManager 实现动态配置 |

**可选键组**:
- `pageupdown`: PgUp/PgDn
- `minus_equal`: - / = 键
- `brackets`: [ / ] 键
- `shift_tab`: Shift+Tab / Tab

---

## 五、高级设置 (advanced)

| 序号 | 功能 | 配置项 | 初步状态 | 实测状态 | 备注 |
|------|------|--------|----------|----------|------|
| 25 | 日志级别 | `advanced.log_level` | ✅ | | debug/info/warn/error |

---

## 本次修复内容

### 修复 #1: 统一快捷键管理架构 (HotkeyManager)

**实现内容**:
- 新增 `wind_tsf/include/HotkeyManager.h` 和 `wind_tsf/src/HotkeyManager.cpp`
- C++ 端 HotkeyManager 类负责解析和管理快捷键配置
- 支持三种拦截状态：任意状态、输入状态、候选状态
- Go 端通过 `status_update` 响应将配置同步给 C++ 端

**涉及修改**:
- `wind_tsf/include/TextService.h/cpp`: 集成 HotkeyManager
- `wind_tsf/include/IPCClient.h/cpp`: 解析 hotkeys 配置
- `wind_tsf/src/KeyEventSink.cpp`: 使用 HotkeyManager 判断键拦截
- `wind_input/internal/bridge/protocol.go`: 新增 HotkeyConfig 结构
- `wind_input/internal/coordinator/coordinator.go`: 在状态更新中返回热键配置

---

### 修复 #2: 自动上屏功能热更新

**实现内容**:
- `engine/manager.go`: 新增 `UpdateWubiOptions()` 方法
- `settings/config_handler.go`: 配置变更时调用热更新方法
- `engine/wubi/wubi.go`: 添加详细调试日志

---

### 修复 #3: 五码顶字功能

**实现内容**:
- `engine/wubi/wubi.go`: `HandleTopCode()` 添加详细调试日志
- 配置通过 `UpdateWubiOptions()` 热更新

---

### 修复 #4: CommitOnSwitch 功能

**问题**: C++ 端 `ToggleInputMode()` 调用 `SendToggleMode()`，而 Go 端 `HandleToggleMode()` 没有 CommitOnSwitch 逻辑

**修复内容**:
- `bridge/server.go`: 修改 `HandleToggleMode` 接口返回 `(commitText, chineseMode)`
- `coordinator/coordinator.go`: 在 `HandleToggleMode()` 中实现 CommitOnSwitch 逻辑
- 返回 `insert_text` 类型响应（带 mode_changed 标记）

---

## 验证检查表

请在实际测试后更新 "实测状态" 列:

- [ ] 第 1-5 项: 常用设置
- [ ] 第 6-12 项: 输入习惯
- [ ] 第 13-17 项: 外观设置
- [ ] 第 18-24 项: 按键设置
- [ ] 第 25 项: 高级设置

---

## 调试建议

对于标记为 🔧 的功能，可通过以下方式验证：

1. **查看日志输出**:
   - 五笔引擎日志以 `[Wubi]` 开头
   - 可观察 `checkAutoCommit` 和 `HandleTopCode` 的调试信息

2. **验证配置同步**:
   - C++ 端 `OutputDebugStringW` 输出可用 DebugView 查看
   - 确认 `hotkeys` 配置已正确传递给 HotkeyManager

3. **测试 CommitOnSwitch**:
   - 输入拼音后按 Shift 切换，验证首选是否自动上屏
