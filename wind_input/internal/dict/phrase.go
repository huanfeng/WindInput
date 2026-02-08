package dict

import (
	"fmt"
	"os"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/huanfeng/wind_input/internal/candidate"
	"gopkg.in/yaml.v3"
)

// PhraseLayer 特殊短语层
// 支持用户自定义短语和内置命令（date, time, uuid 等）
type PhraseLayer struct {
	mu       sync.RWMutex
	name     string
	filePath string

	// 普通短语: code -> []PhraseEntry
	phrases map[string][]PhraseEntry

	// 命令短语: code -> CommandHandler
	commands map[string]CommandHandler

	// 命令结果缓存：同一 code 重复查询时返回缓存，避免 UUID/时间 候选不断刷新
	cmdCache    map[string][]candidate.Candidate
	cmdCacheKey string // 当前缓存对应的输入（输入变化时整体失效）
}

// PhraseEntry 短语条目
type PhraseEntry struct {
	Text   string // 输出文本（可包含模板变量）
	Weight int    // 权重
}

// CommandHandler 命令处理器
type CommandHandler func() []candidate.Candidate

// PhrasesConfig phrases.yaml 配置结构
type PhrasesConfig struct {
	Phrases []PhraseConfig `yaml:"phrases"`
}

// PhraseConfig 单个短语配置
type PhraseConfig struct {
	Code       string   `yaml:"code"`       // 触发编码
	Text       string   `yaml:"text"`       // 单个输出（与 candidates 二选一）
	Candidates []string `yaml:"candidates"` // 多个候选输出
	Type       string   `yaml:"type"`       // 类型: 空=普通短语, "command"=命令
	Handler    string   `yaml:"handler"`    // 命令处理器名称
	Weight     int      `yaml:"weight"`     // 权重（默认 100）
}

// NewPhraseLayer 创建特殊短语层
func NewPhraseLayer(name string, filePath string) *PhraseLayer {
	pl := &PhraseLayer{
		name:     name,
		filePath: filePath,
		phrases:  make(map[string][]PhraseEntry),
		commands: make(map[string]CommandHandler),
		cmdCache: make(map[string][]candidate.Candidate),
	}

	// 注册内置命令
	pl.registerBuiltinCommands()

	return pl
}

// registerBuiltinCommands 注册内置命令
func (pl *PhraseLayer) registerBuiltinCommands() {
	// 日期相关
	pl.commands["date"] = func() []candidate.Candidate {
		now := time.Now()
		return []candidate.Candidate{
			{Text: now.Format("2006-01-02"), Weight: 100},
			{Text: now.Format("2006年01月02日"), Weight: 99},
			{Text: now.Format("01/02/2006"), Weight: 98},
			{Text: now.Format("2006/01/02"), Weight: 97},
		}
	}

	pl.commands["time"] = func() []candidate.Candidate {
		now := time.Now()
		return []candidate.Candidate{
			{Text: now.Format("15:04:05"), Weight: 100},
			{Text: now.Format("15:04"), Weight: 99},
			{Text: now.Format("15时04分05秒"), Weight: 98},
		}
	}

	pl.commands["datetime"] = func() []candidate.Candidate {
		now := time.Now()
		return []candidate.Candidate{
			{Text: now.Format("2006-01-02 15:04:05"), Weight: 100},
			{Text: now.Format("2006年01月02日 15:04:05"), Weight: 99},
		}
	}

	pl.commands["week"] = func() []candidate.Candidate {
		weekdays := []string{"星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六"}
		weekday := weekdays[time.Now().Weekday()]
		return []candidate.Candidate{
			{Text: weekday, Weight: 100},
		}
	}

	pl.commands["uuid"] = func() []candidate.Candidate {
		return []candidate.Candidate{
			{Text: uuid.New().String(), Weight: 100},
		}
	}

	pl.commands["timestamp"] = func() []candidate.Candidate {
		return []candidate.Candidate{
			{Text: fmt.Sprintf("%d", time.Now().Unix()), Weight: 100},
			{Text: fmt.Sprintf("%d", time.Now().UnixMilli()), Weight: 99},
		}
	}
}

// Name 返回层名称
func (pl *PhraseLayer) Name() string {
	return pl.name
}

// Type 返回层类型
func (pl *PhraseLayer) Type() LayerType {
	return LayerTypeLogic
}

// Search 精确查询
// 注意：此方法仅查询普通短语，不返回命令结果（uuid, date 等）。
// 命令结果仅通过 SearchCommand() 方法访问，防止任意代码路径意外触发命令。
func (pl *PhraseLayer) Search(code string, limit int) []candidate.Candidate {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	code = strings.ToLower(code)
	var results []candidate.Candidate

	// 仅查询普通短语（不含命令）
	if entries, ok := pl.phrases[code]; ok {
		for _, e := range entries {
			text := pl.expandTemplate(e.Text)
			results = append(results, candidate.Candidate{
				Text:   text,
				Code:   code,
				Weight: e.Weight,
			})
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

// InvalidateCache 清除命令结果缓存
// 当用户输入变化或需要刷新命令结果时调用
func (pl *PhraseLayer) InvalidateCache() {
	pl.mu.Lock()
	defer pl.mu.Unlock()
	pl.cmdCache = make(map[string][]candidate.Candidate)
}

// InvalidateCacheForInput 根据输入变化决定是否清除缓存
// 如果输入与上次缓存的输入不同，则清除缓存
func (pl *PhraseLayer) InvalidateCacheForInput(input string) {
	pl.mu.Lock()
	defer pl.mu.Unlock()
	if pl.cmdCacheKey != input {
		pl.cmdCache = make(map[string][]candidate.Candidate)
		pl.cmdCacheKey = input
	}
}

// SearchCommand 仅查找命令（不含普通短语和词条）
// 用于引擎步骤 0 的特殊命令匹配，只返回 uuid/date/time 等命令结果
func (pl *PhraseLayer) SearchCommand(code string, limit int) []candidate.Candidate {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)
	handler, ok := pl.commands[code]
	if !ok {
		return nil
	}

	// 使用缓存
	if cached, hit := pl.cmdCache[code]; hit {
		if limit > 0 && len(cached) > limit {
			return cached[:limit]
		}
		return cached
	}

	cmdResults := handler()
	for i := range cmdResults {
		cmdResults[i].IsCommand = true
	}
	pl.cmdCache[code] = cmdResults

	if limit > 0 && len(cmdResults) > limit {
		return cmdResults[:limit]
	}
	return cmdResults
}

// SearchPrefix 前缀查询
// 注意：命令（uuid、date 等）仅支持精确匹配，不参与前缀搜索
// 避免输入 "u" 就触发 uuid 命令生成 UUID 字符串
func (pl *PhraseLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	prefix = strings.ToLower(prefix)
	var results []candidate.Candidate

	// 遍历短语（不包含命令）
	for code, entries := range pl.phrases {
		if strings.HasPrefix(code, prefix) {
			for _, e := range entries {
				text := pl.expandTemplate(e.Text)
				results = append(results, candidate.Candidate{
					Text:   text,
					Code:   code,
					Weight: e.Weight,
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

// expandTemplate 展开模板变量
func (pl *PhraseLayer) expandTemplate(text string) string {
	now := time.Now()

	replacements := map[string]string{
		"{year}":   now.Format("2006"),
		"{month}":  now.Format("01"),
		"{day}":    now.Format("02"),
		"{hour}":   now.Format("15"),
		"{minute}": now.Format("04"),
		"{second}": now.Format("05"),
		"{week}":   []string{"日", "一", "二", "三", "四", "五", "六"}[now.Weekday()],
	}

	result := text
	for placeholder, value := range replacements {
		result = strings.ReplaceAll(result, placeholder, value)
	}

	return result
}

// Load 从 YAML 文件加载
func (pl *PhraseLayer) Load() error {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	data, err := os.ReadFile(pl.filePath)
	if err != nil {
		if os.IsNotExist(err) {
			// 文件不存在是正常的
			return nil
		}
		return fmt.Errorf("failed to read phrases file: %w", err)
	}

	var config PhrasesConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return fmt.Errorf("failed to parse phrases file: %w", err)
	}

	// 清空现有短语（保留内置命令）
	pl.phrases = make(map[string][]PhraseEntry)

	// 加载配置
	for _, p := range config.Phrases {
		code := strings.ToLower(p.Code)
		weight := p.Weight
		if weight == 0 {
			weight = 100 // 默认权重
		}

		if p.Type == "command" {
			// 命令类型：注册到 commands
			if handler, ok := pl.commands[p.Handler]; ok {
				pl.commands[code] = handler
			}
		} else {
			// 普通短语
			if p.Text != "" {
				pl.phrases[code] = append(pl.phrases[code], PhraseEntry{
					Text:   p.Text,
					Weight: weight,
				})
			}
			for i, text := range p.Candidates {
				pl.phrases[code] = append(pl.phrases[code], PhraseEntry{
					Text:   text,
					Weight: weight - i, // 候选项按顺序递减权重
				})
			}
		}
	}

	return nil
}

// AddPhrase 添加短语（运行时添加，不持久化）
func (pl *PhraseLayer) AddPhrase(code string, text string, weight int) {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)
	pl.phrases[code] = append(pl.phrases[code], PhraseEntry{
		Text:   text,
		Weight: weight,
	})
}

// RegisterCommand 注册自定义命令
func (pl *PhraseLayer) RegisterCommand(code string, handler CommandHandler) {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	pl.commands[strings.ToLower(code)] = handler
}

// GetPhraseCount 获取短语数量
func (pl *PhraseLayer) GetPhraseCount() int {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	count := 0
	for _, entries := range pl.phrases {
		count += len(entries)
	}
	return count
}

// GetCommandCount 获取命令数量
func (pl *PhraseLayer) GetCommandCount() int {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	return len(pl.commands)
}
