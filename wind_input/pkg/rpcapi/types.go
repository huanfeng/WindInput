// Package rpcapi 定义 JSON-RPC 的请求/响应类型
// 供服务端和客户端（Wails 设置端）共用
package rpcapi

// RPCPipeName / RPCEventPipeName 已迁至 endpoint_windows.go / endpoint_darwin.go,
// 让两个平台用同一变量名指向不同形态的端点 (Win Named Pipe / darwin Unix Socket)。

// ── Event 类型 ──

// EventType 数据变化事件的类型
type EventType string

const (
	EventTypeConfig   EventType = "config"
	EventTypeUserDict EventType = "userdict"
	EventTypeTemp     EventType = "temp"
	EventTypeShadow   EventType = "shadow"
	EventTypeFreq     EventType = "freq"
	EventTypePhrase   EventType = "phrase"
	EventTypeStats    EventType = "stats"
	EventTypeSystem   EventType = "system"
)

// Valid 校验 EventType 是否为已知值
func (t EventType) Valid() bool {
	switch t {
	case EventTypeConfig, EventTypeUserDict, EventTypeTemp, EventTypeShadow, EventTypeFreq, EventTypePhrase,
		EventTypeStats, EventTypeSystem:
		return true
	}
	return false
}

// EventAction 数据变化事件的动作
type EventAction string

const (
	EventActionAdd      EventAction = "add"
	EventActionRemove   EventAction = "remove"
	EventActionUpdate   EventAction = "update"
	EventActionClear    EventAction = "clear"
	EventActionReset    EventAction = "reset"
	EventActionBatchPut EventAction = "batch_put"
	EventActionBatchAdd EventAction = "batch_add"
	EventActionBatchSet EventAction = "batch_set"
	EventActionUpdated  EventAction = "updated"
	EventActionPaused   EventAction = "paused"
	EventActionResumed  EventAction = "resumed"
)

// Valid 校验 EventAction 是否为已知值
func (a EventAction) Valid() bool {
	switch a {
	case EventActionAdd, EventActionRemove, EventActionUpdate, EventActionClear,
		EventActionReset, EventActionBatchPut, EventActionBatchAdd, EventActionBatchSet,
		EventActionUpdated, EventActionPaused, EventActionResumed:
		return true
	}
	return false
}

// EventMessage 数据变化事件
type EventMessage struct {
	Type     EventType   `json:"type"`
	SchemaID string      `json:"schema_id,omitempty"` // 方案 ID
	Action   EventAction `json:"action"`
}

// ── Wails 前端事件名 ──

const (
	WailsEventConfig = "config-event"
	WailsEventDict   = "dict-event"
	WailsEventStats  = "stats-event"
	WailsEventSystem = "system-event"
)

// ── Config Section ──

// ConfigSection 配置分区标识
type ConfigSection string

const (
	ConfigSectionStartup  ConfigSection = "startup"
	ConfigSectionSchema   ConfigSection = "schema"
	ConfigSectionHotkeys  ConfigSection = "hotkeys"
	ConfigSectionUI       ConfigSection = "ui"
	ConfigSectionToolbar  ConfigSection = "toolbar"
	ConfigSectionInput    ConfigSection = "input"
	ConfigSectionAdvanced ConfigSection = "advanced"
	ConfigSectionStats    ConfigSection = "stats"
	ConfigSectionS2T      ConfigSection = "s2t"
)

// Valid 校验 ConfigSection 是否为已知值
func (s ConfigSection) Valid() bool {
	switch s {
	case ConfigSectionStartup, ConfigSectionSchema, ConfigSectionHotkeys, ConfigSectionUI,
		ConfigSectionToolbar, ConfigSectionInput, ConfigSectionAdvanced, ConfigSectionStats,
		ConfigSectionS2T:
		return true
	}
	return false
}

// ── Dict 服务类型 ──

// DictSearchArgs 词库搜索请求
type DictSearchArgs struct {
	SchemaID  string `json:"schema_id,omitempty"`  // 方案 ID（空=当前活跃方案）
	Prefix    string `json:"prefix"`               // 编码前缀
	TextQuery string `json:"text_query,omitempty"` // 词条内容包含匹配（与 Prefix 取并集）
	Limit     int    `json:"limit,omitempty"`      // 每页数量（默认 50）
	Offset    int    `json:"offset,omitempty"`     // 偏移量
}

// DictSearchReply 词库搜索响应
type DictSearchReply struct {
	Words []WordEntry `json:"words"`
	Total int         `json:"total"` // 总数（用于分页）
}

// WordEntry 词条
type WordEntry struct {
	Code      string `json:"code"`
	Text      string `json:"text"`
	Weight    int    `json:"weight"`
	Count     int    `json:"count,omitempty"`
	CreatedAt int64  `json:"created_at,omitempty"`
}

// DictAddArgs 添加词条请求
type DictAddArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Text     string `json:"text"`
	Weight   int    `json:"weight"`
}

// DictRemoveArgs 删除词条请求
type DictRemoveArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Text     string `json:"text"`
}

// DictUpdateArgs 更新词条权重请求
type DictUpdateArgs struct {
	SchemaID  string `json:"schema_id,omitempty"`
	Code      string `json:"code"`
	Text      string `json:"text"`
	NewWeight int    `json:"new_weight"`
}

// DictStatsReply 词库统计响应
type DictStatsReply struct {
	Stats map[string]int `json:"stats"`
}

// ── Shadow 服务类型 ──
//
// 2026-05-17 R2: 新增 CandID 字段以匹配动态候选 (短语模板每次展开 Text 不同)。
// 匹配优先级见 ApplyShadowPins: rule.CandID 非空 → 按 cand.ID 匹配;
// 否则按 rule.Word 匹配 cand.Text (向后兼容手输文本规则)。
// 详见 docs/design/command-bar-followup.md R2 方案 Step 2。

// ShadowPinArgs 置顶请求
type ShadowPinArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Word     string `json:"word"`
	// CandID 候选稳定 id (deterministic, 通常 "phrase:<code>:<template>"),
	// 优先于 Word 匹配; 空字符串表示按 Word 匹配 cand.Text (旧行为)。
	CandID   string `json:"cand_id,omitempty"`
	Position int    `json:"position"`
}

// ShadowDeleteArgs 隐藏/移除请求
type ShadowDeleteArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Word     string `json:"word"`
	// CandID 候选稳定 id, 与 ShadowPinArgs.CandID 同语义。
	CandID string `json:"cand_id,omitempty"`
}

// ShadowGetRulesArgs 获取规则请求
type ShadowGetRulesArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
}

// ShadowRulesReply 规则响应
type ShadowRulesReply struct {
	Pinned  []PinnedEntry        `json:"pinned,omitempty"`
	Deleted []ShadowDeletedEntry `json:"deleted,omitempty"`
}

// PinnedEntry 置顶条目
type PinnedEntry struct {
	Word     string `json:"word"`
	CandID   string `json:"cand_id,omitempty"` // 候选稳定 id, 优先于 Word 匹配 (见 ShadowPinArgs)
	Position int    `json:"position"`
}

// ShadowDeletedEntry 删除规则条目 (2026-05-17 升级, 替代 []string)。
//
// 2026-05-17 R2 后续: Bug 2 修复时 ShadowDelete 结构已带 CandID, 但
// RPC GetAllRules.Deleted / GetRules.Deleted 当时只暴露 word 字段, 导致
// 设置 UI 无法定位短语候选规则。本结构补齐 CandID, 让 UI 端能按 id
// 删除短语 delete 规则。
type ShadowDeletedEntry struct {
	Word   string `json:"word"`
	CandID string `json:"cand_id,omitempty"` // 候选稳定 id, 同 PinnedEntry.CandID 语义
}

// DictGetTempArgs 临时词库查询请求
type DictGetTempArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Prefix   string `json:"prefix,omitempty"`
	Limit    int    `json:"limit,omitempty"`
	Offset   int    `json:"offset,omitempty"`
}

// DictClearTempArgs 清空临时词库请求
type DictClearTempArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
}

// DictClearTempReply 清空临时词库响应
type DictClearTempReply struct {
	Count int `json:"count"`
}

// DictPromoteTempArgs 临时词条晋升请求
type DictPromoteTempArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Text     string `json:"text"`
}

// DictPromoteAllTempArgs 全部晋升请求
type DictPromoteAllTempArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
}

// DictPromoteAllTempReply 全部晋升响应
type DictPromoteAllTempReply struct {
	Count int `json:"count"`
}

// DictRemoveTempArgs 删除临时词条请求
type DictRemoveTempArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Text     string `json:"text"`
}

// DictSchemaStatsArgs 方案统计请求
type DictSchemaStatsArgs struct {
	SchemaID string `json:"schema_id"`
}

// DictSchemaStatsReply 方案统计响应
type DictSchemaStatsReply struct {
	DataSchemaID  string `json:"data_schema_id"` // 实际存储桶 ID（经 SchemaIDMapper 解析后）
	WordCount     int    `json:"word_count"`
	ShadowCount   int    `json:"shadow_count"`
	TempWordCount int    `json:"temp_word_count"`
}

// DictClearUserWordsArgs 清空用户词库请求
type DictClearUserWordsArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
}

// DictClearUserWordsReply 清空用户词库响应
type DictClearUserWordsReply struct {
	Count int `json:"count"`
}

// DictBatchAddArgs 批量添加请求
type DictBatchAddArgs struct {
	SchemaID string      `json:"schema_id,omitempty"`
	Words    []WordEntry `json:"words"`
}

// DictBatchAddReply 批量添加响应
type DictBatchAddReply struct {
	Count int `json:"count"`
}

// ── Shadow 扩展类型 ──

// ShadowGetAllRulesArgs 获取所有规则请求
type ShadowGetAllRulesArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
}

// ShadowGetAllRulesReply 所有规则响应
type ShadowGetAllRulesReply struct {
	Rules []ShadowCodeRules `json:"rules"`
}

// ShadowCodeRules 单个编码下的规则
type ShadowCodeRules struct {
	Code    string               `json:"code"`
	Pinned  []PinnedEntry        `json:"pinned,omitempty"`
	Deleted []ShadowDeletedEntry `json:"deleted,omitempty"` // 2026-05-17 从 []string 升级以承载 CandID
}

// ── System 服务类型 ──

// Empty 空参数/响应
type Empty struct{}

// SystemResetDBArgs 重置数据库请求
type SystemResetDBArgs struct {
	SchemaID string `json:"schema_id,omitempty"` // 指定方案（空=全部清除）
}

// SystemResetDBReply 重置数据库响应
type SystemResetDBReply struct {
	Success bool `json:"success"`
}

// SystemRebuildDictCacheReply 重建词库缓存响应
type SystemRebuildDictCacheReply struct {
	Deleted int `json:"deleted"` // 已删除的缓存文件数量
}

// ── Phrase 服务类型 ──
//
// 2026-05-16 schema 简化: 短语统一为 (code, text, weight) 三元组,
// 不再保留派生字段 Type/Texts/Name。短语分类 (普通 / $AA / $SS / $CC)
// 完全由 PhraseLayer 在 LoadFromStore 时从 Text 内容推断。

// PhraseEntry 短语条目
type PhraseEntry struct {
	Code string `json:"code"`
	Text string `json:"text,omitempty"`
	// Weight 是显式权重 (0~10000, 与码表/拼音范化后同区间),
	// 0 表示未设置, 后端走 Position fallback。
	Weight   int  `json:"weight,omitempty"`
	Position int  `json:"position"`
	Enabled  bool `json:"enabled"`
	IsSystem bool `json:"is_system"`
}

// PhraseListReply 短语列表响应
type PhraseListReply struct {
	Phrases []PhraseEntry `json:"phrases"`
	Total   int           `json:"total"`
}

// PhraseAddArgs 添加短语请求
type PhraseAddArgs struct {
	Code string `json:"code"`
	Text string `json:"text,omitempty"`
	// Weight 显式权重 (0~10000), 优先于 Position; 0 表示未设置。
	Weight   int `json:"weight,omitempty"`
	Position int `json:"position"`
}

// PhraseRemoveArgs 删除短语请求
type PhraseRemoveArgs struct {
	Code string `json:"code"`
	Text string `json:"text,omitempty"`
}

// PhraseUpdateArgs 更新短语请求
type PhraseUpdateArgs struct {
	Code        string `json:"code"`
	Text        string `json:"text,omitempty"`
	NewCode     string `json:"new_code,omitempty"`
	NewText     string `json:"new_text,omitempty"`
	NewPosition int    `json:"new_position,omitempty"`
	// NewWeight 是新的显式权重 (0~10000), 0 表示不修改;
	// 用 *int 区分"显式设为 0" (清空) 与 "不修改"。
	NewWeight *int  `json:"new_weight,omitempty"`
	Enabled   *bool `json:"enabled,omitempty"`
}

// ── Freq 服务类型 ──

// FreqSearchArgs 词频搜索请求
type FreqSearchArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Prefix   string `json:"prefix,omitempty"`
	Limit    int    `json:"limit,omitempty"`
	Offset   int    `json:"offset,omitempty"`
}

// FreqEntryItem 词频条目
type FreqEntryItem struct {
	Code     string `json:"code"`
	Text     string `json:"text"`
	Count    int    `json:"count"`
	LastUsed int64  `json:"last_used"`
	Streak   int    `json:"streak"`
	Boost    int    `json:"boost"`
}

// FreqSearchReply 词频搜索响应
type FreqSearchReply struct {
	Entries []FreqEntryItem `json:"entries"`
	Total   int             `json:"total"`
}

// FreqDeleteArgs 删除词频请求
type FreqDeleteArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Text     string `json:"text"`
}

// FreqClearArgs 清空词频请求
type FreqClearArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
}

// FreqClearReply 清空词频响应
type FreqClearReply struct {
	Count int `json:"count"`
}

// ── System 扩展类型 ──

// SchemaStatus 方案状态
type SchemaStatus struct {
	SchemaID    string `json:"schema_id"`
	Status      string `json:"status"` // "enabled" | "disabled" | "orphaned"
	UserWords   int    `json:"user_words"`
	TempWords   int    `json:"temp_words"`
	ShadowRules int    `json:"shadow_rules"`
	FreqRecords int    `json:"freq_records"`
}

// ListSchemasReply 方案列表响应
type ListSchemasReply struct {
	Schemas []SchemaStatus `json:"schemas"`
}

// SystemStatusReply 系统状态响应
type SystemStatusReply struct {
	Running      bool   `json:"running"`
	SchemaID     string `json:"schema_id"`
	EngineType   string `json:"engine_type"`
	ChineseMode  bool   `json:"chinese_mode"`
	FullWidth    bool   `json:"full_width"`
	ChinesePunct bool   `json:"chinese_punct"`
	StoreEnabled bool   `json:"store_enabled"`
	UserWords    int    `json:"user_words"`
	TempWords    int    `json:"temp_words"`
	Phrases      int    `json:"phrases"`
	ShadowRules  int    `json:"shadow_rules"`
}

// NotifyReloadArgs 通知重载请求
type NotifyReloadArgs struct {
	Target string `json:"target"` // "config" | "phrases" | "shadow" | "userdict" | "all"
}

// SystemShutdownReply 关闭服务响应
type SystemShutdownReply struct {
	OK bool `json:"ok"`
}

// SystemPauseReply 暂停服务响应
type SystemPauseReply struct {
	OK bool `json:"ok"`
}

// SystemResumeArgs 恢复服务请求
type SystemResumeArgs struct {
	NewDataDir string `json:"new_data_dir,omitempty"` // 如果非空，使用新的数据目录恢复
}

// SystemResumeReply 恢复服务响应
type SystemResumeReply struct {
	OK bool `json:"ok"`
}

// ── 导入导出扩展类型 ──

// BatchEncodeArgs 批量反向编码请求
type BatchEncodeArgs struct {
	SchemaID string   `json:"schema_id,omitempty"`
	Words    []string `json:"words"`
}

// EncodeResultItem 单个词语的编码结果
type EncodeResultItem struct {
	Word   string `json:"word"`
	Code   string `json:"code"`
	Status string `json:"status"` // ok, no_code, no_rule
	Error  string `json:"error,omitempty"`
}

// BatchEncodeReply 批量反向编码响应
type BatchEncodeReply struct {
	Results []EncodeResultItem `json:"results"`
}

// FreqBatchPutArgs 批量写入词频请求
type FreqBatchPutArgs struct {
	SchemaID string         `json:"schema_id,omitempty"`
	Entries  []FreqPutEntry `json:"entries"`
}

// FreqPutEntry 单条词频写入条目
type FreqPutEntry struct {
	Code     string `json:"code"`
	Text     string `json:"text"`
	Count    uint32 `json:"count"`
	LastUsed int64  `json:"last_used"`
	Streak   uint8  `json:"streak"`
}

// FreqBatchPutReply 批量写入词频响应
type FreqBatchPutReply struct {
	Count int `json:"count"`
}

// ShadowBatchSetArgs 批量写入 Shadow 规则请求
type ShadowBatchSetArgs struct {
	SchemaID string          `json:"schema_id,omitempty"`
	Pins     []ShadowPinItem `json:"pins,omitempty"`
	Deletes  []ShadowDelItem `json:"deletes,omitempty"`
}

// ShadowPinItem 批量 Pin 条目
type ShadowPinItem struct {
	Code     string `json:"code"`
	Word     string `json:"word"`
	CandID   string `json:"cand_id,omitempty"` // 候选稳定 id (见 ShadowPinArgs)
	Position int    `json:"position"`
}

// ShadowDelItem 批量 Delete 条目
type ShadowDelItem struct {
	Code   string `json:"code"`
	Word   string `json:"word"`
	CandID string `json:"cand_id,omitempty"` // 候选稳定 id (见 ShadowDeleteArgs)
}

// ShadowBatchSetReply 批量写入 Shadow 响应
type ShadowBatchSetReply struct {
	PinCount int `json:"pin_count"`
	DelCount int `json:"del_count"`
}

// ── Phrase: cmdbar 值校验 ──

// PhraseValidateValueArgs 校验短语 value 的请求 (无方案/上下文依赖, 纯解析)。
type PhraseValidateValueArgs struct {
	Value string `json:"value"`
}

// PhraseValidateValueReply 校验响应。
// Kind 取值:
//   - "array"          含 $AA("name","chars") 字符组 marker
//     (Display="<name> · N 字", ActionsCount = 字符数)
//   - "command"        含 $CC(  且解析成功 (仅精确匹配)
//   - "command-prefix" 含 $CC1( 且解析成功 (精确 + 前缀都匹配)
//   - "template"       含已知 $X 模板变量 (date/uuid 等)
//   - "literal"        纯字面量
//   - "error"          解析失败 (ErrorMsg 给出原因)
type PhraseValidateValueReply struct {
	Kind         string `json:"kind"`
	Display      string `json:"display,omitempty"`       // command/template 展开后的显示文本
	ActionsCount int    `json:"actions_count,omitempty"` // command 类型的 action 数
	ErrorMsg     string `json:"error_msg,omitempty"`     // error 类型的错误描述
}

// PhraseBatchAddArgs 批量添加短语请求
type PhraseBatchAddArgs struct {
	Phrases []PhraseAddArgs `json:"phrases"`
}

// PhraseBatchAddReply 批量添加短语响应
type PhraseBatchAddReply struct {
	Count int `json:"count"`
}

// PhraseBatchRemoveArgs 批量删除短语请求 (单事务执行, 减少 reload 次数)
type PhraseBatchRemoveArgs struct {
	Items []PhraseRemoveArgs `json:"items"`
}

// PhraseBatchRemoveReply 批量删除短语响应
type PhraseBatchRemoveReply struct {
	Count int `json:"count"`
}

// ── Stats 服务类型 ──

// StatsGetDailyArgs 获取每日统计请求
type StatsGetDailyArgs struct {
	From string `json:"from"` // 开始日期 "2006-01-02"
	To   string `json:"to"`   // 结束日期 "2006-01-02"
}

// StatsDailyItem 每日统计条目
type StatsDailyItem struct {
	Date          string                      `json:"d"`
	TotalChars    int                         `json:"tc"`
	ChineseChars  int                         `json:"cc"`
	EnglishChars  int                         `json:"ec"`
	PunctChars    int                         `json:"pc"`
	OtherChars    int                         `json:"oc"`
	Hours         [24]int                     `json:"h"`
	CommitCount   int                         `json:"cn"`
	CodeLenSum    int                         `json:"cls"`
	CodeLenCount  int                         `json:"clc"`
	CodeLenDist   [6]int                      `json:"cld"`
	CandPosDist   [5]int                      `json:"cpd"`
	ActiveSeconds int                         `json:"as"`
	BySchema      map[string]*SchemaStatsItem `json:"bs,omitempty"`
	BySource      [10]int                     `json:"src"`
}

// SchemaStatsItem 方案统计条目
type SchemaStatsItem struct {
	TotalChars   int    `json:"tc"`
	CommitCount  int    `json:"cn"`
	CodeLenSum   int    `json:"cls"`
	CodeLenCount int    `json:"clc"`
	CandPosDist  [5]int `json:"cpd"`
}

// StatsGetDailyReply 每日统计响应
type StatsGetDailyReply struct {
	Days []StatsDailyItem `json:"days"`
}

// StatsSummaryReply 统计概览响应
type StatsSummaryReply struct {
	TodayChars      int     `json:"today_chars"`
	TodayChinese    int     `json:"today_chinese"`
	TodayEnglish    int     `json:"today_english"`
	TotalChars      int64   `json:"total_chars"`
	ActiveDays      int     `json:"active_days"`
	DailyAvg        int     `json:"daily_avg"`
	StreakCurrent   int     `json:"streak_current"`
	StreakMax       int     `json:"streak_max"`
	WeekChars       int     `json:"week_chars"`
	MonthChars      int     `json:"month_chars"`
	MaxDayChars     int     `json:"max_day_chars"`
	MaxDayDate      string  `json:"max_day_date"`
	AvgCodeLen      float64 `json:"avg_code_len"`
	FirstSelectRate float64 `json:"first_select_rate"`
	TodaySpeed      int     `json:"today_speed"`   // 今日平均速度（字/分钟）
	OverallSpeed    int     `json:"overall_speed"` // 统计区间平均速度（字/分钟）
	MaxSpeed        int     `json:"max_speed"`     // 历史最快速度（字/分钟）
}

// StatsPruneArgs 清理指定天数之前的统计数据
type StatsPruneArgs struct {
	Days int `json:"days"`
}

// StatsPruneReply 清理统计数据响应
type StatsPruneReply struct {
	Count  int    `json:"count"`
	Before string `json:"before"`
}

// ── Config 服务类型 ──

type ConfigGetAllReply struct {
	Config []byte `json:"config"` // JSON-encoded config.Config
}

type ConfigGetArgs struct {
	Keys []string `json:"keys"`
}

type ConfigGetReply struct {
	Values map[string]any `json:"values"`
}

type ConfigSetItem struct {
	Key   string `json:"key"`
	Value any    `json:"value"`
}

type ConfigSetArgs struct {
	Items []ConfigSetItem `json:"items"`
}

type ConfigSetReply struct {
	Applied         []string `json:"applied"`
	RequiresRestart bool     `json:"requires_restart"`
}

type ConfigSetAllArgs struct {
	Config []byte `json:"config"` // JSON-encoded config.Config
}

type ConfigSetAllReply struct {
	Applied         []string `json:"applied"`
	RequiresRestart bool     `json:"requires_restart"`
}

type ConfigGetDefaultsReply struct {
	Config []byte `json:"config"` // JSON-encoded config.Config
}

type ConfigResetArgs struct {
	Keys []string `json:"keys"`
}

type ConfigResetReply struct {
	Reset []string `json:"reset"`
}

// ── Schema Override 类型 ──

type SchemaOverrideArgs struct {
	SchemaID string `json:"schema_id"`
}

type SchemaOverrideReply struct {
	Data map[string]any `json:"data,omitempty"`
}

type SchemaOverrideSetArgs struct {
	SchemaID string         `json:"schema_id"`
	Data     map[string]any `json:"data"`
}

type SetActiveSchemaArgs struct {
	SchemaID string `json:"schema_id"`
}

// ── Perf（性能采样）类型 ──

// SystemDumpPerfArgs 主动导出按键链路性能样本到 JSONL 文件。
// Path 留空时由服务端选择默认路径（一般为日志目录下 perf_<timestamp>.jsonl）。
// Clear=true 表示导出后清空内存缓冲。
type SystemDumpPerfArgs struct {
	Path  string `json:"path,omitempty"`
	Clear bool   `json:"clear,omitempty"`
}

type SystemDumpPerfReply struct {
	Path      string `json:"path"`
	Count     int    `json:"count"`
	Summary   string `json:"summary"`             // 单行可读统计摘要
	Content   string `json:"content,omitempty"`   // 完整 JSONL 内容（仅 ReadPerfFile 填充）
	Cancelled bool   `json:"cancelled,omitempty"` // 用户取消保存对话框
}

// SystemPerfStatsReply 不落盘，直接返回当前的统计摘要。
type SystemPerfStatsReply struct {
	Count    int    `json:"count"`
	Capacity int    `json:"capacity"`
	Summary  string `json:"summary"`
}

// ── Dict 拼音编码生成 ──

// DictGeneratePinyinCodeArgs 生成拼音编码请求
type DictGeneratePinyinCodeArgs struct {
	Text string `json:"text"`
}

// DictGeneratePinyinCodeReply 生成拼音编码响应
type DictGeneratePinyinCodeReply struct {
	Code string `json:"code"` // 全拼编码，如 "nihao"；空串表示无法生成
}

// ── 备份/还原/重置 ──

type SchemaBackupStats struct {
	SchemaID      string `json:"schema_id"`
	UserWordCount int    `json:"user_word_count"`
	TempWordCount int    `json:"temp_word_count"`
	FreqCount     int    `json:"freq_count"`
	PhraseCount   int    `json:"phrase_count"`
}

type BackupPreview struct {
	Schemas       []SchemaBackupStats `json:"schemas"`
	GlobalPhrases int                 `json:"global_phrases"`
	ThemeCount    int                 `json:"theme_count"`
	StatsDays     int                 `json:"stats_days"`
	EstimatedSize int64               `json:"estimated_size"`
}

type RestorePreview struct {
	CreatedAt     string              `json:"created_at"`
	AppVersion    string              `json:"app_version"`
	DataDirMode   string              `json:"data_dir_mode"`
	Schemas       []SchemaBackupStats `json:"schemas"`
	GlobalPhrases int                 `json:"global_phrases"`
	ThemeCount    int                 `json:"theme_count"`
	StatsDays     int                 `json:"stats_days"`
	TotalSize     int64               `json:"total_size"`
}

type SystemBackupArgs struct {
	ZipPath string `json:"zip_path"`
}

type SystemBackupReply struct {
	Path string `json:"path"`
}

type SystemRestoreArgs struct {
	ZipPath string `json:"zip_path"`
}

type SystemRestoreReply struct {
	OK bool `json:"ok"`
}

type SystemResetReply struct {
	OK bool `json:"ok"`
}

// ── 内存诊断 ──

// GoMemStatsReply Go runtime 内存统计响应
type GoMemStatsReply struct {
	HeapAlloc    uint64 `json:"heap_alloc"`     // 当前堆上活跃对象占用字节
	HeapSys      uint64 `json:"heap_sys"`       // 向 OS 申请的堆虚拟内存总量
	HeapIdle     uint64 `json:"heap_idle"`      // 空闲但未归还 OS 的 span 字节
	HeapInuse    uint64 `json:"heap_inuse"`     // 正在使用的 span 字节
	HeapReleased uint64 `json:"heap_released"`  // 已归还 OS 的空闲 span 字节
	HeapObjects  uint64 `json:"heap_objects"`   // 当前堆上活跃对象数量
	StackInuse   uint64 `json:"stack_inuse"`    // 栈 span 占用字节
	StackSys     uint64 `json:"stack_sys"`      // 向 OS 申请的栈字节
	Sys          uint64 `json:"sys"`            // 向 OS 申请的虚拟内存总量（含堆、栈、元数据等）
	NumGC        uint32 `json:"num_gc"`         // 累计 GC 次数
	PauseTotalNs uint64 `json:"pause_total_ns"` // GC 累计暂停时间（纳秒）
	GCSys        uint64 `json:"gc_sys"`         // GC 元数据字节
	OtherSys     uint64 `json:"other_sys"`      // 其他系统字节（mspan、mcache 等）
}

// DumpHeapProfileArgs 导出堆内存 profile 请求
type DumpHeapProfileArgs struct {
	Path string `json:"path,omitempty"` // 输出路径；空串表示由服务端选择默认路径（datadir/diag/）
}

// DumpHeapProfileReply 导出堆内存 profile 响应
type DumpHeapProfileReply struct {
	Path string `json:"path"` // 实际写入的文件路径
}
