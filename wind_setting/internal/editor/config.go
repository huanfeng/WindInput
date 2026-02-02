package editor

import (
	"github.com/huanfeng/wind_input/pkg/config"
)

// ConfigEditor 配置编辑器
type ConfigEditor struct {
	*BaseEditor
	data *config.Config
}

// NewConfigEditor 创建配置编辑器
func NewConfigEditor() (*ConfigEditor, error) {
	path, err := config.GetConfigPath()
	if err != nil {
		return nil, err
	}

	return &ConfigEditor{
		BaseEditor: NewBaseEditor(path),
	}, nil
}

// Load 加载配置
func (e *ConfigEditor) Load() error {
	cfg, err := config.Load()
	if err != nil {
		return err
	}

	e.mu.Lock()
	e.data = cfg
	e.dirty = false
	e.mu.Unlock()

	return e.UpdateFileState()
}

// Save 保存配置
func (e *ConfigEditor) Save() error {
	e.mu.RLock()
	cfg := e.data
	e.mu.RUnlock()

	if cfg == nil {
		return nil
	}

	if err := config.Save(cfg); err != nil {
		return err
	}

	e.ClearDirty()
	return e.UpdateFileState()
}

// Reload 重新加载（丢弃本地修改）
func (e *ConfigEditor) Reload() error {
	return e.Load()
}

// GetConfig 获取配置数据
func (e *ConfigEditor) GetConfig() *config.Config {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.data
}

// SetConfig 设置配置数据
func (e *ConfigEditor) SetConfig(cfg *config.Config) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.data = cfg
	e.dirty = true
}

// UpdateConfig 更新部分配置
func (e *ConfigEditor) UpdateConfig(updater func(*config.Config)) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.data != nil {
		updater(e.data)
		e.dirty = true
	}
}
