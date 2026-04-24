# 独立状态提示窗口设计文档

> 日期：2026-04-11
> 状态：已实现

## 概述

将状态提示从候选窗口共用模式升级为独立窗口（StatusWindow），解决候选窗口与状态提示互斥显示的问题，并支持合并状态显示、常驻/临时两种显示模式、可配置外观和鼠标交互。

## 背景

### 原有问题

1. **窗口共用** — 状态提示和候选窗口共享同一个 HWND，显示提示时必须清除候选窗口的交互数据，无法同时显示
2. **信息割裂** — 切换中英文显示"中/英"、切换标点显示"中。/英."、切换全半角显示"全/半"，无法一眼看到完整状态
3. **瞬时显示** — 仅在切换时闪现后消失，用户切到其他窗口后忘记当前状态
4. **样式固定** — 渲染参数写死，用户无法自定义外观

## 功能设计

### 核心特性

1. **独立窗口** — StatusWindow 拥有独立 HWND（`IMEStatusWindow`），与候选窗口完全解耦，可同时显示
2. **合并状态显示** — 输入模式 + 标点状态 + 全半角合并为一行紧凑显示
3. **临时/常驻模式** — 临时模式切换时闪现后自动隐藏；常驻模式有焦点时始终显示
4. **可配置外观** — 字号、透明度、圆角、自定义颜色
5. **鼠标交互** — 拖动定位、右键菜单快捷操作

### 合并状态显示格式

| 状态 | 显示示例 | 说明 |
|------|---------|------|
| 五笔 + 中文标点 + 半角 | `五笔 。 ◑` | 默认状态 |
| 英文 + 英文标点 + 半角 | `英 . ◑` | 英文模式 |
| CapsLock + 中文标点 + 全角 | `A 。 ●` | 大写锁定 |

- 输入模式：支持全称（"五笔"）和简写（"五"）两种风格
- 标点状态：中文标点 `。`，英文标点 `.`
- 全半角：全角 `●`（实心圆），半角 `◑`（半实心圆）

### 显示模式

| 模式 | 行为 | 适用场景 |
|------|------|---------|
| 临时显示 | 切换时闪现，可配置时长后自动隐藏 | 默认模式，适合大多数用户 |
| 常驻显示 (beta) | 有输入焦点时始终显示，失去焦点时隐藏 | 需要常看状态的用户 |

### 位置策略

- **临时模式**：跟随光标位置，支持偏移配置。拖动后位置临时有效，下次显示自动归位
- **常驻模式**：相对前台窗口偏移定位，默认左上角 (10, 10)。拖动后记忆相对偏移，窗口移动时跟随

### 鼠标交互

- **拖动**：任何模式下均可拖动。临时模式下拖动位置在下次显示时重置；常驻模式下保存相对偏移
- **悬停**：临时模式下鼠标悬停时暂停自动隐藏，离开后重新开始倒计时
- **右键菜单**：
  - 切换为常驻/临时显示
  - 打开外观设置
  - 隐藏状态提示

## 技术架构

### 新增文件

| 文件 | 职责 |
|------|------|
| `wind_input/internal/ui/status_window.go` | 独立分层窗口、消息处理、鼠标拖动、右键菜单、自动隐藏 |
| `wind_input/internal/ui/status_renderer.go` | 状态文本渲染、主题/自定义颜色、透明度、圆角 |

### 窗口创建

- 使用 `CreateLayeredWindow`（与候选窗口、工具栏相同的基础设施）
- 窗口类名 `IMEStatusWindow`，注册在 `WindowRegistry[StatusWindow]`
- 样式：`WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`
- 渲染引擎默认 DirectWrite（与系统默认一致）

### 数据流

```
状态切换（快捷键/工具栏/菜单）
    │
    ▼
Coordinator.updateStatusIndicator()
    │ 合并 ModeLabel + PunctLabel + WidthLabel
    ▼
UIManager.ShowStatusIndicator(StatusState, x, y)
    │ 通过 cmdCh 发送到 UI 线程
    ▼
Manager.doShowStatus()
    │
    ├─ Host Render 路径 → 共享内存渲染
    │
    └─ 本地窗口路径
       │
       ├─ StatusRenderer.Render() → *image.RGBA
       └─ StatusWindow.Show(x, y) → UpdateLayeredWindow
```

### 配置结构

```yaml
ui:
  status_indicator:
    enabled: true
    duration: 800          # 临时显示时长(ms)
    display_mode: "temp"   # "temp" | "always"
    schema_name_style: "full"  # "full" | "short"
    show_mode: true
    show_punct: true
    show_full_width: false
    position_mode: "follow_caret"  # "follow_caret" | "custom"
    offset_x: 0
    offset_y: 0
    font_size: 18
    opacity: 0.9
    border_radius: 6
```

### 关键设计决策

| 决策 | 原因 |
|------|------|
| 独立 HWND 而非分区域绘制 | 分层窗口 `UpdateLayeredWindow` 是整体替换，无法局部更新；独立窗口有独立生命周期 |
| 版本号 + goroutine 实现自动隐藏 | 与候选窗口现有的 `modeIndicatorVersion` 模式一致，简单可靠 |
| 右键菜单期间不自动隐藏 | PopupMenu 使用 `SetCapture`，强制隐藏会破坏全局鼠标捕获状态 |
| 常驻模式用相对偏移 | 绝对坐标在窗口移动后位置错位，相对偏移能跟随前台窗口 |
| 渲染引擎默认 DirectWrite | 反锯齿效果显著优于 GDI，与系统默认配置一致 |
| 字重固定 400 (Normal) | 小尺寸文字使用 Medium(500) 过粗，Normal 更清晰 |

## 已知限制

- 常驻显示模式标记为 beta，前台窗口检测依赖 `GetForegroundWindow`，在某些场景下可能不精确
- Host Render 路径（用于 SearchHost.exe 等特殊进程）仅支持显示，需 DLL 侧协议扩展才能完整支持
- 自定义颜色配置（`background_color`/`text_color`）前端 UI 暂未提供颜色选择器，需手动编辑配置文件

## 设置界面

位于外观设置页面（AppearancePage），包含：

- 总开关
- 显示模式选择（临时/常驻）
- 临时显示时长（仅临时模式）
- 方案名风格（全称/简写）
- 显示内容选择（模式/标点/全半角）
- 位置模式（跟随光标/自定义）
- 偏移量设置（跟随光标模式）
- 字体大小、透明度、圆角
