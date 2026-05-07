package s2t

import (
	"fmt"
	"path/filepath"
	"sync"
)

// DictPool 按需加载 OpenCC 词典并缓存。
//
// 设计：
//   - 启用 / 切换变体时调用 Acquire(name)，未加载则同步读盘加载
//   - 关闭功能时调用 ReleaseAll() 清空缓存，让 GC 回收
//   - 词典加载后只读，并发查询安全
type DictPool struct {
	dir    string
	mu     sync.RWMutex
	loaded map[string]*Dict
}

// NewDictPool 在给定目录上构造池。dir 应是 build/data/opencc 或运行时同等位置。
func NewDictPool(dir string) *DictPool {
	return &DictPool{
		dir:    dir,
		loaded: make(map[string]*Dict, 8),
	}
}

// Dir 返回词典所在目录。
func (p *DictPool) Dir() string { return p.dir }

// Acquire 获取指定词典；未加载则尝试加载。
func (p *DictPool) Acquire(name string) (*Dict, error) {
	p.mu.RLock()
	if d, ok := p.loaded[name]; ok {
		p.mu.RUnlock()
		return d, nil
	}
	p.mu.RUnlock()

	p.mu.Lock()
	defer p.mu.Unlock()
	// 双检
	if d, ok := p.loaded[name]; ok {
		return d, nil
	}
	path := filepath.Join(p.dir, name+".octrie")
	d, err := LoadDict(name, path)
	if err != nil {
		return nil, fmt.Errorf("dictpool acquire %s: %w", name, err)
	}
	p.loaded[name] = d
	return d, nil
}

// AcquireAll 批量加载，遇到任一失败立即返回。
func (p *DictPool) AcquireAll(names []string) ([]*Dict, error) {
	out := make([]*Dict, 0, len(names))
	for _, n := range names {
		d, err := p.Acquire(n)
		if err != nil {
			return nil, err
		}
		out = append(out, d)
	}
	return out, nil
}

// ReleaseAll 清空全部已加载词典，触发 GC 回收。
func (p *DictPool) ReleaseAll() {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.loaded = make(map[string]*Dict, 8)
}

// LoadedNames 返回当前已加载词典名（用于诊断/日志）。
func (p *DictPool) LoadedNames() []string {
	p.mu.RLock()
	defer p.mu.RUnlock()
	out := make([]string, 0, len(p.loaded))
	for n := range p.loaded {
		out = append(out, n)
	}
	return out
}
