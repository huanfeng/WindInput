package editor

import (
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/dictfile"
)

// ShadowEditor Shadow 规则编辑器
type ShadowEditor struct {
	*BaseEditor
	data *dictfile.ShadowConfig
}

// NewShadowEditor 创建 Shadow 编辑器
func NewShadowEditor() (*ShadowEditor, error) {
	path, err := config.GetShadowPath()
	if err != nil {
		return nil, err
	}

	return &ShadowEditor{
		BaseEditor: NewBaseEditor(path),
	}, nil
}

// Load 加载 Shadow 配置
func (e *ShadowEditor) Load() error {
	cfg, err := dictfile.LoadShadowFrom(e.filePath)
	if err != nil {
		return err
	}

	e.mu.Lock()
	e.data = cfg
	e.dirty = false
	e.mu.Unlock()

	return e.UpdateFileState()
}

// Save 保存 Shadow 配置
func (e *ShadowEditor) Save() error {
	e.mu.RLock()
	cfg := e.data
	e.mu.RUnlock()

	if cfg == nil {
		return nil
	}

	if err := dictfile.SaveShadowTo(cfg, e.filePath); err != nil {
		return err
	}

	e.ClearDirty()
	return e.UpdateFileState()
}

// Reload 重新加载
func (e *ShadowEditor) Reload() error {
	return e.Load()
}

// GetShadowConfig 获取 Shadow 配置
func (e *ShadowEditor) GetShadowConfig() *dictfile.ShadowConfig {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.data
}

// AddRule 添加规则
func (e *ShadowEditor) AddRule(code, word, action string, weight int) {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		e.data = &dictfile.ShadowConfig{Rules: make(map[string][]dictfile.ShadowRuleConfig)}
	}

	dictfile.AddShadowRule(e.data, code, word, action, weight)
	e.dirty = true
}

// RemoveRule 删除规则
func (e *ShadowEditor) RemoveRule(code, word string) bool {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		return false
	}

	removed := dictfile.RemoveShadowRule(e.data, code, word)
	if removed {
		e.dirty = true
	}
	return removed
}

// TopWord 置顶词条
func (e *ShadowEditor) TopWord(code, word string) {
	e.AddRule(code, word, "top", 0)
}

// DeleteWord 删除（隐藏）词条
func (e *ShadowEditor) DeleteWord(code, word string) {
	e.AddRule(code, word, "delete", 0)
}

// ReweightWord 调整权重
func (e *ShadowEditor) ReweightWord(code, word string, weight int) {
	e.AddRule(code, word, "reweight", weight)
}

// GetRulesByCode 获取指定编码的规则
func (e *ShadowEditor) GetRulesByCode(code string) []dictfile.ShadowRuleConfig {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return nil
	}

	return dictfile.GetShadowRules(e.data, code)
}

// GetRuleCount 获取规则数量
func (e *ShadowEditor) GetRuleCount() int {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return 0
	}

	return dictfile.GetRuleCount(e.data)
}
