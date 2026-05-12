package dictio

import (
	"bufio"
	"fmt"
	"io"
	"strconv"
	"strings"
)

// TSVImporter 解析纯文本 TSV 格式（code\ttext\tweight）。
// 仅支持 3 列：编码 / 词条 / 权重。
// 时间戳与使用次数等元数据不在 TSV 范围内（如需完整元数据请用 WindDict 格式）。
type TSVImporter struct{}

func (p *TSVImporter) Name() string         { return "纯文本TSV" }
func (p *TSVImporter) Extensions() []string { return []string{".txt"} }

// Import 从 reader 中解析 TSV 格式数据，输出为 UserWords。
// 仅解析前 3 列（code / text / weight），额外的列被忽略。
func (p *TSVImporter) Import(r io.Reader, opts ImportOptions) (*ImportResult, error) {
	result := &ImportResult{}
	lineNum := 0

	scanner := bufio.NewScanner(r)
	for scanner.Scan() {
		lineNum++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			result.Warnings = append(result.Warnings, fmt.Sprintf("第 %d 行: 列数不足，已跳过", lineNum))
			result.Stats.SkippedCount++
			continue
		}

		code := strings.TrimSpace(parts[0])
		text := strings.TrimSpace(parts[1])

		// 合法性校验：编码和文本都不能为空
		if code == "" || text == "" {
			result.Warnings = append(result.Warnings, fmt.Sprintf("第 %d 行: 编码或文本为空，已跳过", lineNum))
			result.Stats.SkippedCount++
			continue
		}

		// 编码应为可打印 ASCII（字母/数字/符号），检测到非 ASCII 高概率是格式不匹配
		if !isValidCode(code) {
			result.Warnings = append(result.Warnings, fmt.Sprintf("第 %d 行: 编码含无效字符 %q，已跳过", lineNum, code))
			result.Stats.SkippedCount++
			continue
		}

		entry := UserWordEntry{
			Code: code,
			Text: text,
		}

		if len(parts) >= 3 {
			if w, err := strconv.Atoi(parts[2]); err == nil {
				entry.Weight = w
			}
		}

		result.UserWords = append(result.UserWords, entry)
	}

	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("读取 TSV 文件失败: %w", err)
	}

	result.UpdateStats()
	return result, nil
}

// isValidCode 检查编码是否合法（仅含可打印 ASCII 字符）。
// 用于过滤非输入法编码的乱码行。
func isValidCode(code string) bool {
	for i := 0; i < len(code); i++ {
		c := code[i]
		if c < 0x20 || c > 0x7E {
			return false
		}
	}
	return true
}
