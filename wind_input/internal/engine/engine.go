package engine

import "github.com/huanfeng/wind_input/internal/candidate"

// EngineType 引擎类型
type EngineType string

const (
	EngineTypePinyin EngineType = "pinyin" // 拼音
	EngineTypeWubi   EngineType = "wubi"   // 五笔
	EngineTypeMixed  EngineType = "mixed"  // 五笔拼音混输
)

// Engine 输入引擎接口
type Engine interface {
	// Convert 转换输入为候选词
	Convert(input string, maxCandidates int) ([]candidate.Candidate, error)

	// Reset 重置引擎状态
	Reset()

	// Type 返回引擎类型
	Type() string
}

// ExtendedEngine 扩展引擎接口，支持更多功能
type ExtendedEngine interface {
	Engine

	// GetMaxCodeLength 获取最大码长
	GetMaxCodeLength() int

	// ShouldAutoCommit 检查是否应该自动上屏
	ShouldAutoCommit(input string, candidates []candidate.Candidate) (bool, string)

	// HandleEmptyCode 处理空码
	HandleEmptyCode(input string) (shouldClear bool, toEnglish bool, englishText string)

	// HandleTopCode 处理顶码（如五笔的五码顶字）
	HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool)
}

// ConvertResult 转换结果（扩展信息）
type ConvertResult struct {
	Candidates   []candidate.Candidate
	ShouldCommit bool   // 是否应该自动上屏
	CommitText   string // 自动上屏的文字
	IsEmpty      bool   // 是否空码
	ShouldClear  bool   // 是否应该清空
	ToEnglish    bool   // 是否转为英文
	NewInput     string // 新的输入（用于顶码场景）

	// 拼音专用字段
	PreeditDisplay     string   // 预编辑区显示文本（如 "ni'hao'zh"）
	CompletedSyllables []string // 已完成的音节（如 ["ni", "hao"]）
	PartialSyllable    string   // 未完成的音节（如 "zh"）
	HasPartial         bool     // 是否有未完成音节
}
