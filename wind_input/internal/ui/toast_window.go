package ui

import (
	"log/slog"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
	"unsafe"

	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

// ToastWindow 是一个独立的 layered 通知窗口，用于显示一次性提示（错误、词库就绪等）。
// 与 StatusWindow（持续状态指示器）和 TooltipWindow（候选悬停提示）平级，互不共享实例。
//
// 设计要点：
//   - 不抢焦点（继承 CreateLayeredWindow 的 WS_EX_NOACTIVATE）。
//   - 左键单击立即隐藏。
//   - 右键预留 onRightClick 回调（暂未启用，方便未来加"复制内容"等菜单）。
//   - 自动隐藏：用 atomic 版本号取消旧定时器，新 Show 不被旧 Hide 抢跑。
//   - 不依赖用户配置：尺寸/时长全部使用合理默认值。
type ToastWindow struct {
	hwnd   windows.HWND
	logger *slog.Logger

	mu      sync.Mutex
	visible bool
	width   int
	height  int

	hideVersion atomic.Uint64 // 取消待执行 Hide 用

	renderer      *ToastRenderer
	resolvedTheme *theme.ResolvedTheme

	onLeftClick  func()         // 默认为 Hide；外部可覆盖
	onRightClick func(x, y int) // 预留：未来用于弹"复制 / 关闭"右键菜单
	currentOpts  ToastOptions   // 最近一次 Show 的内容，便于回调时使用
	hideCallback func()         // 一次性的"隐藏后"回调（如有），自动隐藏 / 点击关闭都会触发
}

var toastWindows = NewWindowRegistry[ToastWindow]()

// NewToastWindow 创建 ToastWindow 实例（未创建底层 HWND）。
func NewToastWindow(logger *slog.Logger) *ToastWindow {
	return &ToastWindow{
		logger:   logger,
		renderer: NewToastRenderer(logger),
	}
}

// SetTheme 同步主题。
func (w *ToastWindow) SetTheme(resolved *theme.ResolvedTheme) {
	w.mu.Lock()
	w.resolvedTheme = resolved
	w.mu.Unlock()
	if w.renderer != nil {
		w.renderer.SetTheme(resolved)
	}
}

// SetOnRightClick 设置右键回调；当前未启用 UI（保留接口）。
func (w *ToastWindow) SetOnRightClick(cb func(x, y int)) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.onRightClick = cb
}

// IsVisible 返回 toast 当前是否可见。
func (w *ToastWindow) IsVisible() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.visible
}

// toastWndProc 是 ToastWindow 的 WndProc。
// 拦截 mouse 事件用于"点击关闭 + 预留右键回调"。
func toastWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		toastWindows.Unregister(windows.HWND(hwnd))
		return 0
	case WM_NCHITTEST:
		return HTCLIENT
	case WM_LBUTTONUP:
		if w := toastWindows.Get(windows.HWND(hwnd)); w != nil {
			w.mu.Lock()
			cb := w.onLeftClick
			w.mu.Unlock()
			if cb != nil {
				cb()
			} else {
				w.Hide()
			}
		}
		return 0
	case WM_RBUTTONUP:
		if w := toastWindows.Get(windows.HWND(hwnd)); w != nil {
			w.mu.Lock()
			cb := w.onRightClick
			w.mu.Unlock()
			if cb != nil {
				var pt POINT
				procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))
				cb(int(pt.X), int(pt.Y))
			} else {
				// 默认右键也关闭，避免"无反应"的尴尬。
				w.Hide()
			}
		}
		return 0
	}
	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// Create 创建底层 layered 窗口（必须在 UI 线程调用）。
func (w *ToastWindow) Create() error {
	hwnd, err := CreateLayeredWindow(LayeredWindowConfig{
		ClassName: "IMEToastWindow",
		WndProc:   syscall.NewCallback(toastWndProc),
	})
	if err != nil {
		return err
	}
	w.hwnd = hwnd
	toastWindows.Register(w.hwnd, w)
	w.logger.Debug("Toast window created", "hwnd", w.hwnd)
	return nil
}

// Destroy 销毁底层窗口与渲染资源。
func (w *ToastWindow) Destroy() {
	w.hideVersion.Add(1) // 取消任何待执行的自动隐藏
	if w.hwnd != 0 {
		procDestroyWindow.Call(uintptr(w.hwnd))
		w.hwnd = 0
	}
	if w.renderer != nil {
		w.renderer.Close()
	}
}

// toastTargetWorkArea 返回 toast 应当显示的目标显示器工作区（left, top, right, bottom）。
// 取鼠标光标所在显示器，比"前台窗口所在显示器"更贴近"用户当前正在看哪块屏幕"：
//   - 用户通过菜单点击触发 toast 时, 光标就在那块屏幕上;
//   - 后台触发的 toast (如"词库加载完成") 也会落在用户当下视线所在的屏幕;
//   - 前台窗口可能跨屏或左上角恰好越界, 用窗口位置会反而误判.
func toastTargetWorkArea() (left, top, right, bottom int) {
	return GetCurrentMonitorWorkArea()
}

// computeToastPosition 根据 Position + 目标显示器工作区 计算 toast 左上角屏幕坐标。
func computeToastPosition(pos ToastPosition, width, height int, wl, wt, wr, wb int) (int, int) {
	scale := GetDPIScale()
	margin := int(16 * scale)

	switch pos {
	case ToastCenter:
		return wl + (wr-wl-width)/2, wt + (wb-wt-height)/2
	case ToastBottomRight:
		return wr - margin - width, wb - margin - height
	case ToastTopRight:
		return wr - margin - width, wt + margin
	case ToastTop:
		return wl + (wr-wl-width)/2, wt + margin
	default:
		return wr - margin - width, wb - margin - height
	}
}

// Show 展示一次 toast。线程要求：必须在 UI 线程调用（由 manager 的命令循环保证）。
// 行为：
//  1. 取消任何在途的自动隐藏定时器；
//  2. 按 opts 渲染图像并更新 layered window；
//  3. 按 opts.Position 计算位置；
//  4. 若 opts.Duration >= 0，调度一次"<Duration>ms 后隐藏"（0 用默认 5000）。
func (w *ToastWindow) Show(opts ToastOptions) {
	if w.hwnd == 0 {
		return
	}

	// 一次性确定目标显示器工作区, 后续宽度上限 + 位置计算都基于这一份, 避免两次取屏
	// 中间光标恰好跨屏导致 maxContent 与 position 落在不同显示器上.
	wl, wt, wr, wb := toastTargetWorkArea()
	workWidth := wr - wl
	maxContentPx := opts.MaxWidth
	if maxContentPx <= 0 {
		maxContentPx = workWidth / 2
	}

	img := w.renderer.Render(opts, maxContentPx)
	if img == nil {
		w.logger.Warn("Toast render produced nil image")
		return
	}
	width := img.Bounds().Dx()
	height := img.Bounds().Dy()

	x, y := computeToastPosition(opts.Position, width, height, wl, wt, wr, wb)

	w.mu.Lock()
	w.width = width
	w.height = height
	w.currentOpts = opts
	w.visible = true
	// 默认左键回调 = 立即关闭
	if w.onLeftClick == nil {
		w.onLeftClick = func() { w.Hide() }
	}
	w.mu.Unlock()

	if err := UpdateLayeredWindowFromImage(w.hwnd, img, x, y); err != nil {
		w.logger.Warn("更新 Toast 窗口失败", "error", err)
		return
	}
	procShowWindow.Call(uintptr(w.hwnd), SW_SHOWNA)
	// INFO 级别：toast 是用户感知的一次性通知, 出问题时这条日志是定位起点。
	// 不记录 Message 原文以避免敏感提示场景泄漏内容, 仅记元数据 + 屏幕几何信息。
	w.logger.Info("Toast shown",
		"hasTitle", opts.Title != "",
		"messageLen", len(opts.Message),
		"level", opts.Level,
		"position", opts.Position,
		"durationMs", opts.Duration,
		"width", width,
		"height", height,
		"x", x,
		"y", y,
		"monitorWorkArea", [4]int{wl, wt, wr, wb},
	)

	// 调度自动隐藏。Duration < 0 表示不自动隐藏。
	if opts.Duration < 0 {
		w.hideVersion.Add(1) // 仍然递增，让此前的 timer 失效
		return
	}
	duration := opts.Duration
	if duration == 0 {
		duration = 5000
	}
	w.scheduleHide(duration)
}

// Hide 立即隐藏 toast（并取消待执行的自动隐藏）。
func (w *ToastWindow) Hide() {
	if w.hwnd == 0 {
		return
	}
	w.hideVersion.Add(1)
	procShowWindow.Call(uintptr(w.hwnd), SW_HIDE)
	w.mu.Lock()
	w.visible = false
	cb := w.hideCallback
	w.hideCallback = nil
	w.mu.Unlock()
	if cb != nil {
		cb()
	}
}

// scheduleHide 在 durationMs 毫秒后隐藏 toast，可被新的 Show / Hide 取消。
func (w *ToastWindow) scheduleHide(durationMs int) {
	version := w.hideVersion.Add(1)
	go func() {
		time.Sleep(time.Duration(durationMs) * time.Millisecond)
		if w.hideVersion.Load() != version {
			return // 已被新的 Show 或 Hide 取消
		}
		w.Hide()
	}()
}
