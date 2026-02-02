package editor

import (
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/dictfile"
)

// UserDictEditor 用户词库编辑器
type UserDictEditor struct {
	*BaseEditor
	data *dictfile.UserDictData
}

// NewUserDictEditor 创建用户词库编辑器
func NewUserDictEditor() (*UserDictEditor, error) {
	path, err := config.GetUserDictPath()
	if err != nil {
		return nil, err
	}

	return &UserDictEditor{
		BaseEditor: NewBaseEditor(path),
	}, nil
}

// Load 加载用户词库
func (e *UserDictEditor) Load() error {
	data, err := dictfile.LoadUserDictFrom(e.filePath)
	if err != nil {
		return err
	}

	e.mu.Lock()
	e.data = data
	e.dirty = false
	e.mu.Unlock()

	return e.UpdateFileState()
}

// Save 保存用户词库
func (e *UserDictEditor) Save() error {
	e.mu.RLock()
	data := e.data
	e.mu.RUnlock()

	if data == nil {
		return nil
	}

	if err := dictfile.SaveUserDictTo(data, e.filePath); err != nil {
		return err
	}

	e.ClearDirty()
	return e.UpdateFileState()
}

// Reload 重新加载
func (e *UserDictEditor) Reload() error {
	return e.Load()
}

// GetUserDict 获取用户词库数据
func (e *UserDictEditor) GetUserDict() *dictfile.UserDictData {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.data
}

// AddWord 添加词条
func (e *UserDictEditor) AddWord(code, text string, weight int) bool {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		e.data = &dictfile.UserDictData{Words: []dictfile.UserWord{}}
	}

	isNew := dictfile.AddUserWord(e.data, code, text, weight)
	e.dirty = true
	return isNew
}

// RemoveWord 删除词条
func (e *UserDictEditor) RemoveWord(code, text string) bool {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		return false
	}

	removed := dictfile.RemoveUserWord(e.data, code, text)
	if removed {
		e.dirty = true
	}
	return removed
}

// UpdateWordWeight 更新词条权重
func (e *UserDictEditor) UpdateWordWeight(code, text string, weight int) bool {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		return false
	}

	updated := dictfile.UpdateUserWordWeight(e.data, code, text, weight)
	if updated {
		e.dirty = true
	}
	return updated
}

// SearchWords 搜索词条
func (e *UserDictEditor) SearchWords(query string, limit int) []dictfile.UserWord {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return nil
	}

	return dictfile.SearchUserDict(e.data, query, limit)
}

// GetWordsByCode 获取指定编码的词条
func (e *UserDictEditor) GetWordsByCode(code string) []dictfile.UserWord {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return nil
	}

	return dictfile.GetWordsByCode(e.data, code)
}

// GetWordCount 获取词条数量
func (e *UserDictEditor) GetWordCount() int {
	e.mu.RLock()
	defer e.mu.RUnlock()

	if e.data == nil {
		return 0
	}

	return dictfile.GetWordCount(e.data)
}

// ImportFromFile 从文件导入
func (e *UserDictEditor) ImportFromFile(path string) (int, error) {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.data == nil {
		e.data = &dictfile.UserDictData{Words: []dictfile.UserWord{}}
	}

	count, err := dictfile.ImportUserDict(e.data, path)
	if err != nil {
		return 0, err
	}

	if count > 0 {
		e.dirty = true
	}
	return count, nil
}

// ExportToFile 导出到文件
func (e *UserDictEditor) ExportToFile(path string) error {
	e.mu.RLock()
	data := e.data
	e.mu.RUnlock()

	if data == nil {
		data = &dictfile.UserDictData{Words: []dictfile.UserWord{}}
	}

	return dictfile.ExportUserDict(data, path)
}
