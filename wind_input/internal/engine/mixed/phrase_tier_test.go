package mixed

import (
	"sort"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// TestPhraseWeightBoost_TierConstants 锁住 PhraseWeightBoost 与 CodetableWeightBoost
// 的相对关系: phrase tier 严格夹在拼音 (0~10000) 与码表 (10M+) 之间。
//
// 这是 PR1 (docs/design/2026-05-16-cmdbar-followup.md §2.2) 的核心架构承诺,
// 必须由常量值层面就保证, 任何后续调整 boost 数值时本测试会先报错提示。
func TestPhraseWeightBoost_TierConstants(t *testing.T) {
	cfgDefault := DefaultConfig().CodetableWeightBoost

	if PhraseWeightBoost != 1_000_000 {
		t.Errorf("PhraseWeightBoost = %d, want 1_000_000", PhraseWeightBoost)
	}

	// 1) 短语 boost 必须严格小于码表 boost (防止短语越过码表 tier)
	if PhraseWeightBoost >= cfgDefault {
		t.Errorf("phrase boost (%d) must be < codetable boost (%d)", PhraseWeightBoost, cfgDefault)
	}

	// 2) 短语 tier 上限 (boost + 10000) 必须严格小于码表 tier 下限 (boost)
	//    即使 phrase weight 顶到 10000, 也不能跨越码表 tier
	phraseUpper := PhraseWeightBoost + 10000
	if phraseUpper >= cfgDefault {
		t.Errorf("phrase tier upper bound (%d) overlaps codetable tier lower bound (%d)", phraseUpper, cfgDefault)
	}

	// 3) 短语 boost 必须严格大于拼音 tier 上限 (10000), 否则 phrase 会
	//    被拼音淹没。weight=1 时实际值 = boost+1, 仍需 > 10000。
	if PhraseWeightBoost <= 10000 {
		t.Errorf("phrase boost (%d) must > pinyin tier upper (10000) for tier separation", PhraseWeightBoost)
	}
}

// TestPhraseTier_BoostSeparation 模拟 mixed.convertMixed 内 boost 循环,
// 验证 IsPhrase 候选与码表候选分离到不同 tier。
//
// 场景: 用户输入 "bd" 触发, codetableCandidates 切片包含混合的 codetable
// 词 (从 codetable engine 返回) 与 PhraseLayer 短语候选 (经 compositeDict
// 流入)。boost 阶段必须用 IsPhrase 把短语分到 PhraseWeightBoost tier。
//
// 排序断言: 同 weight=1000 的短语永远落在任何码表词之后 (即使该码表词
// weight 极小), 永远落在任何拼音候选之前。
func TestPhraseTier_BoostSeparation(t *testing.T) {
	cfg := DefaultConfig()

	// 模拟 codetable engine 返回的混合候选 (含 phrase via compositeDict 路径)
	cands := []candidate.Candidate{
		{Text: "码表低频", Code: "bd", Weight: 100},                  // codetable, 极低 weight
		{Text: "短语高优", Code: "bd", Weight: 9000, IsPhrase: true}, // phrase 标"必置顶"
		{Text: "短语默认", Code: "bd", Weight: 1000, IsPhrase: true}, // phrase 默认 weight
		{Text: "码表精确", Code: "bd", Weight: 5000},                 // codetable 中频
		{Text: "码表前缀", Code: "bdx", Weight: 3000},                // codetable 前缀匹配
	}

	// 等效 mixed.convertMixed 的 boost 循环 (2026-05-17 起拆分组合 tier 独立到 PartialMatchBoost)
	for i := range cands {
		if cands[i].IsPhrase {
			cands[i].Source = candidate.SourcePhrase
			cands[i].Weight += PhraseWeightBoost
			continue
		}
		cands[i].Source = candidate.SourceCodetable
		if cands[i].Code == "bd" {
			cands[i].Weight += cfg.CodetableWeightBoost
		} else {
			cands[i].Weight += PartialMatchBoost
		}
	}

	sort.SliceStable(cands, func(i, j int) bool { return cands[i].Weight > cands[j].Weight })

	// 精确匹配码表词 (Code==input) 必须排在短语之前; 短语必须排在拆分组合
	// (Code!=input, 即 partial tier) 之前。验证: 见到第一个 partial 后不能
	// 再出现 phrase, 见到第一个 phrase 后不能再出现 codetable exact。
	var seenPhrase, seenPartial bool
	for _, c := range cands {
		isPartial := !c.IsPhrase && c.Code != "bd"
		isExact := !c.IsPhrase && c.Code == "bd"
		if isExact && (seenPhrase || seenPartial) {
			t.Errorf("codetable exact %q (weight=%d) appears AFTER phrase/partial — tier order broken", c.Text, c.Weight)
		}
		if c.IsPhrase {
			if seenPartial {
				t.Errorf("phrase %q (weight=%d) appears AFTER a partial candidate — tier order broken", c.Text, c.Weight)
			}
			seenPhrase = true
		}
		if isPartial {
			seenPartial = true
		}
	}

	// 短语 weight 应当严格 < 任何码表 tier 边界 且 > partial tier 上限
	for _, c := range cands {
		if c.IsPhrase && c.Weight >= cfg.CodetableWeightBoost {
			t.Errorf("phrase %q got weight %d (>= codetable boost %d)", c.Text, c.Weight, cfg.CodetableWeightBoost)
		}
		if c.IsPhrase && c.Weight <= PartialMatchBoost+10000 {
			t.Errorf("phrase %q got weight %d (<= partial tier upper %d)", c.Text, c.Weight, PartialMatchBoost+10000)
		}
	}
}

// TestPhraseTier_ShortInputDoesNotPromote 验证短码场景 (1~2 字符) 短语
// **不会**因 boost 占据首位 — 即使用户给 phrase weight 9000, 它仍在
// codetable tier 之下。
//
// 这是 PR1 主要修复目标: 用户反映"短字符输入时短语太靠前"。
func TestPhraseTier_ShortInputDoesNotPromote(t *testing.T) {
	cfg := DefaultConfig()

	// 模拟用户输入 "z" (单字符), 码表返回一个高频常用字, phrase 也有 entry
	// (用户标了 weight=9000 的"必置顶"短语)
	cands := []candidate.Candidate{
		{Text: "之", Code: "z", Weight: 8000},                   // 码表高频常用单字
		{Text: "签名块", Code: "z", Weight: 9000, IsPhrase: true}, // 短语标"必置顶"
	}

	// 应用 boost (input="z" 精确匹配)
	for i := range cands {
		if cands[i].IsPhrase {
			cands[i].Weight += PhraseWeightBoost
		} else if cands[i].Code == "z" {
			cands[i].Weight += cfg.CodetableWeightBoost
		}
	}

	// 码表"之"的最终 weight = 8000 + 10M = 10,008,000
	// 短语"签名块"的最终 weight = 9000 + 1M = 1,009,000
	// 码表 > 短语 (即使短语 weight 更大)

	sort.SliceStable(cands, func(i, j int) bool { return cands[i].Weight > cands[j].Weight })

	if cands[0].Text != "之" {
		t.Errorf("first candidate should be codetable common char '之', got %q (weight=%d)", cands[0].Text, cands[0].Weight)
	}
	if cands[1].Text != "签名块" {
		t.Errorf("second candidate should be phrase '签名块', got %q (weight=%d)", cands[1].Text, cands[1].Weight)
	}
}

// TestPartialMatchBoost_TierConstants 锁住 PartialMatchBoost 与上下 tier 边界:
// pinyin (10000) < partial (PartialMatchBoost) < phrase (PhraseWeightBoost)
//
// 这是 2026-05-17 修复 (拆分组合候选低于短语全码匹配) 的核心架构承诺,
// 任何后续调整这些常量时本测试会先报错提示。
func TestPartialMatchBoost_TierConstants(t *testing.T) {
	if PartialMatchBoost != 500_000 {
		t.Errorf("PartialMatchBoost = %d, want 500_000", PartialMatchBoost)
	}
	// partial 必须 < phrase, 否则拆分候选会盖过短语全码匹配
	if PartialMatchBoost >= PhraseWeightBoost {
		t.Errorf("partial boost (%d) must be < phrase boost (%d) — 拆分组合不得超过短语全码", PartialMatchBoost, PhraseWeightBoost)
	}
	// partial tier 上限 (boost + 10000) 仍 < phrase boost, 即使 partial 候选
	// weight 顶到 10000 也不应越界
	if PartialMatchBoost+10000 >= PhraseWeightBoost {
		t.Errorf("partial tier upper (%d) overlaps phrase tier lower (%d)", PartialMatchBoost+10000, PhraseWeightBoost)
	}
	// partial 必须 > 拼音 tier 上限 (10000) — 拆分候选仍优先于普通拼音
	if PartialMatchBoost <= 10000 {
		t.Errorf("partial boost (%d) must > pinyin tier upper (10000)", PartialMatchBoost)
	}
}

// TestPartialMatchBoost_PhraseBeatsPartial 模拟 mixed.convertMixed 的 boost 循环,
// 验证用户输入 "date" 时短语全码命中排在拆分组合 "d→大" 之前。
//
// 场景: 用户输入 "date"。
//   - 短语 "date" (cmdbar 命令) 全码命中, IsPhrase=true。
//   - 码表层把 "d → 大" 作为前缀补全送进来 (Code="d", Text="大"), 不是 IsPhrase。
//
// 期望: 短语 "date" 候选首位; 拆分组合 "大" 退到 partial tier (排在 phrase 之后)。
//
// 拆分组合可信度 < 原生候选 (2026-05-17): 这是本测试锁定的核心不变量。
func TestPartialMatchBoost_PhraseBeatsPartial(t *testing.T) {
	cfg := DefaultConfig()
	input := "date"

	cands := []candidate.Candidate{
		{Text: "大", Code: "d", Weight: 9000},                      // 码表拆分组合: Code "d" != input "date"
		{Text: "今天", Code: "date", Weight: 1000, IsPhrase: true},  // 短语 cmdbar 全码命中
		{Text: "日期", Code: "date", Weight: 5000, IsPhrase: false}, // 码表精确命中 (Code==input), 应进 codetable tier
	}

	for i := range cands {
		if cands[i].IsPhrase {
			cands[i].Source = candidate.SourcePhrase
			cands[i].Weight += PhraseWeightBoost
			continue
		}
		cands[i].Source = candidate.SourceCodetable
		if cands[i].Code == input {
			cands[i].Weight += cfg.CodetableWeightBoost
		} else {
			cands[i].Weight += PartialMatchBoost
		}
	}

	sort.SliceStable(cands, func(i, j int) bool { return cands[i].Weight > cands[j].Weight })

	// 期望顺序: codetable exact (10M+) → phrase (1M+) → partial (500K+)
	if cands[0].Text != "日期" {
		t.Errorf("first candidate should be codetable exact '日期', got %q (weight=%d)", cands[0].Text, cands[0].Weight)
	}
	if cands[1].Text != "今天" {
		t.Errorf("second candidate should be phrase '今天', got %q (weight=%d) — 拆分组合不得抢在短语全码之前", cands[1].Text, cands[1].Weight)
	}
	if cands[2].Text != "大" {
		t.Errorf("third candidate should be partial '大', got %q (weight=%d)", cands[2].Text, cands[2].Weight)
	}
}

// TestPartialMatchBoost_PartialBeatsPinyin 回归测试: 没有短语命中时, 拆分组合
// 仍应优先于普通拼音候选 (不能把 partial 降到 0)。
//
// 场景: 用户输入 "dat" (无对应短语 / 精确码表词命中), 但码表 "d→大" 作为
// 前缀补全可见。拼音引擎也返回 "大" (从音节 "da" 拆出)。
//
// 期望: 码表拆分 "大" weight > 拼音 "大" weight。
func TestPartialMatchBoost_PartialBeatsPinyin(t *testing.T) {
	cands := []candidate.Candidate{
		{Text: "大", Code: "d", Weight: 8000, Source: candidate.SourceCodetable}, // 码表拆分组合
		{Text: "答", Code: "da", Weight: 9500, Source: candidate.SourcePinyin},   // 拼音候选 (顶到 tier 上限 10000)
	}

	// 仅码表拆分加 PartialMatchBoost, 拼音不加
	cands[0].Weight += PartialMatchBoost

	sort.SliceStable(cands, func(i, j int) bool { return cands[i].Weight > cands[j].Weight })

	if cands[0].Text != "大" {
		t.Errorf("first candidate should be partial-tier '大', got %q (weight=%d) — partial 必须优先于拼音", cands[0].Text, cands[0].Weight)
	}
}
