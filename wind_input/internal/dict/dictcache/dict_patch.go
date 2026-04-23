package dictcache

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"
)

// DictPatch 词库补丁配置
// 用于在不修改原始词库文件的情况下自定义词条（调整权重、新增、删除）。
// 补丁文件在编译词库缓存（wdb/wdat）时自动发现并应用，不影响原始词库文件。
type DictPatch struct {
	// Entries 新增或修改的词条
	// code+text 已存在时更新权重，否则作为新词条插入
	Entries []DictPatchEntry `yaml:"entries"`

	// Delete 要删除的词条（按 code+text 精确匹配）
	Delete []DictPatchRef `yaml:"delete"`
}

// DictPatchEntry 补丁词条（新增/修改）
// 支持全称和缩写两种字段名：code/c, text/t, weight/w, pinyin/p
//
// 全称写法:
//
//   - code: a
//     text: 工
//     weight: 30
//
// 缩写写法（推荐，可搭配 YAML flow 语法写在一行）:
//
//   - {c: a, t: 工, w: 30}
type DictPatchEntry struct {
	Code   string // 编码（五笔编码或拼音无空格拼接，如 "nihao"）
	Text   string // 文字
	Weight int    // 权重
	Pinyin string // 拼音（可选，空格分隔音节，仅拼音词库用于更新简拼索引）
}

// UnmarshalYAML 支持全称和缩写两种字段名
func (e *DictPatchEntry) UnmarshalYAML(value *yaml.Node) error {
	var raw map[string]yaml.Node
	if err := value.Decode(&raw); err != nil {
		return err
	}
	e.Code = decodeString(raw, "code", "c")
	e.Text = decodeString(raw, "text", "t")
	e.Weight = decodeInt(raw, "weight", "w")
	e.Pinyin = decodeString(raw, "pinyin", "p")
	return nil
}

// DictPatchRef 补丁词条引用（删除用）
// 支持全称和缩写两种字段名：code/c, text/t
type DictPatchRef struct {
	Code string // 编码
	Text string // 文字
}

// UnmarshalYAML 支持全称和缩写两种字段名
func (r *DictPatchRef) UnmarshalYAML(value *yaml.Node) error {
	var raw map[string]yaml.Node
	if err := value.Decode(&raw); err != nil {
		return err
	}
	r.Code = decodeString(raw, "code", "c")
	r.Text = decodeString(raw, "text", "t")
	return nil
}

// decodeString 从 raw map 中按优先级读取字符串（全称优先）
func decodeString(raw map[string]yaml.Node, fullKey, shortKey string) string {
	for _, key := range []string{fullKey, shortKey} {
		if node, ok := raw[key]; ok {
			var s string
			if node.Decode(&s) == nil {
				return s
			}
		}
	}
	return ""
}

// decodeInt 从 raw map 中按优先级读取整数（全称优先）
func decodeInt(raw map[string]yaml.Node, fullKey, shortKey string) int {
	for _, key := range []string{fullKey, shortKey} {
		if node, ok := raw[key]; ok {
			var v int
			if node.Decode(&v) == nil {
				return v
			}
		}
	}
	return 0
}

// IsEmpty 判断补丁是否为空（无任何操作）
func (p *DictPatch) IsEmpty() bool {
	return p == nil || (len(p.Entries) == 0 && len(p.Delete) == 0)
}

// patchPath 根据词库文件路径推导补丁文件路径
// 例如: wubi86_jidian.dict.yaml → wubi86_jidian.dict.patch.yaml
func patchPath(dictPath string) string {
	if base, ok := strings.CutSuffix(dictPath, ".dict.yaml"); ok {
		return base + ".dict.patch.yaml"
	}
	return dictPath + ".patch.yaml"
}

// FindPatchFiles 查找指定词库及其所有关联词库的补丁文件
// mainDictPath 为主词库路径，importNames 为通过 import_tables 发现的关联词库名称列表。
// 返回所有存在的补丁文件路径（用于缓存失效检测和补丁加载）。
func FindPatchFiles(mainDictPath string, importNames []string) []string {
	var patches []string
	dictDir := filepath.Dir(mainDictPath)

	// 主词库补丁
	if p := patchPath(mainDictPath); fileExists(p) {
		patches = append(patches, p)
	}

	// 关联词库补丁
	for _, name := range importNames {
		p := patchPath(filepath.Join(dictDir, name+".dict.yaml"))
		if fileExists(p) {
			patches = append(patches, p)
		}
	}

	return patches
}

// LoadDictPatch 加载词库补丁文件
func LoadDictPatch(path string) (*DictPatch, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取补丁文件失败: %w", err)
	}

	var patch DictPatch
	if err := yaml.Unmarshal(data, &patch); err != nil {
		return nil, fmt.Errorf("解析补丁文件失败: %w", err)
	}

	return &patch, nil
}

// LoadAndMergePatchFiles 加载多个补丁文件并合并为一个 DictPatch
func LoadAndMergePatchFiles(paths []string, logger *slog.Logger) *DictPatch {
	merged := &DictPatch{}
	for _, p := range paths {
		patch, err := LoadDictPatch(p)
		if err != nil {
			logger.Warn("加载词库补丁失败", "path", p, "error", err)
			continue
		}
		if patch.IsEmpty() {
			continue
		}
		merged.Entries = append(merged.Entries, patch.Entries...)
		merged.Delete = append(merged.Delete, patch.Delete...)
		logger.Info("加载词库补丁", "path", filepath.Base(p),
			"entries", len(patch.Entries), "deletes", len(patch.Delete))
	}
	return merged
}

// ApplyDictPatch 将补丁应用到已加载的词条集合上
// abbrevEntries 可选（非 nil 时同步更新简拼索引，用于拼音词库）
// 返回 (新增数, 修改数, 删除数)
func ApplyDictPatch(codeEntries map[string][]dictEntry, abbrevEntries map[string][]dictEntry, patch *DictPatch, logger *slog.Logger) (added, modified, deleted int) {
	if patch.IsEmpty() {
		return
	}

	// 1. 删除词条
	for _, d := range patch.Delete {
		code := d.Code
		list, ok := codeEntries[code]
		if !ok {
			continue
		}
		filtered := list[:0]
		for _, e := range list {
			if e.text == d.Text {
				deleted++
			} else {
				filtered = append(filtered, e)
			}
		}
		if len(filtered) == 0 {
			delete(codeEntries, code)
		} else {
			codeEntries[code] = filtered
		}
		// 同步删除简拼索引
		if abbrevEntries != nil {
			removeFromAbbrev(abbrevEntries, d.Text)
		}
	}

	// 2. 新增/修改词条
	for _, e := range patch.Entries {
		code := e.Code
		found := false
		if list, ok := codeEntries[code]; ok {
			for i := range list {
				if list[i].text == e.Text {
					list[i].weight = e.Weight
					found = true
					modified++
					break
				}
			}
		}
		if !found {
			codeEntries[code] = append(codeEntries[code], dictEntry{
				text:         e.Text,
				weight:       e.Weight,
				naturalOrder: len(codeEntries[code]),
			})
			added++
		}

		// 更新简拼索引（需要 pinyin 字段提供音节信息）
		if abbrevEntries != nil && e.Pinyin != "" {
			upsertAbbrev(abbrevEntries, e.Text, e.Weight, e.Pinyin)
		}
	}

	return
}

// removeFromAbbrev 从简拼索引中删除指定 text 的所有条目
func removeFromAbbrev(abbrevEntries map[string][]dictEntry, text string) {
	for abbrev, list := range abbrevEntries {
		filtered := list[:0]
		for _, e := range list {
			if e.text != text {
				filtered = append(filtered, e)
			}
		}
		if len(filtered) == 0 {
			delete(abbrevEntries, abbrev)
		} else {
			abbrevEntries[abbrev] = filtered
		}
	}
}

// upsertAbbrev 在简拼索引中新增或更新词条
func upsertAbbrev(abbrevEntries map[string][]dictEntry, text string, weight int, pinyin string) {
	syllables := strings.Fields(pinyin)
	if len(syllables) < 2 {
		return
	}
	var b strings.Builder
	for _, s := range syllables {
		if len(s) == 0 {
			return
		}
		b.WriteByte(s[0])
	}
	abbrev := b.String()
	if abbrev == "" {
		return
	}

	// 查找已有条目并更新
	if list, ok := abbrevEntries[abbrev]; ok {
		for i := range list {
			if list[i].text == text {
				list[i].weight = weight
				return
			}
		}
	}

	// 新增
	abbrevEntries[abbrev] = append(abbrevEntries[abbrev], dictEntry{
		text:         text,
		weight:       weight,
		naturalOrder: len(abbrevEntries[abbrev]),
	})
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}
