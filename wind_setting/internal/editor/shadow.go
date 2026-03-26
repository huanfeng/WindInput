package editor

import (
	"strings"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/dictfile"
)

// ShadowEditor Shadow 规则编辑器
type ShadowEditor struct {
	*BaseEditor
	data *dictfile.ShadowConfig
}

// NewShadowEditor 创建 Shadow 编辑器（根据当前活跃方案加载对应文件）
func NewShadowEditor() (*ShadowEditor, error) {
	cfg, err := config.Load()
	if err != nil {
		cfg = config.DefaultConfig()
	}
	return NewShadowEditorForSchema(cfg.Schema.Active)
}

// NewShadowEditorForSchema 根据方案 ID 创建 Shadow 编辑器
func NewShadowEditorForSchema(schemaID string) (*ShadowEditor, error) {
	configDir, err := config.GetConfigDir()
	if err != nil {
		return nil, err
	}

	// 按方案 ID 确定 shadow 文件名: {schemaID}.shadow.yaml
	shadowFile := schemaID + ".shadow.yaml"
	path := configDir + "/" + shadowFile

	return &ShadowEditor{
		BaseEditor: NewBaseEditor(path),
	}, nil
}

// NewShadowEditorWithPath 使用指定文件路径创建 Shadow 编辑器
func NewShadowEditorWithPath(filePath string) *ShadowEditor {
	return &ShadowEditor{
		BaseEditor: NewBaseEditor(filePath),
	}
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

// PinWord 固定词到指定位置
func (e *ShadowEditor) PinWord(code, word string, position int) {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		e.data = &dictfile.ShadowConfig{Rules: make(map[string]*dictfile.ShadowCodeConfig)}
	}

	dictfile.PinWord(e.data, code, word, position)
	e.dirty = true
}

// DeleteWord 删除（隐藏）词条
func (e *ShadowEditor) DeleteWord(code, word string) {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		e.data = &dictfile.ShadowConfig{Rules: make(map[string]*dictfile.ShadowCodeConfig)}
	}

	dictfile.DeleteWord(e.data, code, word)
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

// GetRulesByCode 获取指定编码的规则
func (e *ShadowEditor) GetRulesByCode(code string) *dictfile.ShadowCodeConfig {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return nil
	}

	if e.data == nil || e.data.Rules == nil {
		return nil
	}
	return e.data.Rules[strings.ToLower(code)]
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
