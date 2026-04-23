// Package rpcapi 定义 JSON-RPC 的请求/响应类型
// 供服务端和客户端（Wails 设置端）共用
package rpcapi

import "github.com/huanfeng/wind_input/pkg/buildvariant"

// RPC 管道名称
var RPCPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_rpc`

// ── Event 类型 ──

// RPCEventPipeName 事件推送管道名称
var RPCEventPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_events`

// EventMessage 数据变化事件
type EventMessage struct {
	Type     string `json:"type"`                // "userdict" | "temp" | "shadow" | "freq" | "phrase"
	SchemaID string `json:"schema_id,omitempty"` // 方案 ID
	Action   string `json:"action"`              // "add" | "remove" | "update" | "clear" | "reset"
}

// ── Dict 服务类型 ──

// DictSearchArgs 词库搜索请求
type DictSearchArgs struct {
	SchemaID string `json:"schema_id,omitempty"` // 方案 ID（空=当前活跃方案）
	Prefix   string `json:"prefix"`              // 编码前缀
	Limit    int    `json:"limit,omitempty"`     // 每页数量（默认 50）
	Offset   int    `json:"offset,omitempty"`    // 偏移量
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

// ShadowPinArgs 置顶请求
type ShadowPinArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Word     string `json:"word"`
	Position int    `json:"position"`
}

// ShadowDeleteArgs 隐藏/移除请求
type ShadowDeleteArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Word     string `json:"word"`
}

// ShadowGetRulesArgs 获取规则请求
type ShadowGetRulesArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
}

// ShadowRulesReply 规则响应
type ShadowRulesReply struct {
	Pinned  []PinnedEntry `json:"pinned,omitempty"`
	Deleted []string      `json:"deleted,omitempty"`
}

// PinnedEntry 置顶条目
type PinnedEntry struct {
	Word     string `json:"word"`
	Position int    `json:"position"`
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
	WordCount     int `json:"word_count"`
	ShadowCount   int `json:"shadow_count"`
	TempWordCount int `json:"temp_word_count"`
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
	Code    string        `json:"code"`
	Pinned  []PinnedEntry `json:"pinned,omitempty"`
	Deleted []string      `json:"deleted,omitempty"`
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

// ── Phrase 服务类型 ──

// PhraseEntry 短语条目
type PhraseEntry struct {
	Code     string `json:"code"`
	Text     string `json:"text,omitempty"`
	Texts    string `json:"texts,omitempty"`
	Name     string `json:"name,omitempty"`
	Type     string `json:"type"`
	Position int    `json:"position"`
	Enabled  bool   `json:"enabled"`
	IsSystem bool   `json:"is_system"`
}

// PhraseListReply 短语列表响应
type PhraseListReply struct {
	Phrases []PhraseEntry `json:"phrases"`
	Total   int           `json:"total"`
}

// PhraseAddArgs 添加短语请求
type PhraseAddArgs struct {
	Code     string `json:"code"`
	Text     string `json:"text,omitempty"`
	Texts    string `json:"texts,omitempty"`
	Name     string `json:"name,omitempty"`
	Type     string `json:"type"`
	Position int    `json:"position"`
}

// PhraseRemoveArgs 删除短语请求
type PhraseRemoveArgs struct {
	Code string `json:"code"`
	Text string `json:"text,omitempty"`
	Name string `json:"name,omitempty"`
}

// PhraseUpdateArgs 更新短语请求
type PhraseUpdateArgs struct {
	Code        string `json:"code"`
	Text        string `json:"text,omitempty"`
	Name        string `json:"name,omitempty"`
	NewText     string `json:"new_text,omitempty"`
	NewPosition int    `json:"new_position,omitempty"`
	Enabled     *bool  `json:"enabled,omitempty"`
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
	Position int    `json:"position"`
}

// ShadowDelItem 批量 Delete 条目
type ShadowDelItem struct {
	Code string `json:"code"`
	Word string `json:"word"`
}

// ShadowBatchSetReply 批量写入 Shadow 响应
type ShadowBatchSetReply struct {
	PinCount int `json:"pin_count"`
	DelCount int `json:"del_count"`
}

// PhraseBatchAddArgs 批量添加短语请求
type PhraseBatchAddArgs struct {
	Phrases []PhraseAddArgs `json:"phrases"`
}

// PhraseBatchAddReply 批量添加短语响应
type PhraseBatchAddReply struct {
	Count int `json:"count"`
}
