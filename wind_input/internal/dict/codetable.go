// Package dict 提供词库管理功能
package dict

import (
	"bufio"
	"bytes"
	"fmt"
	"io"
	"os"
	"sort"
	"strconv"
	"strings"
	"unicode/utf16"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict/binformat"
	"github.com/huanfeng/wind_input/internal/dict/hotcache"
)

// codetableHotPrefixIndexN 内存模式 CodeTable 的 hot index 容量。
// 与 binformat.HotPrefixIndexN 保持一致，多份 reader（mmap / 内存模式）
// 指向同一 wdb 文件时通过 hotcache 共享。
const codetableHotPrefixIndexN = 500

// CodeTableHeader 码表头信息
type CodeTableHeader struct {
	Name          string // 词库名称
	Version       string // 版本
	Author        string // 作者
	CodeScheme    string // 编码方案（拼音/五笔86等）
	CodeLength    int    // 最大码长
	BWCodeLength  int    // 反查码长
	SpecialPrefix string // 特殊前缀（如zz用于反查）
	PhraseRule    int    // 短语规则
	HasWeight     bool   // 标记是否全文件无显式权重
}

// CodeTable 码表数据结构
type CodeTable struct {
	Header     CodeTableHeader
	entries    map[string][]candidate.Candidate // code -> candidates
	entryOrder int                              // 用于跟踪词条顺序，作为默认权重
	binReader  *binformat.DictReader            // 二进制模式读取器（mmap）
	// hotKey：内存模式（LoadBinaryMemory）下 hot index 的缓存键
	// LoadBinary 模式不需要——查询直接路由到 binReader 自身的 hotcache 路径
	hotKey hotcache.FileKey
}

// NewCodeTable 创建新的码表
func NewCodeTable() *CodeTable {
	return &CodeTable{
		entries: make(map[string][]candidate.Candidate),
	}
}

// LoadBinary 加载二进制格式码表（mmap 模式）
func (ct *CodeTable) LoadBinary(wdbPath string) error {
	reader, err := binformat.OpenDict(wdbPath)
	if err != nil {
		return fmt.Errorf("打开二进制码表失败: %w", err)
	}
	ct.binReader = reader
	ct.entries = nil // 释放内存模式数据
	return nil
}

// LoadBinaryMemory 加载二进制格式码表（全内存模式）
// 读取完 mmap 数据后构建 map，然后关闭 mmap 文件，以换取极致性能
func (ct *CodeTable) LoadBinaryMemory(wdbPath string) error {
	reader, err := binformat.OpenDict(wdbPath)
	if err != nil {
		return fmt.Errorf("打开二进制码表失败: %w", err)
	}
	defer reader.Close()

	if ct.entries == nil {
		ct.entries = make(map[string][]candidate.Candidate)
	}

	reader.ForEachEntry(func(code string, entries []candidate.Candidate) {
		// 完全拷贝切片数据，脱离 mmap 内存块。
		// Text 与 Code 都是 mmap-backed 的 string，必须复制底层字节。
		clonedCode := string([]byte(code))
		cloned := make([]candidate.Candidate, len(entries))
		for i, c := range entries {
			cloned[i] = c
			cloned[i].Text = string([]byte(c.Text))
			cloned[i].Code = clonedCode
		}
		ct.entries[clonedCode] = cloned
	})

	// hot index 共享键：与同文件 binformat.DictReader 一致，可跨 reader 共享
	ct.hotKey = hotcache.MakeFileKey(wdbPath)
	return nil
}

// IsBinaryMode 判断是否为二进制模式
func (ct *CodeTable) IsBinaryMode() bool {
	return ct.binReader != nil
}

// Close 关闭码表资源（二进制模式下释放 mmap）
func (ct *CodeTable) Close() error {
	if ct.binReader != nil {
		return ct.binReader.Close()
	}
	return nil
}

// LoadCodeTable 加载码表文件
// 支持 UTF-8 和 UTF-16 LE 编码
// 支持 [CODETABLEHEADER] 和 [CODETABLE] 格式
func LoadCodeTable(path string) (*CodeTable, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取文件失败: %w", err)
	}

	// 检测并转换编码
	content, err := decodeContent(data)
	if err != nil {
		return nil, fmt.Errorf("解码文件失败: %w", err)
	}

	ct := NewCodeTable()
	if err := ct.parse(content); err != nil {
		return nil, err
	}

	return ct, nil
}

// decodeContent 检测编码并转换为 UTF-8 字符串
func decodeContent(data []byte) (string, error) {
	// 检查 UTF-16 LE BOM (FF FE)
	if len(data) >= 2 && data[0] == 0xFF && data[1] == 0xFE {
		return decodeUTF16LE(data[2:])
	}

	// 检查 UTF-16 BE BOM (FE FF)
	if len(data) >= 2 && data[0] == 0xFE && data[1] == 0xFF {
		return decodeUTF16BE(data[2:])
	}

	// 检查 UTF-8 BOM (EF BB BF)
	if len(data) >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF {
		return string(data[3:]), nil
	}

	// 尝试检测是否为 UTF-16 LE（无 BOM 但每个字符后有 0x00）
	if isLikelyUTF16LE(data) {
		return decodeUTF16LE(data)
	}

	// 默认 UTF-8
	return string(data), nil
}

// isLikelyUTF16LE 检测数据是否可能是 UTF-16 LE 编码
func isLikelyUTF16LE(data []byte) bool {
	if len(data) < 10 {
		return false
	}
	// 检查前几个字符是否符合 UTF-16 LE 模式（ASCII 字符后跟 0x00）
	nullCount := 0
	for i := 1; i < len(data) && i < 20; i += 2 {
		if data[i] == 0x00 {
			nullCount++
		}
	}
	return nullCount > 5
}

// decodeUTF16LE 解码 UTF-16 LE 数据
func decodeUTF16LE(data []byte) (string, error) {
	if len(data)%2 != 0 {
		data = data[:len(data)-1] // 确保偶数长度
	}

	u16s := make([]uint16, len(data)/2)
	for i := 0; i < len(data); i += 2 {
		u16s[i/2] = uint16(data[i]) | uint16(data[i+1])<<8
	}

	runes := utf16.Decode(u16s)
	return string(runes), nil
}

// decodeUTF16BE 解码 UTF-16 BE 数据
func decodeUTF16BE(data []byte) (string, error) {
	if len(data)%2 != 0 {
		data = data[:len(data)-1]
	}

	u16s := make([]uint16, len(data)/2)
	for i := 0; i < len(data); i += 2 {
		u16s[i/2] = uint16(data[i])<<8 | uint16(data[i+1])
	}

	runes := utf16.Decode(u16s)
	return string(runes), nil
}

// parse 解析码表内容
func (ct *CodeTable) parse(content string) error {
	reader := bufio.NewReader(strings.NewReader(content))

	inHeader := false
	inTable := false
	lineNum := 0
	entryCount := 0

	for {
		line, err := reader.ReadString('\n')
		if err != nil && err != io.EOF {
			return fmt.Errorf("读取行失败: %w", err)
		}

		lineNum++
		line = strings.TrimSpace(line)

		// 跳过空行
		if line == "" {
			if err == io.EOF {
				break
			}
			continue
		}

		// 检查段落标记
		if strings.HasPrefix(line, "[") && strings.HasSuffix(line, "]") {
			section := strings.ToUpper(strings.Trim(line, "[]"))
			switch section {
			case "CODETABLEHEADER":
				inHeader = true
				inTable = false
			case "CODETABLE":
				inHeader = false
				inTable = true
			default:
				inHeader = false
				inTable = false
			}
			if err == io.EOF {
				break
			}
			continue
		}

		// 解析头部
		if inHeader {
			ct.parseHeaderLine(line)
		}

		// 解析码表条目
		if inTable {
			if ct.parseEntryLine(line) {
				entryCount++
			}
		}

		if err == io.EOF {
			break
		}
	}

	if entryCount == 0 {
		return fmt.Errorf("码表为空，未找到有效条目")
	}

	return nil
}

// parseHeaderLine 解析头部行
func (ct *CodeTable) parseHeaderLine(line string) {
	parts := strings.SplitN(line, "=", 2)
	if len(parts) != 2 {
		return
	}

	key := strings.TrimSpace(parts[0])
	value := strings.TrimSpace(parts[1])

	switch strings.ToLower(key) {
	case "name":
		ct.Header.Name = value
	case "version":
		ct.Header.Version = value
	case "author":
		ct.Header.Author = value
	case "codescheme":
		ct.Header.CodeScheme = value
	case "codelength":
		if v, err := strconv.Atoi(value); err == nil {
			ct.Header.CodeLength = v
		}
	case "bwcodelength":
		if v, err := strconv.Atoi(value); err == nil {
			ct.Header.BWCodeLength = v
		}
	case "specialprefix":
		ct.Header.SpecialPrefix = value
	case "phraserule":
		if v, err := strconv.Atoi(value); err == nil {
			ct.Header.PhraseRule = v
		}
	}
}

// parseEntryLine 解析码表条目行
// 格式: 编码\t汉字\t词频（词频可选）
func (ct *CodeTable) parseEntryLine(line string) bool {
	// 跳过注释
	if strings.HasPrefix(line, "#") || strings.HasPrefix(line, "//") {
		return false
	}

	// 使用 tab 分割
	parts := strings.Split(line, "\t")
	if len(parts) < 2 {
		// 尝试用空格分割
		parts = strings.Fields(line)
		if len(parts) < 2 {
			return false
		}
	}

	code := strings.TrimSpace(parts[0])
	text := strings.TrimSpace(parts[1])

	if code == "" || text == "" {
		return false
	}

	// 解析词频（可选）
	// 如果没有词频字段，使用文件顺序作为权重（越靠前权重越高）
	weight := 0
	hasExplicitWeight := false
	if len(parts) >= 3 {
		if w, err := strconv.Atoi(strings.TrimSpace(parts[2])); err == nil {
			weight = w
			hasExplicitWeight = true
			ct.Header.HasWeight = true
		}
	}

	// 如果没有显式词频，使用递减的顺序权重
	// 基数设为 1000000，确保文件靠前的词条有更高权重
	if !hasExplicitWeight {
		weight = 1000000 - ct.entryOrder
		if weight < 0 {
			weight = 0
		}
	}

	// 添加到码表
	cand := candidate.Candidate{
		Text:         text,
		Code:         code,
		Weight:       weight,
		NaturalOrder: ct.entryOrder, // 全局顺序（文件中的出现位置，跨编码递增）
		IsCommon:     IsStringCommon(text),
	}

	ct.entries[code] = append(ct.entries[code], cand)
	ct.entryOrder++
	return true
}

// patchIsCommon 为二进制模式返回的候选补充 IsCommon 标记
// 二进制格式不存储 IsCommon，需要在 CodeTable 层补充
func patchIsCommon(candidates []candidate.Candidate) []candidate.Candidate {
	for i := range candidates {
		candidates[i].IsCommon = IsStringCommon(candidates[i].Text)
	}
	return candidates
}

// Lookup 查找编码对应的候选词
func (ct *CodeTable) Lookup(code string) []candidate.Candidate {
	if ct.binReader != nil {
		return patchIsCommon(ct.binReader.Lookup(code))
	}
	code = strings.ToLower(code)
	return ct.entries[code]
}

// LookupPrefix 前缀匹配查找。
//
// 二进制模式：路由到 binReader.LookupPrefix（自带 hot index 与 top-K 优化）。
// 内存模式：单字母前缀走 hotcache 共享缓存（与同文件 binReader 共享条目）；
// 多字母前缀用 min-heap top-K，避免对全量候选做 O(N log N) 排序；
// limit == 0 返回未排序全量（由调用方排序）。
func (ct *CodeTable) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	if ct.binReader != nil {
		return patchIsCommon(ct.binReader.LookupPrefix(prefix, limit))
	}
	prefix = strings.ToLower(prefix)

	if len(prefix) == 1 && limit > 0 && limit <= codetableHotPrefixIndexN && ct.hotKey != "" {
		cached := hotcache.GetOrBuild(ct.hotKey, prefix[0], func() []candidate.Candidate {
			return ct.scanPrefixMem(prefix, codetableHotPrefixIndexN)
		})
		if limit > len(cached) {
			limit = len(cached)
		}
		out := make([]candidate.Candidate, limit)
		copy(out, cached[:limit])
		return out
	}

	if limit > 0 {
		return ct.scanPrefixMem(prefix, limit)
	}

	var results []candidate.Candidate
	for code, candidates := range ct.entries {
		if strings.HasPrefix(code, prefix) {
			results = append(results, candidates...)
		}
	}
	return results
}

// scanPrefixMem 遍历 entries map 并通过 min-heap 选 top-K（仅内存模式）。
func (ct *CodeTable) scanPrefixMem(prefix string, limit int) []candidate.Candidate {
	picker := newMemTopKPicker(limit)
	for code, candidates := range ct.entries {
		if strings.HasPrefix(code, prefix) {
			for _, c := range candidates {
				picker.offer(c)
			}
		}
	}
	return picker.sorted()
}

// LookupPrefixExcludeExact 前缀匹配查找（排除精确匹配）
func (ct *CodeTable) LookupPrefixExcludeExact(prefix string, limit int) []candidate.Candidate {
	if ct.binReader != nil {
		return patchIsCommon(ct.binReader.LookupPrefixExcludeExact(prefix, limit))
	}
	prefix = strings.ToLower(prefix)
	var results []candidate.Candidate

	for code, candidates := range ct.entries {
		if code != prefix && strings.HasPrefix(code, prefix) {
			results = append(results, candidates...)
		}
	}

	sort.Sort(candidate.CandidateList(results))
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// LookupPrefixBFS 广度优先前缀查找
func (ct *CodeTable) LookupPrefixBFS(prefix string, limitPerBucket int, maxDepth int) []candidate.Candidate {
	if ct.binReader != nil {
		// 二进制模式：使用底层的 BFS，并注入 IsCommon 检查（内存缓存的单字判断）
		return ct.binReader.LookupPrefixBFS(prefix, limitPerBucket, maxDepth, IsStringCommon)
	}

	// 内存模式降级实现：收集后手动分桶
	prefix = strings.ToLower(prefix)
	var results []candidate.Candidate
	buckets := make([][]candidate.Candidate, maxDepth)

	for code, candidates := range ct.entries {
		if code != prefix && strings.HasPrefix(code, prefix) {
			depth := len(code) - len(prefix)
			if depth > 0 && depth <= maxDepth {
				bucketIdx := depth - 1
				// 复制并补充 IsCommon
				for _, c := range candidates {
					c.IsCommon = IsStringCommon(c.Text)
					buckets[bucketIdx] = append(buckets[bucketIdx], c)
				}
			}
		}
	}

	for _, bucket := range buckets {
		if len(bucket) == 0 {
			continue
		}
		sort.Sort(candidate.CandidateList(bucket))

		if limitPerBucket > 0 && len(bucket) > limitPerBucket {
			var common, rare []candidate.Candidate
			for _, c := range bucket {
				if c.IsCommon {
					common = append(common, c)
				} else {
					rare = append(rare, c)
				}
			}

			var truncated []candidate.Candidate
			if len(common) >= limitPerBucket {
				truncated = common[:limitPerBucket]
			} else {
				truncated = append(truncated, common...)
				needed := limitPerBucket - len(common)
				if needed > len(rare) {
					needed = len(rare)
				}
				truncated = append(truncated, rare[:needed]...)
			}
			results = append(results, truncated...)
		} else {
			results = append(results, bucket...)
		}
	}
	return results
}

// EntryCount 返回词条数量
func (ct *CodeTable) EntryCount() int {
	if ct.binReader != nil {
		return ct.binReader.EntryCount()
	}
	count := 0
	for _, candidates := range ct.entries {
		count += len(candidates)
	}
	return count
}

// GetMaxCodeLength 获取最大码长
func (ct *CodeTable) GetMaxCodeLength() int {
	if ct.Header.CodeLength > 0 {
		return ct.Header.CodeLength
	}
	if ct.binReader != nil {
		return 0
	}
	// 如果头部没有指定，从数据中推断
	maxLen := 0
	for code := range ct.entries {
		if len(code) > maxLen {
			maxLen = len(code)
		}
	}
	return maxLen
}

// GetCodeScheme 获取编码方案
func (ct *CodeTable) GetCodeScheme() string {
	return ct.Header.CodeScheme
}

// IsWubi 判断是否为五笔码表
func (ct *CodeTable) IsWubi() bool {
	scheme := strings.ToLower(ct.Header.CodeScheme)
	return strings.Contains(scheme, "五笔") ||
		strings.Contains(scheme, "wubi") ||
		ct.Header.CodeLength == 4
}

// IsPinyin 判断是否为拼音码表
func (ct *CodeTable) IsPinyin() bool {
	scheme := strings.ToLower(ct.Header.CodeScheme)
	return strings.Contains(scheme, "拼音") ||
		strings.Contains(scheme, "pinyin")
}

// GetEntries 获取所有条目（用于反向查找）
func (ct *CodeTable) GetEntries() map[string][]candidate.Candidate {
	if ct.binReader != nil {
		// 二进制模式下构建 map（仅用于转换时，正常运行不走此路径）
		result := make(map[string][]candidate.Candidate)
		ct.binReader.ForEachEntry(func(code string, entries []candidate.Candidate) {
			result[code] = entries
		})
		return result
	}
	return ct.entries
}

// BuildReverseIndex 构建反向索引（文字 -> 编码列表，按词条权重降序排序）
//
// 排序规则确保下游（如自动造词的编码计算器）取 codes[0] 即得到"最常用全码"，
// 不会被 import_tables 中的异体字代码（如四叠字 cccc 对应"晶/淼/众"等同码）干扰。
//
// 排序优先级：weight 降序 → code 长度降序 → code 字典序升序（保证稳定）。
func (ct *CodeTable) BuildReverseIndex() map[string][]string {
	type codeRef struct {
		code   string
		weight int
	}
	collect := make(map[string][]codeRef)
	if ct.binReader != nil {
		ct.binReader.ForEachEntry(func(code string, entries []candidate.Candidate) {
			for _, cand := range entries {
				collect[cand.Text] = append(collect[cand.Text], codeRef{code: code, weight: cand.Weight})
			}
		})
	} else {
		for code, candidates := range ct.entries {
			for _, cand := range candidates {
				collect[cand.Text] = append(collect[cand.Text], codeRef{code: code, weight: cand.Weight})
			}
		}
	}

	reverseIndex := make(map[string][]string, len(collect))
	for text, refs := range collect {
		sort.Slice(refs, func(i, j int) bool {
			if refs[i].weight != refs[j].weight {
				return refs[i].weight > refs[j].weight
			}
			if len(refs[i].code) != len(refs[j].code) {
				return len(refs[i].code) > len(refs[j].code)
			}
			return refs[i].code < refs[j].code
		})
		codes := make([]string, len(refs))
		for i, r := range refs {
			codes[i] = r.code
		}
		reverseIndex[text] = codes
	}
	return reverseIndex
}

// ExportTo 将码表写入 writer（用于导出）
func (ct *CodeTable) ExportTo(w io.Writer) error {
	buf := &bytes.Buffer{}

	// 写入头部
	buf.WriteString("[CODETABLEHEADER]\n")
	if ct.Header.Name != "" {
		buf.WriteString(fmt.Sprintf("Name=%s\n", ct.Header.Name))
	}
	if ct.Header.Version != "" {
		buf.WriteString(fmt.Sprintf("Version=%s\n", ct.Header.Version))
	}
	if ct.Header.Author != "" {
		buf.WriteString(fmt.Sprintf("Author=%s\n", ct.Header.Author))
	}
	if ct.Header.CodeScheme != "" {
		buf.WriteString(fmt.Sprintf("CodeScheme=%s\n", ct.Header.CodeScheme))
	}
	buf.WriteString(fmt.Sprintf("CodeLength=%d\n", ct.Header.CodeLength))
	buf.WriteString("[CODETABLE]\n")

	// 写入条目
	for code, candidates := range ct.entries {
		for _, cand := range candidates {
			buf.WriteString(fmt.Sprintf("%s\t%s\t%d\n", code, cand.Text, cand.Weight))
		}
	}

	_, err := w.Write(buf.Bytes())
	return err
}
