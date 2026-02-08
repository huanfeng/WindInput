package dict

import (
	"bufio"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// PinyinDict 拼音专用词库（基于 Trie 索引）
// 支持从 Rime dict.yaml 格式或 CodeTable 格式加载
type PinyinDict struct {
	trie       *Trie // Trie 索引，用于精确和前缀搜索
	abbrevTrie *Trie // 简拼索引（声母首字母 → 词条），用于简拼词组匹配
	entryCount int
}

// NewPinyinDict 创建拼音词库
func NewPinyinDict() *PinyinDict {
	return &PinyinDict{}
}

// LoadRimeDir 从目录加载 Rime dict.yaml 格式词库
// 自动查找并加载 8105.dict.yaml 和 base.dict.yaml
func (d *PinyinDict) LoadRimeDir(dirPath string) error {
	d.trie = NewTrie()
	d.abbrevTrie = NewTrie()
	d.entryCount = 0

	files := []string{
		"8105.dict.yaml", // 单字
		"base.dict.yaml", // 基础词组
	}

	loaded := 0
	for _, name := range files {
		path := filepath.Join(dirPath, name)
		if _, err := os.Stat(path); os.IsNotExist(err) {
			log.Printf("[PinyinDict] 跳过不存在的文件: %s", path)
			continue
		}
		count, err := d.loadRimeFile(path)
		if err != nil {
			log.Printf("[PinyinDict] 加载 %s 失败: %v", name, err)
			continue
		}
		log.Printf("[PinyinDict] 加载 %s: %d 条", name, count)
		loaded++
	}

	if loaded == 0 {
		return fmt.Errorf("未找到任何 Rime 词库文件（目录: %s）", dirPath)
	}

	d.entryCount = d.trie.EntryCount()
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
	if d.trie == nil {
		return nil
	}
	return d.trie.Search(strings.ToLower(pinyin))
}

// LookupPhrase 查找短语（将音节拼接后查找）
func (d *PinyinDict) LookupPhrase(syllables []string) []candidate.Candidate {
	if d.trie == nil || len(syllables) == 0 {
		return nil
	}
	key := strings.ToLower(strings.Join(syllables, ""))
	return d.trie.Search(key)
}

// LookupPrefix 前缀查找，返回所有以 prefix 开头的候选词
func (d *PinyinDict) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	if d.trie == nil {
		return nil
	}
	prefix = strings.ToLower(prefix)
	results := d.trie.SearchPrefix(prefix, limit)
	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// HasPrefix 检查是否有以 prefix 开头的词条
func (d *PinyinDict) HasPrefix(prefix string) bool {
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
	return results
}

// SearchPrefix 前缀查询
func (l *PinyinDictLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	return l.dict.LookupPrefix(prefix, limit)
}

// SearchAbbrev 简拼查询
func (l *PinyinDictLayer) SearchAbbrev(code string, limit int) []candidate.Candidate {
	return l.dict.LookupAbbrev(code, limit)
}

// LookupAbbrev 简拼查找，返回匹配声母缩写的词条
func (d *PinyinDict) LookupAbbrev(code string, limit int) []candidate.Candidate {
	if d.abbrevTrie == nil {
		return nil
	}
	code = strings.ToLower(code)
	results := d.abbrevTrie.Search(code)
	sort.Slice(results, func(i, j int) bool {
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
