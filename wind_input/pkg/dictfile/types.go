// Package dictfile 提供词库文件的读写功能
package dictfile

import "time"

// PhraseEntry 短语条目
type PhraseEntry struct {
	Text   string `json:"text"`   // 输出文本（可包含模板变量）
	Weight int    `json:"weight"` // 权重
}

// PhraseConfig 单个短语配置
type PhraseConfig struct {
	Code       string   `yaml:"code" json:"code"`             // 触发编码
	Text       string   `yaml:"text" json:"text"`             // 单个输出（与 candidates 二选一）
	Candidates []string `yaml:"candidates" json:"candidates"` // 多个候选输出
	Type       string   `yaml:"type" json:"type"`             // 类型: 空=普通短语, "command"=命令
	Handler    string   `yaml:"handler" json:"handler"`       // 命令处理器名称
	Weight     int      `yaml:"weight" json:"weight"`         // 权重（默认 100）
}

// PhrasesConfig phrases.yaml 配置结构
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
