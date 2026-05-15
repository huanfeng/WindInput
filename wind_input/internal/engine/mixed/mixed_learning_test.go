package mixed

// 流程级学习回归测试
//
// 注意：mixed 包被 schema 包（factory.go）导入，所以此包不能反向 import schema。
// 测试用的学习策略通过本地轻量实现绕开循环依赖，直接与 dict.StoreTempLayer /
// dict.StoreUserLayer 交互，验证学习路由逻辑而不依赖 schema.AutoLearning 实现。

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/store"
)

// openMixedTestStore 创建测试用临时 store（测试结束自动清理）
func openMixedTestStore(t *testing.T) *store.Store {
	t.Helper()
	s, err := store.Open(filepath.Join(t.TempDir(), "mixed_test.db"))
	if err != nil {
		t.Fatalf("store.Open: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}

// newMinimalPinyinEngine 创建无词库的最小化拼音引擎，仅用于学习路由测试
func newMinimalPinyinEngine() *pinyin.Engine {
	comp := dict.NewCompositeDict()
	return pinyin.NewEngineWithConfig(comp, &pinyin.Config{}, nil)
}

// --- 测试用轻量学习策略（不导入 schema 包，避免循环依赖） ---

// tempPromoteLearning 模拟 AutoLearning：先写临时词库，达到阈值后晋升到用户词库。
// 满足 pinyin.LearningStrategy 接口。
type tempPromoteLearning struct {
	tempLayer   *dict.StoreTempLayer
	userLayer   *dict.StoreUserLayer
	weightDelta int
}

func (l *tempPromoteLearning) OnWordCommitted(code, text string) {
	if l.tempLayer == nil {
		return
	}
	promoted := l.tempLayer.LearnWord(code, text, l.weightDelta)
	if promoted {
		l.tempLayer.PromoteWord(code, text)
	}
}

var _ pinyin.LearningStrategy = (*tempPromoteLearning)(nil)

// directUserLearning 模拟 AutoLearning 无 tempLayer 时直接写用户词库。
type directUserLearning struct {
	userLayer *dict.StoreUserLayer
	addWeight int
}

func (l *directUserLearning) OnWordCommitted(code, text string) {
	if l.userLayer != nil {
		l.userLayer.OnWordSelected(code, text, l.addWeight, 10, 1)
	}
}

var _ pinyin.LearningStrategy = (*directUserLearning)(nil)

// mockCodetableLearning 记录 OnWordCommitted 和 OnPhraseTerminated 调用次数，
// 用于验证 SourcePinyin 路由是否正确通知码表 charBuffer。
// 满足 codetable.LearningStrategy 接口。
type mockCodetableLearning struct {
	committed  []string
	terminated int
}

func (m *mockCodetableLearning) OnWordCommitted(_, text string) {
	m.committed = append(m.committed, text)
}

var _ codetable.LearningStrategy = (*mockCodetableLearning)(nil)

// --- 测试用例 ---

// TestMixedLearning_PinyinTempPromotion 验证拼音学习策略经过 N 次调用后将词晋升到用户词库。
//
// 回归目标：wubi86_pinyin 混输模式下选词达到阈值但不晋升的 bug。
// 根本原因：混输方案 TempPromoteCount=0，pinyin temp layer SetLimits 未继承
// 主方案值（wubi86 配置 promoteCount=3），导致 LearnWord 永远返回 false。
//
// 注意：此测试绕过 pinyin.Engine.OnCandidateSelected 的 code-text 验证层（无词库时
// 验证必然失败），直接调用学习策略，专注于"SetLimits 正确后晋升是否生效"这一回归点。
// 混输引擎的路由逻辑由 TestMixedLearning_PinyinSingleCharNotifiesCodetableCharBuffer 覆盖。
func TestMixedLearning_PinyinTempPromotion(t *testing.T) {
	const (
		schemaID     = "pinyin"
		word         = "幻枫"
		code         = "huanfeng"
		promoteCount = 3
	)

	s := openMixedTestStore(t)
	userLayer := dict.NewStoreUserLayer(s, schemaID)
	tempLayer := dict.NewStoreTempLayer(s, schemaID)
	tempLayer.SetLimits(100, promoteCount)

	ls := &tempPromoteLearning{
		tempLayer:   tempLayer,
		userLayer:   userLayer,
		weightDelta: 10,
	}

	// 前 promoteCount-1 次：词仍在临时词库，未晋升
	for i := 0; i < promoteCount-1; i++ {
		ls.OnWordCommitted(code, word)
	}

	foundInTemp := false
	for _, c := range tempLayer.Search(code, 0) {
		if c.Text == word {
			foundInTemp = true
		}
	}
	if !foundInTemp {
		t.Errorf("第 %d 次 OnWordCommitted 后，词 %q 应在临时词库中", promoteCount-1, word)
	}
	for _, c := range userLayer.Search(code, 0) {
		if c.Text == word {
			t.Errorf("未达晋升阈值时，词 %q 不应出现在用户词库中", word)
		}
	}

	// 第 promoteCount 次：触发晋升
	ls.OnWordCommitted(code, word)

	// 晋升后：用户词库中应有该词
	promotedToUser := false
	for _, c := range userLayer.Search(code, 0) {
		if c.Text == word {
			promotedToUser = true
		}
	}
	if !promotedToUser {
		t.Errorf("达到晋升阈值（%d 次）后，词 %q 应出现在用户词库中", promoteCount, word)
	}

	// 晋升后：临时词库中不应再有该词
	for _, c := range tempLayer.Search(code, 0) {
		if c.Text == word {
			t.Errorf("晋升后词 %q 不应继续留在临时词库中", word)
		}
	}
}

// TestMixedLearning_PromoteCountZero_DirectToUser 验证 promoteCount=0 时词直接进用户词库。
//
// 设计约定：TempPromoteCount=0 表示"跳过临时词库，直接学习"（非"继承"语义）。
// 与 TestMixedLearning_PinyinTempPromotion 同理，直接调用学习策略，
// 绕开无词库时拼音引擎的 code-text 验证。
func TestMixedLearning_PromoteCountZero_DirectToUser(t *testing.T) {
	const (
		schemaID = "pinyin"
		word     = "直接学习"
		code     = "zhijie"
	)

	s := openMixedTestStore(t)
	userLayer := dict.NewStoreUserLayer(s, schemaID)

	ls := &directUserLearning{
		userLayer: userLayer,
		addWeight: 50,
	}

	ls.OnWordCommitted(code, word)

	found := false
	for _, c := range userLayer.Search(code, 0) {
		if c.Text == word {
			found = true
		}
	}
	if !found {
		t.Errorf("无 tempLayer（promoteCount=0 语义）时，词 %q 应直接写入用户词库", word)
	}
}

// TestMixedLearning_PinyinSingleCharNotifiesCodetableCharBuffer 验证拼音来源单字上屏后
// 通知码表引擎的 charBuffer（跨源同步），拼音多字词触发 OnPhraseTerminated。
//
// 回归目标：P1 修复——五笔+拼音交替输入时，拼音选字不通知码表 charBuffer，
// 导致自动造词只能感知纯五笔子序列的 bug。
func TestMixedLearning_PinyinSingleCharNotifiesCodetableCharBuffer(t *testing.T) {
	mock := &mockCodetableLearning{}

	ct := codetable.NewEngine(codetable.DefaultConfig(), nil)
	ct.SetLearningStrategy(mock)

	me := NewEngine(ct, newMinimalPinyinEngine(), &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10_000_000,
	}, nil)

	// 拼音来源单字：每个字都应通知码表 charBuffer
	me.OnCandidateSelected("huan", "幻", candidate.SourcePinyin)
	me.OnCandidateSelected("feng", "枫", candidate.SourcePinyin)

	if len(mock.committed) != 2 {
		t.Errorf("拼音来源2个单字应通知 charBuffer 2 次，实际 %d 次；committed=%v",
			len(mock.committed), mock.committed)
	}
	if len(mock.committed) >= 1 && mock.committed[0] != "幻" {
		t.Errorf("第1次 charBuffer 通知应为 '幻'，实际为 %q", mock.committed[0])
	}
	if len(mock.committed) >= 2 && mock.committed[1] != "枫" {
		t.Errorf("第2次 charBuffer 通知应为 '枫'，实际为 %q", mock.committed[1])
	}
}

// TestMixedLearning_PinyinMulticharTerminatesCharBuffer 验证拼音来源多字词触发
// OnPhraseTerminated，清空码表 charBuffer。
func TestMixedLearning_PinyinMulticharTerminatesCharBuffer(t *testing.T) {
	mock := &mockCodetableLearning{}
	ct := codetable.NewEngine(codetable.DefaultConfig(), nil)
	ct.SetLearningStrategy(mock)

	me := NewEngine(ct, newMinimalPinyinEngine(), &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10_000_000,
	}, nil)

	// 先积累单字
	me.OnCandidateSelected("huan", "幻", candidate.SourcePinyin)

	// 再选多字词：应触发 OnPhraseTerminated 而非追加 OnWordCommitted
	me.OnCandidateSelected("guanliyuan", "管理员", candidate.SourcePinyin)

	// 单字通知 1 次，多字词不追加
	if len(mock.committed) != 1 {
		t.Errorf("多字词不应追加 charBuffer 通知，committed 次数应为 1，实际 %d；committed=%v",
			len(mock.committed), mock.committed)
	}
}

// TestMixedLearning_CodetableSourceDoesNotPollutePinyinLearning 验证码表来源候选
// 不会触发拼音学习，确保两个学习桶的数据隔离。
func TestMixedLearning_CodetableSourceDoesNotPollutePinyinLearning(t *testing.T) {
	s := openMixedTestStore(t)
	const schemaID = "pinyin"
	userLayer := dict.NewStoreUserLayer(s, schemaID)
	tempLayer := dict.NewStoreTempLayer(s, schemaID)
	tempLayer.SetLimits(100, 3)

	pe := newMinimalPinyinEngine()
	pe.SetLearningStrategy(&tempPromoteLearning{
		tempLayer:   tempLayer,
		userLayer:   userLayer,
		weightDelta: 10,
	})

	me := NewEngine(nil, pe, &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10_000_000,
	}, nil)

	// 码表来源选词 3 次：不应触发拼音学习
	for i := 0; i < 3; i++ {
		me.OnCandidateSelected("sfgh", "幻", candidate.SourceCodetable)
	}

	// 拼音用户词库不应有该词
	for _, c := range userLayer.Search("sfgh", 0) {
		if c.Text == "幻" {
			t.Errorf("码表来源候选不应写入拼音用户词库，但发现 %q", c.Text)
		}
	}
	// 拼音临时词库也不应有该词
	for _, c := range tempLayer.Search("sfgh", 0) {
		if c.Text == "幻" {
			t.Errorf("码表来源候选不应写入拼音临时词库，但发现 %q", c.Text)
		}
	}
}

// TestMixedLearning_GetBuiltDictEngine_PinyinPromotion 使用真实词库的端到端晋升测试。
// 如未构建词库则自动跳过。
func TestMixedLearning_GetBuiltDictEngine_PinyinPromotion(t *testing.T) {
	dictRoot := getBuiltDictRoot(t)

	s := openMixedTestStore(t)
	const schemaID = "pinyin"
	userLayer := dict.NewStoreUserLayer(s, schemaID)
	tempLayer := dict.NewStoreTempLayer(s, schemaID)
	tempLayer.SetLimits(100, 3)

	_, pinyinEng := createPinyinEngine(t, dictRoot)
	pinyinEng.SetLearningStrategy(&tempPromoteLearning{
		tempLayer:   tempLayer,
		userLayer:   userLayer,
		weightDelta: 10,
	})

	me := NewEngine(nil, pinyinEng, &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10_000_000,
		ShowSourceHint:       true,
	}, nil)

	const (
		word = "幻枫"
		code = "huanfeng"
	)

	for i := 0; i < 3; i++ {
		me.OnCandidateSelected(code, word, candidate.SourcePinyin)
	}

	promoted := false
	for _, c := range userLayer.Search(code, 0) {
		if c.Text == word {
			promoted = true
		}
	}
	if !promoted {
		t.Errorf("3 次拼音选词后，词 %q 应晋升到用户词库", word)
	}
}
