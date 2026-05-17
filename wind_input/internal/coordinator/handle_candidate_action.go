// handle_candidate_action.go — 候选词快捷键操作（删除、置顶）
package coordinator

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/bridge"
)

// matchCandidateActionKey checks if the current key event matches a candidate action hotkey.
// hotkeyType is "ctrl+number" or "ctrl+shift+number".
// Returns the 1-based candidate number (1-9) if matched, or 0 if not.
func (c *Coordinator) matchCandidateActionKey(hotkeyType string, hasCtrl, hasShift bool, keyCode int) int {
	switch hotkeyType {
	case "ctrl+number":
		if hasCtrl && !hasShift && keyCode >= 0x31 && keyCode <= 0x39 {
			return keyCode - 0x30
		}
	case "ctrl+shift+number":
		if hasCtrl && hasShift && keyCode >= 0x31 && keyCode <= 0x39 {
			return keyCode - 0x30
		}
	}
	return 0
}

// handleDeleteCandidateByKey deletes the num-th candidate (1-based) on the current page.
// Caller must hold c.mu before calling; this function releases and re-acquires the lock around shadow ops.
func (c *Coordinator) handleDeleteCandidateByKey(num int) *bridge.KeyEventResult {
	consumed := &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		return consumed
	}

	cand := c.candidates[actualIndex]

	// 命令直通车候选不允许通过热键删除 (action 候选只渲染, 不参与 Shadow)
	if len(cand.Actions) > 0 {
		return consumed
	}

	// 字符组 / 字符串组子项: 不允许任何 pin/delete (defensive 与 UI 菜单同步)
	if cand.IsGroupMember {
		return consumed
	}

	// 单字不允许删除 (短语 ID 例外, 用户主动挑了具体单字候选)
	if cand.ID == "" && len([]rune(cand.Text)) <= 1 {
		c.logger.Debug("Cannot delete single character via hotkey")
		return consumed
	}

	code := c.inputBuffer

	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		// 短语候选 (cand.ID 以 "phrase:" 开头) 走 PhraseRecord.Enabled = false
		// (软删除), 不写 Shadow。与右键菜单 handleCandidateDelete 同步分发逻辑。
		if strings.HasPrefix(cand.ID, "phrase:") && cand.PhraseTemplate != "" {
			if err := dm.DisablePhrase(code, cand.PhraseTemplate); err != nil {
				c.logger.Error("Failed to disable phrase via hotkey", "error", err, "code", code)
			}
		} else {
			dm.DeleteWord(code, cand.Text, cand.ID)
			if err := dm.SaveShadow(); err != nil {
				c.logger.Error("Failed to save shadow layer after hotkey delete", "error", err)
			}
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()

	return consumed
}

// handlePinCandidateByKey pins the num-th candidate (1-based) on the current page to the top.
// Caller must hold c.mu before calling; this function releases and re-acquires the lock around shadow ops.
func (c *Coordinator) handlePinCandidateByKey(num int) *bridge.KeyEventResult {
	consumed := &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		return consumed
	}

	// 已经是第一个，无需置顶
	if actualIndex == 0 {
		return consumed
	}

	cand := c.candidates[actualIndex]
	code := c.inputBuffer

	// 命令直通车候选 (cmdbar) 不允许通过热键置顶: display 只是渲染,
	// 真正的"短语顺序"由 Shadow pin 管, 与 action 渲染无关。
	if len(cand.Actions) > 0 {
		c.logger.Debug("Cannot pin cmdbar action candidate via hotkey")
		return consumed
	}

	// 字符组 / 字符串组子项: 不允许任何 pin (defensive 与 UI 菜单同步)
	if cand.IsGroupMember {
		return consumed
	}

	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.PinWord(code, cand.Text, cand.ID, 0)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer after hotkey pin", "error", err)
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()

	return consumed
}
