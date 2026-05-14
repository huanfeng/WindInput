package dictio

import "strconv"

// DefaultColumns 各 section 的默认列定义。
// 当文件头缺少 columns 声明时使用这些默认值。
var DefaultColumns = map[string][]string{
	SectionUserWords: {"code", "text", "weight", "count", "created_at"},
	SectionTempWords: {"code", "text", "weight", "count", "created_at"},
	SectionFreq:      {"code", "text", "count", "last_used", "streak"},
	SectionShadow:    {"action", "code", "word", "position"},
	SectionPhrases:   {"code", "text", "weight", "position", "enabled"},
}

// ColumnDef 列定义，提供列名到位置索引的映射。
type ColumnDef struct {
	Names    []string
	IndexMap map[string]int
}

// NewColumnDef 从列名列表创建列定义。
func NewColumnDef(columns []string) *ColumnDef {
	def := &ColumnDef{
		Names:    columns,
		IndexMap: make(map[string]int, len(columns)),
	}
	for i, name := range columns {
		def.IndexMap[name] = i
	}
	return def
}

// Get 从 TSV 分割后的字段数组中按列名提取值。
// 如果列不存在或字段数不足，返回空字符串。
func (d *ColumnDef) Get(fields []string, name string) string {
	idx, ok := d.IndexMap[name]
	if !ok || idx >= len(fields) {
		return ""
	}
	return fields[idx]
}

// GetUnescaped 同 Get，但对结果进行反转义。
func (d *ColumnDef) GetUnescaped(fields []string, name string) string {
	return UnescapeField(d.Get(fields, name))
}

// GetInt 从字段中按列名提取整数值，解析失败返回 defaultVal。
func (d *ColumnDef) GetInt(fields []string, name string, defaultVal int) int {
	s := d.Get(fields, name)
	if s == "" {
		return defaultVal
	}
	v, err := strconv.Atoi(s)
	if err != nil {
		return defaultVal
	}
	return v
}

// GetInt64 从字段中按列名提取 int64 值，解析失败返回 defaultVal。
func (d *ColumnDef) GetInt64(fields []string, name string, defaultVal int64) int64 {
	s := d.Get(fields, name)
	if s == "" {
		return defaultVal
	}
	v, err := strconv.ParseInt(s, 10, 64)
	if err != nil {
		return defaultVal
	}
	return v
}

// GetUint32 从字段中按列名提取 uint32 值，解析失败返回 defaultVal。
func (d *ColumnDef) GetUint32(fields []string, name string, defaultVal uint32) uint32 {
	s := d.Get(fields, name)
	if s == "" {
		return defaultVal
	}
	v, err := strconv.ParseUint(s, 10, 32)
	if err != nil {
		return defaultVal
	}
	return uint32(v)
}

// GetUint8 从字段中按列名提取 uint8 值，解析失败返回 defaultVal。
func (d *ColumnDef) GetUint8(fields []string, name string, defaultVal uint8) uint8 {
	s := d.Get(fields, name)
	if s == "" {
		return defaultVal
	}
	v, err := strconv.ParseUint(s, 10, 8)
	if err != nil {
		return defaultVal
	}
	return uint8(v)
}

// GetBool 从字段中按列名提取布尔值。
// "1"、"true"、"yes" 返回 true，其他返回 defaultVal。
func (d *ColumnDef) GetBool(fields []string, name string, defaultVal bool) bool {
	s := d.Get(fields, name)
	if s == "" {
		return defaultVal
	}
	switch s {
	case "1", "true", "yes":
		return true
	case "0", "false", "no":
		return false
	default:
		return defaultVal
	}
}

// FormatBool 将布尔值格式化为 "1" 或 "0"。
func FormatBool(b bool) string {
	if b {
		return "1"
	}
	return "0"
}
