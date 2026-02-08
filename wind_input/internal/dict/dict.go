package dict

import (
	"github.com/huanfeng/wind_input/internal/candidate"
)

// Dict 词库接口
type Dict interface {
	// Lookup 查找拼音对应的候选词
	Lookup(pinyin string) []candidate.Candidate

	// LookupPhrase 查找短语
	LookupPhrase(syllables []string) []candidate.Candidate
}

// PrefixSearchable 支持前缀搜索的词库接口（可选扩展）
// 拼音引擎通过类型断言使用：if ps, ok := d.(PrefixSearchable); ok { ... }
type PrefixSearchable interface {
	LookupPrefix(prefix string, limit int) []candidate.Candidate
}

// AbbrevSearchable 支持简拼搜索的词库接口（可选扩展）
// 简拼是每个音节首字母的拼接，如 "bzd" 匹配 "bu zhi dao"（不知道）
type AbbrevSearchable interface {
	LookupAbbrev(code string, limit int) []candidate.Candidate
}

// CommandSearchable 支持特殊命令查找的词库接口（可选扩展）
// 仅查找命令（uuid, date, time 等），不返回普通词条
type CommandSearchable interface {
	LookupCommand(code string) []candidate.Candidate
}
