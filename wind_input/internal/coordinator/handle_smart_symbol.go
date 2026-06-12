package coordinator

import (
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/transform"
)

// 智能符号模式（Smart Symbol Mode）
//
// 行为：在中文标点模式下，提交一个「参与集合内的中文标点」（默认见 DefaultConfig，可在
// 设置中配置）后，若在时限内（默认 500ms）再次按下**同一按键**，则删除刚上屏的中文标点、
// 替换为该键的英文产物。净效果（纠错语义）：`。`+快速`。` → `.`。
//
// 触发判定基于「已武装的 press1 产物 + 同一按键 + 光标前字符」，而非 press2 重新计算的
// 产物——因此引号也能命中：`"` 键交替产生 “/”，press1 产 “、press2 产 ”，按 press1 的
// “ 与光标前字符匹配即可触发，替换为英文直引号 "。
//
// 多字符（……/——）：删除数 = 已武装中文标点串的 rune 数（…… 删 2）。
// 全角 / 自定义映射：computePunctStrPure 镜像 convertPunct 的优先级（自定义列 > 中文/英文
// 转换 > 全角），全角下替换输出全角英文（。。 → ．）；自定义键也参与（/→、 与 \→、 凭
// 记录的原始按键各自还原）。
//
// 稳健性核心：触发前校验「光标前一个字符（PrevChar，来自 ITfTextEditSink）确为已武装中文
// 标点串的末位 rune」，且仍处于中文标点模式。校验不过即安全退化，绝不误删。
//
// 注意：开启自动配对（中文/英文标点配对）的符号，press1 会插入配对并回退光标，与本特性
// 的 PrevChar 假设冲突，故对被配对的符号不生效（设置对话框已说明）。
//
// 详见 docs/design/smart-symbol-mode.md。

// trySmartSymbolReplace 在 handlePunctuation 入口判定智能符号替换。
//   - 返回非 nil：本次为 press2 触发，调用方应直接短路返回该替换响应。
//   - 返回 nil：未触发；本函数已按需更新「待命（arm）」状态，调用方继续普通标点流程。
//
// 必须持锁调用（与 handlePunctuation 同）。
func (c *Coordinator) trySmartSymbolReplace(r rune, afterDigit bool, prevChar rune) *bridge.KeyEventResult {
	if c.config == nil || !c.config.Input.SmartSymbolMode {
		return nil
	}

	// press2 触发判定：仍在中文标点模式 + 已武装 + 同一按键 + 时限内 + 光标前字符为
	// 已武装中文标点串的末位 rune。匹配的是 press1 的产物，故引号（“→”）也能命中。
	if c.smartSymbolArmed && r == c.smartSymbolKey && c.isEffectiveChinesePunct() &&
		time.Since(c.smartSymbolAt) < c.smartSymbolTimeout() {
		armedRunes := []rune(c.smartSymbolStr)
		if len(armedRunes) > 0 && prevChar == armedRunes[len(armedRunes)-1] {
			if rep, ok := c.computePunctStrPure(r, false); ok {
				c.smartSymbolArmed = false
				// 吃掉一个引号后回退引号交替状态，使下一次同引号仍从左引号开始。
				if r == '\'' || r == '"' {
					c.punctConverter.RevertLastQuote(r)
				}
				c.logger.Debug("SmartSymbol: replace prev chinese punct with english", "count", len(armedRunes))
				return &bridge.KeyEventResult{
					Type:         bridge.ResponseTypeReplaceBackward,
					ReplaceCount: len(armedRunes),
					Text:         rep,
				}
			}
		}
	}

	// 未触发：尝试以本次按键的中文产物武装，等待下一次同键快速重复。
	cn, ok := c.smartSymbolArmStr(r, afterDigit, prevChar)
	if !ok {
		c.smartSymbolArmed = false
		return nil
	}
	c.smartSymbolArmed = true
	c.smartSymbolKey = r
	c.smartSymbolStr = cn
	c.smartSymbolAt = time.Now()
	return nil
}

// smartSymbolArmStr 计算按键 r 当前会产生的「参与集合内的中文标点串」，用于武装。
// 无副作用。不参与时返回 ("", false)。
func (c *Coordinator) smartSymbolArmStr(r rune, afterDigit bool, prevChar rune) (string, bool) {
	// 仅中文标点模式参与（英文标点 / CapsLock 视为英文时不产生中文标点）。
	if !c.isEffectiveChinesePunct() {
		return "", false
	}
	// 数字后智能标点会把该键转成英文，不参与。
	if c.shouldSmartPunct(r, afterDigit, prevChar) {
		return "", false
	}
	cn, ok := c.computePunctStrPure(r, true)
	if !ok {
		return "", false
	}
	if !c.smartSymbolParticipates(cn) {
		return "", false
	}
	return cn, true
}

// computePunctStrPure 无副作用地计算按键 r 在当前模式下产生的标点串，**镜像** convertPunct
// 的优先级：自定义映射列（colIdx 0=中文半角 / 1=英文全角 / 2=中文全角）> 中文/英文转换 >
// 全角转换。
//   - chinese=true：算"中文标点"产物（智能符号武装/匹配用），引号经 PeekChineseStr 预测。
//   - chinese=false：算"英文标点"产物（智能符号替换用，即该键在英文标点模式下的输出）。
//
// 引号的自定义映射有状态、键名特殊（"1/"2），此处保守跳过自定义、走标准引号/英文产物。
func (c *Coordinator) computePunctStrPure(r rune, chinese bool) (string, bool) {
	fullWidth := c.fullWidth
	isQuote := r == '\'' || r == '"'

	// 自定义映射优先（引号除外）。
	if !isQuote && c.config.Input.PunctCustom.Enabled {
		colIdx := -1
		switch {
		case chinese && fullWidth:
			colIdx = 2 // 中文全角
		case chinese:
			colIdx = 0 // 中文半角
		case fullWidth:
			colIdx = 1 // 英文全角
		}
		if colIdx >= 0 {
			if v, ok := c.smartSymbolCustomLookup(r, colIdx); ok {
				return v, true
			}
		}
	}

	s := string(r)
	if chinese {
		cs, ok := c.punctConverter.PeekChineseStr(r)
		if !ok {
			return "", false // 不是可转换的中文标点键
		}
		s = cs
	}
	if fullWidth {
		s = transform.ToFullWidth(s)
	}
	return s, true
}

// smartSymbolCustomLookup 从配置直接读自定义标点映射的指定列（纯查表，不碰转换器状态）。
// 与 PunctuationConverter.LookupCustom 的非引号分支等价，但无副作用。
func (c *Coordinator) smartSymbolCustomLookup(r rune, colIdx int) (string, bool) {
	m := c.config.Input.PunctCustom.Mappings
	if m == nil {
		return "", false
	}
	vals, ok := m[string(r)]
	if !ok || colIdx >= len(vals) || vals[colIdx] == "" {
		return "", false
	}
	return vals[colIdx], true
}

// smartSymbolParticipates 判断中文标点串 cn 是否在用户配置的参与集合内。
// SmartSymbolChars 为中文标点字符串，用子串包含匹配——单字符、多字符（…… / ——）、
// 引号（“”‘’）均可正确命中。
func (c *Coordinator) smartSymbolParticipates(cn string) bool {
	return cn != "" && strings.Contains(c.config.Input.SmartSymbolChars, cn)
}

// smartSymbolTimeout 返回判定时限；非法值回退到 500ms。
func (c *Coordinator) smartSymbolTimeout() time.Duration {
	ms := c.config.Input.SmartSymbolTimeoutMs
	if ms <= 0 {
		ms = 500
	}
	return time.Duration(ms) * time.Millisecond
}

// disarmSmartSymbol 解除智能符号待命态（焦点变化等场景的防御性复位）。需持锁调用。
func (c *Coordinator) disarmSmartSymbol() {
	c.smartSymbolArmed = false
}
