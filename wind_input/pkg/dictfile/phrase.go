package dictfile

import (
	"fmt"
	"os"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/fileutil"
	"gopkg.in/yaml.v3"
)

// LoadPhrasesFrom 从指定路径加载短语配置
func LoadPhrasesFrom(path string) (*PhrasesConfig, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &PhrasesConfig{Phrases: []PhraseConfig{}}, nil
		}
		return nil, fmt.Errorf("failed to read phrases file: %w", err)
	}

	var cfg PhrasesConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("failed to parse phrases file: %w", err)
	}

	return &cfg, nil
}

// SavePhrasesTo 保存短语配置到指定路径
func SavePhrasesTo(cfg *PhrasesConfig, path string) error {
	if err := config.EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to ensure config dir: %w", err)
	}

	data, err := yaml.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("failed to marshal phrases config: %w", err)
	}

	return fileutil.AtomicWrite(path, data, 0644)
}

// AddPhrase 添加短语到配置
// 返回 true 表示新增，false 表示更新已有项
func AddPhrase(cfg *PhrasesConfig, code, text string, position int) bool {
	// 检查是否已存在
	for i, p := range cfg.Phrases {
		if p.Code == code && p.Text == text {
			cfg.Phrases[i].Position = position
			return false
		}
	}

	// 添加新短语
	cfg.Phrases = append(cfg.Phrases, PhraseConfig{
		Code:     code,
		Text:     text,
		Position: position,
	})
	return true
}

// RemovePhrase 从配置中删除短语
// 返回 true 表示删除成功
func RemovePhrase(cfg *PhrasesConfig, code, text string) bool {
	for i, p := range cfg.Phrases {
		if p.Code == code && p.Text == text {
			cfg.Phrases = append(cfg.Phrases[:i], cfg.Phrases[i+1:]...)
			return true
		}
	}
	return false
}

// GetPhrasesByCode 获取指定编码的所有短语
func GetPhrasesByCode(cfg *PhrasesConfig, code string) []PhraseConfig {
	var result []PhraseConfig
	for _, p := range cfg.Phrases {
		if p.Code == code {
			result = append(result, p)
		}
	}
	return result
}
