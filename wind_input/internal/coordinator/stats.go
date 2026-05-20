package coordinator

import (
	"time"

	"github.com/huanfeng/wind_input/internal/store"
)

// HandleInputStats 处理来自 TSF 英文模式的统计上报
func (c *Coordinator) HandleInputStats(chars, digits, puncts, spaces, elapsedMs int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.statCollector == nil {
		return
	}
	if c.config != nil && (!c.config.Stats.IsEnabled() || !c.config.Stats.IsTrackEnglish()) {
		return
	}
	c.statCollector.RecordTSFEnglish(chars, digits, puncts, spaces, elapsedMs)
}

// GetStatCollector 获取统计采集器
func (c *Coordinator) GetStatCollector() *store.StatCollector {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.statCollector
}

// SetStatCollector 设置统计采集器
func (c *Coordinator) SetStatCollector(sc *store.StatCollector) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.statCollector = sc
}

// recordCommit 记录一次上屏事件（必须持有锁）
// codeLen: 编码长度（0=标点/直接输入）
// candidatePos: 候选位置（0=首选, -1=非候选）
// source: 上屏来源
func (c *Coordinator) recordCommit(text string, codeLen int, candidatePos int, source store.CommitSource) {
	// 标记 IME 自家提交时间：HandleSelectionChanged 在 grace 窗口内会跳过 OnPhraseTerminated，
	// 避免自家 InsertText 触发的回响 SelectionChanged 把刚 append 的单字立即 flush 掉。
	// 即便统计关闭也要更新该时间戳（造词依赖它，与 stats 解耦）。
	if text != "" {
		c.lastSelfCommitTime = time.Now()
	}

	if c.statCollector == nil || text == "" {
		return
	}
	if c.config != nil && !c.config.Stats.IsEnabled() {
		return
	}

	schemaID := ""
	if c.engineMgr != nil {
		schemaID = c.engineMgr.GetCurrentSchemaID()
	}

	chinese, english, punct, other := store.ClassifyChars(text)

	c.statCollector.Record(store.StatEvent{
		Timestamp:    time.Now(),
		RuneCount:    chinese + english + punct + other,
		ChineseCount: chinese,
		EnglishCount: english,
		PunctCount:   punct,
		OtherCount:   other,
		CodeLen:      codeLen,
		CandidatePos: candidatePos,
		SchemaID:     schemaID,
		Source:       source,
	})
	c.statRecorded = true

	// cmdbar 的 last() 从 c.inputHistory 取最近上屏, 不再维护独立历史;
	// inputHistory.Record 由各 commit 路径在调用 recordCommit 前后按规则
	// 写入 (普通候选 / cmdbar text 上屏 → 记录; 纯 effect 不记录, 见
	// commitCmdbarCandidate 注释)。
}

// recordCommitFallback 在 HandleKeyEvent/HandleCommitRequest 返回 InsertText 时，
// 如果 recordCommit 未被任何具体路径调用，则以通用标点/其他来源记录。
// 这样避免修改 40+ 个返回点，同时保证不遗漏。
func (c *Coordinator) recordCommitFallback(text string) {
	if c.statRecorded || c.statCollector == nil || text == "" {
		return
	}

	// 推测来源：含中文大概率是候选/拼音，纯 ASCII 大概率是标点/全角
	source := store.SourcePunctuation
	for _, r := range text {
		if r > 0x7F {
			source = store.SourceCandidate
			break
		}
	}
	c.recordCommit(text, 0, -1, source)
}
