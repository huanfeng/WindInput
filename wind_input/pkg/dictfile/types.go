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

// ShadowAction Shadow 层操作类型
type ShadowAction string

const (
	ShadowActionTop      ShadowAction = "top"      // 置顶
	ShadowActionDelete   ShadowAction = "delete"   // 删除（隐藏）
	ShadowActionReweight ShadowAction = "reweight" // 调整权重
)

// ShadowRuleConfig 单个规则配置
type ShadowRuleConfig struct {
	Word   string `yaml:"word" json:"word"`
	Action string `yaml:"action" json:"action"` // "top", "delete", "reweight"
	Weight int    `yaml:"weight" json:"weight"` // 仅 reweight 时有效
}

// ShadowConfig shadow.yaml 配置结构
type ShadowConfig struct {
	Rules map[string][]ShadowRuleConfig `yaml:"rules" json:"rules"`
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
