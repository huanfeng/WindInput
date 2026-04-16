package schema

import (
	"github.com/huanfeng/wind_input/internal/dict"
)

// LearningStrategy 学习策略接口（只负责造词）
// 调频由 dict.FreqHandler 独立处理
type LearningStrategy interface {
	// OnWordCommitted 用户提交词时的造词回调
	OnWordCommitted(code, text string)

	// Reset 重置学习状态
	Reset()
}

// SystemWordChecker 系统词库检查接口
// 用于判断一个 code+text 是否已在系统词库中存在
type SystemWordChecker interface {
	ExistsInSystemDict(code, text string) bool
}

// ManualLearning 手动学习策略
// 不自动造词，用户通过快捷键手动加词
type ManualLearning struct{}

func (m *ManualLearning) OnWordCommitted(code, text string) {
	// 手动模式不自动学词
}

func (m *ManualLearning) Reset() {}

// AutoLearning 自动学习策略
// 选词即学，优先写入临时词库（如有），达标后晋升到用户词库
// 系统词库已有的词不会重复写入
type AutoLearning struct {
	userLayer     *dict.StoreUserLayer
	tempLayer     *dict.StoreTempLayer
	systemChecker SystemWordChecker
	config        AutoLearnSpec
}

// NewAutoLearning 创建自动学习策略
func NewAutoLearning(userLayer *dict.StoreUserLayer, config AutoLearnSpec) *AutoLearning {
	return &AutoLearning{
		userLayer: userLayer,
		config:    config,
	}
}

// SetTempLayer 设置临时词库层（自动学习优先写入临时词库）
func (a *AutoLearning) SetTempLayer(tl *dict.StoreTempLayer) {
	a.tempLayer = tl
}

// SetSystemChecker 设置系统词库检查器
func (a *AutoLearning) SetSystemChecker(checker SystemWordChecker) {
	a.systemChecker = checker
}

func (a *AutoLearning) OnWordCommitted(code, text string) {
	if len([]rune(text)) < a.config.MinWordLength {
		return
	}

	// 系统词库已有该词，跳过造词（词频由 FreqHandler 单独处理）
	if a.systemChecker != nil && a.systemChecker.ExistsInSystemDict(code, text) {
		return
	}

	// 优先写入临时词库
	if a.tempLayer != nil {
		promoted := a.tempLayer.LearnWord(code, text, a.config.WeightDelta)
		if promoted {
			a.tempLayer.PromoteWord(code, text)
		}
		return
	}

	// 没有临时词库时，直接写入用户词库（带误选保护）
	if a.userLayer != nil {
		a.userLayer.OnWordSelected(code, text,
			a.config.AddWeight, a.config.WeightDelta, a.config.CountThreshold)
	}
}

func (a *AutoLearning) Reset() {}

// NewLearningStrategy 根据方案配置创建学习策略
func NewLearningStrategy(ls *LearningSpec, userLayer *dict.StoreUserLayer) LearningStrategy {
	if !ls.IsAutoLearnEnabled() {
		return &ManualLearning{}
	}
	config := ls.GetAutoLearnConfig()
	return NewAutoLearning(userLayer, config)
}
