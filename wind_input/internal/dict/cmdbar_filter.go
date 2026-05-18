// cmdbar_filter.go — 命令直通车 marker 识别 + 前缀候选过滤。
//
// 设计:
//   - `$CC(`  命令直通车: 仅精确编码匹配触发, 不应在前缀候选中出现
//   - `$CC1(` 命令直通车: 同时参与前缀展开, 显式 prefix-visible
//   - `$AA(`  字符组: 走 nav 路径, 允许前缀
//   - 含已知模板变量 ($Y/$M/$D/$H/$WC/$YC/$uuid/$ts/...) 的动态短语:
//     仅精确编码匹配触发, 不污染前缀候选 (date / time / now 等)
//
// 把"是否前缀展开"编码进 marker / 模板本身, 避免引入额外字段/配置。
// 各 DictLayer 的 SearchPrefix 在返回前调 filterExactOnly 收尾,
// 同一 helper 让"前缀过滤"语义在所有层完全一致。

package dict

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// HasCmdbarMarker 检测 value 是否带 cmdbar 命令 marker ($CC( 或 $CC1()。
// 上层 ValueExpander.Expand 用本函数决定是否走 hook。
//
// 例外: 顶层是 $SS marker 的 value (字符串数组短语) 内部允许嵌入 $CC 元素,
// 子串扫描会把 $SS("...", $CC(...)) 整段也识别为命令短语, 让 cmdbarPhraseHook
// 拿到整段 $SS marker 后 Parse 出 ArrayPhrase, Evaluate 又拒绝处理 ArrayPhrase
// → 错误候选 / 噪音日志。$SS 应统一走 ArrayHook, 这里在源头排除。
func HasCmdbarMarker(value string) bool {
	if HasSSMarker(value) {
		return false
	}
	return strings.Contains(value, "$CC(") || strings.Contains(value, "$CC1(")
}

// templateVarNames 列出"会在精确匹配场景动态求值, 不应出现在前缀候选"的
// 模板变量名 (与 TemplateEngine 内置变量保持一致)。当 value 含其中之一作为
// $XXX 整体出现, 视为模板动态短语, 仅精确匹配触发。
//
// 注意: 不包含 CC / CC1 / AA — 那三个由 marker 形式 ($CC(/$CC1(/$AA() 单独识别,
// 不属于模板变量语义。
var templateVarNames = []string{
	// 日期 / 时间 (长名优先, 避免 $YY 误匹配 $Y)
	"YYYY", "YY", "Y", "y",
	"MM", "M",
	"DD", "D",
	"HH", "H", "h",
	"mm", "mi", "m",
	"ss", "s",
	// 星期
	"WC", "W", "w",
	// 中文
	"YC", "MC", "DC",
	// 特殊
	"uuid", "tsms", "ts",
}

// hasTemplateVar 检测 value 是否含 templateVarNames 中任一变量名, 形式为 $X
// (或 ${X})。遍历 value 中每个 '$' 后跟的 token, 排除 $CC(/$CC1(/$AA(
// marker 形式 (它们以 '(' 紧随其后, 不视为模板变量)。
func hasTemplateVar(value string) bool {
	if !strings.Contains(value, "$") {
		return false
	}
	n := len(value)
	for i := 0; i < n; i++ {
		if value[i] != '$' {
			continue
		}
		// 跳过 $$ 转义
		if i+1 < n && value[i+1] == '$' {
			i++
			continue
		}
		// ${var} 语法
		if i+1 < n && value[i+1] == '{' {
			end := strings.IndexByte(value[i+2:], '}')
			if end < 0 {
				continue
			}
			name := value[i+2 : i+2+end]
			if isTemplateVarName(name) {
				return true
			}
			i += end + 2
			continue
		}
		// $name 语法 — 贪心匹配, 长名优先
		rest := value[i+1:]
		for _, name := range templateVarNames {
			if !strings.HasPrefix(rest, name) {
				continue
			}
			// 排除 $CC(/$CC1(/$AA( marker (虽然 templateVarNames 已去除 CC/CC1/AA,
			// 仍兜底校验: 名字后紧跟 '(' 视为 marker 调用而非模板变量)。
			tail := rest[len(name):]
			if len(tail) > 0 && tail[0] == '(' {
				continue
			}
			return true
		}
	}
	return false
}

func isTemplateVarName(name string) bool {
	for _, v := range templateVarNames {
		if v == name {
			return true
		}
	}
	return false
}

// IsExactOnly 判断 value 是否为"仅精确编码匹配时出现"的候选, 前缀搜索阶段应
// 过滤掉。包括:
//   - 命令直通车 $CC(...) (不含 $CC1(...))
//   - 模板类动态短语 (含 $Y / $M / $WC / $uuid / $ts 等已知变量, 但不含
//     $CC1(/$AA(/$CC( 这三种 marker)
//
// 检测顺序:
//  1. 含 $CC1( → false (显式 prefix-visible)
//  2. 含 $AA(  → false (字符组, 走 nav 前缀路径)
//  3. 含 $CC(  → true  (命令直通车精确匹配)
//  4. 含已知模板变量 → true (动态模板, 仅精确匹配)
//  5. 其它 → false (字面量短语, 允许前缀)
func IsExactOnly(value string) bool {
	if strings.Contains(value, "$CC1(") {
		return false
	}
	if strings.Contains(value, "$AA(") {
		return false
	}
	if strings.Contains(value, "$CC(") {
		return true
	}
	return hasTemplateVar(value)
}

// IsCmdbarExactOnly 旧名保留, 转调 IsExactOnly, 维持二进制 / 文档兼容。
// 现在语义已扩展到模板变量, 不仅是 cmdbar marker。
func IsCmdbarExactOnly(value string) bool {
	return IsExactOnly(value)
}

// cmdbarSource 取候选的"原始 value":
//   - 优先用 PhraseTemplate (PhraseLayer 出口候选已展开, Text 是 display,
//     marker 信息只在原模板里保留)
//   - 否则用 Text (码表/用户词库等候选的 Text 即原始 value, 尚未展开)
func cmdbarSource(c candidate.Candidate) string {
	if c.PhraseTemplate != "" {
		return c.PhraseTemplate
	}
	return c.Text
}

// candidateIsExactOnly 判断单个候选是否仅精确匹配 (在前缀搜索阶段应过滤掉)。
//
// 2026-05-16 优先级:
//  1. candidate.Modifiers["prefix"] 存在 → 直接用 (新 options bag 路径,
//     由 cmdbar parser 在解析期填充, 含 marker syntax sugar 默认值合并;
//     prefix=true 表示候选愿意在前缀阶段出现, 即 *非* exact-only)
//  2. 否则回退到 IsExactOnly(cmdbarSource(c)) 字符串扫描
//     (用户词库/系统词库等未经过 cmdbar hook 的候选走此路径)
//
// 这套优先级让"显式 modifier" 永远胜过"marker 字符串推断", 保持新旧路径兼容。
func candidateIsExactOnly(c candidate.Candidate) bool {
	if c.Modifiers != nil {
		if v, ok := c.Modifiers["prefix"]; ok {
			if prefix, isBool := v.(bool); isBool {
				return !prefix
			}
		}
	}
	return IsExactOnly(cmdbarSource(c))
}

// filterCmdbarExactOnly 原地过滤掉 exact-only 命中的候选,
// 用 cs[:0] 共享底层数组节省分配。空切片直接返回。
//
// 函数名保留 (历史调用点多), 内部走 candidateIsExactOnly 综合判定:
// 新候选 (PhraseLayer cmdbar hook 路径) 看 Modifiers, 老候选 (用户/系统词库)
// 看 marker 字符串。
func filterCmdbarExactOnly(cs []candidate.Candidate) []candidate.Candidate {
	if len(cs) == 0 {
		return cs
	}
	out := cs[:0]
	for _, c := range cs {
		if !candidateIsExactOnly(c) {
			out = append(out, c)
		}
	}
	return out
}
