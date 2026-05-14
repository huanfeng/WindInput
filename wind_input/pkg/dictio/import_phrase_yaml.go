package dictio

import (
	"fmt"
	"io"

	"gopkg.in/yaml.v3"
)

// PhraseYAMLImporter 解析短语 YAML 格式 (新版, 单一格式)。
//
// 字段:
//   - code:     编码 (必填)
//   - text:     文本内容; 字符组用 $AA("name", "chars") marker, 动态短语
//     可含 $X / $CC1 等模板, 普通静态短语为纯字面量。
//   - weight:   显式权重 (0~10000, 可选; 0 表示走 position fallback)
//   - position: 位置 (兼容字段, 可选, 默认 1)
//   - disabled: 是否禁用 (可选, 默认 false)
type PhraseYAMLImporter struct{}

func (p *PhraseYAMLImporter) Name() string         { return "短语YAML" }
func (p *PhraseYAMLImporter) Extensions() []string { return []string{".yaml", ".yml"} }

// phraseYAMLFile 短语 YAML 文件结构。
type phraseYAMLFile struct {
	Phrases []phraseYAMLEntry `yaml:"phrases"`
}

type phraseYAMLEntry struct {
	Code     string `yaml:"code"`
	Text     string `yaml:"text,omitempty"`
	Weight   int    `yaml:"weight,omitempty"`
	Position int    `yaml:"position,omitempty"`
	Disabled bool   `yaml:"disabled,omitempty"`
}

// Import 从 reader 中解析短语 YAML 格式。
func (p *PhraseYAMLImporter) Import(r io.Reader, opts ImportOptions) (*ImportResult, error) {
	data, err := io.ReadAll(r)
	if err != nil {
		return nil, fmt.Errorf("读取数据失败: %w", err)
	}

	// 如果是 WindDict 格式，拒绝处理
	if IsWindDictFile(data) {
		return nil, fmt.Errorf("此文件是 WindDict 格式，请使用 WindDict 导入器")
	}

	var file phraseYAMLFile
	if err := yaml.Unmarshal(data, &file); err != nil {
		return nil, fmt.Errorf("解析 YAML 失败: %w", err)
	}

	result := &ImportResult{}

	for i, e := range file.Phrases {
		if e.Code == "" || e.Text == "" {
			result.Warnings = append(result.Warnings, fmt.Sprintf("第 %d 条: 缺少编码或文本，已跳过", i+1))
			result.Stats.SkippedCount++
			continue
		}

		pos := e.Position
		if pos <= 0 {
			pos = 1
		}

		result.Phrases = append(result.Phrases, PhraseEntry{
			Code:     e.Code,
			Text:     e.Text,
			Weight:   e.Weight,
			Position: pos,
			Enabled:  !e.Disabled,
		})
	}

	result.UpdateStats()
	return result, nil
}
