// Package rpcapi 定义 JSON-RPC 的请求/响应类型
// 供服务端和客户端（Wails 设置端）共用
package rpcapi

import "github.com/huanfeng/wind_input/pkg/buildvariant"

// RPC 管道名称
var RPCPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_rpc`

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

// SystemStatusReply 系统状态响应
type SystemStatusReply struct {
	Running      bool   `json:"running"`
	SchemaID     string `json:"schema_id"`
	EngineType   string `json:"engine_type"`
	ChineseMode  bool   `json:"chinese_mode"`
	StoreEnabled bool   `json:"store_enabled"`
	UserWords    int    `json:"user_words"`
	TempWords    int    `json:"temp_words"`
	Phrases      int    `json:"phrases"`
	ShadowRules  int    `json:"shadow_rules"`
}
