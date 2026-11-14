# Linux 端工具栏与软键盘

> 状态：已合入 main，随 0.127.0 发布。
> 前置阅读：[linux-port.md](linux-port.md)、`wind_linux/AGENTS.md`。

## 1. 意图

Linux 端（Fcitx5 addon + `linux-host` 服务）补上**和 Windows 版一样**的悬浮工具栏和软键盘。

- 早先认定「Linux 输入法普遍没有工具栏」，所以 linux-port.md 写的是工具栏「确定不做」。复查后发现
  多数中文输入法都带工具栏，这个决定撤回。
- 软键盘当时只是前期验证没来得及加，Linux 形态下被门控关掉了（6eabfc7c 等）。

**发布**：先在分支上做完、真机验完，再由维护者决定随哪个版本发。不绑下一个正式版。

## 2. 目标 / 非目标 / 反目标

**目标**

- 工具栏：固定悬浮条，可拖动，位置会被记住。显隐、格子内容、点击动作、右键菜单、自动隐藏、
  横竖向、主题都和 Windows 一致。
- 软键盘：布局、绘制、点击上屏、功能键、按住连发、拖动、位置记忆、关闭按钮都和 Windows 一致。
- 入口全部恢复：主菜单里的「工具栏」「软键盘」开关、工具栏上的软键盘格、`keys.toggle_toolbar`
  和 `keys.softkeyboard` 两个热键（只在输入焦点内生效，见非目标）。
- 设置端对 Linux 放开这两节设置。
- 覆盖 X11、Wayland（input-method-v2 合成器，以及 KDE 的 v1）、GNOME（走 XWayland）三种会话。

**非目标**

- 全局热键：仍不做。上面两个热键只在 Fcitx5 把按键交给我们时生效，与现在的其它热键一样。
- macOS 补浮动工具栏：macOS 继续用菜单栏状态图标，本次不动。
- 多显示器下按屏记位置的精度做到与 Windows 完全一致：做「按输出名记」，拿不到输出名就退化成
  单一位置（见 §5.4）。

**反目标**（技术上能跑，但这样就算失败）

- 和 Windows 长得不一样、行为不一样：比如另画一套 UI，或者交给 Fcitx5 原生 UI 来画。
- 点工具栏或软键盘会**抢走应用的输入焦点**，导致上屏落空、组合被打断。
- 拖动时卡顿、跟不上鼠标（每个移动事件都去服务端绕一圈）。
- Linux 的门控拆掉后，Windows / macOS 的行为发生变化。

## 3. 现状（2026-10-18）

| 部件 | Windows | macOS | Linux |
|---|---|---|---|
| 软键盘绘制 / 交互 | `wind-ui/src/soft_keyboard.rs`，不分平台 | 同一份，窗口壳是服务进程的 `MacPanel` | 窗口壳是 mock，被门控关闭 |
| 工具栏 | `wind-ui/src/toolbar.rs`，`LayeredWindow` + `WindowMouse` | 没有浮动条，状态走 `CMD_MODE_STATUS` 进菜单栏 | 无 |
| 浮层呈现 | 服务进程自己开窗 | 服务光栅化 → SHM → `CMD_OVERLAY_FRAME` → `.app` 贴图 | 同 macOS，宿主是 addon（X11 override-redirect / Wayland popup / layer-shell / KDE v1） |
| 鼠标回传 | wnd_proc | 菜单走 `CMD_MENU_POINTER` | 菜单走 `CMD_MENU_POINTER`（只有 motion / press） |
| 位置回报 | `ToolbarMoved` / `SoftKeyboardMoved` | — | 气泡拖动走扩展信封 `pos.status_tip` |

Linux 上显式关掉这两样的地方：

- `handle_softkeyboard.rs`：Linux 形态直接返回 `PassThrough`；另有一条测试断言「开不出来」。
- `handle_menu.rs`：主菜单摘掉「显示状态图标」和「软键盘」两项。
- `manager_macos.rs`：软键盘命令在 Linux 下落入兜底分支，什么都不做。
- `wind_linux/src/.../WindEngine.cpp`：overlay 分派只认 kind 1–3 和菜单级。
- wind-setting `settings_manifest.toml`：`keys.toggle_toolbar`、`keys.softkeyboard`、`ui.toolbar.*`
  的 `platform` 不含 linux。

## 4. 已定的部分

以下由维护者在讨论中确定：

1. **样子和行为与 Windows 一致**：固定悬浮条，可拖动。
2. **架构**：沿用 Linux 候选窗的路子，即服务端光栅化、addon 只贴图、鼠标回传服务端。两样东西的布局、
   绘制、命中、状态机继续共用 `toolbar.rs` / `soft_keyboard.rs`，不在 addon 里重写。
3. **顺序**：先做软键盘，同时搭出「可交互浮层」这层通用壳，再让工具栏复用这层壳。
4. **发布**：分支上做完、验完再定（见 §1）。

## 5. 设计

### 5.1 可交互浮层（新的通用壳）

现有 overlay 只能贴图；菜单虽然收鼠标，但需要抓住指针，属于模态。工具栏和软键盘是常驻、非模态的，
需要一种新的浮层：

- **新增 overlay kind**：`OVERLAY_KIND_TOOLBAR`、`OVERLAY_KIND_SOFTKEYBOARD`，取未占用的值，并与
  `ExtProtocol.h` 同步（`protocol_sync_test` 会逐个比对）。
- **落位**：沿用 `OVERLAY_PLACE_EXACT` 摆放；首次显示、或位置失效时用 `OVERLAY_PLACE_ANCHOR`
  （工具栏是右下，软键盘是底部居中，与 Windows 默认一致，以实现时读 Windows 代码为准）。
  宿主负责把浮层夹进工作区。
- **服务端窗口壳**：给 `PanelWindow` / `LayeredWindow` 加一个 `linux-host` 形态的实现，名称由实现者定，
  如 `ExtPanel`。它把 `UpdateLayeredWindow` 的位图写进该 kind 的 SHM，推 `CMD_OVERLAY_FRAME`；
  show / hide / move 都翻译成帧。参照 `overlay_host.rs` 写 SHM 和推帧的现成代码。
- **鼠标回传**：新增一条上行指针消息，不复用 `CMD_MENU_POINTER`，因为它没有 release、leave，也不带
  目标 kind。载荷与 `CMD_MENU_POINTER` 同构、定长：`kind`、`event`（motion / press / release / leave）、
  `button`、浮层内坐标 `x` / `y`。服务端把它翻译成 `WindowMouse::on_message` 能吃的
  `WM_MOUSEMOVE / WM_LBUTTONDOWN / WM_LBUTTONUP / WM_RBUTTONDOWN / WM_MOUSELEAVE`，让 `ToolbarMouse`
  和软键盘现有的状态机原样跑起来。这条路径属于高频，按 `CMD_EXT` 的两档划分要占专用码位。
- **拖动由宿主本地做**（对应反目标「拖动卡顿」）：
  - 帧里带上**拖动区**矩形（工具栏是拖动柄；软键盘是 Windows 上允许拖动的那片区域，实现时照
    `soft_keyboard.rs` 的命中规则导出）。
  - 在拖动区里按下，宿主就本地跟随移动窗口或 layer surface，松手后用扩展信封上报最终位置
    （新增 `pos.toolbar`、`pos.softkeyboard` 两个 kind，在 `ext_kind` 里登记）。服务端落进现有的
    `save_toolbar_anchor` / `save_softkeyboard_anchor`。
  - Wayland 上的拖动沿用气泡已经在用的 relative-pointer 做法。
  - 拖动区以外的按下才回传给服务端。
- **不抢焦点**（对应反目标）：
  - X11：override-redirect 窗口，不接受焦点（不设 `WM_TAKE_FOCUS`，`input=False`）。
  - Wayland：layer-shell 的 `keyboard_interactivity = none`。
  - 点击之后，Fcitx5 当前的输入上下文必须保持不变。这一点是验收项（§7）。
- **计时**：工具栏的自动隐藏和迟滞、软键盘的按住连发都要定时器。Windows 由 UI 线程的消息循环驱动
  `next_deadline` / `tick`；Linux 的 forwarder 线程目前阻塞在命令通道上，没有到期唤醒（见
  `overlay_host.rs` 头注释）。要给 Linux forwarder 补上「按最近 deadline 限时等待」，把 `tick` 驱动起来。
  不要把连发挪到宿主去做，否则连发节奏就和 Windows 是两套了。

### 5.2 软键盘

- 拆掉 §3 列出的门控。被关掉的测试改成正向断言：Linux 形态下能打开，按键能上屏。
- 符号键：沿用 `maybe_convert` 再 `push_commit_text`，走现有的上屏路径。
- 键面 / Ctrl / 功能键要合成真实按键：沿用 macOS 已有的 `CMD_KEY_TAP` 下行，addon 收到后对当前
  输入上下文调用 Fcitx5 的 `forwardKey`。没有焦点上下文时安静丢弃，并记 debug 日志。
- 软键盘打开期间要拦截主键区，这一点由服务端权威驱动（会话位）。6eabfc7c 为 Linux 关掉了这个接管，
  现在要恢复。addon 那边要确认 `STATUS_SOFT_KEYBOARD` 位能决定吃哪些键，对照
  `wind_tsf/src/KeyEventSink.cpp` 的做法。

### 5.3 工具栏

- `toolbar.rs` 的 Win32 依赖（`SetCapture`、`GetWindowRect`、`SetWindowPos`、光标形状）都集中在拖动和
  落位上。拖动在 Linux 由宿主做，这部分代码在 `linux-host` 下不走；绘制、命中、点击、右键都留在原处。
- 显隐：Windows 由 `toolbar_gate.rs` 按「清风是否处于激活」驱动。Linux 用 Fcitx5 的焦点进出和输入法
  切换事件（已有 `CMD_FOCUS_GAINED` / `CMD_FOCUS_LOST` / `CMD_IME_ACTIVATED` / `CMD_IME_DEACTIVATED`）
  喂同一个 gate，迟滞参数不变。
- 全屏隐藏（`hide_in_fullscreen`，维护者定）：
  - X11 精确实现：addon 监听根窗口 `_NET_ACTIVE_WINDOW`，以及活动窗口的
    `_NET_WM_STATE_FULLSCREEN`，状态变化时上报服务端，喂给 `blocked_by_exclusive_fullscreen`
    同一判据。上报走扩展信封，属低频。
  - Wayland 近似实现：拿不到前台全屏状态，工具栏放在 layer-shell 的 **TOP** 层，全屏应用自然会盖住它。
  - 设置项对 Linux 开放。说明文案注明 Wayland 下是近似效果。
- 右键：沿用现有的 `RequestToolbarMenu`，走 Linux 已有的自绘菜单。
- 指针翻译要覆盖 Windows 工具栏用到的全部消息，包括中键（`WM_MBUTTONUP`）。
- 主菜单的「显示工具栏」：Linux 上文案和 Windows 一致（显示工具栏），不用 macOS 的「显示状态图标」。

### 5.4 位置与多显示器

- 服务端拿不到屏幕几何。要么宿主在每次显示时附带「当前输出名 + 工作区」，要么沿用 `pos.*` 回报里已有
  的坐标系。实现时先看 `pos.status_tip` 在 X11 / Wayland 下各用什么坐标，统一成一种。
- 存储：`runtime_state` 的 `toolbar_anchors` / `softkeyboard_anchors` 目前按显示器键存右下角。Linux 用
  输出名（X11 RandR output name / Wayland `wl_output.name`）当键。拿不到输出名时用一个固定键，只记
  一个位置。
- Wayland 跨屏拖动：原面板保留映射与鼠标隐式抓取（仅将像素置透明），额外的无输入预览
  随未钳制的全局逻辑位置移动；按预览中心选输出，再夹进该输出工作区。换屏只重建预览的
  layer 角色，松手才重绑原面板并上报 `pos.*`，避免销毁拖动源后丢失松开事件。
  覆盖负坐标输出、不同尺寸 / 上下错位布局与布局空洞的纯几何测试在
  `wind_linux/tests/overlay_input_test.cpp`；实际隐式抓取行为仍需合成器真机验证。
- 记下的位置落在当前工作区之外（换了显示器布局）时，回落到默认锚点。

### 5.5 设置端（wind-setting）

- `settings_manifest.toml`：`keys.toggle_toolbar`、`keys.softkeyboard`、`ui.toolbar.*` 的 `platform`
  加上 linux。确有不适用的子项（比如依赖 Windows 任务栏的项），逐项判断并保持隐藏，在 PR 里列出来。
- `docs/linux.md` 的对照表跟着改。
- 顺手修 manifest 2608 行那句「macOS 软键盘是 `.app` 的 AppKit 面板」的错误注释（实际是服务进程的面板）。

### 5.6 文档

- `docs/design/linux-port.md` 的范围决策改成指向本文档。
- 文档站（WindInputDocs）的 Linux 页，等发布时一起改，本分支不动。

## 6. 留给实现者的

- 新 kind 的取值、新上行指针消息的码位和命名、`ExtPanel` 的具体形态。
- 软键盘拖动区怎么从 `soft_keyboard.rs` 导出。
- 软键盘在 Wayland 下挂在 layer-shell 的哪一层（工具栏已定为 TOP，见 §5.3）。
- 新的上行指针消息带不带滚轮，以 Windows 两个窗口实际处理的消息为准。
- KDE v1 会话：工具栏和软键盘不走 v1 面板（KWin 全局只认一个面板），走 layer-shell。实现时在 KDE
  靶机上确认。

## 7. 验收

**自动化**

- 服务端单测：`ExtPanel` 的帧编码；指针翻译成 `WM_*`；拖动区导出；`pos.toolbar` / `pos.softkeyboard`
  的落盘；forwarder 的 deadline 驱动（软键盘连发、工具栏自动隐藏）。
- `protocol_sync_test` 覆盖新增常量。
- addon C++ 单测：可交互浮层的指针转发和本地拖动（`wind_linux/tests`）。
- `scripts/linux/e2e.sh`：加两条，「打开软键盘点一个键能上屏」和「工具栏随焦点显隐」。
- Windows / macOS 全量测试不回归。

**真机**（逐条给判据）

| 环境 | 机器 |
|---|---|
| X11（GNOME + Xorg） | Ubuntu 22.04 `.55` |
| X11 与 treeland | Deepin 25 `.50` |
| KDE Wayland | Kubuntu `.51` |
| GNOME Wayland（XWayland 路径） | 尽力而为，不算完成门槛（维护者定）；归到 X11 路径一起验，问题等用户反馈 |

每个环境都要验：

- 工具栏显隐跟随焦点。
- 拖动跟手；重启后位置还在。
- 点格子能切状态，右键能出菜单。
- 软键盘点键能上屏，功能键生效，按住会连发。
- 点工具栏或软键盘不会让应用失焦、不会打断组合。
- X11：前台窗口全屏（如视频播放器全屏）时工具栏隐藏，退出全屏后恢复。Wayland：全屏应用能盖住工具栏。
