package ui

import (
	"image"
	"strings"

	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/pkg/config"
)

// DefaultCmdbarCandidatePrefix 副作用候选在候选框中渲染时的默认前缀符号。
// 让用户在候选列表里一眼分辨"普通文本候选"和"会跑副作用的命令直通车候选"。
// 用户可在 UI 配置中改成空串 (完全不显示) 或自定义符号 (如 ▶ / · / *)。
// candidate 自身的 Text 字段保持原状, 仅渲染时拼字符串, 避免污染历史记录与
// 右键菜单文案。
const DefaultCmdbarCandidatePrefix = "⚡"

// hasSideEffectAction 判定候选的 Actions 是否包含至少一个 ActionEffect (真副作用)。
// 仅含 ActionText (即 type(...) 纯文本上屏) 的 cmdbar 候选视觉上与普通候选无差,
// 不需要 ⚡ 标注。
func hasSideEffectAction(actions []cmdbar.ResolvedAction) bool {
	for _, a := range actions {
		if a.Kind == cmdbar.ActionEffect {
			return true
		}
	}
	return false
}

// CandidateNewlineGlyph 候选标签中换行符 (\r / \n) 的占位渲染符号。
// 候选框是单行控件, 候选文本含真实换行会撑破布局或与相邻候选重叠,
// 故渲染前统一替换为该符号。candidate.Text 本身不受影响 (上屏仍多行)。
// 不同字体渲染效果可能有差异, 后续可调 (如改用 ⏎ / ¶)。
const CandidateNewlineGlyph = "↵"

// candidateNewlineReplacer 把候选文本里的换行符折叠为 CandidateNewlineGlyph。
// \r\n 列在最前, NewReplacer 按参数顺序优先匹配, 保证 CRLF 折叠为单个符号。
var candidateNewlineReplacer = strings.NewReplacer(
	"\r\n", CandidateNewlineGlyph,
	"\r", CandidateNewlineGlyph,
	"\n", CandidateNewlineGlyph,
)

// candidateDisplayText 返回候选实际渲染到候选框的文本。
//   - 换行符 (\r / \n) 替换为 CandidateNewlineGlyph, 保证单行渲染;
//   - 命令直通车候选 (Actions 含 ActionEffect) 在文本前加 cmdbarPrefix; 当
//     cmdbarPrefix 为空时永远不加; 候选所有 Action 均为 ActionText (即仅
//     type(...) 上屏) 时也不加, 因为视觉上跟普通候选无差。
//
// candidate 自身的 Text 字段保持原状, 仅渲染时变换, 避免污染历史记录与
// 右键菜单文案。
func candidateDisplayText(cand Candidate, cmdbarPrefix string) string {
	text := candidateNewlineReplacer.Replace(cand.Text)
	if cmdbarPrefix != "" && hasSideEffectAction(cand.Actions) {
		return cmdbarPrefix + text
	}
	return text
}

// indexLabel returns the display string for a candidate index.
// Priority: overrideLabel > IndexLabels config > default 1-9,0
//
// IndexLabels 支持两种格式：
//   - 10 个字符的字符串（如 "①②③④⑤⑥⑦⑧⑨⑩"），按位取字符
//   - 包含 '/' 分隔符的模板（如 "1./2./3./4./5./6./7./8./9./0."），按斜杠分割取槽位
//
// index 取值 1..9 对应槽位 0..8；index=0 对应槽位 9（第十个候选）。
func indexLabel(indexLabels string, index int, overrideLabel string) string {
	if overrideLabel != "" {
		return overrideLabel
	}
	if indexLabels != "" {
		pos := index - 1
		if index == 0 {
			pos = 9
		}
		// 斜杠分隔的多字符模板
		if strings.Contains(indexLabels, "/") {
			parts := strings.Split(indexLabels, "/")
			if pos >= 0 && pos < len(parts) {
				return parts[pos]
			}
		} else {
			labels := []rune(indexLabels)
			if pos >= 0 && pos < len(labels) {
				return string(labels[pos])
			}
		}
	}
	return string(rune('0' + index))
}

// RenderCandidates renders candidates to an image
// hoverIndex: index of the hovered candidate (-1 for none)
// Returns the rendered image and candidate bounding rectangles for hit testing
func (r *Renderer) RenderCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string, selectedIndex int) (*image.RGBA, *RenderResult) {
	// Auto-refresh DPI-dependent config if DPI changed since last render
	r.refreshDPIIfNeeded()
	cfg := r.config

	// 盒模型 View 引擎是唯一渲染路径（旧固定化渲染器已退役）。
	if cfg.Layout == config.LayoutHorizontal {
		return r.renderHorizontalV2(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn, selectedIndex)
	}
	return r.renderVerticalV2(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn, selectedIndex)
}
