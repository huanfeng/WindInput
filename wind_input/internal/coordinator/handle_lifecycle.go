// handle_lifecycle.go — IME 生命周期事件（焦点、激活、停用）与 CommitRequest 处理
package coordinator

import (
	"fmt"
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
		case c.quickInputPinyinMode:
			c.logger.Debug("pendingFirstShow timeout: forcing showPinyinModeUI (quick input pinyin)")
			c.showPinyinModeUI(c.quickInputPinyinOps())
		case c.quickInputMode:
			c.logger.Debug("pendingFirstShow timeout: forcing showQuickInputUI")
			c.showQuickInputUI()
		case c.specialMode:
			c.logger.Debug("pendingFirstShow timeout: forcing showSpecialUI")
			c.showSpecialUI()
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
	c.muLockTraceWait("HandleCaretPending")
	defer c.mu.Unlock()
	if !c.pendingFirstShow {
		return
	}
	c.logger.Debug("CARET_PENDING handshake: extending first-show timeout")
	c.armPendingFirstShowWithTimeout(firstShowExtendedTimeout)
}

// HandleCaretUpdate handles caret position updates from C++ Bridge
func (c *Coordinator) HandleCaretUpdate(data bridge.CaretData) error {
	c.muLockTraceWait("HandleCaretUpdate")
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

	// 同步 caret 缓存到 reducer：非阻塞（按键热路径不应被工具栏 reducer 阻塞）。
	// 不参与 show/hide 决策——reducer 只把它存起来用于下一次真正的 Show 时定位。
	if c.toolbarReducer != nil {
		c.toolbarReducer.sendNonBlocking(toolbarEvent{
			kind:  tevCaretChanged,
			x:     data.X,
			y:     data.Y,
			valid: true,
		})
	}

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
	// tempEnglish 用 mode 标记而非 buffer 长度：触发键进入时 buffer 为空，
	// 但 preedit 含触发键 prefix，仍需 show 候选窗。
	hasTempEnglish := c.tempEnglishMode
	hasTempPinyin := c.tempPinyinMode
	hasQuickInputPinyin := c.quickInputPinyinMode
	hasQuickInput := c.quickInputMode
	hasSpecial := c.specialMode
	hasInput := hasMainInput || hasTempEnglish || hasTempPinyin || hasQuickInput || hasSpecial
	hasCandidates := len(c.candidates) > 0
	hasUI := c.uiManager != nil
	// 主输入流程必须有候选才 show；模式入口（quickInput / tempPinyin / tempEnglish）
	// 允许空候选，因为 preedit 中含触发键 prefix 或用户已输入的字母，
	// 仍需把候选窗（即便只有 preedit）渲染出来。
	// 非嵌入模式：主输入也允许空候选 show，让用户看到 inputBuffer（如 v/i/u/o
	// 等无对应候选的拼音字母），避免编码既不嵌入宿主又看不到候选窗的死角。
	canShow := hasUI && (hasCandidates || hasTempEnglish || hasTempPinyin || hasQuickInput || hasSpecial || (hasMainInput && !c.isInlinePreedit()))
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
		case hasQuickInputPinyin:
			c.showPinyinModeUI(c.quickInputPinyinOps())
		case hasQuickInput:
			c.showQuickInputUI()
		case hasSpecial:
			c.showSpecialUI()
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

	// 自动造词关键补救（2026-05-20）：
	// composition 关闭后，用户敲 Space/Enter 由 TSF 直接透传给宿主，Go 端不会被调用，
	// 因此 handleSpace/handleEnter 的 OnPhraseTerminated 永远不会触发。但宿主接收
	// Space/Enter 时光标会移动 → ITfTextEditSink::OnEndEdit → 本函数。借此补回
	// "句子结束"信号，让自动造词在用户预期的时机（敲完空格/回车）就写词，
	// 而不必等焦点切换或 5s idle 兜底。
	//
	// 关键 grace window：IME 自己提交候选时宿主也会发 SelectionChanged，
	// 若不过滤会让刚 append 的单字立即被 flush 掉（bufLen=1 不足 → 清空 buffer），
	// 实测延迟通常 < 50ms（见 wind_input_debug.log 中"select 中→Selection changed"约 16ms）。
	// 200ms 留出余量；之后到来的 SelectionChanged 视为真正的用户操作（敲空格/回车/点击）。
	if c.engineMgr != nil {
		const selfCommitGracePeriod = 200 * time.Millisecond
		if c.lastSelfCommitTime.IsZero() || time.Since(c.lastSelfCommitTime) > selfCommitGracePeriod {
			c.logger.Debug("SelectionChanged → OnPhraseTerminated (likely Space/Enter/click outside composition)")
			c.engineMgr.OnPhraseTerminated()
		} else {
			c.logger.Debug("SelectionChanged within self-commit grace, skipping OnPhraseTerminated",
				"sinceCommitMs", time.Since(c.lastSelfCommitTime).Milliseconds())
		}
	}
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

// HandleFocusLost handles focus lost events (real focus change, e.g., user clicked another window).
//
// 兄弟实例守护：同 HandleIMEDeactivated 注释，关 tab 场景下 server 已 markUnfocused
// 当前 clientID，但同 PID 兄弟实例（另一 tab）仍在 focusedClients 中。此时跳过
// IME 失活，工具栏与输入状态保留；待用户在兄弟实例上继续输入即可无缝衔接。
func (c *Coordinator) HandleFocusLost() {
	// 焦点彻底丢失（如点到按钮/桌面）：清除敏感字段抑制。聚焦到另一控件由
	// HandleFocusGained 重新评估。
	c.mu.Lock()
	c.clearSensitiveFieldNoLock()
	c.mu.Unlock()

	if c.bridgeServer != nil {
		c.muLockTraceWait("HandleFocusLost/peek")
		pid := c.activeProcessID
		c.mu.Unlock()
		if c.bridgeServer.IsActivelyFocusedPID(pid) {
			// 兄弟实例存活：清候选/输入状态，跳过 IME 翻转和重放路径。
			// 不走 shouldDeferClearForReplay 的原因：replay 是为"打字驱动焦点
			// 切换到新文档"设计的，关 tab 场景下用户的下一次输入会在兄弟实例
			// 上启动新的 composition，没有重建必要。
			c.logger.Debug("FocusLost: sibling client still focused, keeping IME active but clearing input",
				"pid", pid)
			if c.engineMgr != nil {
				c.engineMgr.OnPhraseTerminated()
			}
			c.mu.Lock()
			c.clearState()
			c.hideUI()
			c.mu.Unlock()
			return
		}
	}

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

// TSF InputScope 枚举值（见 Windows SDK inputscope.h）。C++ 端把焦点控件的 InputScope
// 集合编码为 bitmask（bit N = 枚举值 N 存在）随 focus_gained 上报，这里按位判定。
const (
	inputScopePassword        = 31 // IS_PASSWORD
	inputScopeNumericPassword = 63 // IS_NUMERIC_PASSWORD
)

// inputScopeHas 判断 InputScope bitmask 是否包含指定枚举值。
func inputScopeHas(mask uint64, scope uint) bool {
	if scope >= 64 {
		return false
	}
	return mask&(uint64(1)<<scope) != 0
}

// isSensitiveInputScope 判断焦点控件是否属于"应抑制中文（只输英文）"的敏感输入域。
//
// 只认 IS_PASSWORD / IS_NUMERIC_PASSWORD 位。注意 Chromium 系浏览器对 <input type=password>
// 只下发 IS_PRIVATE 而非 IS_PASSWORD，且无痕模式下所有字段也都是 IS_PRIVATE（二者信号相同，
// 无法用 InputScope 区分）。因此 C++ 端在读到 IS_PRIVATE 时用 UI Automation 的 IsPassword
// 二次确认，**确为密码框才补置 IS_PASSWORD 位**再上报——所以这里不直接判 IS_PRIVATE，
// 既能识别浏览器密码框，又不会误伤无痕/autocomplete=off 的普通中文输入框。
// IS_SEARCH(50) 等同样不在此列——中文搜索须保持中文。
func isSensitiveInputScope(mask uint64) bool {
	return inputScopeHas(mask, inputScopePassword) ||
		inputScopeHas(mask, inputScopeNumericPassword)
}

// applyPasswordFieldPolicyNoLock 依据焦点控件的 InputScope 设置敏感字段输入抑制。
// 调用方必须持有 c.mu。进入敏感（密码/隐私，见 isSensitiveInputScope）控件时置位
// sensitiveFieldActive，使输入侧按英文半角直通；**不改变** chineseMode（图标不变）。
// 首次进入时清空可能残留的中文合成，避免被带进敏感字段。聚焦非敏感控件时清除标志。
func (c *Coordinator) applyPasswordFieldPolicyNoLock(inputScopeMask uint64) {
	// 诊断：记录收到的 InputScope bitmask（纯元数据，不含输入内容）
	c.logger.Debug("Apply password field policy", "inputScopeMask", fmt.Sprintf("0x%016X", inputScopeMask))
	sensitive := isSensitiveInputScope(inputScopeMask)
	if sensitive && !c.sensitiveFieldActive {
		// 首次进入敏感字段：清掉残留的拼音/五笔合成，确保不会把候选/编码带进去
		c.clearState()
		c.hideUI()
		c.logger.Debug("Sensitive field focused, suppressing Chinese input (mode unchanged)")
	} else if !sensitive && c.sensitiveFieldActive {
		c.logger.Debug("Left sensitive field, restoring normal input")
	}
	c.sensitiveFieldActive = sensitive
}

// clearSensitiveFieldNoLock 清除敏感字段输入抑制（焦点彻底丢失时调用）。
// 调用方必须持有 c.mu。幂等。
func (c *Coordinator) clearSensitiveFieldNoLock() {
	c.sensitiveFieldActive = false
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
	if c.specialMode {
		return len(c.specialPrefix()) + len(c.specialBuffer)
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
// This is called from TSF's Deactivate method, before the client disconnects.
//
// 兄弟实例守护：当 Notepad11 这种多 tab 应用关闭其中一个 tab 时，被关的
// TextService 实例会发 CmdIMEDeactivated；但同 PID 内的其它 tab 仍持有焦点。
// 若此处无条件翻转 imeActivated → 工具栏会被错误隐藏，而 Notepad11 不会为
// 已存在的剩余 DocMgr 重发 FOCUS_GAINED/IME_ACTIVATED（Win11 XamlIsland
// 行为）→ 工具栏永远不再恢复。bridge.IsActivelyFocusedPID(activeProcessID)
// 报告同 PID 是否还有其它 clientID 在 focusedClients 中：true 即兄弟存活，
// 跳过整个 deactivate 流程（不清输入、不动 IME 状态、不投递 reducer 事件）。
//
// 真正失活路径（用户切换 IME / 最后一个实例销毁）下，server 端 markUnfocused
// 后 focusedClients 必然不含该 PID，守护放行，走原逻辑。
func (c *Coordinator) HandleIMEDeactivated() {
	if c.bridgeServer != nil {
		c.muLockTraceWait("HandleIMEDeactivated/peek")
		pid := c.activeProcessID
		c.mu.Unlock()
		if c.bridgeServer.IsActivelyFocusedPID(pid) {
			// 兄弟实例存活：跳过 IME 翻转和 reducer 事件，但仍要清当前实例的
			// 候选/输入状态 —— 否则用户在被销毁实例上未选词的 inputBuffer 会
			// 浮到兄弟实例上。OnPhraseTerminated 同步通知造词策略本段输入终止。
			c.logger.Debug("IMEDeactivated: sibling client still focused, keeping IME active but clearing input",
				"pid", pid)
			if c.engineMgr != nil {
				c.engineMgr.OnPhraseTerminated()
			}
			c.mu.Lock()
			c.clearState()
			c.hideUI()
			c.mu.Unlock()
			return
		}
	}

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

	// 候选窗与状态指示器与工具栏正交，直接 hide。工具栏走 reducer。
	if c.uiManager != nil {
		c.uiManager.Hide()
		c.uiManager.HideStatusIndicator()
	}
	if c.toolbarReducer != nil {
		c.toolbarReducer.sendCritical(toolbarEvent{kind: tevIMEDeactivated})
	}
}

// HandleClientDisconnected handles TSF client disconnection.
// When all clients disconnect (activeClients == 0), hide the toolbar via reducer.
func (c *Coordinator) HandleClientDisconnected(activeClients int) {
	c.logger.Debug("Client disconnected", "activeClients", activeClients)

	if activeClients == 0 {
		c.logger.Info("All TSF clients disconnected, hiding toolbar")
		c.mu.Lock()
		c.imeActivated = false
		c.mu.Unlock()

		// 候选窗与工具栏正交，候选窗直接 hide；工具栏由 reducer 单点决策。
		if c.uiManager != nil {
			c.uiManager.Hide()
		}
		if c.toolbarReducer != nil {
			c.toolbarReducer.sendCritical(toolbarEvent{kind: tevAllClientsDisconnected})
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

// HandleFocusGained handles focus gained events and returns current status.
// inputScopeMask 是焦点控件的 TSF InputScope bitmask（bit N = 枚举值 N 存在，
// 见 BinaryProtocol.h / coordinator 的 inputScope* 常量）。据此实现密码框强制英文：
// 进入 IS_PASSWORD 控件时切英文并记忆原模式，聚焦非密码控件时恢复。
func (c *Coordinator) HandleFocusGained(processID uint32, inputScopeMask uint64) *bridge.StatusUpdateData {
	// 保存变更前的 PID，用于后续检测同 PID 内部 DocMgr 切换（如 Explorer XamlIsland）。
	prevActiveProcessID := c.activeProcessID
	if processID != 0 {
		c.activeProcessID = processID
		c.activeProcessName = bridge.GetProcessName(processID)
		c.activeCompatRule = c.appCompat.GetRule(c.activeProcessName)
		if c.activeCompatRule != nil {
			c.logger.Debug("Compat rule matched", "process", c.activeProcessName, "caretUseTop", c.activeCompatRule.CaretUseTop, "skipCaretPending", c.activeCompatRule.SkipCaretPending, "pinCandidatePosition", c.activeCompatRule.PinCandidatePosition)
		}
		// 同步「固定候选位置」状态：让 doShowCandidates 在新焦点应用上使用对应的 pin 位置
		c.syncCandidatePinStateToUI(c.activeProcessName)
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
	c.muLockTraceWait("HandleFocusGained")
	c.lastOutputWasDigit = false

	// 密码框自动英文：在清理输入/构建状态之前应用，使后续 hideUI/状态构建/工具栏同步
	// 都基于已切换好的模式（强制英文时图标自然显示"英"）。
	c.applyPasswordFieldPolicyNoLock(inputScopeMask)

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
				case c.specialMode:
					replayText = c.specialPrefix() + c.specialBuffer
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
		// Explorer/XamlIsland 兼容：TSF 在 composition 进行中会对同一 DocMgr 重复触发
		// OnSetFocus（focus==prev，无中间 focus_lost），C++ 端照常发 focus_gained。
		// 若是同 PID 且最近有按键，视为宿主内部状态刷新而非真正的焦点切换，保留
		// buffer 并走 replay 路径重建 composition，避免候选框消失。
		samePIDWithRecentKey := processID != 0 && processID == prevActiveProcessID &&
			!c.lastKeyTime.IsZero() && time.Since(c.lastKeyTime) < 3*time.Second
		if samePIDWithRecentKey {
			if !c.isInlinePreedit() {
				replayText = ""
				replayCaretPos = 0
			} else {
				replayText = c.compositionText()
				replayCaretPos = c.displayCursorPos()
			}
			doReplay = true
			c.armPendingFirstShowWithTimeout(firstShowExtendedTimeout)
			c.logger.Debug("Same-PID focus gained with active input, replaying composition",
				"bufferLen", len(c.inputBuffer), "sinceLastKey", time.Since(c.lastKeyTime).String())
		} else {
			c.inputBuffer = ""
			c.inputCursorPos = 0
			c.candidates = nil
			c.currentPage = 1
			c.totalPages = 1
			c.expandedGroupTemplate = "" // buffer 重置, 清除二级展开标记
			c.logger.Debug("Cleared input buffer on focus gained")
		}
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

	// Sync CapsLock state from system on focus gain
	c.capsLockOn = ui.GetCapsLockState()

	// 在锁内读取返回值及 push 所需字段，然后立即解锁。
	// push 调用内部做阻塞式 named pipe 写入，若对端缓冲区满则会长时间阻塞；
	// 若在持锁期间执行，会导致 c.mu 被长时间占用，使所有后续命令超时。
	status := &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		IconLabel:          c.getIconLabelNoLock(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
	var (
		bridgeServer    = c.bridgeServer
		shouldPush      bool
		autoPairEnabled bool
		autoPairs       []string
		statsEnabled    bool
		statsTrackEng   bool
	)
	if c.bridgeServer != nil && c.config != nil {
		shouldPush = true
		autoPairEnabled = c.config.Input.AutoPair.English
		autoPairs = c.config.Input.AutoPair.EnglishPairs
		statsEnabled = c.config.Stats.IsEnabled()
		statsTrackEng = c.config.Stats.IsTrackEnglish()
	}
	c.mu.Unlock()

	// Push 在独立 goroutine 中执行：既不持有 c.mu，也不阻塞响应返回。
	// 同步写入 push pipe 可能因对端缓冲区满而阻塞超过 C++ 端 200ms 读超时，
	// 导致 C++ 断连并丢失此次状态同步。
	if shouldPush {
		go bridgeServer.PushEnglishPairConfigToActiveClient(autoPairEnabled, autoPairs)
		go bridgeServer.PushStatsConfigToActiveClient(statsEnabled, statsTrackEng)
	}

	return status
}

// HandleIMEActivated handles IME being switched back (user selected this IME again)
// This is called from TSF's Activate method
func (c *Coordinator) HandleIMEActivated(processID uint32) *bridge.StatusUpdateData {
	if processID != 0 {
		c.activeProcessID = processID
		c.activeProcessName = bridge.GetProcessName(processID)
		c.activeCompatRule = c.appCompat.GetRule(c.activeProcessName)
		c.syncCandidatePinStateToUI(c.activeProcessName)
	}
	c.logger.Info("IME activated (user switched back to this IME)", "processID", processID)

	// Clear any pending input state when IME is reactivated
	// This ensures composition state is consistent
	c.muLockTraceWait("HandleIMEActivated")
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = ""
		c.inputCursorPos = 0
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.expandedGroupTemplate = "" // buffer 重置, 清除二级展开标记
		c.logger.Debug("Cleared input buffer on IME activated")
	}
	// 未开启「记忆上次状态」时，切换回本输入法应回到配置的默认状态，
	// 而不是延续上次切走前的状态。
	if c.config != nil && !c.config.Startup.RememberLastState {
		c.chineseMode = c.config.Startup.DefaultChineseMode
		c.fullWidth = c.config.Startup.DefaultFullWidth
		c.chinesePunctuation = c.config.Startup.DefaultChinesePunct
		c.punctConverter.Reset()
		c.logger.Debug("Applied default mode on IME activation (remember_last_state=false)",
			"chineseMode", c.chineseMode, "fullWidth", c.fullWidth, "chinesePunct", c.chinesePunctuation)
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

	// Sync CapsLock state from system on IME activation
	c.capsLockOn = ui.GetCapsLockState()

	// 在锁内读取返回值及 push 所需字段，然后立即解锁。
	// push 调用（PushEnglishPairConfigToActiveClient / PushStatsConfigToActiveClient）
	// 内部做阻塞式 named pipe 写入，若对端缓冲区满则会长时间阻塞；
	// 若在持锁期间执行，会导致 c.mu 被长时间占用，使所有慢路径命令超时。
	status := &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		IconLabel:          c.getIconLabelNoLock(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
	var (
		bridgeServer    = c.bridgeServer
		shouldPush      bool
		autoPairEnabled bool
		autoPairs       []string
		statsEnabled    bool
		statsTrackEng   bool
	)
	if bridgeServer != nil && c.config != nil {
		shouldPush = true
		autoPairEnabled = c.config.Input.AutoPair.English
		autoPairs = c.config.Input.AutoPair.EnglishPairs
		statsEnabled = c.config.Stats.IsEnabled()
		statsTrackEng = c.config.Stats.IsTrackEnglish()
	}
	c.mu.Unlock()

	// Push 在独立 goroutine 中执行：既不持有 c.mu，也不阻塞响应返回。
	// 同步写入 push pipe 可能因对端缓冲区满而阻塞超过 C++ 端 200ms 读超时，
	// 导致 C++ 断连并丢失此次状态同步。
	if shouldPush {
		go bridgeServer.PushEnglishPairConfigToActiveClient(autoPairEnabled, autoPairs)
		go bridgeServer.PushStatsConfigToActiveClient(statsEnabled, statsTrackEng)
	}

	return status
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
	// Select the highlighted candidate on the current page
	if len(c.candidates) > 0 {
		index := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
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
