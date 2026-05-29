//go:build darwin

package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ui"
)

// 本文件集中 darwin 专用的 UI 回调导出包装与统一菜单转换:
// 这些方法仅被 darwin bridge (server_darwin.go) 经可选接口类型断言调用,
// Windows 端无调用方, 故 build-tag 隔离避免编进 Windows 二进制成为死代码。

// HandleCandidateSelect 是 handleCandidateSelect 的导出包装, 供 darwin bridge
// 在收到 IMKit `.app` 的 CmdCandidateSelect 帧 (NSPanel 鼠标点击命中候选) 时调用。
// index 为当前页内的 0-based 候选索引 (与 Win 鼠标回调语义一致)。
// 异步执行避免阻塞 bridge dispatch goroutine; 结果经 push 管道 (PushCommitTextToActiveClient) 交付。
func (c *Coordinator) HandleCandidateSelect(index int) {
	go c.handleCandidateSelect(index)
}

// HandleCandidateHover 是 handleCandidateHoverChange 的导出包装, 供 darwin bridge
// 在收到 IMKit `.app` 的 CmdCandidateHover 帧 (NSPanel 鼠标悬停候选) 时调用, 触发
// 异步 tooltip 查询。index 为页内 0-based 索引 (-1=无悬停)。位置参数传 0: macOS 端
// tooltip 由 .app 据悬停候选矩形自行定位, 不依赖 Go 计算屏幕坐标。
func (c *Coordinator) HandleCandidateHover(index int) {
	c.handleCandidateHoverChange(index, 0, 0, 0)
}

// HandleCandidateContextMenu 处理 darwin NSPanel 右键菜单动作 (页内索引 → 全局索引)。
// action: move_up/move_down/move_top/delete/reset_default/copy。各 handle* 期望全局索引
// (与 Win window_mouse.go 一致), 而鼠标传来的是页内索引, 故此处换算。
func (c *Coordinator) HandleCandidateContextMenu(index int, action string) {
	c.mu.Lock()
	global := (c.currentPage-1)*c.candidatesPerPage + index
	c.mu.Unlock()
	switch action {
	case "move_up":
		go c.handleCandidateMoveUp(global)
	case "move_down":
		go c.handleCandidateMoveDown(global)
	case "move_top":
		go c.handleCandidateMoveTop(global)
	case "delete":
		go c.handleCandidateDelete(global)
	case "reset_default":
		go c.handleCandidateResetDefault(global)
	case "copy":
		go c.handleCandidateCopy(global)
	default:
		c.logger.Debug("Unknown candidate context menu action", "action", action)
	}
}

// UnifiedMenuItems 构建统一菜单树供 darwin bridge 请求 (转 bridge.MenuItem)。
// macOS 裁掉 Win 专属项: 浮动工具栏开关 + caret-pending/pin-position 高级兼容项。
func (c *Coordinator) UnifiedMenuItems() []bridge.MenuItem {
	state := c.buildUnifiedMenuState()
	state.OmitToolbarToggle = true
	state.OmitAdvanced = true
	return toBridgeMenuItems(ui.BuildUnifiedMenuItems(state))
}

// HandleUnifiedMenuAction 派发 darwin 统一菜单动作 (异步避免阻塞 bridge dispatch)。
func (c *Coordinator) HandleUnifiedMenuAction(id int) {
	c.mu.Lock()
	proc := c.activeProcessName
	c.mu.Unlock()
	go c.handleUnifiedMenuAction(id, proc)
}

// toBridgeMenuItems 递归把 ui.MenuItem 转为 bridge.MenuItem (用于 darwin 下发)。
func toBridgeMenuItems(items []ui.MenuItem) []bridge.MenuItem {
	out := make([]bridge.MenuItem, len(items))
	for i, it := range items {
		out[i] = bridge.MenuItem{
			ID:        int32(it.ID),
			Label:     it.Text,
			Separator: it.Separator,
			Checked:   it.Checked,
			Disabled:  it.Disabled,
			Children:  toBridgeMenuItems(it.Children),
		}
	}
	return out
}
