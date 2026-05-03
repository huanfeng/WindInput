// handle_lifecycle.go — IME 生命周期事件（焦点、激活、停用）与 CommitRequest 处理
package coordinator

import (
	"runtime"
	"runtime/debug"
	"time"
	"unicode/utf8"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

// 默认首次 show 兜底超时；C++ 端 SendCaretPending 握手会延长到 firstShowExtendedTimeout。
const (
	firstShowDefaultTimeout  = 150 * time.Millisecond
	firstShowExtendedTimeout = 600 * time.Millisecond

	// Excel/WPS 表格风格焦点切换检测窗口（HandleFocusLost）。
	replayDetectKeyWindow = 200 * time.Millisecond
	// 同 PID focus_gained 必须在此窗口内到达才会重放（HandleFocusGained）。
	replayGainedWindow = 500 * time.Millisecond
	// 重放 buffer 长度上限：超过则视为长串输入，直接清空避免误重放。
	replayMaxBufferLen = 8
)

// armPendingFirstShow 推迟首次 showUI（持锁状态下调用）。
// 启动兜底 goroutine：若到时仍未收到 caret update，强制 show 防止候选窗永远不显示。
// token 比对避免后续按键覆盖时旧定时器误触发。
func (c *Coordinator) armPendingFirstShow() {
	c.armPendingFirstShowWithTimeout(firstShowDefaultTimeout)
}

// armPendingFirstShowWithTimeout 用指定超时启动兜底 goroutine。
// 总会推进 token, 因此后调用会自动作废先前 goroutine。
func (c *Coordinator) armPendingFirstShowWithTimeout(d time.Duration) {
	c.pendingFirstShow = true
	c.pendingFirstShowToken++
	token := c.pendingFirstShowToken
	go func() {
		time.Sleep(d)
		c.mu.Lock()
		defer c.mu.Unlock()
		if !c.pendingFirstShow || c.pendingFirstShowToken != token {
			return
		}
		c.pendingFirstShow = false
		if c.uiManager == nil {
			return
		}
		// 兜底超时：按模式派发到对应 showUI，避免主流程 showUI 覆盖模式标签 / 候选编号 / preedit。
		switch {
		case c.tempEnglishMode:
			c.logger.Debug("pendingFirstShow timeout: forcing showTempEnglishUI")
			c.showTempEnglishUI()
		case c.tempPinyinMode:
			c.logger.Debug("pendingFirstShow timeout: forcing showPinyinModeUI (temp pinyin)")
			c.showPinyinModeUI(c.tempPinyinOps())
		case c.quickInputMode:
			c.logger.Debug("pendingFirstShow timeout: forcing showQuickInputUI")
			c.showQuickInputUI()
		default:
			// 非嵌入模式：即使无候选也要显示窗口，让用户看到 inputBuffer（如 v/i/u/o
			// 等无对应候选的拼音字母），避免编码既不嵌入宿主又看不到候选窗的死角。
			if len(c.inputBuffer) > 0 && (len(c.candidates) > 0 || !c.isInlinePreedit()) {
				c.logger.Debug("pendingFirstShow timeout: forcing showUI with current caret")
				c.showUI()
			}
		}
	}()
}

// HandleCaretPending 接收 C++ 端握手：composition 刚启动, 真正 caret 会在 reflow 后到达。
// 延长 pendingFirstShow 超时, 避免某些应用 (如 EverEdit) OnLayoutChange burst 较慢
// 时 Go 端先回退到按键前坐标 show, 然后才被真实坐标覆盖, 造成可见跳动。
func (c *Coordinator) HandleCaretPending() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if !c.pendingFirstShow {
		return
	}
	c.logger.Debug("CARET_PENDING handshake: extending first-show timeout")
	c.armPendingFirstShowWithTimeout(firstShowExtendedTimeout)
}

// HandleCaretUpdate handles caret position updates from C++ Bridge
func (c *Coordinator) HandleCaretUpdate(data bridge.CaretData) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	// C++ 端传递原始 height：h=0 表示退化矩形（应用尚未 reflow，坐标不可靠），
	// 跳过此次更新，等待 OnLayoutChange 提供真实坐标。
	if data.Height == 0 {
		return nil
	}

	// 跨焦点 buffer 重放等待中：FocusGained 包内嵌的 caret 是新文档的"初始"坐标
	// （Excel 单元格选中区/编辑栏区域），不是 composition reflow 后的真实位置。
	// 此时不能 showUI，否则会先在错位置出现再跳到正确位置。仅缓存坐标，等
	// HandleFocusGained 完成 PushUpdateComposition 后由真实 OnLayoutChange caret 触发 show。
	if c.pendingReplay {
		c.caretX = data.X
		c.caretY = data.Y
		c.caretHeight = data.Height
		return nil
	}

	// 应用兼容性规则：caret_use_top 将 Y 从 rect.bottom 转换为 rect.top。
	// 微信等 WebView 应用的 GetTextExt 返回的 height 不稳定（h=1 或 h=20），
	// 导致 rect.bottom 在不同时刻差异达 20px，但 rect.top 始终稳定（差异 ≤1px）。
	if c.activeCompatRule != nil && c.activeCompatRule.CaretUseTop && data.Height > 0 {
		rawH := data.Height
		data.Y = data.Y - rawH // bottom → top
		data.Height = 1        // 最小高度，确保候选框紧贴文字下方
		if data.CompositionStartY != 0 {
			data.CompositionStartY = data.CompositionStartY - rawH
		}
	}

	prevCaretX := c.caretX
	prevCaretY := c.caretY

	c.caretX = data.X
	c.caretY = data.Y
	c.caretHeight = data.Height
	c.caretValid = true // Mark that we have received valid caret position

	// 消费 pendingFirstShow：handleAlphaKey 在新 composition 启动时不会立即
	// showUI，等到 reflow 后真实坐标到达再 show，避免先错位再跳的闪烁。
	wasPendingFirstShow := c.pendingFirstShow
	if wasPendingFirstShow {
		c.pendingFirstShow = false
	}

	// Store composition start position from C++ TSF (via ITfComposition::GetRange).
	// 锁定语义：在同一次 composition 期间只接受首次到达的有效 compositionStart，
	// 后续 update 即便携带新值也不再覆盖。否则 WebView / 微信 / WPS 部分控件
	// 的 GetRange 会让 START anchor 跟随 caret 漂移，导致候选窗口随输入移动。
	// composition 终止 / 焦点切换会调用 clearState() 把 compositionStartValid 复位。
	//
	// C++ 端在 StartComposition 后会推迟首次 SendCaretPositionUpdate 至 OnLayoutChange，
	// 因此首次到达的 compositionStart 已是 reflow 后的权威坐标，无需 large-gap 覆盖。
	//
	// 校验：若 compositionStart 与 caret 距离过大（>500px），视为坐标系不一致
	// （logical vs physical），拒绝。
	if (data.CompositionStartX != 0 || data.CompositionStartY != 0) && !c.compositionStartValid {
		dx := data.CompositionStartX - data.X
		dy := data.CompositionStartY - data.Y
		if dx < 0 {
			dx = -dx
		}
		if dy < 0 {
			dy = -dy
		}
		if dx < 500 && dy < 500 {
			c.compositionStartX = data.CompositionStartX
			c.compositionStartY = data.CompositionStartY
			c.compositionStartValid = true
		} else {
			c.logger.Debug("Rejected compositionStart: too far from caret (coordinate space mismatch)",
				"caretX", data.X, "caretY", data.Y,
				"compStartX", data.CompositionStartX, "compStartY", data.CompositionStartY,
				"dx", dx, "dy", dy)
		}
	}

	// If there's active input, refresh the candidate window position.
	// C++ 端保证每次到达的 caret update 都是 reflow 后的权威坐标。
	// 覆盖所有模式（主输入 / 临时英文 / 临时拼音 / 快捷输入），否则 replay 后
	// 候选窗在新焦点首个 caret 到达时不会重新 show。
	hasMainInput := len(c.inputBuffer) > 0
	hasTempEnglish := len(c.tempEnglishBuffer) > 0
	hasTempPinyin := c.tempPinyinMode
	hasQuickInput := c.quickInputMode
	hasInput := hasMainInput || hasTempEnglish || hasTempPinyin || hasQuickInput
	hasCandidates := len(c.candidates) > 0
	hasUI := c.uiManager != nil
	// 主输入流程必须有候选才 show；模式入口（quickInput / tempPinyin）允许空候选，
	// 因为 preedit 中含触发键 prefix，仍需把候选窗（即便只有 prefix）渲染出来。
	// 非嵌入模式：主输入也允许空候选 show，让用户看到 inputBuffer（如 v/i/u/o
	// 等无对应候选的拼音字母），避免编码既不嵌入宿主又看不到候选窗的死角。
	canShow := hasUI && (hasCandidates || hasTempPinyin || hasQuickInput || (hasMainInput && !c.isInlinePreedit()))
	if hasInput && canShow {
		// 首次 show（pendingFirstShow 刚被消费）必须无条件 show，无视 3px 过滤；
		// 否则若 reflow 后坐标恰好与按键前差 ≤3px，候选窗会一直不显示。
		if !wasPendingFirstShow {
			// 过滤小位移：候选窗口已显示后，caret 位移 ≤3px 时跳过重绘，
			// 避免应用后期微调（如 WPS 的 2px Y 偏移）导致可见闪烁。
			moveDx := data.X - prevCaretX
			moveDy := data.Y - prevCaretY
			if moveDx < 0 {
				moveDx = -moveDx
			}
			if moveDy < 0 {
				moveDy = -moveDy
			}
			if moveDx <= 3 && moveDy <= 3 && c.caretValid {
				return nil
			}
		}
		// 派发到模式特定的 show 函数，避免主流程 showUI 覆盖模式标签 / Index 重编号 / preedit。
		switch {
		case hasTempEnglish:
			c.showTempEnglishUI()
		case hasTempPinyin:
			c.showPinyinModeUI(c.tempPinyinOps())
		case hasQuickInput:
			c.showQuickInputUI()
		default:
			c.showUI()
		}
	}

	return nil
}

// HandleSelectionChanged handles selection/caret change events from ITfTextEditSink::OnEndEdit.
// This is called when the cursor moves outside of composition (e.g., mouse click).
func (c *Coordinator) HandleSelectionChanged(prevChar rune) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// 选区变化时清空配对栈（自动配对插入后 200ms 内的 SelectionChanged 事件除外，
	// 这些事件是 CommitText 和光标移动引发的，不是用户操作）
	if c.pairTracker != nil {
		if c.pairInsertTime.IsZero() || time.Since(c.pairInsertTime) > 200*time.Millisecond {
			c.pairTracker.Clear()
		}
	}
	if c.pairTrackerEn != nil {
		if c.pairInsertTime.IsZero() || time.Since(c.pairInsertTime) > 200*time.Millisecond {
			c.pairTrackerEn.Clear()
		}
	}
	c.lastOutputWasDigit = false
	// 鼠标点击 / 方向键等外部光标移动会触发 SelectionChanged。Composition 之间
	// 不会再有 GetTextExt 上报，缓存的 caret 坐标已不再代表当前位置；标记陈旧后
	// 下一次新输入会走 deferred 路径，等待 OnKeyDown→SendCaretPositionUpdate
	// 上报真实坐标，避免候选窗显示在旧位置。
	c.caretValid = false
	c.compositionStartValid = false
	c.logger.Debug("Selection changed, reset smart punct state", "prevChar", string(prevChar))
}

// HandleHostRenderReady is called when host render shared memory is set up for the active client.
// This triggers updating the UI manager's render callbacks immediately, without waiting for next focus change.
func (c *Coordinator) HandleHostRenderReady() {
	c.updateHostRenderState()
}

// updateHostRenderState checks if the active process has host rendering and updates
// the UI manager's render callbacks accordingly.
func (c *Coordinator) updateHostRenderState() {
	if c.bridgeServer == nil || c.uiManager == nil {
		return
	}

	writeFrame, hideFunc := c.bridgeServer.GetActiveHostRender()
	if writeFrame != nil {
		c.logger.Info("Enabling host render for active process", "alreadyEnabled", c.uiManager.IsHostRendering())
		c.uiManager.SetHostRenderFunc(writeFrame, hideFunc)
	} else {
		if c.uiManager.IsHostRendering() {
			c.logger.Info("Disabling host render for active process")
		}
		c.uiManager.SetHostRenderFunc(nil, nil)
	}
	// TODO: StatusWindow 的 host render 集成需要 DLL 侧协议扩展，
	// 当前状态窗口使用本地窗口渲染。后续可通过 sw.SetHostRenderFunc 接入。
}

// HandleFocusLost handles focus lost events (real focus change, e.g., user clicked another window)
func (c *Coordinator) HandleFocusLost() {
	c.logger.Debug("Focus lost, clearing state and hiding toolbar")

	// 焦点丢失 = 短语终止符，通知造词策略（码表自动造词）
	if c.engineMgr != nil {
		c.engineMgr.OnPhraseTerminated()
	}

	// 焦点变化后异步释放内存（非阻塞，不影响响应速度）
	defer func() {
		go func() {
			runtime.GC()
			debug.FreeOSMemory()
		}()
	}()

	// 注意：不在此处清除 hostRenderFunc。HostRender 绑定到进程级别（共享内存按 PID
	// 建立），不应因进程内焦点变化而清除。showUI() 在每次绑定前调用
	// updateHostRenderState() 自动根据 activeProcessID 重新评估，切换到非 HostRender
	// 进程时会自然清除。若在此清除，开始菜单等受限环境中频繁的焦点抖动会导致
	// doShowCandidates 执行时 hostRenderFunc 为 nil，候选框回退到不可见的本地窗口。

	// 常驻模式：失去焦点时隐藏状态
	if c.config != nil && c.config.UI.StatusIndicator.DisplayMode == "always" {
		if c.uiManager != nil {
			c.uiManager.HideStatusIndicator()
		}
	}

	// Hide toolbar on real focus lost (user switched to another window/app)
	c.SetIMEActivated(false)

	c.mu.Lock()
	defer c.mu.Unlock()
	c.lastOutputWasDigit = false

	// Excel/WPS 表格兼容：在 composition 刚启动后立即丢焦，推断为应用切换
	// ITfDocumentMgr（cell-select → cell-edit），保留 buffer 等待重放。
	if c.shouldDeferClearForReplay() {
		c.pendingReplay = true
		c.pendingReplayPID = c.activeProcessID
		c.pendingReplayDeadline = time.Now().Add(replayGainedWindow)
		// 让旧 pendingFirstShow 兜底定时器作废，避免在新焦点到达前用旧坐标 show。
		c.pendingFirstShow = false
		c.pendingFirstShowToken++
		c.logger.Info("Focus lost during pendingFirstShow, preserving buffer for replay",
			"bufferLen", len(c.inputBuffer), "pid", c.activeProcessID)
		return
	}

	c.pendingReplay = false
	c.caretValid = false // 焦点切换后坐标失效，下次新 composition 需重新等待真实坐标
	c.clearState()
}

// shouldDeferClearForReplay 判定是否处于"打字驱动焦点切换"竞态。
// 调用方必须持有 c.mu 锁。
func (c *Coordinator) shouldDeferClearForReplay() bool {
	if !c.pendingFirstShow {
		c.logger.Debug("shouldDeferClearForReplay=false", "reason", "pendingFirstShow=false")
		return false
	}
	bufLen := c.replayBufferLen()
	if bufLen == 0 || bufLen > replayMaxBufferLen {
		c.logger.Debug("shouldDeferClearForReplay=false", "reason", "bufferLen", "len", bufLen)
		return false
	}
	if c.lastKeyTime.IsZero() {
		c.logger.Debug("shouldDeferClearForReplay=false", "reason", "lastKeyTime zero")
		return false
	}
	if time.Since(c.lastKeyTime) > replayDetectKeyWindow {
		c.logger.Debug("shouldDeferClearForReplay=false", "reason", "key window expired",
			"sinceLastKey", time.Since(c.lastKeyTime).String())
		return false
	}
	// confirmed segments 非空意味着用户已经选过候选，正常焦点切换应清空，不重放。
	if len(c.confirmedSegments) > 0 {
		c.logger.Debug("shouldDeferClearForReplay=false", "reason", "confirmedSegments non-empty")
		return false
	}
	return true
}

// replayBufferLen 返回当前活动模式下的待重放 composition 长度（字节数）。
// 覆盖：主输入 / 临时英文 / 临时拼音 / 快捷输入。
// 注意：临时拼音和快捷输入的触发键字符在 buffer 之外，通过 prefix 注入 composition，
// 所以即便 buffer 为空，只要 mode 已开启，仍需返回 prefix 长度，否则
// shouldDeferClearForReplay 会漏判，触发键直接上屏到宿主。
// 调用方必须持有 c.mu 锁。
func (c *Coordinator) replayBufferLen() int {
	if len(c.inputBuffer) > 0 {
		return len(c.inputBuffer)
	}
	if len(c.tempEnglishBuffer) > 0 {
		return len(c.tempEnglishBuffer)
	}
	if c.tempPinyinMode {
		return len(c.tempPinyinPrefix()) + len(c.tempPinyinBuffer)
	}
	if c.quickInputMode {
		prefix := c.quickInputPrefix()
		if len(c.quickInputPinyinBuffer) > 0 {
			return len(prefix) + len(c.quickInputPinyinBuffer)
		}
		return len(prefix) + len(c.quickInputBuffer)
	}
	return 0
}

// HandleCompositionTerminated handles composition unexpectedly terminated events
// This happens when the user clicks within the input field to change cursor position,
// or when the application forcefully terminates the composition.
// Unlike HandleFocusLost, this does NOT hide the toolbar since the user is still
// in the same input field.
func (c *Coordinator) HandleCompositionTerminated() {
	// HostRender 模式下（开始菜单等受限环境），SearchHost 的搜索框不支持 TSF
	// composition，DLL 每次设置 composition 文本后搜索框会立即终止它。但在
	// HostRender 模式下候选框通过 Band 窗口独立渲染，不依赖 TSF composition，
	// 因此忽略 composition 终止事件，保持输入状态和候选窗口不变。
	if c.uiManager != nil && c.uiManager.IsHostRendering() {
		c.logger.Debug("Composition terminated in host render mode, ignoring")
		return
	}

	c.mu.Lock()
	defer c.mu.Unlock()

	// 安全网：如果 composition 终止事件在最近一次按键后很短时间内到达（<100ms），
	// 且输入缓冲区非空，说明这很可能是应用异步处理 composition 变更导致的竞态
	// （如顶码上屏后 InsertTextAndStartComposition 创建的新 composition 被应用终止），
	// 而非用户主动点击其他位置。此时保留输入状态，下一个按键的 UpdateComposition
	// 会自动重建 composition。
	if len(c.inputBuffer) > 0 && !c.lastKeyTime.IsZero() &&
		time.Since(c.lastKeyTime) < 100*time.Millisecond {
		c.logger.Debug("Composition terminated shortly after key event, preserving input state",
			"sinceLastKey", time.Since(c.lastKeyTime).String(),
			"bufferLen", len(c.inputBuffer))
		return
	}

	c.logger.Debug("Composition terminated, clearing input state")

	// 光标位置可能已变化（用户点击了输入框内其他位置），重置数字后智能标点状态
	c.lastOutputWasDigit = false
	// Only clear input state and hide candidate window, keep toolbar visible
	c.clearState()
	c.hideUI()
}

// HandleIMEDeactivated handles IME being switched away (user selected another IME)
// This is called from TSF's Deactivate method, before the client disconnects
func (c *Coordinator) HandleIMEDeactivated() {
	c.logger.Info("IME deactivated (user switched to another IME), hiding toolbar")

	// IME 停用 = 短语终止符，通知造词策略（码表自动造词）
	if c.engineMgr != nil {
		c.engineMgr.OnPhraseTerminated()
	}

	c.mu.Lock()
	c.imeActivated = false
	c.lastOutputWasDigit = false
	c.pendingReplay = false
	c.clearState()
	c.mu.Unlock()

	// Immediately hide the toolbar and status indicator
	if c.uiManager != nil {
		c.uiManager.SetToolbarVisible(false)
		c.uiManager.Hide()
		c.uiManager.HideStatusIndicator()
	}
}

// HandleClientDisconnected handles TSF client disconnection
// When all clients disconnect (activeClients == 0), hide the toolbar
func (c *Coordinator) HandleClientDisconnected(activeClients int) {
	c.logger.Debug("Client disconnected", "activeClients", activeClients)

	if activeClients == 0 {
		c.logger.Info("All TSF clients disconnected, hiding toolbar")
		c.mu.Lock()
		c.imeActivated = false
		c.mu.Unlock()

		// Hide toolbar and candidate window
		if c.uiManager != nil {
			c.uiManager.SetToolbarVisible(false)
			c.uiManager.Hide()
		}
	}
}

// getCompiledHotkeys returns compiled hotkey hashes for C++ side
// 使用缓存避免每次焦点变化重新编译
func (c *Coordinator) getCompiledHotkeys() (keyDownHotkeys, keyUpHotkeys []uint32) {
	if c.hotkeyCompiler == nil {
		return nil, nil
	}
	if !c.hotkeysDirty && c.cachedKeyDownHotkeys != nil {
		return c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys
	}
	c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys = c.hotkeyCompiler.Compile()
	c.hotkeysDirty = false
	c.logger.Debug("Compiled hotkeys for C++",
		"keyDownCount", len(c.cachedKeyDownHotkeys),
		"keyUpCount", len(c.cachedKeyUpHotkeys))
	return c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys
}

// HandleFocusGained handles focus gained events and returns current status
func (c *Coordinator) HandleFocusGained(processID uint32) *bridge.StatusUpdateData {
	if processID != 0 {
		c.activeProcessID = processID
		c.activeProcessName = bridge.GetProcessName(processID)
		c.activeCompatRule = c.appCompat.GetRule(c.activeProcessName)
		if c.activeCompatRule != nil {
			c.logger.Debug("Compat rule matched", "process", c.activeProcessName, "caretUseTop", c.activeCompatRule.CaretUseTop)
		}
	}
	c.logger.Debug("Focus gained", "processID", processID, "process", c.activeProcessName)

	// 焦点变化后异步释放内存（非阻塞，不影响响应速度）
	defer func() {
		go func() {
			runtime.GC()
			debug.FreeOSMemory()
		}()
	}()

	// Update host render state for the new active process
	c.updateHostRenderState()

	// Clear any pending input state when focus changes
	// This ensures composition state is consistent
	c.mu.Lock()
	c.lastOutputWasDigit = false

	// Excel/WPS 重放：满足"同 PID + 时间窗 + 仍有 buffer"时，保留状态并向新文档
	// 推送 update_composition，让 IME 在新 ITfDocumentMgr 上重新建立 composition。
	replayText := ""
	replayCaretPos := 0
	doReplay := false
	if c.pendingReplay {
		samePID := processID != 0 && processID == c.pendingReplayPID
		inWindow := time.Now().Before(c.pendingReplayDeadline)
		if samePID && inWindow && c.replayBufferLen() > 0 {
			// InlinePreedit=false 时，主流程对 UpdateComposition 发送空 Text，
			// 由 C++ 端注入占位空格并把光标定位到空格前，借此稳定上报真实 caret。
			// Replay 路径必须遵循同样契约，否则首次重建的 composition 会嵌入真实
			// 字符，下一个按键才切回空文本，导致 Excel 中"先嵌入字母再替换为空格"。
			if !c.isInlinePreedit() {
				replayText = ""
				replayCaretPos = 0
			} else {
				// 主输入流程仍走 compositionText（含 preeditDisplay 处理 / 拼音分段光标），
				// 临时英文 / 临时拼音 / 快捷输入则按可视 preedit 重建（prefix + buffer），
				// 因为触发键字符在 buffer 之外，仅通过 prefix 注入 composition。
				switch {
				case len(c.inputBuffer) > 0:
					replayText = c.compositionText()
					replayCaretPos = c.displayCursorPos()
				case c.tempPinyinMode:
					replayText = c.tempPinyinPrefix() + c.tempPinyinBuffer
					replayCaretPos = utf8.RuneCountInString(replayText)
				case c.quickInputMode:
					prefix := c.quickInputPrefix()
					if len(c.quickInputPinyinBuffer) > 0 {
						replayText = prefix + c.quickInputPinyinBuffer
					} else {
						replayText = prefix + c.quickInputBuffer
					}
					replayCaretPos = utf8.RuneCountInString(replayText)
				default:
					replayText = c.getPendingBufferText()
					replayCaretPos = utf8.RuneCountInString(replayText)
				}
			}
			doReplay = true
			c.armPendingFirstShowWithTimeout(firstShowExtendedTimeout)
			c.logger.Info("Replaying preserved buffer in new focus context",
				"bufferLen", c.replayBufferLen(), "pid", processID)
		} else {
			c.logger.Debug("Pending replay skipped",
				"samePID", samePID, "inWindow", inWindow,
				"reqPID", c.pendingReplayPID, "newPID", processID)
		}
		c.pendingReplay = false
	}

	if !doReplay && len(c.inputBuffer) > 0 {
		c.inputBuffer = ""
		c.inputCursorPos = 0
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.logger.Debug("Cleared input buffer on focus gained")
	}
	c.mu.Unlock()

	if doReplay && c.bridgeServer != nil {
		c.bridgeServer.PushUpdateCompositionToActiveClient(replayText, replayCaretPos)
	}

	// Hide candidate window (will be shown again when user starts typing).
	// Replay 路径下不 hide：候选已就绪，等待新焦点 caret 到达后由 pendingFirstShow 兜底显示。
	if !doReplay {
		c.hideUI()
	}

	// Set IME as activated (this will show toolbar if enabled)
	c.SetIMEActivated(true)

	// 常驻模式：获得焦点时显示状态
	c.mu.Lock()
	if c.config != nil && c.config.UI.StatusIndicator.Enabled && c.config.UI.StatusIndicator.DisplayMode == "always" {
		c.updateStatusIndicator()
	}
	c.mu.Unlock()

	// Return current status so TSF can sync state (including compiled hotkeys)
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	c.mu.Lock()
	defer c.mu.Unlock()

	// Sync CapsLock state from system on focus gain
	c.capsLockOn = ui.GetCapsLockState()

	// Push English auto-pair config to C++ side
	if c.bridgeServer != nil && c.config != nil {
		c.bridgeServer.PushEnglishPairConfigToAllClients(
			c.config.Input.AutoPair.English,
			c.config.Input.AutoPair.EnglishPairs,
		)
		c.bridgeServer.PushStatsConfigToAllClients(
			c.config.Stats.IsEnabled(),
			c.config.Stats.IsTrackEnglish(),
		)
	}

	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		IconLabel:          c.getIconLabelNoLock(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// HandleIMEActivated handles IME being switched back (user selected this IME again)
// This is called from TSF's Activate method
func (c *Coordinator) HandleIMEActivated(processID uint32) *bridge.StatusUpdateData {
	if processID != 0 {
		c.activeProcessID = processID
		c.activeProcessName = bridge.GetProcessName(processID)
		c.activeCompatRule = c.appCompat.GetRule(c.activeProcessName)
	}
	c.logger.Info("IME activated (user switched back to this IME)", "processID", processID)

	// Clear any pending input state when IME is reactivated
	// This ensures composition state is consistent
	c.mu.Lock()
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = ""
		c.inputCursorPos = 0
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.logger.Debug("Cleared input buffer on IME activated")
	}
	c.mu.Unlock()

	// Hide candidate window (will be shown again when user starts typing)
	c.hideUI()

	// Set IME as activated (this will show toolbar if enabled)
	c.SetIMEActivated(true)

	// 常驻模式：IME 激活时显示状态
	c.mu.Lock()
	if c.config != nil && c.config.UI.StatusIndicator.Enabled && c.config.UI.StatusIndicator.DisplayMode == "always" {
		c.updateStatusIndicator()
	}
	c.mu.Unlock()

	// Return current status so TSF can sync state (including compiled hotkeys)
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	c.mu.Lock()
	defer c.mu.Unlock()

	// Sync CapsLock state from system on IME activation
	c.capsLockOn = ui.GetCapsLockState()

	// Push English auto-pair config to C++ side
	if c.bridgeServer != nil && c.config != nil {
		c.bridgeServer.PushEnglishPairConfigToAllClients(
			c.config.Input.AutoPair.English,
			c.config.Input.AutoPair.EnglishPairs,
		)
		c.bridgeServer.PushStatsConfigToAllClients(
			c.config.Stats.IsEnabled(),
			c.config.Stats.IsTrackEnglish(),
		)
	}

	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		IconLabel:          c.getIconLabelNoLock(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// HandleCommitRequest handles a commit request from TSF (barrier mechanism)
// This is called when Space/Enter/number key is pressed during composition
func (c *Coordinator) HandleCommitRequest(data bridge.CommitRequestData) *bridge.CommitResultData {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.statRecorded = false
	c.logger.Debug("Handling commit request",
		"barrierSeq", data.BarrierSeq,
		"triggerKey", data.TriggerKey,
		"inputBuffer", data.InputBuffer)

	var text string
	var newComposition string
	var modeChanged bool

	// Determine action based on trigger key
	switch data.TriggerKey {
	case 0x20: // VK_SPACE
		result := c.handleSpaceInternal()
		if result != nil {
			text = result.Text
			modeChanged = result.ModeChanged
			newComposition = result.NewComposition
		}

	case 0x0D: // VK_RETURN
		result := c.handleEnterInternal()
		if result != nil {
			text = result.Text
		}

	default:
		// Number keys 1-9 (VK codes 0x31-0x39)
		if data.TriggerKey >= 0x31 && data.TriggerKey <= 0x39 {
			num := int(data.TriggerKey - 0x30) // Convert VK code to number 1-9
			result := c.handleNumberKeyInternal(num)
			if result != nil {
				text = result.Text
				newComposition = result.NewComposition
			}
		} else if data.TriggerKey == 0x30 {
			// Number key 0 selects 10th candidate
			result := c.handleNumberKeyInternal(10)
			if result != nil {
				text = result.Text
				newComposition = result.NewComposition
			}
		}
	}

	return &bridge.CommitResultData{
		BarrierSeq:     data.BarrierSeq,
		Text:           text,
		NewComposition: newComposition,
		ModeChanged:    modeChanged,
		ChineseMode:    c.chineseMode,
	}
}

// handleSpaceInternal is the internal implementation of handleSpace (without lock)
func (c *Coordinator) handleSpaceInternal() *bridge.KeyEventResult {
	// Select first candidate of current page
	if len(c.candidates) > 0 {
		// Calculate index of first candidate on current page
		index := (c.currentPage - 1) * c.candidatesPerPage
		if index < len(c.candidates) {
			return c.doSelectCandidate(index)
		}
	} else if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		// No candidates, commit confirmed segments + raw input
		var finalText string
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			finalText += t
		}
		if len(c.inputBuffer) > 0 {
			raw := c.inputBuffer
			if c.fullWidth {
				raw = transform.ToFullWidth(raw)
			}
			finalText += raw
		}

		c.recordCommit(finalText, len(c.inputBuffer), -1, store.SourceRawInput)
		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: finalText,
		}
	}
	return nil
}

// handleEnterInternal is the internal implementation of handleEnter (without lock)
func (c *Coordinator) handleEnterInternal() *bridge.KeyEventResult {
	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		var finalText string
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			finalText += t
		}
		if len(c.inputBuffer) > 0 {
			raw := c.inputBuffer
			if c.fullWidth {
				raw = transform.ToFullWidth(raw)
			}
			finalText += raw
		}

		c.recordCommit(finalText, len(c.inputBuffer), -1, store.SourceRawInput)
		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: finalText,
		}
	}
	return nil
}

// handleNumberKeyInternal is the internal implementation of handleNumberKey (without lock)
func (c *Coordinator) handleNumberKeyInternal(num int) *bridge.KeyEventResult {
	// num is 1-9 or 10 (key '0'), convert to 0-based index within current page
	index := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if index < len(c.candidates) {
		return c.doSelectCandidate(index)
	}
	return nil
}
