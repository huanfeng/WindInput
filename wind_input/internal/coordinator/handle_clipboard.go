// handle_clipboard.go — 剪切板相关操作（调试用）
package coordinator

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/clipboard"
)

// handleCandidateCopy copies the candidate text at the given page-local index to clipboard.
func (c *Coordinator) handleCandidateCopy(index int) {
	c.mu.Lock()

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.mu.Unlock()
		return
	}

	text := c.candidates[actualIndex].Text
	c.mu.Unlock()

	if err := clipboard.SetText(text); err != nil {
		c.logger.Error("Failed to copy candidate to clipboard", "error", err)
	} else {
		c.logger.Debug("Candidate copied to clipboard", "len", len([]rune(text)))
	}
}

// handleCandidateCopyBatch copies candidates to clipboard (debug only).
// maxPages=0 means all candidates, maxPages=N means first N pages.
func (c *Coordinator) handleCandidateCopyBatch(maxPages int) {
	c.mu.Lock()

	endIdx := len(c.candidates)
	if maxPages > 0 && maxPages*c.candidatesPerPage < endIdx {
		endIdx = maxPages * c.candidatesPerPage
	}

	if endIdx == 0 {
		c.mu.Unlock()
		return
	}

	texts := make([]string, 0, endIdx)
	for i := 0; i < endIdx; i++ {
		texts = append(texts, c.candidates[i].Text)
	}
	c.mu.Unlock()

	result := strings.Join(texts, " ")

	if err := clipboard.SetText(result); err != nil {
		c.logger.Error("Failed to copy candidates batch to clipboard", "error", err)
	} else {
		c.logger.Debug("Candidates batch copied to clipboard", "count", len(texts), "maxPages", maxPages)
	}
}

// readClipboardCode reads clipboard outside the lock and filters to valid input characters.
// Caller must temporarily release c.mu before calling, then re-acquire after.
func readClipboardCode() (string, error) {
	text, err := clipboard.GetText()
	if err != nil {
		return "", err
	}

	filtered := make([]byte, 0, len(text))
	for i := 0; i < len(text); i++ {
		ch := text[i]
		if (ch >= 'a' && ch <= 'z') || ch == '\'' {
			filtered = append(filtered, ch)
		} else if ch >= 'A' && ch <= 'Z' {
			filtered = append(filtered, ch+32)
		}
	}

	// Truncate to max input buffer length
	if len(filtered) > maxInputBufferLen {
		filtered = filtered[:maxInputBufferLen]
	}

	return string(filtered), nil
}

// handleClipboardPasteCode reads encoding from clipboard and replaces the input buffer (Ctrl+Shift+R).
// Works in any state (empty or non-empty input buffer).
// Caller must hold c.mu; this function temporarily releases and re-acquires the lock.
func (c *Coordinator) handleClipboardPasteCode() *bridge.KeyEventResult {
	consumed := &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// Release lock during clipboard read to avoid blocking other goroutines
	c.mu.Unlock()
	code, err := readClipboardCode()
	c.mu.Lock()

	if err != nil {
		c.logger.Error("Failed to read clipboard", "error", err)
		return consumed
	}
	if len(code) == 0 {
		c.logger.Debug("Clipboard contains no valid input characters")
		return consumed
	}

	c.logger.Debug("Replace input buffer from clipboard", "len", len(code))

	// Replace input buffer directly, preserving UI/session state
	c.inputBuffer = code
	c.inputCursorPos = len(code)
	c.preeditDisplay = ""
	c.syllableBoundaries = nil
	c.confirmedSegments = nil
	c.updateCandidates()
	c.showUI()

	return c.compositionUpdateResult()
}
