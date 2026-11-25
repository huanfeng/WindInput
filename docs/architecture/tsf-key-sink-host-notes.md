# TSF 按键回调：宿主行为事实

> 涉及 `wind_tsf/src/KeyEventSink.cpp`。这里记读代码看不出来、又会影响「吃不吃键」决策的宿主行为。
> 「吃了再吐」问题族见 [smart-symbol-compat-notes.md](smart-symbol-compat-notes.md)，
> Ctrl+Space 与 OPENCLOSE 见 [tsf-openclose-compartment-semantics.md](tsf-openclose-compartment-semantics.md)。

## 纯修饰键不能吃，哪怕它被配成切换键

切换判定挂在 keyup 上，吃掉 Shift/Ctrl/Alt 的 keydown 换不来任何东西，却让宿主看不到修饰键：

- **AutoCAD**：按住 Shift 是正交模式覆盖，要在光标移动全程持有 keydown。吃掉后 CAD 反复重建输入
  上下文，每次按键放大成约 10 次焦点重建、每秒近百次，按住 Shift 移光标非常卡。把切换键换成 Ctrl
  就不犯病，只因 CAD 不用 Ctrl 做鼠标移动修饰。
- **Fusion 360**：白名单未加载时按 VK 兜底拦 Ctrl/Shift，会让未配成切换键的修饰键失效；所以兜底只在
  白名单加载前生效。

down 和 up 必须一致放行，只放行一边会让宿主看到卡死的修饰键。CapsLock 不在此列，要吃掉才能压住
大写副作用（有会话时的 CapsLock 绑定由服务进程的 LL 钩子处理）。

## 宿主会为按住的键重复发 keydown

AutoCAD 28 秒内发过 145 次 `test_down`；Word 2010 单次按键也会多次调 `OnTestKeyDown`（Weasel 源码
注明）。切换键的按下计时只能在首次按下时起表，重复事件只刷新修饰键位；否则长按会被误判成轻敲而误切
中英。同理，纯修饰键自身不能走「取消待切换 → 重记」，否则会不断重置计时。

## `ITfKeyEventSink::OnSetFocus` 不可靠

AutoCAD 整场只触发 2 次且全是 `fForeground=1`；曾把 `focus_lost` 挂在它的 else 分支上，结果
`focus_lost` 完全断供。应用切入/切出的权威信号是 `ITfThreadFocusSink::OnSet/KillThreadFocus`
（见 [tsf-docmgr-focus-semantics.md](tsf-docmgr-focus-semantics.md)）。

## 取修饰键与鼠标状态

- `GetAsyncKeyState` 的「自上次调用以来按下过」位每次调用都会清（别的进程调用也会清），所以
  「按着」与「按下过」必须一次调用取；分两次调，第二次恒为 0。这一位只作补充信号。
- WebView2 / Wails 等 Chromium 系宿主里 `GetAsyncKeyState` 对 `VK_LSHIFT/VK_RSHIFT` 返回 0。解析
  左右键优先用 `GetCurrentModifiers` 的双源结果，`GetAsyncKeyState` 只作降级；只靠它会让 Shift
  切换中英失效，或把 Ctrl+Shift 误判成「无其它修饰」。
