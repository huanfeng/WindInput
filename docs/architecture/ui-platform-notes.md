# UI 平台层备忘（Win32 / DirectWrite / AppKit）

`wind-ui` 里平台相关窗口与文字渲染代码的 API 坑点与宿主行为。只收「读代码看不出、以后排查用得上」的事实；
代码注释里以「见 docs/architecture/ui-platform-notes.md 的「章节」」引用。

## 语言栏图标（langbar_icon.rs）

设计、分层渲染、墨迹居中与角标取舍见 `docs/design/langbar-icon-shared-render.md`。补充两条：

- `GetIcon` 没有尺寸参数，系统挑哪档、是否二次缩放从接口上看不出来；`size_marks` 调试开关在图上烧档位点来回答。
- 宽标签（`"En"`、「符号」）按 measure 回缩字号时，排版会把字号吸附到整像素（算出 6.58 实按 7 排），
  单次比例换算会原地打转，须循环且每轮至少降 1px。行盒不含 overhang 与抗锯齿外溢，目标宽度要留余量（0.94）。

## macOS 面板（mac_panel.rs）

- `NSPanel.setFloatingPanel(true)` 只做一件事：把 level 改成浮动层(3)。写在 `setLevel` 之后会把它冲掉。
- 浮动层(3)只压得住普通窗口(0)；Spotlight 与「应用程序」搜索窗在 23，会盖住面板。输入法浮层用
  `NSPopUpMenuWindowLevel`(101)。
- 全局坐标原点恒在 `NSScreen.screens[0]` 左下角；`NSScreen.main` 是有 key window 的屏，会变，不能拿来翻转坐标。
- 非激活面板：视图须 `acceptsFirstMouse` 返回 true，否则第一次点击被系统吃掉用于激活；跟踪区须 `ActiveAlways`，
  非 key window 收不到移动事件；鼠标移出视图没有移动事件，靠 `mouseExited` 清悬停。
- `setContentSize` 保持左下角不动，要左上角不动得自己算 frame。
- `NSGlobalDomain` 的 `InitialKeyRepeat` / `KeyRepeat` 单位是 1/60 秒，未设置时读回 0。

## 弹出菜单（popup_menu.rs）

- 非前台进程的 `SetCapture` 是残缺的：Win32 只有前台窗口能真正捕获鼠标。菜单窗口是
  `WS_EX_NOACTIVATE`，服务进程在菜单显示期间从来不是前台，捕获退化成「光标在本窗口可见区内
  才收得到鼠标消息」。点任务栏、别的应用、原文本框时一条消息都收不到，所以菜单外点击改用
  `GetAsyncKeyState` 轮询（8ms）。这次点击已投递给目标窗口，吞不掉。
- 捕获按线程生效：菜单开着时同线程的工具栏也收不到鼠标消息；关菜单 `ReleaseCapture` 后，
  抬起的 `WM_LBUTTONUP` 才能投递到工具栏。
- `GetAsyncKeyState` 只能用 `0x8000` 位。最低位 `0x0001`（自上次调用以来按过）是进程级共享状态，
  任何调用都会清零它，多处消费会互相偷事件。它问的是物理键位，左右手互换（`SM_SWAPBUTTON`）要自己换算。
- `SetCapture` 期间系统不再发 `WM_SETCURSOR`，光标冻结在捕获瞬间的形状。从任务栏语言栏图标唤出时，
  那一刻光标归 explorer 管，服务重启后它可能正处于忙碌态，菜单期间就一直转圈；故捕获前先设箭头。
- `SetWindowPos(HWND_TOPMOST)` 会把窗口提到 topmost 组最前。多级菜单每帧对每层都调会让 z 序反复交替，
  子菜单翻到左侧压住父面板时闪一帧（DWM 异步合成，不必现）。只在首显或几何变化时摆位。
- 剪贴板：`GetClipboardSequenceNumber` 只读窗口站计数器，不需要 `OpenClipboard`、不参与全局锁；
  返回 0 表示无权限，此时不能用作缓存判据。`OpenClipboard` 会因其它进程短暂持有而失败，需重试。
- macOS 剪贴板：`pbcopy`/`pbpaste` 按 locale 转码，LaunchAgent 拉起的服务没有 `LANG`，中文会乱码，
  须显式设 `LANG`/`LC_ALL` 为 UTF-8。Pasteboard Manager 的 `kPasteboardModified` 语义是「自该 ref
  上次同步以来是否改过」，新建 ref 首次同步恒为 0，所以 ref 必须常驻；项索引从 1 起。

## 悬停提示气泡（tooltip.rs）

- 右键菜单一开就 `SetCapture`，同线程的气泡从此收不到任何鼠标消息；鼠标移向菜单时 `WM_MOUSELEAVE`
  已清掉 `mouse_over`、离开跟踪也失效。所以菜单关闭时 `mouse_over` 是陈旧的，判「光标在不在气泡上」
  要问真实光标。
- 菜单释放捕获后、还没处理过一次鼠标移动时就 `TrackMouseEvent`，系统按捕获期间记的「鼠标所在窗口」
  （菜单）判断，会当场投递一条伪 `WM_MOUSELEAVE`。表现：点气泡关菜单、或菜单开着时右键气泡另一处，
  气泡随之消失。对策是「离开」也问真实光标，仍在气泡上就只清跟踪；光标一次都没在气泡上移动时那条
  移动永远不来，需定时复查。

## 软键盘（soft_keyboard.rs）

设计、焦点契约、长按重复、macOS 大写锁定的证伪记录见 docs/design/soft-keyboard.md。补充几条 Win32 事实：

- `WM_MOUSEWHEEL` 的 lparam 是屏幕坐标（其它鼠标消息是客户区坐标），须 `ScreenToClient`。滚轮消息按规矩发给
  焦点窗口，`WS_EX_NOACTIVATE` 面板能收到它全靠系统设置「悬停即滚动非活动窗口」（Win10 起默认开），
  用户关掉就收不到，所以滚轮只能是快捷方式，不能是唯一入口。
- `WM_MOUSELEAVE` 要 `TrackMouseEvent(TME_LEAVE)` 订阅，且是一次性的，收到后系统即注销，需在下次
  `WM_MOUSEMOVE` 时重订。不订阅的话鼠标快速划出窗口时最后一格悬停高亮会一直亮着。
- `SPI_GETWORKAREA` 恒取主屏工作区，多显示器下要按目标点 `MonitorFromPoint` + `GetMonitorInfo` 取。
- `GetKeyState` 高位是按住、最低位是 toggle 态（大写锁定看最低位）。

## DirectWrite 文字渲染（text/dwrite.rs）

- `SetFontFamilyName` 对不存在的字族不报错，静默回落到默认/系统回退字体。自定义字体集
  （方案自带的拆字/词库字体）尤其隐蔽：机器上装了同款字体时画面「有字」但字形来自系统那份，
  没装时 PUA 段直接空白；按测量宽度判断会假绿，要看解析返回值或着墨像素。所以
  `resolve_family_in` 先在集里 `FindFamilyName`，找不到退回集里第一个家族并 warn；设置全局字体时
  另用 `family_exists` 补一个信号。方案作者常把 `font_family` 写成文件名或别名，而不是 TTF `name` 表里的家族名。
- 同一区间先后两次 `SetFontFamilyName`，后设者赢，故调用顺序即优先级。
- 自定义 `IDWriteFontFallback` 是整体替换系统回退，不是叠加；要在末尾 `AddMappings(GetSystemFontFallback())`，
  否则未覆盖的字符（emoji、少数民族文字、符号）全成方框。`AddMapping` 的 base/target 语义见
  docs/design/mongolian-vertical-candidates.md。
- DirectWrite 默认行高取该行所有参与渲染字体中最大的 line metrics，emoji 回退会把行撑高、基线下移。
  `DWRITE_LINE_SPACING_METHOD_UNIFORM` 必须显式给基线，没有按比例自动分配，故基线也要从探针读。
- `DWRITE_WORD_WRAPPING_NO_WRAP` 只关自动换行，`\n` 硬换行照旧生效。
- `IDWriteBitmapRenderTarget::DrawGlyphRun` 只接受不含 alpha 的 COLORREF，半透明文字色要自己在回写时混合。
  GDI 只对不透明背景做抗锯齿混合，故先把背景以不透明拷进 DIB，渲染后按 RGB 是否变化区分文字像素。
- drawing effect 是格式属性，不参与整形；`DrawGlyphRun` 回调收到的 effect 指针与 `SetDrawingEffect`
  传入的相同（wine 与 Windows 都如此），可按指针身份反查。
- `IDWriteGdiInterop::CreateFontFromLOGFONT` 对查不到的名字做 GDI 模糊匹配、返回「最近似」字体，
  且 `lfFaceName` 只有 31 字符；按名字找 face 要遍历字体集比对字体自报的全名与 Win32 family 名。
- wine 下的 DirectWrite 不做抗锯齿，字形按整像素落位；多线程并发建 COM 工厂会让 wine 进程异常退出，
  像素测试须 `--test-threads=1`。

## 工具栏（toolbar.rs）

- Win32 `RECT` 的 right/bottom 是排他边界。拿它们去 `MonitorFromPoint(DEFAULTTONEAREST)` 查屏，贴屏幕右/下边缘时
  会查到相邻那块屏且不报错，须先退 1px。
- 跨 DPI 换屏的那一帧，`window.size()` 还是上一块屏 DPI 下的尺寸；用它钳制位置并写回会让位置永久走样。
  依赖尺寸的落点与钳制只能在按目标屏排完版之后做。
