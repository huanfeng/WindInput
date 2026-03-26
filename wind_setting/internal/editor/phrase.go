package editor

import (
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/dictfile"
)

// PhraseEditor 短语编辑器
type PhraseEditor struct {
	*BaseEditor
	data *dictfile.PhrasesConfig
}

// NewPhraseEditor 创建短语编辑器（加载用户短语 user.phrases.yaml）
func NewPhraseEditor() (*PhraseEditor, error) {
	path, err := config.GetUserPhrasesPath()
	if err != nil {
		return nil, err
	}

	return &PhraseEditor{
		BaseEditor: NewBaseEditor(path),
	}, nil
}

// NewPhraseEditorWithPath 使用指定文件路径创建短语编辑器
func NewPhraseEditorWithPath(filePath string) *PhraseEditor {
	return &PhraseEditor{
		BaseEditor: NewBaseEditor(filePath),
	}
}

// Load 加载短语配置
func (e *PhraseEditor) Load() error {
	cfg, err := dictfile.LoadPhrasesFrom(e.filePath)
	if err != nil {
		return err
	}

	e.mu.Lock()
	e.data = cfg
	e.dirty = false
	e.mu.Unlock()

	return e.UpdateFileState()
}

// Save 保存短语配置
func (e *PhraseEditor) Save() error {
	e.mu.RLock()
	cfg := e.data
	e.mu.RUnlock()

	if cfg == nil {
		return nil
	}

	if err := dictfile.SavePhrasesTo(cfg, e.filePath); err != nil {
		return err
	}

	e.ClearDirty()
	return e.UpdateFileState()
}

// Reload 重新加载
func (e *PhraseEditor) Reload() error {
	return e.Load()
}

// GetPhrases 获取所有短语
func (e *PhraseEditor) GetPhrases() *dictfile.PhrasesConfig {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.data
}

// AddPhrase 添加短语
func (e *PhraseEditor) AddPhrase(code, text string, weight int) bool {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		e.data = &dictfile.PhrasesConfig{Phrases: []dictfile.PhraseConfig{}}
	}

	isNew := dictfile.AddPhrase(e.data, code, text, weight)
	e.dirty = true
	return isNew
}

// RemovePhrase 删除短语
func (e *PhraseEditor) RemovePhrase(code, text string) bool {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		return false
	}

	removed := dictfile.RemovePhrase(e.data, code, text)
	if removed {
		e.dirty = true
	}
	return removed
}

// GetPhrasesByCode 获取指定编码的短语
func (e *PhraseEditor) GetPhrasesByCode(code string) []dictfile.PhraseConfig {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return nil
	}

	return dictfile.GetPhrasesByCode(e.data, code)
}

// GetPhraseCount 获取短语数量
func (e *PhraseEditor) GetPhraseCount() int {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return 0
	}

	return len(e.data.Phrases)
}

// SetPhrases 设置短语配置
func (e *PhraseEditor) SetPhrases(cfg *dictfile.PhrasesConfig) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.data = cfg
	e.dirty = true
}
