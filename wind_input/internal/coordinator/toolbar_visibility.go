// toolbar_visibility.go — 工具栏位置计算 + Shell 全屏事件适配。
//
// 历史上本文件还承担"显隐决策"职责（showToolbarRespectingFullscreen /
// shouldHideToolbarDueToFullscreen），已迁移至 toolbar_reducer.go 单点决策。
// 这里仅保留：
//   - computeToolbarPositionLocked: 用 caret + per-monitor 持久化位置算出工具栏坐标
//   - OnShellFullscreenChange: 把 ShellHook 全屏事件投递给 reducer
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/ui"
)

// OnShellFullscreenChange 由 UI 层在收到系统 Shell 全屏通知
// (HSHELL_WINDOWENTERFULLSCREEN=53 / HSHELL_WINDOWEXITFULLSCREEN=54) 时调用。
//
// 与按键 / IME activate 完全解耦：浏览器 F11、视频全屏、PPT 放映、D3D 全屏均
// 走这条通道，全屏切换发生时立即收到通知，不引入按键路径延迟。
//
// 实际显隐由 toolbarReducer 在 reconcile 时统一决策，本函数只是事件投递。
// 非关键事件——若 reducer 拥塞，drop 即可：下次全屏进/出会重新通知，状态会自动收敛。
func (c *Coordinator) OnShellFullscreenChange(enter bool) {
	if c.toolbarReducer == nil {
		return
	}
	c.toolbarReducer.sendNonBlocking(toolbarEvent{
		kind:    tevFullscreenChanged,
		visible: enter,
	})
}

// computeToolbarPositionLocked 按当前 caret 位置（或默认位置）计算工具栏坐标，
// 并复用用户曾在该显示器上的拖拽位置。
// 调用方必须持有 c.mu。
func (c *Coordinator) computeToolbarPositionLocked() (int, int) {
	const toolbarWidth, toolbarHeight = 140, 30
	scaledW := ui.ScaleIntForDPI(toolbarWidth)
	scaledH := ui.ScaleIntForDPI(toolbarHeight)

	var posX, posY int
	if c.caretValid {
		posX, posY = ui.GetToolbarPositionForCaret(c.caretX, c.caretY, scaledW, scaledH)
	} else {
		posX, posY = ui.GetDefaultToolbarPosition(scaledW, scaledH)
	}

	_, _, monRight, monBottom := ui.GetMonitorWorkAreaFromPoint(posX, posY)
	key := ui.MonitorKeyStr(monRight, monBottom)
	if saved, ok := c.toolbarUserPos[key]; ok {
		posX, posY = saved.X, saved.Y
	}
	return posX, posY
}
