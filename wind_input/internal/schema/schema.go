// Package schema 提供输入方案定义和管理功能
package schema

import (
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
)

// Schema 输入方案定义
type Schema struct {
	Schema   SchemaInfo   `yaml:"schema"`
	Engine   EngineSpec   `yaml:"engine"`
	Dicts    []DictSpec   `yaml:"dictionaries"`
	Learning LearningSpec `yaml:"learning"`
	Encoder  *EncoderSpec `yaml:"encoder,omitempty"` // 造词编码规则（codetable 方案用）
}

// SchemaInfo 方案基本信息
type SchemaInfo struct {
	ID          string `yaml:"id"`
	Name        string `yaml:"name"`
	IconLabel   string `yaml:"icon_label"`
	Version     string `yaml:"version"`
	Author      string `yaml:"author"`
	Description string `yaml:"description"`
}

// EngineType 引擎内部处理类型
type EngineType string

const (
	EngineTypeCodeTable EngineType = "codetable"
	EngineTypePinyin    EngineType = "pinyin"
	EngineTypeMixed     EngineType = "mixed" // 五笔拼音混输
)

// EngineSpec 引擎规格
type EngineSpec struct {
	Type       EngineType     `yaml:"type"`
	CodeTable  *CodeTableSpec `yaml:"codetable,omitempty"`
	Pinyin     *PinyinSpec    `yaml:"pinyin,omitempty"`
	Mixed      *MixedSpec     `yaml:"mixed,omitempty"`
	FilterMode string         `yaml:"filter_mode"`
}

// MixedSpec 混输引擎配置
type MixedSpec struct {
	PrimarySchema        string `yaml:"primary_schema"`           // 主形码方案ID（如 wubi86）
	SecondarySchema      string `yaml:"secondary_schema"`         // 拼音方案ID（如 pinyin）
	MinPinyinLength      int    `yaml:"min_pinyin_length"`        // 拼音最小触发长度，默认2
	CodetableWeightBoost int    `yaml:"codetable_weight_boost"`   // 码表权重提升值，默认10000000
	ShowSourceHint       bool   `yaml:"show_source_hint"`         // 是否在候选提示中显示来源标记
	EnableAbbrevMatch    *bool  `yaml:"enable_abbrev_match"`      // 混输模式下是否启用简拼匹配（默认 false）
	PinyinOnlyOverflow   *bool  `yaml:"pinyin_only_overflow"`     // 超过最大码长时仅查拼音（默认 true）
	ZKeyRepeat           *bool  `yaml:"z_key_repeat,omitempty"`   // Z键重复上屏：输入z时首选为上次上屏的内容
	EnableEnglish        *bool  `yaml:"enable_english,omitempty"` // 混输模式下是否启用英文候选（默认 false）
}

// TempPinyinSpec 码表方案的临时拼音配置
type TempPinyinSpec struct {
	Enabled bool   `yaml:"enabled"`          // 是否开启临时拼音
	Schema  string `yaml:"schema,omitempty"` // 使用的拼音方案ID（默认 "pinyin"）
}

// CodeTableSpec 码表引擎配置
type CodeTableSpec struct {
	MaxCodeLength      int             `yaml:"max_code_length"`
	AutoCommitUnique   bool            `yaml:"auto_commit_unique"`
	ClearOnEmptyMax    bool            `yaml:"clear_on_empty_max"`
	TopCodeCommit      bool            `yaml:"top_code_commit"`
	PunctCommit        bool            `yaml:"punct_commit"`
	ShowCodeHint       bool            `yaml:"show_code_hint"`
	SingleCodeInput    bool            `yaml:"single_code_input"`
	SingleCodeComplete bool            `yaml:"single_code_complete"` // 逐码空码补全
	CandidateSortMode  string          `yaml:"candidate_sort_mode"`
	DedupCandidates    *bool           `yaml:"dedup_candidates,omitempty"`
	SkipSingleCharFreq *bool           `yaml:"skip_single_char_freq"`  // 单字不自动调频（指针以区分未设置和 false）
	TempPinyin         *TempPinyinSpec `yaml:"temp_pinyin,omitempty"`  // 临时拼音配置
	ZKeyRepeat         *bool           `yaml:"z_key_repeat,omitempty"` // Z键重复上屏：输入z时首选为上次上屏的内容
}

// PinyinSpec 拼音引擎配置
type PinyinSpec struct {
	Scheme          string         `yaml:"scheme"`
	Shuangpin       *ShuangpinSpec `yaml:"shuangpin,omitempty"`
	ShowCodeHint    bool           `yaml:"show_code_hint"`
	UseSmartCompose bool           `yaml:"use_smart_compose"`
	CandidateOrder  string         `yaml:"candidate_order"`
	Fuzzy           *FuzzySpec     `yaml:"fuzzy,omitempty"`
	DictFormat      string         `yaml:"dict_format"` // 词库格式: ""/"wdb"(默认) 或 "dat"
}

// ShuangpinSpec 双拼子配置
type ShuangpinSpec struct {
	Layout string `yaml:"layout"` // "ziranma" | "xiaohe" | "sogou" | "mspy"
}

// FuzzySpec 模糊音配置
type FuzzySpec struct {
	Enabled bool `yaml:"enabled"`
	ZhZ     bool `yaml:"zh_z"`
	ChC     bool `yaml:"ch_c"`
	ShS     bool `yaml:"sh_s"`
	NL      bool `yaml:"n_l"`
	FH      bool `yaml:"f_h"`
	RL      bool `yaml:"r_l"`
	AnAng   bool `yaml:"an_ang"`
	EnEng   bool `yaml:"en_eng"`
	InIng   bool `yaml:"in_ing"`
	IanIang bool `yaml:"ian_iang"`
	UanUang bool `yaml:"uan_uang"`
}

// DictRole 词库角色
type DictRole string

const (
	DictRoleSystem        DictRole = "system"
	DictRoleReverseLookup DictRole = "reverse_lookup"
)

// DictSpec 词库规格
type DictSpec struct {
	ID         string      `yaml:"id"`
	Path       string      `yaml:"path"`
	Type       string      `yaml:"type"`
	Default    bool        `yaml:"default"`
	Role       DictRole    `yaml:"role,omitempty"`
	WeightSpec *WeightSpec `yaml:"weight_spec,omitempty"` // 权重归一化参数
}

// WeightNormMode 权重归一化算法
type WeightNormMode string

const (
	WeightNormLinear WeightNormMode = "linear" // 分段线性映射（适合跨度小的码表词库）
	WeightNormLog    WeightNormMode = "log"    // 对数映射（适合长尾分布的拼音词库）
)

// NormalizedWeightMax 归一化后的权重上限
const NormalizedWeightMax = 10000

// WeightSpec 词库权重归一化参数
// 用于将不同词库的原始权重映射到统一的 [0, NormalizedWeightMax] 区间
type WeightSpec struct {
	Median int            `yaml:"median"`           // 原始权重中位数（映射到统一区间的基准点）
	Max    int            `yaml:"max"`              // 原始权重最大值
	Min    int            `yaml:"min,omitempty"`    // 原始权重最小值（默认 0）
	Mode   WeightNormMode `yaml:"mode"`             // 映射算法
	Target int            `yaml:"target,omitempty"` // 中位映射目标值（默认 1000）
}

// LearningSpec 学习策略配置
type LearningSpec struct {
	AutoLearn  *AutoLearnSpec  `yaml:"auto_learn,omitempty"`  // 自动造词配置（拼音：选词即学）
	AutoPhrase *AutoPhraseSpec `yaml:"auto_phrase,omitempty"` // 自动造词配置（码表：连续单字组词）
	Freq       *FreqSpec       `yaml:"freq,omitempty"`        // 自动调频配置

	UnigramPath      string `yaml:"unigram_path,omitempty"`
	TempMaxEntries   int    `yaml:"temp_max_entries,omitempty"`   // 临时词库最大条目数（默认 5000）
	TempPromoteCount int    `yaml:"temp_promote_count,omitempty"` // 选择几次后晋升到用户词库（默认 5）
}

// AutoLearnSpec 自动造词配置
type AutoLearnSpec struct {
	Enabled        bool `yaml:"enabled"`                   // 是否启用自动造词
	CountThreshold int  `yaml:"count_threshold,omitempty"` // 误选保护阈值（默认 2）
	MinWordLength  int  `yaml:"min_word_length,omitempty"` // 最小造词字数（默认 2）
	WeightDelta    int  `yaml:"weight_delta,omitempty"`    // 每次选词权重增量（默认 20）
	AddWeight      int  `yaml:"add_weight,omitempty"`      // 新词初始权重（默认 800）
}

// AutoPhraseSpec 码表自动造词配置（连续单字 + 终止符 = 自动组词）
type AutoPhraseSpec struct {
	Enabled        bool `yaml:"enabled"`                   // 是否启用
	MinPhraseLen   int  `yaml:"min_phrase_len,omitempty"`  // 最小造词字数（默认 2）
	MaxPhraseLen   int  `yaml:"max_phrase_len,omitempty"`  // 最大造词字数（默认 5）
	AddWeight      int  `yaml:"add_weight,omitempty"`      // 新词初始权重（默认 800）
	WeightDelta    int  `yaml:"weight_delta,omitempty"`    // 每次命中权重增量（默认 20）
	CountThreshold int  `yaml:"count_threshold,omitempty"` // 误选保护阈值（默认 2）
}

// FreqSpec 自动调频配置
type FreqSpec struct {
	Enabled     bool    `yaml:"enabled"`                 // 是否启用自动调频
	ProtectTopN int     `yaml:"protect_top_n,omitempty"` // 锁定前 N 位候选的排序位置（默认 0 不锁定）
	HalfLife    float64 `yaml:"half_life,omitempty"`     // 半衰期（小时，默认 72）
	BoostMax    int     `yaml:"boost_max,omitempty"`     // 加成上限（默认 2000）
	MaxRecency  float64 `yaml:"max_recency,omitempty"`   // 时间衰减峰值（默认 300）
	BaseScale   float64 `yaml:"base_scale,omitempty"`    // base 系数（默认 100）
	StreakScale float64 `yaml:"streak_scale,omitempty"`  // 连续选择系数（默认 50）
	StreakCap   float64 `yaml:"streak_cap,omitempty"`    // 连续选择上限（默认 250）
}

// IsAutoLearnEnabled 是否启用自动造词（拼音：选词即学）
func (ls *LearningSpec) IsAutoLearnEnabled() bool {
	return ls.AutoLearn != nil && ls.AutoLearn.Enabled
}

// IsAutoPhraseEnabled 是否启用码表自动造词
func (ls *LearningSpec) IsAutoPhraseEnabled() bool {
	return ls.AutoPhrase != nil && ls.AutoPhrase.Enabled
}

// IsFreqEnabled 是否启用自动调频
func (ls *LearningSpec) IsFreqEnabled() bool {
	return ls.Freq != nil && ls.Freq.Enabled
}

// GetAutoLearnConfig 获取造词配置（带默认值填充）
func (ls *LearningSpec) GetAutoLearnConfig() AutoLearnSpec {
	cfg := AutoLearnSpec{
		Enabled:        ls.IsAutoLearnEnabled(),
		CountThreshold: 2,
		MinWordLength:  2,
		WeightDelta:    20,
		AddWeight:      800,
	}
	if ls.AutoLearn != nil {
		if ls.AutoLearn.CountThreshold > 0 {
			cfg.CountThreshold = ls.AutoLearn.CountThreshold
		}
		if ls.AutoLearn.MinWordLength > 0 {
			cfg.MinWordLength = ls.AutoLearn.MinWordLength
		}
		if ls.AutoLearn.WeightDelta > 0 {
			cfg.WeightDelta = ls.AutoLearn.WeightDelta
		}
		if ls.AutoLearn.AddWeight > 0 {
			cfg.AddWeight = ls.AutoLearn.AddWeight
		}
	}
	return cfg
}

// GetAutoPhraseConfig 获取码表自动造词配置（带默认值填充）
func (ls *LearningSpec) GetAutoPhraseConfig() AutoPhraseSpec {
	cfg := AutoPhraseSpec{
		Enabled:        ls.IsAutoPhraseEnabled(),
		MinPhraseLen:   2,
		MaxPhraseLen:   5,
		AddWeight:      800,
		WeightDelta:    20,
		CountThreshold: 2,
	}
	if ls.AutoPhrase != nil {
		if ls.AutoPhrase.MinPhraseLen > 0 {
			cfg.MinPhraseLen = ls.AutoPhrase.MinPhraseLen
		}
		if ls.AutoPhrase.MaxPhraseLen > 0 {
			cfg.MaxPhraseLen = ls.AutoPhrase.MaxPhraseLen
		}
		if ls.AutoPhrase.AddWeight > 0 {
			cfg.AddWeight = ls.AutoPhrase.AddWeight
		}
		if ls.AutoPhrase.WeightDelta > 0 {
			cfg.WeightDelta = ls.AutoPhrase.WeightDelta
		}
		if ls.AutoPhrase.CountThreshold > 0 {
			cfg.CountThreshold = ls.AutoPhrase.CountThreshold
		}
	}
	return cfg
}

// GetFreqProfile 从 FreqSpec 生成 store.FreqProfile（带默认值填充）
func (ls *LearningSpec) GetFreqProfile() *store.FreqProfile {
	p := store.DefaultFreqProfile()
	if ls.Freq == nil {
		return p
	}
	if ls.Freq.HalfLife > 0 {
		p.DecayHalfLife = ls.Freq.HalfLife
	}
	if ls.Freq.BoostMax > 0 {
		p.BoostMax = ls.Freq.BoostMax
	}
	if ls.Freq.MaxRecency > 0 {
		p.MaxRecency = ls.Freq.MaxRecency
	}
	if ls.Freq.BaseScale > 0 {
		p.BaseScale = ls.Freq.BaseScale
	}
	if ls.Freq.StreakScale > 0 {
		p.StreakScale = ls.Freq.StreakScale
	}
	if ls.Freq.StreakCap > 0 {
		p.StreakCap = ls.Freq.StreakCap
	}
	return p
}

// EncoderSpec 造词编码规则配置
type EncoderSpec struct {
	Rules           []EncoderRule `yaml:"rules"`
	MaxWordLength   int           `yaml:"max_word_length,omitempty"`
	ExcludePatterns []string      `yaml:"exclude_patterns,omitempty"`
}

// EncoderRule 单条编码规则
type EncoderRule struct {
	LengthEqual   int    `yaml:"length_equal,omitempty"`
	LengthInRange []int  `yaml:"length_in_range,omitempty,flow"`
	Formula       string `yaml:"formula"`
}

// NewWeightNormalizer 从 WeightSpec 创建归一化器，spec 为 nil 时返回 nil
func (ws *WeightSpec) NewWeightNormalizer() *dict.WeightNormalizer {
	if ws == nil {
		return nil
	}
	return dict.NewWeightNormalizer(string(ws.Mode), ws.Median, ws.Max, ws.Min, ws.Target)
}

// GetDefaultDictSpec 获取默认词库规格（dictionaries 中 default=true 的项）
func (s *Schema) GetDefaultDictSpec() *DictSpec {
	for i := range s.Dicts {
		if s.Dicts[i].Default {
			return &s.Dicts[i]
		}
	}
	if len(s.Dicts) > 0 {
		return &s.Dicts[0]
	}
	return nil
}

// DataSchemaID 返回数据方案 ID
// 混输方案返回主方案 ID（与主方案共享用户数据），其他返回自身 ID
func (s *Schema) DataSchemaID() string {
	if s.Engine.Type == EngineTypeMixed && s.Engine.Mixed != nil && s.Engine.Mixed.PrimarySchema != "" {
		return s.Engine.Mixed.PrimarySchema
	}
	return s.Schema.ID
}

// GetDictsByRole 按角色筛选词库规格
func (s *Schema) GetDictsByRole(role DictRole) []DictSpec {
	var result []DictSpec
	for _, d := range s.Dicts {
		r := d.Role
		if r == "" {
			r = DictRoleSystem
		}
		if r == role {
			result = append(result, d)
		}
	}
	return result
}
