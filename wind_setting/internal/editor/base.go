// Package editor 提供配置和词库文件的编辑器
package editor

import (
	"sync"
	"time"

	"github.com/huanfeng/wind_input/pkg/fileutil"
)

// Editor 基础编辑器接口
type Editor interface {
	// Load 加载数据
	Load() error
	// Save 保存数据
	Save() error
	// HasChanged 检查文件是否被外部修改
	HasChanged() (bool, error)
	// Reload 重新加载（丢弃本地修改）
	Reload() error
	// IsDirty 检查是否有未保存的修改
	IsDirty() bool
}

// BaseEditor 基础编辑器实现
type BaseEditor struct {
	mu        sync.RWMutex
	filePath  string
	fileState *fileutil.FileState
	dirty     bool
	loadTime  time.Time
}

// NewBaseEditor 创建基础编辑器
func NewBaseEditor(filePath string) *BaseEditor {
	return &BaseEditor{
		filePath: filePath,
	}
}

// GetFilePath 获取文件路径
func (e *BaseEditor) GetFilePath() string {
	return e.filePath
}

// MarkDirty 标记为已修改
func (e *BaseEditor) MarkDirty() {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.dirty = true
}

// ClearDirty 清除修改标记
func (e *BaseEditor) ClearDirty() {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.dirty = false
}

// IsDirty 检查是否有未保存的修改
func (e *BaseEditor) IsDirty() bool {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.dirty
}

// UpdateFileState 更新文件状态
func (e *BaseEditor) UpdateFileState() error {
	state, err := fileutil.GetFileState(e.filePath)
	if err != nil {
		return err
	}
	e.mu.Lock()
	e.fileState = state
	e.loadTime = time.Now()
	e.mu.Unlock()
	return nil
}

// HasChanged 检查文件是否被外部修改
func (e *BaseEditor) HasChanged() (bool, error) {
	e.mu.RLock()
	state := e.fileState
	e.mu.RUnlock()

	if state == nil {
		return false, nil
	}

	return state.HasChanged()
}

// GetLoadTime 获取加载时间
func (e *BaseEditor) GetLoadTime() time.Time {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.loadTime
}
