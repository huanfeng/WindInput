package tooltip

import "strings"

// FormatContent 将基础 comment 和各 Section 组合成最终显示文本
// 单行 Section 格式为 "标签: 内容"，多行 Section 以 "[标签]" 为标题逐行展开
func FormatContent(comment string, sections []Section) string {
	var parts []string
	if comment != "" {
		parts = append(parts, comment)
	}
	for _, sec := range sections {
		if len(sec.Lines) == 0 {
			continue
		}
		if len(sec.Lines) == 1 && !sec.AlwaysExpand {
			line := sec.Lines[0]
			if sec.Label != "" {
				line = sec.Label + ": " + line
			}
			parts = append(parts, line)
		} else {
			// 多行或强制展开：标签作为标题行，内容逐行展开
			if sec.Label != "" {
				parts = append(parts, "["+sec.Label+"]")
			}
			parts = append(parts, sec.Lines...)
		}
	}
	return strings.Join(parts, "\n")
}
