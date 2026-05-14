// Package dictio 提供词库数据的导入导出功能。
//
// 支持自定义 WindDict 格式（.wdict.yaml）以及多种第三方格式的导入。
// WindDict 格式采用 YAML 头部 + 一行一词 TSV 数据段的混合结构，
// 兼顾可读性与大数据量场景下的性能。
package dictio

import (
	"io"
	"slices"
)

// Section 名称常量
const (
	SectionUserWords = "user_words"
	SectionTempWords = "temp_words"
	SectionFreq      = "freq"
	SectionShadow    = "shadow"
	SectionPhrases   = "phrases"
)

// AllSchemaSections 所有与方案绑定的 section（不含 phrases）
var AllSchemaSections = []string{SectionUserWords, SectionTempWords, SectionFreq, SectionShadow}

// AllSections 所有 section
var AllSections = []string{SectionUserWords, SectionTempWords, SectionFreq, SectionShadow, SectionPhrases}

// FormatVersion 当前格式版本
const FormatVersion = 1

// ---- YAML 头部结构 ----

// WindDictHeader 是 .wdict.yaml 文件的 YAML 头部。
type WindDictHeader struct {
	Version    int                    `yaml:"version"`
	Generator  string                 `yaml:"generator"`
	ExportedAt string                 `yaml:"exported_at"`
	SchemaID   string                 `yaml:"schema_id,omitempty"`
	SchemaName string                 `yaml:"schema_name,omitempty"`
	Sections   map[string]SectionMeta `yaml:"sections"`
}

// SectionMeta 描述一个数据段的列定义。
type SectionMeta struct {
	Columns []string `yaml:"columns"`
}

// ---- 统一数据条目类型 ----

// UserWordEntry 用户词库/临时词库条目。
type UserWordEntry struct {
	Code      string
	Text      string
	Weight    int
	Count     int
	CreatedAt int64
}

// FreqEntry 词频条目。
type FreqEntry struct {
	Code     string
	Text     string
	Count    uint32
	LastUsed int64
	Streak   uint8
}

// ShadowPinEntry 候选置顶规则。
type ShadowPinEntry struct {
	Code     string
	Word     string
	Position int
}

// ShadowDelEntry 候选隐藏规则。
type ShadowDelEntry struct {
	Code string
	Word string
}

// PhraseEntry 短语条目。
//
// 短语类型 (static/dynamic/array) 由 Text 字段内容自描述:
// 含 $AA(...) marker 视为字符组 (array), 含 $X / $CC 等模板视为
// dynamic, 其余为 static, 不再单独存 Type 字段。
//
// Weight 为显式权重 (0~10000); 为 0 时调用方回退到 Position
// 推导的兼容权重 (见 internal/dict.resolvePhraseWeight)。
type PhraseEntry struct {
	Code     string
	Text     string
	Weight   int
	Position int
	Enabled  bool
}

// ---- 导入结果 ----

// ImportResult 是所有导入器的统一输出。
type ImportResult struct {
	UserWords  []UserWordEntry
	TempWords  []UserWordEntry
	FreqData   []FreqEntry
	ShadowPins []ShadowPinEntry
	ShadowDels []ShadowDelEntry
	Phrases    []PhraseEntry
	Stats      ImportStats
	Warnings   []string
}

// ImportStats 导入统计信息。
type ImportStats struct {
	UserWordsCount int
	TempWordsCount int
	FreqCount      int
	ShadowPinCount int
	ShadowDelCount int
	PhraseCount    int
	SkippedCount   int
}

// UpdateStats 根据当前数据更新统计计数（保留已有的 SkippedCount）。
func (r *ImportResult) UpdateStats() {
	skipped := r.Stats.SkippedCount
	r.Stats = ImportStats{
		UserWordsCount: len(r.UserWords),
		TempWordsCount: len(r.TempWords),
		FreqCount:      len(r.FreqData),
		ShadowPinCount: len(r.ShadowPins),
		ShadowDelCount: len(r.ShadowDels),
		PhraseCount:    len(r.Phrases),
		SkippedCount:   skipped,
	}
}

// ---- 导出数据 ----

// ShadowRecord 聚合某个编码下的所有 Shadow 规则。
type ShadowRecord struct {
	Pinned  []ShadowPinEntry
	Deleted []string
}

// ExportData 是导出器的输入，从 Store 收集的完整数据。
type ExportData struct {
	UserWords []UserWordEntry
	TempWords []UserWordEntry
	FreqData  []FreqEntry
	Shadow    map[string]ShadowRecord // code → record
	Phrases   []PhraseEntry
}

// ---- 合并策略 ----

// MergeStrategy 导入时的冲突合并策略。
type MergeStrategy int

const (
	MergeMaxWeight    MergeStrategy = iota // 取较大权重（用户词库/临时词库默认）
	MergeOverwrite                         // 新数据覆盖旧数据
	MergeKeepExisting                      // 保留已有，跳过冲突
	MergeAddWeight                         // 权重相加
)

// FreqMergeStrategy 词频数据的合并策略。
type FreqMergeStrategy int

const (
	FreqMergeAccumulate   FreqMergeStrategy = iota // count 累加，last_used 取较新（默认）
	FreqMergeOverwrite                             // 完全覆盖
	FreqMergeKeepExisting                          // 保留已有
)

// ---- 导入器接口 ----

// Importer 词库导入器接口。
type Importer interface {
	// Name 返回导入器的显示名称。
	Name() string
	// Extensions 返回支持的文件扩展名（含点号，如 ".txt"）。
	Extensions() []string
	// Import 从 reader 中解析数据，返回统一的导入结果。
	Import(r io.Reader, opts ImportOptions) (*ImportResult, error)
}

// ImportOptions 导入选项。
type ImportOptions struct {
	SchemaID string   // 目标方案 ID
	Sections []string // 要导入的 section（nil = 全部）
}

// ShouldImport 判断指定 section 是否在导入范围内。
func (o ImportOptions) ShouldImport(section string) bool {
	if len(o.Sections) == 0 {
		return true
	}
	return slices.Contains(o.Sections, section)
}

// ---- 导出器接口 ----

// Exporter 词库导出器接口。
type Exporter interface {
	// Name 返回导出器的显示名称。
	Name() string
	// Extension 返回输出文件的扩展名。
	Extension() string
	// Export 将数据写入 writer。
	Export(w io.Writer, data *ExportData, opts ExportOptions) error
}

// ExportOptions 导出选项。
type ExportOptions struct {
	SchemaID   string
	SchemaName string
	Sections   []string // 要导出的 section（nil = 全部有数据的 section）
	Generator  string   // 程序版本标识
}

// ShouldExport 判断指定 section 是否在导出范围内。
func (o ExportOptions) ShouldExport(section string) bool {
	if len(o.Sections) == 0 {
		return true
	}
	return slices.Contains(o.Sections, section)
}

// ---- ZIP 相关 ----

// ZipManifest 是 ZIP 备份包中的清单文件。
type ZipManifest struct {
	Version    int              `yaml:"version"`
	Generator  string           `yaml:"generator"`
	ExportedAt string           `yaml:"exported_at"`
	Schemas    []ZipSchemaEntry `yaml:"schemas"`
	Phrases    *ZipPhrasesEntry `yaml:"phrases,omitempty"`
}

// ZipSchemaEntry 清单中的方案条目。
type ZipSchemaEntry struct {
	ID   string `yaml:"id"`
	Name string `yaml:"name"`
	File string `yaml:"file"`
}

// ZipPhrasesEntry 清单中的短语条目。
type ZipPhrasesEntry struct {
	File string `yaml:"file"`
}
