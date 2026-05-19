package tooltip

import "strings"

// MergeChaiziPinyin 当 sections 中同时包含 "拆字" 和 "拼音" 两个 section 时，
// 按字（每行首个 rune）合并为一个 "拆字 / 拼音" section，单行格式为
//
//	"<拆字行>\t<拼音音节>"
//
// tooltip 渲染层识别 '\t' 后做列对齐。无配对项的字保持单列。
// 若两者缺其一则原样返回，不做任何处理。
func MergeChaiziPinyin(sections []Section) []Section {
	var chaiziIdx, pinyinIdx = -1, -1
	for i, s := range sections {
		switch s.Label {
		case "拆字":
			chaiziIdx = i
		case "拼音":
			pinyinIdx = i
		}
	}
	if chaiziIdx < 0 || pinyinIdx < 0 {
		return sections
	}

	chaizi := sections[chaiziIdx]
	pinyin := sections[pinyinIdx]

	// 建 pinyin map: rune → 读音（剥离 "字：" 前缀，避免在合并行中重复出现汉字）。
	// 拼音行格式由 provider_pinyin.go 生成: "<字>：<readings>"，分隔符是全角冒号。
	const pinSep = "："
	pinMap := make(map[rune]string, len(pinyin.Lines))
	pinFullMap := make(map[rune]string, len(pinyin.Lines)) // 拼音独有字符回退时仍用完整行
	pinOrder := make([]rune, 0, len(pinyin.Lines))
	for _, line := range pinyin.Lines {
		runes := []rune(line)
		if len(runes) == 0 {
			continue
		}
		head := runes[0]
		pinFullMap[head] = line
		if _, rest, ok := strings.Cut(line, pinSep); ok {
			pinMap[head] = rest
		} else {
			pinMap[head] = line
		}
		pinOrder = append(pinOrder, head)
	}

	used := make(map[rune]bool, len(pinMap))
	var merged []string
	for _, cz := range chaizi.Lines {
		runes := []rune(cz)
		if len(runes) == 0 {
			merged = append(merged, cz)
			continue
		}
		head := runes[0]
		if reading, ok := pinMap[head]; ok {
			used[head] = true
			merged = append(merged, cz+"\t"+reading)
		} else {
			merged = append(merged, cz)
		}
	}
	// 拼音独有的字符（拆字库未收录）按原顺序补在后面，保留 "字：读音" 完整格式
	for _, r := range pinOrder {
		if !used[r] {
			merged = append(merged, pinFullMap[r])
		}
	}

	combined := Section{
		Label:        "拆字 / 拼音",
		Lines:        merged,
		Copyable:     true,
		AlwaysExpand: true,
	}

	// 用合并 section 替换原 chaizi 位置，删除原 pinyin
	out := make([]Section, 0, len(sections)-1)
	for i, s := range sections {
		if i == chaiziIdx {
			out = append(out, combined)
			continue
		}
		if i == pinyinIdx {
			continue
		}
		out = append(out, s)
	}
	return out
}

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
