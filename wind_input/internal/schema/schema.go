// Package schema 提供输入方案定义和管理功能
package schema

// Schema 输入方案定义
type Schema struct {
	Schema   SchemaInfo   `yaml:"schema"`
	Engine   EngineSpec   `yaml:"engine"`
	Dicts    []DictSpec   `yaml:"dictionaries"`
	UserData UserDataSpec `yaml:"user_data"`
	Learning LearningSpec `yaml:"learning"`
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
)

// EngineSpec 引擎规格
type EngineSpec struct {
	Type       EngineType     `yaml:"type"`
	CodeTable  *CodeTableSpec `yaml:"codetable,omitempty"`
	Pinyin     *PinyinSpec    `yaml:"pinyin,omitempty"`
	FilterMode string         `yaml:"filter_mode"`
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
	ID      string   `yaml:"id"`
	Path    string   `yaml:"path"`
	Type    string   `yaml:"type"`
	Default bool     `yaml:"default"`
	Role    DictRole `yaml:"role,omitempty"`
}

// UserDataSpec 用户数据配置
type UserDataSpec struct {
	ShadowFile   string `yaml:"shadow_file"`
	UserDictFile string `yaml:"user_dict_file"`
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
	Mode        LearningMode `yaml:"mode"`
	UnigramPath string       `yaml:"unigram_path,omitempty"`
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
