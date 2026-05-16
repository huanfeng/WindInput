// Package dict 提供词库管理功能
package dict

import (
	"github.com/huanfeng/wind_input/internal/candidate"
)

// LayerType 词库层类型
type LayerType int

const (
	LayerTypeLogic  LayerType = iota // Lv1: 逻辑/指令层 (date, time, uuid 等)
	LayerTypeShadow                  // Lv2: 用户修正层 (置顶/删除/调序)
	LayerTypeUser                    // Lv3: 用户造词层
	LayerTypeTemp                    // Lv3.5: 临时词库层（自动学习）
	LayerTypeCell                    // Lv4: 细胞词库层
	LayerTypeSystem                  // Lv5: 系统主词库
)

// String 返回层类型的字符串表示
func (t LayerType) String() string {
	switch t {
	case LayerTypeLogic:
		return "logic"
	case LayerTypeShadow:
		return "shadow"
	case LayerTypeUser:
		return "user"
	case LayerTypeTemp:
		return "temp"
	case LayerTypeCell:
		return "cell"
	case LayerTypeSystem:
		return "system"
	default:
		return "unknown"
	}
}

// DictLayer 词库层接口
// 所有类型的词库都需要实现此接口，以便被 CompositeDict 聚合
type DictLayer interface {
	// Name 返回词库层的名称（用于日志和调试）
	Name() string

	// Type 返回词库层的类型
	Type() LayerType

	// Search 根据编码查询候选词
	// code: 输入编码（拼音/五笔等）
	// limit: 最大返回数量，0 表示不限制
	// 返回: 候选词列表（已按权重排序）
	Search(code string, limit int) []candidate.Candidate

	// SearchPrefix 根据编码前缀查询候选词
	// prefix: 输入编码前缀
	// limit: 最大返回数量，0 表示不限制
	// 返回: 候选词列表（已按权重排序）
	SearchPrefix(prefix string, limit int) []candidate.Candidate
}

// PinnedWord 固定位置的词条（呈现层位置覆盖）
//
// 2026-05-17 R2: 新增 CandID 字段。匹配优先级 (ApplyShadowPins):
//   - CandID 非空: 按 cand.ID 匹配 (动态短语场景, id 跨日子稳定)
//   - CandID 空 : 按 Word 匹配 cand.Text (兼容手输文本规则)
type PinnedWord struct {
	Word     string // 词语 (展开后的 text, 用于 UI 显示 / 兼容匹配)
	CandID   string // 候选稳定 id (PhraseLayer 生成的 "phrase:<code>:<template>")
	Position int    // 目标位置（0=首位，1=第二位...）
}

// DeletedWord 被隐藏的候选, 与 PinnedWord 同语义新增 CandID。
type DeletedWord struct {
	Word   string
	CandID string
}

// ShadowRules 一个编码下的所有 Shadow 规则
type ShadowRules struct {
	Pinned  []PinnedWord  // 固定位置的词（数组顺序=时间戳，前面的优先级高）
	Deleted []DeletedWord // 被隐藏的候选 (单字过滤仍在 ApplyShadowPins 内)
}

// ShadowProvider Shadow 规则提供者接口
// 引擎在最终排序后调用，实现呈现层的位置覆盖和过滤
type ShadowProvider interface {
	// GetShadowRules 获取指定编码的 Shadow 规则
	GetShadowRules(code string) *ShadowRules
}

// MutableLayer 可写入的词库层接口
// 用户词库等需要支持写入操作的层需要实现此接口
type MutableLayer interface {
	DictLayer

	// Add 添加词条
	Add(code string, text string, weight int) error

	// Remove 删除词条
	Remove(code string, text string) error

	// Update 更新词条权重
	Update(code string, text string, newWeight int) error

	// Save 持久化到存储
	Save() error
}
