package dict

import (
	"fmt"
	"os"
	"strings"
	"sync"

	"gopkg.in/yaml.v3"
)

// ShadowLayer 用户修正层
// 记录用户对系统词的置顶、删除、调序操作
// 实现 ShadowProvider 接口
type ShadowLayer struct {
	mu       sync.RWMutex
	name     string
	filePath string

	// 规则存储: code -> []ShadowRule
	rules map[string][]ShadowRule

	// 脏标记
	dirty bool
}

// ShadowConfig shadow.yaml 配置结构
type ShadowConfig struct {
	Rules map[string][]ShadowRuleConfig `yaml:"rules"`
}

// ShadowRuleConfig 单个规则配置
type ShadowRuleConfig struct {
	Word   string `yaml:"word"`
	Action string `yaml:"action"` // "top", "delete", "reweight"
	Weight int    `yaml:"weight"` // 仅 reweight 时有效
}

// NewShadowLayer 创建 Shadow 层
func NewShadowLayer(name string, filePath string) *ShadowLayer {
	return &ShadowLayer{
		name:     name,
		filePath: filePath,
		rules:    make(map[string][]ShadowRule),
	}
}

// Name 返回层名称
func (sl *ShadowLayer) Name() string {
	return sl.name
}

// GetShadowRules 获取指定编码的 Shadow 规则（实现 ShadowProvider 接口）
func (sl *ShadowLayer) GetShadowRules(code string) []ShadowRule {
	sl.mu.RLock()
	defer sl.mu.RUnlock()

	code = strings.ToLower(code)
	return sl.rules[code]
}

// Load 从 YAML 文件加载
func (sl *ShadowLayer) Load() error {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	data, err := os.ReadFile(sl.filePath)
	if err != nil {
		if os.IsNotExist(err) {
			// 文件不存在是正常的
			return nil
		}
		return fmt.Errorf("failed to read shadow file: %w", err)
	}

	var config ShadowConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return fmt.Errorf("failed to parse shadow file: %w", err)
	}

	// 清空现有规则
	sl.rules = make(map[string][]ShadowRule)

	// 加载规则
	for code, ruleConfigs := range config.Rules {
		code = strings.ToLower(code)
		for _, rc := range ruleConfigs {
			rule := ShadowRule{
				Code:      code,
				Word:      rc.Word,
				Action:    ShadowAction(rc.Action),
				NewWeight: rc.Weight,
			}
			sl.rules[code] = append(sl.rules[code], rule)
		}
	}

	return nil
}

// Save 保存到 YAML 文件
func (sl *ShadowLayer) Save() error {
	sl.mu.RLock()
	defer sl.mu.RUnlock()

	if !sl.dirty {
		return nil
	}

	config := ShadowConfig{
		Rules: make(map[string][]ShadowRuleConfig),
	}

	for code, rules := range sl.rules {
		var ruleConfigs []ShadowRuleConfig
		for _, r := range rules {
			ruleConfigs = append(ruleConfigs, ShadowRuleConfig{
				Word:   r.Word,
				Action: string(r.Action),
				Weight: r.NewWeight,
			})
		}
		config.Rules[code] = ruleConfigs
	}

	data, err := yaml.Marshal(&config)
	if err != nil {
		return fmt.Errorf("failed to marshal shadow config: %w", err)
	}

	// 写入临时文件
	tmpPath := sl.filePath + ".tmp"
	if err := os.WriteFile(tmpPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write shadow file: %w", err)
	}

	// 原子替换
	if err := os.Rename(tmpPath, sl.filePath); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to rename shadow file: %w", err)
	}

	sl.dirty = false
	return nil
}

// Top 置顶词条
func (sl *ShadowLayer) Top(code string, word string) {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	code = strings.ToLower(code)

	// 检查是否已存在
	for i, r := range sl.rules[code] {
		if r.Word == word {
			sl.rules[code][i].Action = ShadowActionTop
			sl.dirty = true
			return
		}
	}

	// 添加新规则
	sl.rules[code] = append(sl.rules[code], ShadowRule{
		Code:   code,
		Word:   word,
		Action: ShadowActionTop,
	})
	sl.dirty = true
}

// Delete 删除（隐藏）词条
func (sl *ShadowLayer) Delete(code string, word string) {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	code = strings.ToLower(code)

	// 检查是否已存在
	for i, r := range sl.rules[code] {
		if r.Word == word {
			sl.rules[code][i].Action = ShadowActionDelete
			sl.dirty = true
			return
		}
	}

	// 添加新规则
	sl.rules[code] = append(sl.rules[code], ShadowRule{
		Code:   code,
		Word:   word,
		Action: ShadowActionDelete,
	})
	sl.dirty = true
}

// Reweight 调整权重
func (sl *ShadowLayer) Reweight(code string, word string, newWeight int) {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	code = strings.ToLower(code)

	// 检查是否已存在
	for i, r := range sl.rules[code] {
		if r.Word == word {
			sl.rules[code][i].Action = ShadowActionReweight
			sl.rules[code][i].NewWeight = newWeight
			sl.dirty = true
			return
		}
	}

	// 添加新规则
	sl.rules[code] = append(sl.rules[code], ShadowRule{
		Code:      code,
		Word:      word,
		Action:    ShadowActionReweight,
		NewWeight: newWeight,
	})
	sl.dirty = true
}

// RemoveRule 移除规则（恢复默认行为）
func (sl *ShadowLayer) RemoveRule(code string, word string) {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	code = strings.ToLower(code)
	rules, ok := sl.rules[code]
	if !ok {
		return
	}

	for i, r := range rules {
		if r.Word == word {
			sl.rules[code] = append(rules[:i], rules[i+1:]...)
			if len(sl.rules[code]) == 0 {
				delete(sl.rules, code)
			}
			sl.dirty = true
			return
		}
	}
}

// IsDeleted 检查词条是否被删除
func (sl *ShadowLayer) IsDeleted(code string, word string) bool {
	sl.mu.RLock()
	defer sl.mu.RUnlock()

	code = strings.ToLower(code)
	for _, r := range sl.rules[code] {
		if r.Word == word && r.Action == ShadowActionDelete {
			return true
		}
	}
	return false
}

// IsTopped 检查词条是否被置顶
func (sl *ShadowLayer) IsTopped(code string, word string) bool {
	sl.mu.RLock()
	defer sl.mu.RUnlock()

	code = strings.ToLower(code)
	for _, r := range sl.rules[code] {
		if r.Word == word && r.Action == ShadowActionTop {
			return true
		}
	}
	return false
}

// HasRule 检查指定编码和词语是否存在 Shadow 规则
func (sl *ShadowLayer) HasRule(code string, word string) bool {
	sl.mu.RLock()
	defer sl.mu.RUnlock()

	code = strings.ToLower(code)
	for _, r := range sl.rules[code] {
		if r.Word == word {
			return true
		}
	}
	return false
}

// GetRuleCount 获取规则数量
func (sl *ShadowLayer) GetRuleCount() int {
	sl.mu.RLock()
	defer sl.mu.RUnlock()

	count := 0
	for _, rules := range sl.rules {
		count += len(rules)
	}
	return count
}

// IsDirty 检查是否有未保存的修改
func (sl *ShadowLayer) IsDirty() bool {
	sl.mu.RLock()
	defer sl.mu.RUnlock()
	return sl.dirty
}
