package dict

import (
	"bufio"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// UserDict 用户词库
// 实现 MutableLayer 接口，支持用户造词的增删改查和持久化
type UserDict struct {
	mu       sync.RWMutex
	name     string
	filePath string

	// 内存存储: code -> []UserWord
	entries map[string][]UserWord

	// 异步写入
	dirty     bool          // 是否有未保存的修改
	saveChan  chan struct{} // 触发保存的通道
	closeChan chan struct{} // 关闭通道
	wg        sync.WaitGroup
}

// UserWord 用户词条
type UserWord struct {
	Text      string    // 词语
	Weight    int       // 权重
	CreatedAt time.Time // 创建时间
}

// NewUserDict 创建用户词库
func NewUserDict(name string, filePath string) *UserDict {
	ud := &UserDict{
		name:      name,
		filePath:  filePath,
		entries:   make(map[string][]UserWord),
		saveChan:  make(chan struct{}, 1),
		closeChan: make(chan struct{}),
	}

	// 启动异步保存协程
	ud.wg.Add(1)
	go ud.saveLoop()

	return ud
}

// Name 返回层名称
func (ud *UserDict) Name() string {
	return ud.name
}

// Type 返回层类型
func (ud *UserDict) Type() LayerType {
	return LayerTypeUser
}

// Search 精确查询
func (ud *UserDict) Search(code string, limit int) []candidate.Candidate {
	ud.mu.RLock()
	defer ud.mu.RUnlock()

	code = strings.ToLower(code)
	words, ok := ud.entries[code]
	if !ok {
		return nil
	}

	results := make([]candidate.Candidate, 0, len(words))
	for _, w := range words {
		results = append(results, candidate.Candidate{
			Text:   w.Text,
			Code:   code,
			Weight: w.Weight,
		})
	}

	// 排序
	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	// 限制数量
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}

	return results
}

// SearchPrefix 前缀查询
func (ud *UserDict) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	ud.mu.RLock()
	defer ud.mu.RUnlock()

	prefix = strings.ToLower(prefix)
	var results []candidate.Candidate

	for code, words := range ud.entries {
		if strings.HasPrefix(code, prefix) {
			for _, w := range words {
				results = append(results, candidate.Candidate{
					Text:   w.Text,
					Code:   code,
					Weight: w.Weight,
				})
			}
		}
	}

	// 排序
	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	// 限制数量
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}

	return results
}

// Add 添加词条
func (ud *UserDict) Add(code string, text string, weight int) error {
	ud.mu.Lock()
	defer ud.mu.Unlock()

	code = strings.ToLower(code)

	// 检查是否已存在
	for i, w := range ud.entries[code] {
		if w.Text == text {
			// 已存在，更新权重
			ud.entries[code][i].Weight = weight
			ud.markDirty()
			return nil
		}
	}

	// 添加新词
	ud.entries[code] = append(ud.entries[code], UserWord{
		Text:      text,
		Weight:    weight,
		CreatedAt: time.Now(),
	})

	ud.markDirty()
	return nil
}

// Remove 删除词条
func (ud *UserDict) Remove(code string, text string) error {
	ud.mu.Lock()
	defer ud.mu.Unlock()

	code = strings.ToLower(code)
	words, ok := ud.entries[code]
	if !ok {
		return nil
	}

	for i, w := range words {
		if w.Text == text {
			ud.entries[code] = append(words[:i], words[i+1:]...)
			if len(ud.entries[code]) == 0 {
				delete(ud.entries, code)
			}
			ud.markDirty()
			return nil
		}
	}

	return nil
}

// Update 更新词条权重
func (ud *UserDict) Update(code string, text string, newWeight int) error {
	ud.mu.Lock()
	defer ud.mu.Unlock()

	code = strings.ToLower(code)
	words, ok := ud.entries[code]
	if !ok {
		return fmt.Errorf("code not found: %s", code)
	}

	for i, w := range words {
		if w.Text == text {
			ud.entries[code][i].Weight = newWeight
			ud.markDirty()
			return nil
		}
	}

	return fmt.Errorf("word not found: %s", text)
}

// Load 从文件加载
func (ud *UserDict) Load() error {
	ud.mu.Lock()
	defer ud.mu.Unlock()

	file, err := os.Open(ud.filePath)
	if err != nil {
		if os.IsNotExist(err) {
			// 文件不存在是正常的，用户可能还没有造词
			return nil
		}
		return fmt.Errorf("failed to open user dict: %w", err)
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())

		// 跳过空行和注释
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		// 格式: code<tab>text<tab>weight<tab>timestamp
		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}

		code := strings.TrimSpace(parts[0])
		text := strings.TrimSpace(parts[1])

		weight := 100 // 默认权重
		if len(parts) >= 3 {
			if w, err := strconv.Atoi(strings.TrimSpace(parts[2])); err == nil {
				weight = w
			}
		}

		var createdAt time.Time
		if len(parts) >= 4 {
			if ts, err := strconv.ParseInt(strings.TrimSpace(parts[3]), 10, 64); err == nil {
				createdAt = time.Unix(ts, 0)
			}
		}
		if createdAt.IsZero() {
			createdAt = time.Now()
		}

		ud.entries[code] = append(ud.entries[code], UserWord{
			Text:      text,
			Weight:    weight,
			CreatedAt: createdAt,
		})
	}

	return scanner.Err()
}

// Save 保存到文件
func (ud *UserDict) Save() error {
	ud.mu.RLock()
	defer ud.mu.RUnlock()

	return ud.saveToFile()
}

// saveToFile 实际的保存逻辑（调用前需要持有读锁）
func (ud *UserDict) saveToFile() error {
	// 创建临时文件
	tmpPath := ud.filePath + ".tmp"
	file, err := os.Create(tmpPath)
	if err != nil {
		return fmt.Errorf("failed to create temp file: %w", err)
	}

	writer := bufio.NewWriter(file)

	// 写入头部注释
	writer.WriteString("# Wind Input 用户词库\n")
	writer.WriteString("# 格式: 编码<tab>词语<tab>权重<tab>时间戳\n")
	writer.WriteString("# 请勿手动编辑此文件\n\n")

	// 收集所有 code 并排序（保证输出稳定）
	codes := make([]string, 0, len(ud.entries))
	for code := range ud.entries {
		codes = append(codes, code)
	}
	sort.Strings(codes)

	// 写入词条
	for _, code := range codes {
		words := ud.entries[code]
		for _, w := range words {
			line := fmt.Sprintf("%s\t%s\t%d\t%d\n",
				code, w.Text, w.Weight, w.CreatedAt.Unix())
			writer.WriteString(line)
		}
	}

	if err := writer.Flush(); err != nil {
		file.Close()
		os.Remove(tmpPath)
		return fmt.Errorf("failed to flush: %w", err)
	}

	if err := file.Close(); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to close: %w", err)
	}

	// 原子替换
	if err := os.Rename(tmpPath, ud.filePath); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to rename: %w", err)
	}

	return nil
}

// markDirty 标记需要保存
func (ud *UserDict) markDirty() {
	ud.dirty = true
	// 非阻塞发送保存信号
	select {
	case ud.saveChan <- struct{}{}:
	default:
	}
}

// saveLoop 异步保存循环
func (ud *UserDict) saveLoop() {
	defer ud.wg.Done()

	ticker := time.NewTicker(30 * time.Second) // 定期检查
	defer ticker.Stop()

	for {
		select {
		case <-ud.saveChan:
			// 收到保存信号，等待一小段时间合并多次修改
			time.Sleep(500 * time.Millisecond)
			ud.doSave()

		case <-ticker.C:
			// 定期检查是否需要保存
			ud.mu.RLock()
			dirty := ud.dirty
			ud.mu.RUnlock()
			if dirty {
				ud.doSave()
			}

		case <-ud.closeChan:
			// 关闭前保存
			ud.doSave()
			return
		}
	}
}

// doSave 执行保存
func (ud *UserDict) doSave() {
	ud.mu.Lock()
	if !ud.dirty {
		ud.mu.Unlock()
		return
	}
	ud.dirty = false
	ud.mu.Unlock()

	// 实际保存（不持有锁太久）
	ud.mu.RLock()
	err := ud.saveToFile()
	ud.mu.RUnlock()

	if err != nil {
		// 保存失败，重新标记为脏
		ud.mu.Lock()
		ud.dirty = true
		ud.mu.Unlock()
		// TODO: 记录错误日志
	}
}

// Close 关闭用户词库
func (ud *UserDict) Close() error {
	close(ud.closeChan)
	ud.wg.Wait()
	return nil
}

// EntryCount 返回词条数量
func (ud *UserDict) EntryCount() int {
	ud.mu.RLock()
	defer ud.mu.RUnlock()

	count := 0
	for _, words := range ud.entries {
		count += len(words)
	}
	return count
}

// IncreaseWeight 增加词条权重（用于用户选词后提升权重）
func (ud *UserDict) IncreaseWeight(code string, text string, delta int) {
	ud.mu.Lock()
	defer ud.mu.Unlock()

	code = strings.ToLower(code)
	words, ok := ud.entries[code]
	if !ok {
		return
	}

	for i, w := range words {
		if w.Text == text {
			ud.entries[code][i].Weight += delta
			ud.markDirty()
			return
		}
	}
}
