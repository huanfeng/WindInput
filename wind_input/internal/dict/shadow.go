package dict

import (
	"fmt"
	"os"
	"strings"
	"sync"

	"gopkg.in/yaml.v3"
)

// ShadowLayer 用户修正层
// 记录用户对候选词的位置固定（pin）和隐藏（delete）操作。
// 实现 ShadowProvider 接口，在引擎最终排序后应用呈现层覆盖。
type ShadowLayer struct {
	mu       sync.RWMutex
	name     string
	filePath string
	rules    map[string]*ShadowRules // code -> rules
	dirty    bool
}

// ── YAML 序列化结构 ──

// shadowConfig shadow.yaml 顶层结构
type shadowConfig struct {
	Rules map[string]*shadowCodeConfig `yaml:"rules"`
}

// shadowCodeConfig 单个编码下的规则配置
type shadowCodeConfig struct {
	Pinned  []shadowPinConfig `yaml:"pinned,omitempty"`
	Deleted []string          `yaml:"deleted,omitempty"`
}

// shadowPinConfig 单个 pin 规则
type shadowPinConfig struct {
	Word     string `yaml:"word"`
	Position int    `yaml:"position"`
}

// ── 构造和基础方法 ──

func NewShadowLayer(name string, filePath string) *ShadowLayer {
	return &ShadowLayer{
		name:     name,
		filePath: filePath,
		rules:    make(map[string]*ShadowRules),
	}
}

func (sl *ShadowLayer) Name() string {
	return sl.name
}

// GetShadowRules 获取指定编码的 Shadow 规则（实现 ShadowProvider 接口）
func (sl *ShadowLayer) GetShadowRules(code string) *ShadowRules {
	sl.mu.RLock()
	defer sl.mu.RUnlock()
	return sl.rules[strings.ToLower(code)]
}

// ── 加载和保存 ──

func (sl *ShadowLayer) Load() error {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	data, err := os.ReadFile(sl.filePath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("failed to read shadow file: %w", err)
	}

	var config shadowConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return fmt.Errorf("failed to parse shadow file: %w", err)
	}

	sl.rules = make(map[string]*ShadowRules)
	for code, cc := range config.Rules {
		code = strings.ToLower(code)
		sr := &ShadowRules{}
		for _, p := range cc.Pinned {
			sr.Pinned = append(sr.Pinned, PinnedWord{Word: p.Word, Position: p.Position})
		}
		sr.Deleted = append(sr.Deleted, cc.Deleted...)
		sl.rules[code] = sr
	}
	return nil
}

func (sl *ShadowLayer) Save() error {
	sl.mu.RLock()
	defer sl.mu.RUnlock()

	if !sl.dirty {
		return nil
	}

	config := shadowConfig{Rules: make(map[string]*shadowCodeConfig)}
	for code, sr := range sl.rules {
		if len(sr.Pinned) == 0 && len(sr.Deleted) == 0 {
			continue
		}
		cc := &shadowCodeConfig{}
		for _, p := range sr.Pinned {
			cc.Pinned = append(cc.Pinned, shadowPinConfig{Word: p.Word, Position: p.Position})
		}
		cc.Deleted = append(cc.Deleted, sr.Deleted...)
		config.Rules[code] = cc
	}

	data, err := yaml.Marshal(&config)
	if err != nil {
		return fmt.Errorf("failed to marshal shadow config: %w", err)
	}

	tmpPath := sl.filePath + ".tmp"
	if err := os.WriteFile(tmpPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write shadow file: %w", err)
	}
	if err := os.Rename(tmpPath, sl.filePath); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to rename shadow file: %w", err)
	}

	sl.dirty = false
	return nil
}

// ── 操作方法 ──

// Pin 将词固定到指定位置。
// 置顶 = Pin(code, word, 0)。前移 = Pin(code, word, 当前位置-1)。
// 如果已存在该词的 pin，更新 position 并移到数组头部（LIFO 后发先至）。
// 如果该词在 deleted 中，自动移除 deleted 状态。
func (sl *ShadowLayer) Pin(code string, word string, position int) {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	code = strings.ToLower(code)
	if position < 0 {
		position = 0
	}

	sr := sl.getOrCreate(code)

	// 从 deleted 中移除（如果存在）
	sr.Deleted = removeString(sr.Deleted, word)

	// 从 pinned 中移除旧记录
	for i, p := range sr.Pinned {
		if p.Word == word {
			sr.Pinned = append(sr.Pinned[:i], sr.Pinned[i+1:]...)
			break
		}
	}

	// 插入到数组头部（LIFO：最后操作的优先级最高）
	sr.Pinned = append([]PinnedWord{{Word: word, Position: position}}, sr.Pinned...)
	sl.dirty = true
}

// Delete 隐藏词条（仅多字词，单字由调用方检查）。
// 如果该词在 pinned 中，自动移除 pin 状态。
func (sl *ShadowLayer) Delete(code string, word string) {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	code = strings.ToLower(code)
	sr := sl.getOrCreate(code)

	// 从 pinned 中移除
	for i, p := range sr.Pinned {
		if p.Word == word {
			sr.Pinned = append(sr.Pinned[:i], sr.Pinned[i+1:]...)
			break
		}
	}

	// 添加到 deleted（去重）
	for _, d := range sr.Deleted {
		if d == word {
			return // 已存在
		}
	}
	sr.Deleted = append(sr.Deleted, word)
	sl.dirty = true
}

// RemoveRule 移除词的所有规则（恢复默认行为）
func (sl *ShadowLayer) RemoveRule(code string, word string) {
	sl.mu.Lock()
	defer sl.mu.Unlock()

	code = strings.ToLower(code)
	sr, ok := sl.rules[code]
	if !ok {
		return
	}

	changed := false
	// 从 pinned 移除
	for i, p := range sr.Pinned {
		if p.Word == word {
			sr.Pinned = append(sr.Pinned[:i], sr.Pinned[i+1:]...)
			changed = true
			break
		}
	}
	// 从 deleted 移除
	newDeleted := removeString(sr.Deleted, word)
	if len(newDeleted) != len(sr.Deleted) {
		sr.Deleted = newDeleted
		changed = true
	}

	if changed {
		// 清理空规则
		if len(sr.Pinned) == 0 && len(sr.Deleted) == 0 {
			delete(sl.rules, code)
		}
		sl.dirty = true
	}
}

// ── 查询方法 ──

func (sl *ShadowLayer) IsPinned(code string, word string) bool {
	sl.mu.RLock()
	defer sl.mu.RUnlock()
	code = strings.ToLower(code)
	if sr, ok := sl.rules[code]; ok {
		for _, p := range sr.Pinned {
			if p.Word == word {
				return true
			}
		}
	}
	return false
}

func (sl *ShadowLayer) IsDeleted(code string, word string) bool {
	sl.mu.RLock()
	defer sl.mu.RUnlock()
	code = strings.ToLower(code)
	if sr, ok := sl.rules[code]; ok {
		for _, d := range sr.Deleted {
			if d == word {
				return true
			}
		}
	}
	return false
}

func (sl *ShadowLayer) HasRule(code string, word string) bool {
	return sl.IsPinned(code, word) || sl.IsDeleted(code, word)
}

func (sl *ShadowLayer) GetRuleCount() int {
	sl.mu.RLock()
	defer sl.mu.RUnlock()
	count := 0
	for _, sr := range sl.rules {
		count += len(sr.Pinned) + len(sr.Deleted)
	}
	return count
}

func (sl *ShadowLayer) IsDirty() bool {
	sl.mu.RLock()
	defer sl.mu.RUnlock()
	return sl.dirty
}

func (sl *ShadowLayer) GetFilePath() string {
	return sl.filePath
}

// ── 内部辅助 ──

func (sl *ShadowLayer) getOrCreate(code string) *ShadowRules {
	sr, ok := sl.rules[code]
	if !ok {
		sr = &ShadowRules{}
		sl.rules[code] = sr
	}
	return sr
}

func removeString(slice []string, s string) []string {
	for i, v := range slice {
		if v == s {
			return append(slice[:i], slice[i+1:]...)
		}
	}
	return slice
}
