// handle_special_mode.go — 引导键特殊模式（自定义码表）状态机。
// 结构上紧跟 handle_quick_input.go；查询码表而非日期/计算生成器，并实施三档自动上屏。
package coordinator

import (
	"path/filepath"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
)

// schemasDirs 返回码表文件解析的候选基目录（按优先级，靠前者覆盖），
// 与 schema.DiscoverSchemas 同源：用户配置目录/schemas（覆盖）+ 内置 dataRoot/schemas。
// 其中 dataRoot = GetDataDir(exeDir) = exeDir/data（内置方案/词库根目录）。
func (c *Coordinator) schemasDirs() []string {
	var dirs []string
	if cfgDir, err := config.GetConfigDir(); err == nil {
		dirs = append(dirs, filepath.Join(cfgDir, "schemas"))
	}
	if exeDir, err := config.GetExeDir(); err == nil {
		dirs = append(dirs, filepath.Join(config.GetDataDir(exeDir), "schemas"))
	}
	if len(dirs) == 0 {
		dirs = append(dirs, "schemas")
	}
	return dirs
}

// matchSpecialTrigger 检查 (key, keyCode) 是否匹配指定 id 的触发键，返回匹配的触发键字符串。
func (c *Coordinator) matchSpecialTrigger(id, key string, keyCode int) string {
	if c.specialModeReg == nil {
		return ""
	}
	inst := c.specialModeReg.get(id)
	if inst == nil {
		return ""
	}
	return matchTriggerKeyInList(inst.cfg.TriggerKeys, key, keyCode)
}

// setupSpecialMode 进入特殊模式，初始化状态，返回 (prefix, true)；失败返回 ("", false)。
func (c *Coordinator) setupSpecialMode(id, triggerKey string) (string, bool) {
	if c.specialModeReg == nil {
		return "", false
	}
	inst := c.specialModeReg.get(id)
	if inst == nil {
		return "", false
	}
	_, err := c.specialModeReg.ensureLoaded(inst)
	if err != nil {
		c.logger.Warn("special mode 码表加载失败",
			"id", id, "err", err.Error())
		return "", false
	}

	c.specialMode = true
	c.specialActiveID = id
	c.specialTriggerKey = triggerKey
	c.specialBuffer = ""

	// 强制竖排：保存当前布局并切换
	if inst.cfg.ForceVertical {
		if c.config != nil {
			c.specialSavedLayout = c.config.UI.CandidateLayout
		}
		if c.uiManager != nil {
			c.uiManager.SetCandidateLayout(config.LayoutVertical)
		}
	}

	c.logger.Debug("Entered special mode", "id", id)

	c.updateSpecialCandidates()

	// 首次进入触发 C++ 端 StartComposition，等 OnLayoutChange 坐标到达后再 show。
	c.armPendingFirstShow()

	return c.specialPrefix(), true
}

// specialPrefix 返回当前特殊模式触发键对应的字符。
func (c *Coordinator) specialPrefix() string {
	return triggerKeyToChar(c.specialTriggerKey)
}

// updateSpecialCandidates 根据 specialBuffer 查表更新候选，返回是否应自动上屏。
func (c *Coordinator) updateSpecialCandidates() (autoCommit bool) {
	if c.specialModeReg == nil {
		return false
	}
	inst := c.specialModeReg.get(c.specialActiveID)
	if inst == nil || inst.table == nil {
		return false
	}
	buf := c.specialBuffer
	if len(buf) == 0 && !inst.cfg.ShowAllOnEntry {
		// 默认：刚进入（空编码）只显示模式徽标提示，不列候选。
		c.candidates = nil
		c.totalPages = 1
		c.currentPage = 1
		c.selectedIndex = 0
		c.specialHasMore = false
		c.specialCandidateLimit = 0
		c.specialCandidateInput = ""
		return false
	}

	// 展示用前缀匹配（进入后/打字时即时提示），自动上屏判定用精确匹配。
	// 空编码 + ShowAllOnEntry：prefix "" 列出整张码表（Lookup/HasLongerCode 对空串
	// 分别返回 空/false，故不会误触自动上屏）。
	// 动态分级：初始只取一小批，翻页到末尾再 expandSpecialCandidates 翻倍。
	exact := inst.table.Lookup(buf)
	hasLonger := inst.table.HasLongerCode(buf)
	c.specialCandidateLimit = specialInitialCandidateLimit
	c.specialCandidateInput = buf
	display := c.specialLookup(inst, buf, c.specialCandidateLimit)
	c.specialHasMore = len(display) >= c.specialCandidateLimit
	c.candidates = c.buildSpecialUICandidates(display)

	auto := decideSpecialAutoCommit(inst.cfg.AutoCommit, inst.cfg.FixedLength, len(buf), len(exact), hasLonger)

	// 计算分页（与 updateQuickInputCandidates 一致）
	c.refreshEffectivePerPage()
	total := len(c.candidates)
	c.totalPages = (total + c.candidatesPerPage - 1) / c.candidatesPerPage
	if c.totalPages < 1 {
		c.totalPages = 1
	}
	if c.currentPage > c.totalPages {
		c.currentPage = c.totalPages
	}
	if c.currentPage < 1 {
		c.currentPage = 1
	}

	return auto
}

// buildSpecialUICandidates 将码表候选转换为 UI 候选，并应用 $CC/$X/$AA/$SS 展开。
// $AA/$SS 会展开为多条候选；$CC/$X 展开为单条；普通文本原样保留。
func (c *Coordinator) buildSpecialUICandidates(raw []ui.Candidate) []ui.Candidate {
	if len(raw) == 0 {
		return nil
	}
	out := make([]ui.Candidate, 0, len(raw))
	for _, cand := range raw {
		switch {
		case c.cmdbarValueExpander != nil && (dict.HasExpandable(cand.Text) || dict.HasSSMarker(cand.Text)):
			// $CC/$X/$SS：需要 hook，必须有 expander
			expanded := c.cmdbarValueExpander.ExpandToCandidates(cand.Code, cand.Text)
			out = append(out, expanded...)
		case dict.HasAAMarker(cand.Text):
			// $AA：纯解析，不需要 hook，可无 expander
			if c.cmdbarValueExpander != nil {
				expanded := c.cmdbarValueExpander.ExpandToCandidates(cand.Code, cand.Text)
				out = append(out, expanded...)
			} else {
				// 无 expander 时直接调解析路径
				if name, chars, ok := dict.ParseAAMarker(cand.Text); ok {
					for _, r := range chars {
						out = append(out, ui.Candidate{Text: string(r), Code: cand.Code, Comment: name})
					}
				} else {
					out = append(out, cand)
				}
			}
		default:
			out = append(out, cand)
		}
	}
	for i := range out {
		out[i].Index = i + 1
	}
	return out
}

// handleSpecialModeKey 处理特殊模式下的按键，镜像 handleQuickInputKey。
func (c *Coordinator) handleSpecialModeKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	vk := uint32(data.KeyCode)

	switch {
	// 空格：缓冲区非空 → 选当前高亮；空 → 上屏触发符
	case vk == ipc.VK_SPACE:
		if len(c.specialBuffer) > 0 && len(c.candidates) > 0 {
			index := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
			return c.selectSpecialCandidate(index)
		}
		return c.exitSpecialMode(true, c.specialPrefix())

	// 回车：有 buffer → 上屏原文；空 → 上屏触发符
	case vk == ipc.VK_RETURN:
		if len(c.specialBuffer) > 0 {
			return c.exitSpecialMode(true, c.specialBuffer)
		}
		return c.exitSpecialMode(true, c.specialPrefix())

	// 退格
	case vk == ipc.VK_BACK:
		if len(c.specialBuffer) > 0 {
			c.specialBuffer = c.specialBuffer[:len(c.specialBuffer)-1]
			c.currentPage = 1
			c.selectedIndex = 0
			c.updateSpecialCandidates()
			c.showSpecialUI()
			prefix := c.specialPrefix()
			preedit := prefix + c.specialBuffer
			return c.modeCompositionResult(preedit, len(preedit))
		}
		return c.exitSpecialMode(false, "")

	// ESC：退出
	case vk == ipc.VK_ESCAPE:
		return c.exitSpecialMode(false, "")

	// 翻页上
	case c.isQuickInputPageUpKey(key, int(vk), uint32(data.Modifiers)):
		if c.currentPage > 1 {
			c.currentPage--
			c.selectedIndex = 0
			c.showSpecialUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// 翻页下
	case c.isQuickInputPageDownKey(key, int(vk), uint32(data.Modifiers)):
		if c.currentPage < c.totalPages {
			c.currentPage++
			c.selectedIndex = 0
			if c.specialHasMore && c.currentPage >= c.totalPages-1 {
				c.expandSpecialCandidates()
			}
			c.showSpecialUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// 高亮上移
	case c.isHighlightUpKey(vk, uint32(data.Modifiers)):
		if len(c.candidates) > 0 {
			if c.selectedIndex > 0 {
				c.selectedIndex--
				c.showSpecialUI()
			} else if c.currentPage > 1 {
				c.currentPage--
				startIdx := (c.currentPage - 1) * c.candidatesPerPage
				endIdx := startIdx + c.candidatesPerPage
				if endIdx > len(c.candidates) {
					endIdx = len(c.candidates)
				}
				c.selectedIndex = endIdx - startIdx - 1
				c.showSpecialUI()
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// 高亮下移
	case c.isHighlightDownKey(vk, uint32(data.Modifiers)):
		if len(c.candidates) > 0 {
			startIdx := (c.currentPage - 1) * c.candidatesPerPage
			endIdx := startIdx + c.candidatesPerPage
			if endIdx > len(c.candidates) {
				endIdx = len(c.candidates)
			}
			pageCount := endIdx - startIdx
			if c.selectedIndex < pageCount-1 {
				c.selectedIndex++
				c.showSpecialUI()
			} else if c.currentPage < c.totalPages {
				c.currentPage++
				c.selectedIndex = 0
				if c.specialHasMore && c.currentPage >= c.totalPages-1 {
					c.expandSpecialCandidates()
				}
				c.showSpecialUI()
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// 二候选键：候选 ≥ 2 选第二候选，否则回落标点顶屏
	case c.isSelectKey2(key, data.KeyCode):
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		idx := pageStart + 1
		if idx < len(c.candidates) && idx-pageStart < c.candidatesPerPage {
			return c.selectSpecialCandidate(idx)
		}
		return c.specialPunctCommit(key)

	// 三候选键：候选 ≥ 3 选第三候选，否则回落标点顶屏
	case c.isSelectKey3(key, data.KeyCode):
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		idx := pageStart + 2
		if idx < len(c.candidates) && idx-pageStart < c.candidatesPerPage {
			return c.selectSpecialCandidate(idx)
		}
		return c.specialPunctCommit(key)

	// 数字 1-9：选当前页第 n-1 候选
	case len(key) == 1 && key[0] >= '1' && key[0] <= '9':
		n := int(key[0] - '0')
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		globalIdx := pageStart + n - 1
		if globalIdx < len(c.candidates) {
			return c.selectSpecialCandidate(globalIdx)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// 字母 a-z（小写归一）：追加到缓冲区
	case len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')):
		lower := key[0]
		if lower >= 'A' && lower <= 'Z' {
			lower = lower - 'A' + 'a'
		}
		c.specialBuffer += string(lower)
		c.currentPage = 1
		c.selectedIndex = 0
		if c.updateSpecialCandidates() {
			// 自动上屏：提交精确匹配候选（而非前缀展示列表的首项）
			return c.commitSpecialAuto()
		}
		c.showSpecialUI()
		prefix := c.specialPrefix()
		preedit := prefix + c.specialBuffer
		return c.modeCompositionResult(preedit, len(preedit))

	// 可打印符号（含引导符、标点）：顶屏当前高亮候选 + 输出该符号，然后退出。
	// 解决：再次按引导符即可输入该符号、标点顶屏、无效符号顶屏。
	case len(key) == 1 && key[0] >= '!' && key[0] <= '~':
		return c.specialPunctCommit(key)

	default:
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
}

// specialPunctCommit 顶屏当前高亮候选（仅 buffer 非空且为文本候选时）+ 输出按键字符
// （标点按中英文模式转换），然后退出特殊模式。供再次按引导符 / 标点顶屏 / 无效符号顶屏复用。
func (c *Coordinator) specialPunctCommit(key string) *bridge.KeyEventResult {
	var head string
	if len(c.specialBuffer) > 0 && len(c.candidates) > 0 {
		idx := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
		if idx >= 0 && idx < len(c.candidates) && len(c.candidates[idx].Actions) == 0 {
			head = c.candidates[idx].Text
			if c.fullWidth {
				head = transform.ToFullWidth(head)
			}
		}
	}
	tail := key
	if len(key) == 1 {
		tail = c.convertPunct(rune(key[0]), false, 0)
	}
	return c.exitSpecialMode(true, head+tail)
}

// 动态分级加载参数（对标正常模式：初始小批 + 翻页到末尾翻倍，上限封顶）。
const (
	specialInitialCandidateLimit = 100  // 初始加载候选上限
	specialMaxCandidateLimit     = 5000 // 翻倍加载的封顶
)

// specialLookup 按编码取展示候选：空编码走 AllCandidates（列全表），否则前缀匹配。
// 返回原始码表候选（未展开 $AA/$SS），limit 为本次加载上限。
func (c *Coordinator) specialLookup(inst *specialModeInstance, buf string, limit int) []ui.Candidate {
	if len(buf) == 0 {
		// LookupPrefix("") 在 wdb 下短路返回 nil，列全表须用 AllCandidates。
		return inst.table.AllCandidates(limit)
	}
	return inst.table.LookupPrefix(buf, limit)
}

// expandSpecialCandidates 翻页到已加载末尾时翻倍重查更多候选（镜像 expandCandidates）。
func (c *Coordinator) expandSpecialCandidates() {
	if !c.specialHasMore || c.specialCandidateInput != c.specialBuffer {
		return
	}
	if c.specialModeReg == nil {
		return
	}
	inst := c.specialModeReg.get(c.specialActiveID)
	if inst == nil || inst.table == nil {
		return
	}
	newLimit := c.specialCandidateLimit * 2
	if newLimit > specialMaxCandidateLimit {
		newLimit = specialMaxCandidateLimit
	}
	if newLimit <= c.specialCandidateLimit {
		c.specialHasMore = false
		return
	}
	display := c.specialLookup(inst, c.specialBuffer, newLimit)
	if len(display) <= c.specialCandidateLimit {
		// 没拿到更多（已到表尾）：停止扩展。
		c.specialHasMore = false
		return
	}
	c.candidates = c.buildSpecialUICandidates(display)
	c.specialCandidateLimit = newLimit
	c.specialHasMore = len(display) >= newLimit

	c.refreshEffectivePerPage()
	total := len(c.candidates)
	c.totalPages = (total + c.candidatesPerPage - 1) / c.candidatesPerPage
	if c.totalPages < 1 {
		c.totalPages = 1
	}
	if c.currentPage > c.totalPages {
		c.currentPage = c.totalPages
	}
	if c.currentPage < 1 {
		c.currentPage = 1
	}
}

// commitSpecialAuto 自动上屏：提交当前 buffer 的精确匹配候选（唯一），
// 而非前缀展示列表的首项——避免 fixed_length 档存在更长码时选错。
func (c *Coordinator) commitSpecialAuto() *bridge.KeyEventResult {
	inst := c.specialModeReg.get(c.specialActiveID)
	if inst == nil || inst.table == nil {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
	expanded := c.buildSpecialUICandidates(inst.table.Lookup(c.specialBuffer))
	if len(expanded) == 0 {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
	c.candidates = expanded
	return c.selectSpecialCandidate(0)
}

// selectSpecialCandidate 选择特殊模式候选并上屏或执行命令。
func (c *Coordinator) selectSpecialCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
	cand := c.candidates[index]

	// 命令候选（带 Actions）
	if len(cand.Actions) > 0 {
		return c.commitSpecialCommand(cand)
	}

	// 普通文本候选
	text := cand.Text
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}
	if c.inputHistory != nil {
		c.inputHistory.Record(text, "", "", 0)
	}
	return c.exitSpecialMode(true, text)
}

// commitSpecialCommand 执行命令候选（有副作用 Actions），复用 commitCmdbarCandidate。
func (c *Coordinator) commitSpecialCommand(cand ui.Candidate) *bridge.KeyEventResult {
	// 先退出特殊模式（重置状态、恢复布局），然后走 cmdbar 候选上屏流程。
	// 注意：commitCmdbarCandidate 内部会调用 clearState + hideUI，
	// 所以这里只需重置 specialMode 特有字段，不重复调用 exitSpecialMode。
	if c.specialSavedLayout != "" && c.uiManager != nil {
		c.uiManager.SetCandidateLayout(c.specialSavedLayout)
	}
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
		c.uiManager.SetModeAccentColor(nil)
	}
	c.specialMode = false
	c.specialActiveID = ""
	c.specialTriggerKey = ""
	c.specialBuffer = ""
	c.specialSavedLayout = ""

	// 复用 commitCmdbarCandidate：它负责执行 Actions、记录历史、clearState、hideUI、返回结果。
	return c.commitCmdbarCandidate(cand, 0, 0)
}

// exitSpecialMode 退出特殊模式，镜像 exitQuickInputMode。
func (c *Coordinator) exitSpecialMode(commit bool, text string) *bridge.KeyEventResult {
	// 恢复布局
	if c.specialSavedLayout != "" && c.uiManager != nil {
		c.uiManager.SetCandidateLayout(c.specialSavedLayout)
	}

	// 重置模式标签和光效
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
		c.uiManager.SetModeAccentColor(nil)
	}

	// 重置所有特殊模式字段
	c.specialMode = false
	c.specialActiveID = ""
	c.specialTriggerKey = ""
	c.specialBuffer = ""
	c.specialSavedLayout = ""
	c.specialCandidateLimit = 0
	c.specialCandidateInput = ""
	c.specialHasMore = false
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.selectedIndex = 0
	c.hideUI()

	c.logger.Debug("Exited special mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		c.recordCommit(text, 0, -1, store.SourceSpecialMode)
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// showSpecialUI 显示特殊模式候选窗，镜像 showQuickInputUI。
func (c *Coordinator) showSpecialUI() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	caretX := c.caretX
	caretY := c.caretY
	caretHeight := c.caretHeight
	if c.config != nil && c.config.UI.InlinePreedit && c.compositionStartValid {
		caretX = c.compositionStartX
		caretY = c.compositionStartY
	}

	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || caretX > maxCoord || caretX < -maxCoord || caretY > maxCoord || caretY < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			caretX = c.lastValidX
			caretY = c.lastValidY
			caretHeight = 20
		} else {
			caretX = 400
			caretY = 300
			caretHeight = 20
		}
	}

	// 当前页候选切片
	startIdx := (c.currentPage - 1) * c.candidatesPerPage
	endIdx := startIdx + c.candidatesPerPage
	if endIdx > len(c.candidates) {
		endIdx = len(c.candidates)
	}
	var pageCandidates []ui.Candidate
	if startIdx < len(c.candidates) {
		pageCandidates = c.candidates[startIdx:endIdx]
	}

	displayCandidates := make([]ui.Candidate, len(pageCandidates))
	copy(displayCandidates, pageCandidates)
	for i := range displayCandidates {
		displayCandidates[i].Index = i + 1
	}

	// 预编辑文本
	preedit := c.specialPrefix() + c.specialBuffer
	// 嵌入编码且 buffer 为空：置空，让渲染层显示「只含模式徽标」的提示条
	if c.isInlinePreedit() && len(c.specialBuffer) == 0 {
		preedit = ""
	}

	// 模式标签与光效
	id := c.specialActiveID
	inst := c.specialModeReg.get(id)
	label := id
	if inst != nil {
		label = inst.cfg.Name
	}
	c.uiManager.SetModeLabel(label)
	c.uiManager.SetModeAccentColor(c.modeAccentColor("special:" + id))

	// 分级加载：还有更多未加载时传负 totalPages，告知渲染层「页数不止于此」（镜像主流程）。
	displayTotalPages := c.totalPages
	if c.specialHasMore {
		displayTotalPages = -c.totalPages
	}

	c.uiManager.ShowCandidates(
		displayCandidates,
		preedit,
		len(preedit),
		caretX,
		caretY,
		caretHeight,
		c.currentPage,
		displayTotalPages,
		len(c.candidates),
		c.candidatesPerPage,
		c.selectedIndex,
	)
}
