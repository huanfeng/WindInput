// Package schema 提供输入方案定义和管理功能
package schema

import "github.com/huanfeng/wind_input/internal/dict"

// Schema 输入方案定义
type Schema struct {
	Schema   SchemaInfo   `yaml:"schema"`
	Engine   EngineSpec   `yaml:"engine"`
	Dicts    []DictSpec   `yaml:"dictionaries"`
	UserData UserDataSpec `yaml:"user_data"`
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
	MinPinyinLength int  `yaml:"min_pinyin_length"` // 拼音最小触发长度，默认2
	WubiWeightBoost int  `yaml:"wubi_weight_boost"` // 五笔权重提升值，默认10000000
	ShowSourceHint  bool `yaml:"show_source_hint"`  // 是否在候选提示中显示来源标记
}

// CodeTableSpec 码表引擎配置
type CodeTableSpec struct {
	MaxCodeLength     int    `yaml:"max_code_length"`
	AutoCommitUnique  bool   `yaml:"auto_commit_unique"`
	ClearOnEmptyMax   bool   `yaml:"clear_on_empty_max"`
	TopCodeCommit     bool   `yaml:"top_code_commit"`
	PunctCommit       bool   `yaml:"punct_commit"`
	ShowCodeHint      bool   `yaml:"show_code_hint"`
	SingleCodeInput   bool   `yaml:"single_code_input"`
	CandidateSortMode string `yaml:"candidate_sort_mode"`
}

// PinyinSpec 拼音引擎配置
type PinyinSpec struct {
	Scheme          string         `yaml:"scheme"`
	Shuangpin       *ShuangpinSpec `yaml:"shuangpin,omitempty"`
	ShowWubiHint    bool           `yaml:"show_wubi_hint"`
	UseSmartCompose bool           `yaml:"use_smart_compose"`
	CandidateOrder  string         `yaml:"candidate_order"`
	Fuzzy           *FuzzySpec     `yaml:"fuzzy,omitempty"`
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

// UserDataSpec 用户数据配置
type UserDataSpec struct {
	ShadowFile   string `yaml:"shadow_file"`
	UserDictFile string `yaml:"user_dict_file"`
	TempDictFile string `yaml:"temp_dict_file,omitempty"`
	UserFreqFile string `yaml:"user_freq_file,omitempty"`
}

// LearningMode 学习模式
type LearningMode string

const (
	LearningManual    LearningMode = "manual"
	LearningAuto      LearningMode = "auto"
	LearningFrequency LearningMode = "frequency"
)

// LearningSpec 学习策略配置
type LearningSpec struct {
	Mode             LearningMode `yaml:"mode"`
	UnigramPath      string       `yaml:"unigram_path,omitempty"`
	ProtectTopN      int          `yaml:"protect_top_n,omitempty"`      // 首选保护：前 N 位锁定码表原始顺序
	TempMaxEntries   int          `yaml:"temp_max_entries,omitempty"`   // 临时词库最大条目数（默认 5000）
	TempPromoteCount int          `yaml:"temp_promote_count,omitempty"` // 选择几次后晋升到用户词库（默认 5）
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
