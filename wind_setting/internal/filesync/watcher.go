// Package filesync 提供文件变化监控功能
package filesync

import (
	"sync"
	"time"

	"github.com/huanfeng/wind_input/pkg/fileutil"
)

// FileWatcher 文件变化监控器
type FileWatcher struct {
	mu     sync.RWMutex
	files  map[string]*fileutil.FileState
	stopCh chan struct{}
	wg     sync.WaitGroup

	// 回调函数
	onChange func(path string)
}

// NewFileWatcher 创建文件监控器
func NewFileWatcher() *FileWatcher {
	return &FileWatcher{
		files:  make(map[string]*fileutil.FileState),
		stopCh: make(chan struct{}),
	}
}

// SetOnChange 设置变化回调
func (w *FileWatcher) SetOnChange(callback func(path string)) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.onChange = callback
}

// Watch 开始监控文件
func (w *FileWatcher) Watch(path string) error {
	state, err := fileutil.GetFileState(path)
	if err != nil {
		return err
	}

	w.mu.Lock()
	w.files[path] = state
	w.mu.Unlock()

	return nil
}

// Unwatch 停止监控文件
func (w *FileWatcher) Unwatch(path string) {
	w.mu.Lock()
	delete(w.files, path)
	w.mu.Unlock()
}

// UpdateState 更新文件状态（在保存后调用）
func (w *FileWatcher) UpdateState(path string) error {
	state, err := fileutil.GetFileState(path)
	if err != nil {
		return err
	}

	w.mu.Lock()
	w.files[path] = state
	w.mu.Unlock()

	return nil
}

// CheckChanged 检查文件是否变化
func (w *FileWatcher) CheckChanged(path string) (bool, error) {
	w.mu.RLock()
	state, ok := w.files[path]
	w.mu.RUnlock()

	if !ok {
		return false, nil
	}

	return state.HasChanged()
}

// CheckAllChanged 检查所有文件是否变化
func (w *FileWatcher) CheckAllChanged() map[string]bool {
	w.mu.RLock()
	files := make(map[string]*fileutil.FileState, len(w.files))
	for k, v := range w.files {
		files[k] = v
	}
	w.mu.RUnlock()

	result := make(map[string]bool)
	for path, state := range files {
		changed, err := state.HasChanged()
		if err == nil && changed {
			result[path] = true
		}
	}

	return result
}

// Start 启动定期检查
func (w *FileWatcher) Start(interval time.Duration) {
	w.wg.Add(1)
	go w.watchLoop(interval)
}

// Stop 停止监控
func (w *FileWatcher) Stop() {
	close(w.stopCh)
	w.wg.Wait()
}

// watchLoop 监控循环
func (w *FileWatcher) watchLoop(interval time.Duration) {
	defer w.wg.Done()

	ticker := time.NewTicker(interval)
	defer ticker.Stop()

	for {
		select {
		case <-ticker.C:
			w.checkFiles()
		case <-w.stopCh:
			return
		}
	}
}

// checkFiles 检查所有监控的文件
func (w *FileWatcher) checkFiles() {
	changed := w.CheckAllChanged()

	w.mu.RLock()
	callback := w.onChange
	w.mu.RUnlock()

	if callback == nil {
		return
	}

	for path := range changed {
		callback(path)
	}
}

// GetWatchedFiles 获取所有监控的文件路径
func (w *FileWatcher) GetWatchedFiles() []string {
	w.mu.RLock()
	defer w.mu.RUnlock()

	paths := make([]string, 0, len(w.files))
	for path := range w.files {
		paths = append(paths, path)
	}
	return paths
}
