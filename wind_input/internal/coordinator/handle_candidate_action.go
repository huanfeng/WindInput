// handle_candidate_action.go — 候选词快捷键操作（删除、置顶）
package coordinator

import (
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

	// 命令候选不允许删除
	if cand.IsCommand {
		return consumed
	}

	// 单字不允许删除
	if len([]rune(cand.Text)) <= 1 {
		c.logger.Debug("Cannot delete single character via hotkey", "text", cand.Text)
		return consumed
	}

	code := c.inputBuffer

	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.DeleteWord(code, cand.Text)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer after hotkey delete", "error", err)
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
	// 真正的"短语顺序"由 PhraseLayer 的 position 字段管, 与热键置顶语义无关。
	if len(cand.Actions) > 0 {
		c.logger.Debug("Cannot pin cmdbar action candidate via hotkey")
		return consumed
	}

	// 命令候选（短语）：通过 PhraseLayer 置顶
	if cand.IsCommand && cand.PhraseTemplate != "" {
		c.mu.Unlock()
		c.handlePhraseMoveToTop(code, cand.PhraseTemplate)
		c.mu.Lock()
		c.updateCandidates()
		c.showUI()
		return consumed
	}

	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.PinWord(code, cand.Text, 0)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer after hotkey pin", "error", err)
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()

	return consumed
}
