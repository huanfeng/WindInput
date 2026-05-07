package dict

import (
	"bufio"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict/binformat"
	"github.com/huanfeng/wind_input/internal/dict/datformat"
)

// PinyinDict 拼音专用词库（基于 Trie 索引或 mmap 二进制文件）
// 支持从 Rime dict.yaml 格式或预编译的 .wdb 二进制格式加载
type PinyinDict struct {
	logger     *slog.Logger
	trie       *Trie // Trie 索引，用于精确和前缀搜索（YAML 模式）
	abbrevTrie *Trie // 简拼索引（声母首字母 → 词条），用于简拼词组匹配（YAML 模式）
	entryCount int

	// 二进制模式（mmap）
	binReader *binformat.DictReader

	// DAT 模式（mmap）
	datReader *datformat.WdatReader
}

// NewPinyinDict 创建拼音词库
func NewPinyinDict(logger *slog.Logger) *PinyinDict {
	if logger == nil {
		logger = slog.Default()
	}
	return &PinyinDict{logger: logger}
}

// LoadRimeDir 从目录加载 Rime dict.yaml 格式词库
// 自动查找并加载 8105.dict.yaml 和 base.dict.yaml
func (d *PinyinDict) LoadRimeDir(dirPath string) error {
	d.trie = NewTrie()
	d.abbrevTrie = NewTrie()
	d.entryCount = 0

	files := []string{
		"8105.dict.yaml",        // 单字
		"41448.dict.yaml",       // 扩展字表（生僻字）
		"base.dict.yaml",        // 基础词组
		"ext.dict.yaml",         // 扩展词组
		"others.dict.yaml",      // 容错词（多音字异读）
		"corrections.dict.yaml", // 错音词（weight=0，可查但不影响排序）
	}

	loaded := 0
	for _, name := range files {
		path := filepath.Join(dirPath, name)
		if _, err := os.Stat(path); os.IsNotExist(err) {
			d.logger.Info("跳过不存在的文件", "path", path)
			continue
		}
		count, err := d.loadRimeFile(path)
		if err != nil {
			d.logger.Warn("加载词库文件失败", "name", name, "error", err)
			continue
		}
		d.logger.Info("加载词库文件", "name", name, "count", count)
		loaded++
	}

	if loaded == 0 {
		return fmt.Errorf("未找到任何 Rime 词库文件（目录: %s）", dirPath)
	}

	d.entryCount = d.trie.EntryCount()
	return nil
}

// LoadBinary 从预编译的 .wdb 文件加载词库（mmap 模式）
func (d *PinyinDict) LoadBinary(wdbPath string) error {
	reader, err := binformat.OpenDict(wdbPath)
	if err != nil {
		return fmt.Errorf("打开二进制词库失败: %w", err)
	}
	d.binReader = reader
	d.entryCount = reader.KeyCount()
	d.trie = nil
	d.abbrevTrie = nil
	return nil
}

// IsBinaryMode 检查是否为二进制模式
func (d *PinyinDict) IsBinaryMode() bool {
	return d.binReader != nil
}

// LoadDAT 从预编译的 .wdat 文件加载词库（DAT mmap 模式）
func (d *PinyinDict) LoadDAT(wdatPath string) error {
	reader, err := datformat.OpenWdat(wdatPath)
	if err != nil {
		return fmt.Errorf("打开 wdat 词库失败: %w", err)
	}
	d.datReader = reader
	d.entryCount = reader.KeyCount()
	d.trie = nil
	d.abbrevTrie = nil
	d.binReader = nil
	return nil
}

// IsDATMode 检查是否为 DAT 模式
func (d *PinyinDict) IsDATMode() bool {
	return d.datReader != nil
}

// Close 关闭词库（释放 mmap 资源）
func (d *PinyinDict) Close() error {
	if d.datReader != nil {
		return d.datReader.Close()
	}
	if d.binReader != nil {
		return d.binReader.Close()
	}
	return nil
}

// loadRimeFile 解析单个 Rime dict.yaml 文件
// 格式: 文字\t拼音(空格分隔)\t词频
func (d *PinyinDict) loadRimeFile(path string) (int, error) {
	file, err := os.Open(path)
	if err != nil {
		return 0, err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	inHeader := true
	count := 0

	for scanner.Scan() {
		line := scanner.Text()

		// 跳过 YAML 头部（--- 到 ... 之间）
		if inHeader {
			if strings.TrimSpace(line) == "..." {
				inHeader = false
			}
			continue
		}

		// 跳过空行和注释
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 3 {
			continue
		}

		text := parts[0]
		pinyin := parts[1]
		weight, err := strconv.Atoi(strings.TrimSpace(parts[2]))
		if err != nil || weight <= 0 {
			continue
		}

		// 拼音去空格作为查找键（"ni hao" → "nihao"）
		code := strings.ReplaceAll(pinyin, " ", "")

		cand := candidate.Candidate{
			Text:   text,
			Code:   code,
			Weight: weight,
		}
		d.trie.Insert(code, cand)

		// 构建简拼索引：对 2 字及以上的词条，取每个音节首字母拼接
		syllables := strings.Fields(pinyin)
		if len(syllables) >= 2 && d.abbrevTrie != nil {
			abbrev := buildAbbrev(syllables)
			if abbrev != "" {
				d.abbrevTrie.Insert(abbrev, cand)
			}
		}

		count++
	}

	if err := scanner.Err(); err != nil {
		return count, err
	}

	return count, nil
}

// Lookup 查找拼音对应的候选词
func (d *PinyinDict) Lookup(pinyin string) []candidate.Candidate {
	if d.datReader != nil {
		return d.datReader.Lookup(pinyin)
	}
	if d.binReader != nil {
		return d.binReader.Lookup(pinyin)
	}
	if d.trie == nil {
		return nil
	}
	return d.trie.Search(strings.ToLower(pinyin))
}

// LookupPhrase 查找短语（将音节拼接后查找）
func (d *PinyinDict) LookupPhrase(syllables []string) []candidate.Candidate {
	if len(syllables) == 0 {
		return nil
	}
	if d.datReader != nil {
		return d.datReader.Lookup(strings.ToLower(strings.Join(syllables, "")))
	}
	if d.binReader != nil {
		return d.binReader.LookupPhrase(syllables)
	}
	if d.trie == nil {
		return nil
	}
	key := strings.ToLower(strings.Join(syllables, ""))
	return d.trie.Search(key)
}

// LookupPrefix 前缀查找，返回所有以 prefix 开头的候选词
func (d *PinyinDict) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	if d.datReader != nil {
		return d.datReader.LookupPrefix(prefix, limit)
	}
	if d.binReader != nil {
		return d.binReader.LookupPrefix(prefix, limit)
	}
	if d.trie == nil {
		return nil
	}
	prefix = strings.ToLower(prefix)
	results := d.trie.SearchPrefix(prefix, limit)
	sort.SliceStable(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// HasPrefix 检查是否有以 prefix 开头的词条
func (d *PinyinDict) HasPrefix(prefix string) bool {
	if d.datReader != nil {
		return d.datReader.HasPrefix(prefix)
	}
	if d.binReader != nil {
		return d.binReader.HasPrefix(prefix)
	}
	if d.trie == nil {
		return false
	}
	return d.trie.HasPrefix(strings.ToLower(prefix))
}

// EntryCount 返回词条数量
func (d *PinyinDict) EntryCount() int {
	return d.entryCount
}

// GetTrie 获取 Trie 索引
func (d *PinyinDict) GetTrie() *Trie {
	return d.trie
}

// PinyinDictLayer 将 PinyinDict 适配为 DictLayer
type PinyinDictLayer struct {
	name      string
	layerType LayerType
	dict      *PinyinDict
}

// NewPinyinDictLayer 创建 PinyinDict 适配器
func NewPinyinDictLayer(name string, layerType LayerType, d *PinyinDict) *PinyinDictLayer {
	return &PinyinDictLayer{
		name:      name,
		layerType: layerType,
		dict:      d,
	}
}

// Name 返回层名称
func (l *PinyinDictLayer) Name() string {
	return l.name
}

// Type 返回层类型
func (l *PinyinDictLayer) Type() LayerType {
	return l.layerType
}

// Search 精确查询
func (l *PinyinDictLayer) Search(code string, limit int) []candidate.Candidate {
	results := l.dict.Lookup(code)
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	patchPinyinIsCommon(results)
	return results
}

// SearchPrefix 前缀查询
func (l *PinyinDictLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	results := l.dict.LookupPrefix(prefix, limit)
	patchPinyinIsCommon(results)
	return results
}

// SearchAbbrev 简拼查询
func (l *PinyinDictLayer) SearchAbbrev(code string, limit int) []candidate.Candidate {
	results := l.dict.LookupAbbrev(code, limit)
	patchPinyinIsCommon(results)
	return results
}

// patchPinyinIsCommon 为拼音词库候选补充 IsCommon 标记
// 拼音词库中的词条均来自标准词库文件，应视为通用词，不应被 smart filter 过滤
func patchPinyinIsCommon(candidates []candidate.Candidate) {
	for i := range candidates {
		candidates[i].IsCommon = true
	}
}

// LookupAbbrev 简拼查找，返回匹配声母缩写的词条
func (d *PinyinDict) LookupAbbrev(code string, limit int) []candidate.Candidate {
	if d.datReader != nil {
		return d.datReader.LookupAbbrev(code, limit)
	}
	if d.binReader != nil {
		return d.binReader.LookupAbbrev(code, limit)
	}
	if d.abbrevTrie == nil {
		return nil
	}
	code = strings.ToLower(code)
	results := d.abbrevTrie.Search(code)
	sort.SliceStable(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// buildAbbrev 从音节列表构建简拼编码（取每个音节首字母）
func buildAbbrev(syllables []string) string {
	var b strings.Builder
	for _, s := range syllables {
		if len(s) == 0 {
			return ""
		}
		b.WriteByte(s[0])
	}
	return b.String()
}
