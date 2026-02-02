package dictfile

import (
	"fmt"
	"os"
	"strings"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/fileutil"
	"gopkg.in/yaml.v3"
)

// LoadShadow 从默认路径加载 Shadow 配置
func LoadShadow() (*ShadowConfig, error) {
	path, err := config.GetShadowPath()
	if err != nil {
		return &ShadowConfig{Rules: make(map[string][]ShadowRuleConfig)}, err
	}
	return LoadShadowFrom(path)
}

// LoadShadowFrom 从指定路径加载 Shadow 配置
func LoadShadowFrom(path string) (*ShadowConfig, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &ShadowConfig{Rules: make(map[string][]ShadowRuleConfig)}, nil
		}
		return nil, fmt.Errorf("failed to read shadow file: %w", err)
	}

	var cfg ShadowConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("failed to parse shadow file: %w", err)
	}

	if cfg.Rules == nil {
		cfg.Rules = make(map[string][]ShadowRuleConfig)
	}

	return &cfg, nil
}

// SaveShadow 保存 Shadow 配置到默认路径
func SaveShadow(cfg *ShadowConfig) error {
	path, err := config.GetShadowPath()
	if err != nil {
		return err
	}
	return SaveShadowTo(cfg, path)
}

// SaveShadowTo 保存 Shadow 配置到指定路径
func SaveShadowTo(cfg *ShadowConfig, path string) error {
	if err := config.EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to ensure config dir: %w", err)
	}

	data, err := yaml.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("failed to marshal shadow config: %w", err)
	}

	return fileutil.AtomicWrite(path, data, 0644)
}

// AddShadowRule 添加 Shadow 规则
// action: "top", "delete", "reweight"
func AddShadowRule(cfg *ShadowConfig, code, word, action string, weight int) {
	code = strings.ToLower(code)

	// 检查是否已存在
	rules := cfg.Rules[code]
	for i, r := range rules {
		if r.Word == word {
			// 更新已有规则
			cfg.Rules[code][i].Action = action
			cfg.Rules[code][i].Weight = weight
			return
		}
	}

	// 添加新规则
	cfg.Rules[code] = append(cfg.Rules[code], ShadowRuleConfig{
		Word:   word,
		Action: action,
		Weight: weight,
	})
}

// RemoveShadowRule 删除 Shadow 规则
func RemoveShadowRule(cfg *ShadowConfig, code, word string) bool {
	code = strings.ToLower(code)
	rules, ok := cfg.Rules[code]
	if !ok {
		return false
	}

	for i, r := range rules {
		if r.Word == word {
			cfg.Rules[code] = append(rules[:i], rules[i+1:]...)
			if len(cfg.Rules[code]) == 0 {
				delete(cfg.Rules, code)
			}
			return true
		}
	}
	return false
}

// GetShadowRules 获取指定编码的所有规则
func GetShadowRules(cfg *ShadowConfig, code string) []ShadowRuleConfig {
	code = strings.ToLower(code)
	return cfg.Rules[code]
}

// TopWord 置顶词条
func TopWord(cfg *ShadowConfig, code, word string) {
	AddShadowRule(cfg, code, word, string(ShadowActionTop), 0)
}

// DeleteWord 删除（隐藏）词条
func DeleteWord(cfg *ShadowConfig, code, word string) {
	AddShadowRule(cfg, code, word, string(ShadowActionDelete), 0)
}

// ReweightWord 调整词条权重
func ReweightWord(cfg *ShadowConfig, code, word string, weight int) {
	AddShadowRule(cfg, code, word, string(ShadowActionReweight), weight)
}

// GetRuleCount 获取规则总数
func GetRuleCount(cfg *ShadowConfig) int {
	count := 0
	for _, rules := range cfg.Rules {
		count += len(rules)
	}
	return count
}
