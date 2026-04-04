// Package dictfile 提供词库文件的读写功能
package dictfile

import "time"

// PhraseEntry 短语条目
type PhraseEntry struct {
	Text   string `json:"text"`   // 输出文本（可包含模板变量）
	Weight int    `json:"weight"` // 权重
}

// PhraseConfig 单个短语配置
// Text 支持变量模板语法（$Y, $MM, $DD, ${var} 等），运行时自动展开
// Texts 字段用于数组映射，每个字符展开为一个独立候选
type PhraseConfig struct {
	Code     string `yaml:"code" json:"code"`                             // 触发编码
	Text     string `yaml:"text" json:"text"`                             // 输出文本（可包含 $变量）
	Texts    string `yaml:"texts,omitempty" json:"texts,omitempty"`       // 数组映射：每个字符展开为独立候选
	Name     string `yaml:"name,omitempty" json:"name,omitempty"`         // 显示名称（用于组候选展示）
	Position int    `yaml:"position" json:"position"`                     // 候选位置（1=第一候选, 2=第二候选...）
	Disabled bool   `yaml:"disabled,omitempty" json:"disabled,omitempty"` // 是否禁用
}

// PhrasesConfig 短语配置文件结构（system.phrases.yaml / user.phrases.yaml）
type PhrasesConfig struct {
	Phrases []PhraseConfig `yaml:"phrases" json:"phrases"`
}

// ShadowPinConfig pin 规则配置（固定位置）
type ShadowPinConfig struct {
	Word     string `yaml:"word" json:"word"`
	Position int    `yaml:"position" json:"position"`
}

// ShadowCodeConfig 单个编码下的规则
type ShadowCodeConfig struct {
	Pinned  []ShadowPinConfig `yaml:"pinned,omitempty" json:"pinned,omitempty"`
	Deleted []string          `yaml:"deleted,omitempty" json:"deleted,omitempty"`
}

// ShadowConfig shadow.yaml 配置结构
type ShadowConfig struct {
	Rules map[string]*ShadowCodeConfig `yaml:"rules" json:"rules"`
}

// UserWord 用户词条
type UserWord struct {
	Code      string    `json:"code"`       // 编码
	Text      string    `json:"text"`       // 词语
	Weight    int       `json:"weight"`     // 权重
	CreatedAt time.Time `json:"created_at"` // 创建时间
}

// UserDictData 用户词库数据
type UserDictData struct {
	Words []UserWord `json:"words"`
}
