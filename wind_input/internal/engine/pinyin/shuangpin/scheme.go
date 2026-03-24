// Package shuangpin 提供双拼方案定义和键码转换功能。
// 双拼输入法将每个汉字的拼音映射为固定两个按键：第一键=声母，第二键=韵母。
// 本包负责定义声母/韵母映射表，并将双拼键序列转换为全拼字符串，
// 交由全拼引擎复用现有词库和算法完成候选词查询。
package shuangpin

import (
	"fmt"
	"sync"
)

// Scheme 双拼方案定义
type Scheme struct {
	ID   string // 方案标识（如 "xiaohe"）
	Name string // 显示名称（如 "小鹤双拼"）

	// InitialMap 键 → 声母映射
	// 例如：'v' → "zh", 'i' → "ch", 'u' → "sh"
	// 大多数键直接映射为自身（b→b, p→p, ...）
	InitialMap map[byte]string

	// FinalMap 键 → 韵母列表映射
	// 一个键可能映射多个韵母（如 'k' → ["uai", "ing"]），
	// 实际匹配时通过声母+韵母是否构成合法拼音来消歧。
	FinalMap map[byte][]string

	// ZeroInitialKeys 零声母韵母的特殊映射
	// 零声母音节（如 a, o, e, ai, ei, an, en, ang, eng, er, ou, ao）
	// 在双拼中需要特殊处理。常见策略：
	// - 单韵母（a, o, e）：重复按键（如 aa→a, oo→o, ee→e）
	// - 复韵母：首字母作为"伪声母"（如 ai：a+i键对应的韵母）
	// 此映射表定义了零声母下"伪声母键"到完整音节列表的映射。
	// 键为伪声母键，值为该键开头的零声母音节列表。
	ZeroInitialKeys map[byte][]string
}

// registry 内置方案注册表
var (
	registryMu sync.RWMutex
	registry   = make(map[string]*Scheme)
)

// Register 注册双拼方案
func Register(scheme *Scheme) {
	registryMu.Lock()
	defer registryMu.Unlock()
	registry[scheme.ID] = scheme
}

// Get 获取双拼方案（nil 表示不存在）
func Get(id string) *Scheme {
	registryMu.RLock()
	defer registryMu.RUnlock()
	return registry[id]
}

// List 列出所有已注册的双拼方案
func List() []*Scheme {
	registryMu.RLock()
	defer registryMu.RUnlock()
	result := make([]*Scheme, 0, len(registry))
	for _, s := range registry {
		result = append(result, s)
	}
	return result
}

// ListIDs 列出所有已注册方案的 ID
func ListIDs() []string {
	registryMu.RLock()
	defer registryMu.RUnlock()
	result := make([]string, 0, len(registry))
	for id := range registry {
		result = append(result, id)
	}
	return result
}

// NewCustomScheme 创建自定义双拼方案
func NewCustomScheme(id, name string, initialMap map[byte]string, finalMap map[byte][]string, zeroInitialKeys map[byte][]string) (*Scheme, error) {
	if id == "" || name == "" {
		return nil, fmt.Errorf("方案 ID 和名称不能为空")
	}
	if len(initialMap) == 0 || len(finalMap) == 0 {
		return nil, fmt.Errorf("声母/韵母映射表不能为空")
	}
	return &Scheme{
		ID:              id,
		Name:            name,
		InitialMap:      initialMap,
		FinalMap:        finalMap,
		ZeroInitialKeys: zeroInitialKeys,
	}, nil
}
