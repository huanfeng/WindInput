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
		return &ShadowConfig{Rules: make(map[string]*ShadowCodeConfig)}, err
	}
	return LoadShadowFrom(path)
}

// LoadShadowFrom 从指定路径加载 Shadow 配置
func LoadShadowFrom(path string) (*ShadowConfig, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &ShadowConfig{Rules: make(map[string]*ShadowCodeConfig)}, nil
		}
		return nil, fmt.Errorf("failed to read shadow file: %w", err)
	}

	var cfg ShadowConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("failed to parse shadow file: %w", err)
	}

	if cfg.Rules == nil {
		cfg.Rules = make(map[string]*ShadowCodeConfig)
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

// PinWord 固定词到指定位置（置顶 = position 0）
func PinWord(cfg *ShadowConfig, code, word string, position int) {
	code = strings.ToLower(code)
	cc := getOrCreateCode(cfg, code)

	// 从 deleted 移除
	cc.Deleted = removeStr(cc.Deleted, word)

	// 从 pinned 移除旧记录
	for i, p := range cc.Pinned {
		if p.Word == word {
			cc.Pinned = append(cc.Pinned[:i], cc.Pinned[i+1:]...)
			break
		}
	}

	// 插入到头部（LIFO）
	cc.Pinned = append([]ShadowPinConfig{{Word: word, Position: position}}, cc.Pinned...)
}

// DeleteWord 隐藏词条
func DeleteWord(cfg *ShadowConfig, code, word string) {
	code = strings.ToLower(code)
	cc := getOrCreateCode(cfg, code)

	// 从 pinned 移除
	for i, p := range cc.Pinned {
		if p.Word == word {
			cc.Pinned = append(cc.Pinned[:i], cc.Pinned[i+1:]...)
			break
		}
	}

	// 去重添加到 deleted
	for _, d := range cc.Deleted {
		if d == word {
			return
		}
	}
	cc.Deleted = append(cc.Deleted, word)
}

// RemoveShadowRule 移除词的所有规则
func RemoveShadowRule(cfg *ShadowConfig, code, word string) bool {
	code = strings.ToLower(code)
	cc, ok := cfg.Rules[code]
	if !ok {
		return false
	}

	changed := false
	for i, p := range cc.Pinned {
		if p.Word == word {
			cc.Pinned = append(cc.Pinned[:i], cc.Pinned[i+1:]...)
			changed = true
			break
		}
	}
	newDeleted := removeStr(cc.Deleted, word)
	if len(newDeleted) != len(cc.Deleted) {
		cc.Deleted = newDeleted
		changed = true
	}
	if changed && len(cc.Pinned) == 0 && len(cc.Deleted) == 0 {
		delete(cfg.Rules, code)
	}
	return changed
}

// GetRuleCount 获取规则总数
func GetRuleCount(cfg *ShadowConfig) int {
	count := 0
	for _, cc := range cfg.Rules {
		count += len(cc.Pinned) + len(cc.Deleted)
	}
	return count
}

func getOrCreateCode(cfg *ShadowConfig, code string) *ShadowCodeConfig {
	if cfg.Rules == nil {
		cfg.Rules = make(map[string]*ShadowCodeConfig)
	}
	cc, ok := cfg.Rules[code]
	if !ok {
		cc = &ShadowCodeConfig{}
		cfg.Rules[code] = cc
	}
	return cc
}

func removeStr(slice []string, s string) []string {
	for i, v := range slice {
		if v == s {
			return append(slice[:i], slice[i+1:]...)
		}
	}
	return slice
}
