package dictcache

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log/slog"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"io"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/binformat"
	"github.com/huanfeng/wind_input/internal/dict/datformat"
)

// CodeTableMeta 存储 CodeTable 的 Header 信息（sidecar 文件）
type CodeTableMeta struct {
	Name          string `json:"name"`
	Version       string `json:"version"`
	Author        string `json:"author"`
	CodeScheme    string `json:"code_scheme"`
	CodeLength    int    `json:"code_length"`
	BWCodeLength  int    `json:"bw_code_length"`
	SpecialPrefix string `json:"special_prefix"`
	PhraseRule    int    `json:"phrase_rule"`
	EntryCount    int    `json:"entry_count"`
	HasWeight     bool   `json:"has_weight"`
}

// MetaPath 返回 wdb 文件对应的 meta.json 路径
func MetaPath(wdbPath string) string {
	return wdbPath + ".meta.json"
}

// ConvertCodeTableToWdb 将文本码表转换为 wdb 二进制格式
func ConvertCodeTableToWdb(srcPath, wdbPath string, logger *slog.Logger) error {
	logger.Info("转换码表", "src", srcPath, "dst", wdbPath)

	ct, err := dict.LoadCodeTable(srcPath)
	if err != nil {
		return fmt.Errorf("加载码表失败: %w", err)
	}

	// 构建 DictWriter
	writer := binformat.NewDictWriter()
	entries := ct.GetEntries()

	for code, candidates := range entries {
		binEntries := make([]binformat.DictEntry, len(candidates))
		for i, c := range candidates {
			binEntries[i] = binformat.DictEntry{
				Text:   c.Text,
				Weight: int32(c.Weight),
				Order:  int32(c.NaturalOrder),
			}
		}
		writer.AddCode(code, binEntries)
	}

	// 将 CodeTableHeader 编为 JSON 嵌入 wdb
	meta := CodeTableMeta{
		Name:          ct.Header.Name,
		Version:       ct.Header.Version,
		Author:        ct.Header.Author,
		CodeScheme:    ct.Header.CodeScheme,
		CodeLength:    ct.Header.CodeLength,
		BWCodeLength:  ct.Header.BWCodeLength,
		SpecialPrefix: ct.Header.SpecialPrefix,
		PhraseRule:    ct.Header.PhraseRule,
		EntryCount:    ct.EntryCount(),
		HasWeight:     ct.Header.HasWeight,
	}
	metaJSON, err := json.Marshal(&meta)
	if err != nil {
		return fmt.Errorf("序列化 meta 失败: %w", err)
	}
	writer.SetMeta(metaJSON)

	if err := atomicWriteWdb(wdbPath, func(w io.Writer) error {
		return writer.Write(w)
	}); err != nil {
		return err
	}

	// Deprecated: 写入 meta.json sidecar（Phase 3 移除，manager_init.go 仍在使用）
	if err := writeMetaJSON(MetaPath(wdbPath), &meta); err != nil {
		logger.Warn("写入 sidecar meta.json 失败", "error", err)
	}

	logger.Info("码表转换完成", "codes", len(entries))
	return nil
}

// LoadCodeTableMeta 加载 meta.json（Deprecated: Phase 3 移除，改用 LoadCodeTableMetaFromWdb）
func LoadCodeTableMeta(wdbPath string) (*CodeTableMeta, error) {
	data, err := os.ReadFile(MetaPath(wdbPath))
	if err != nil {
		return nil, err
	}
	var meta CodeTableMeta
	if err := json.Unmarshal(data, &meta); err != nil {
		return nil, err
	}
	return &meta, nil
}

// LoadCodeTableMetaFromWdb 从 wdb 文件嵌入的 meta 段读取元数据
func LoadCodeTableMetaFromWdb(reader *binformat.DictReader) (*CodeTableMeta, error) {
	data := reader.ReadMeta()
	if data == nil {
		return nil, fmt.Errorf("wdb 文件不包含元数据")
	}
	var meta CodeTableMeta
	if err := json.Unmarshal(data, &meta); err != nil {
		return nil, fmt.Errorf("解析 wdb 元数据失败: %w", err)
	}
	return &meta, nil
}

func writeMetaJSON(path string, meta *CodeTableMeta) error {
	data, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0644)
}

// ConvertPinyinToWdb 将拼音 YAML 词库转换为 wdb 二进制格式
// mainDictPath 为主词库 .dict.yaml 文件路径（如 rime_frost.dict.yaml），
// 自动从其 import_tables 发现关联词库（如 cn_dicts/8105.dict.yaml）。
// normalizer 可选，非 nil 时对权重做归一化映射。
func ConvertPinyinToWdb(mainDictPath, wdbPath string, logger *slog.Logger, normalizer ...*dict.WeightNormalizer) error {
	logger.Info("转换拼音词库", "src", mainDictPath, "dst", wdbPath)

	dictDir := filepath.Dir(mainDictPath)
	codeEntries := make(map[string][]dictEntry)
	abbrevEntries := make(map[string][]dictEntry)
	totalCount := 0
	globalOrder := 0

	// 从 import_tables 发现关联词库
	allFiles := discoverRimePinyinFiles(mainDictPath)
	for _, name := range allFiles {
		path := filepath.Join(dictDir, name)
		if _, err := os.Stat(path); os.IsNotExist(err) {
			continue
		}

		count, err := loadRimeFile(path, codeEntries, abbrevEntries, &globalOrder, logger)
		if err != nil {
			logger.Warn("加载词库失败", "name", name, "error", err)
			continue
		}
		logger.Info("加载词库", "name", name, "count", count)
		totalCount += count
	}

	if totalCount == 0 {
		return fmt.Errorf("未加载到任何拼音词条")
	}

	// 发现并应用词库补丁
	var importNames []string
	for _, f := range allFiles {
		importNames = append(importNames, strings.TrimSuffix(f, ".dict.yaml"))
	}
	patchFiles := FindPatchFiles(mainDictPath, importNames)
	if len(patchFiles) > 0 {
		patch := LoadAndMergePatchFiles(patchFiles, logger)
		if !patch.IsEmpty() {
			added, modified, deleted := ApplyDictPatch(codeEntries, abbrevEntries, patch, &globalOrder, logger)
			logger.Info("拼音词库补丁已应用", "added", added, "modified", modified, "deleted", deleted)
			totalCount += added - deleted
		}
	}

	writer := binformat.NewDictWriter()

	// 获取归一化器（可选）
	var norm *dict.WeightNormalizer
	if len(normalizer) > 0 {
		norm = normalizer[0]
	}

	for code, entries := range codeEntries {
		sort.SliceStable(entries, func(i, j int) bool {
			if entries[i].weight != entries[j].weight {
				return entries[i].weight > entries[j].weight
			}
			return entries[i].naturalOrder < entries[j].naturalOrder
		})
		binEntries := make([]binformat.DictEntry, len(entries))
		for i, e := range entries {
			w := e.weight
			if norm != nil {
				w = norm.Normalize(w)
			}
			binEntries[i] = binformat.DictEntry{
				Text:   e.text,
				Weight: int32(w),
				Order:  int32(e.naturalOrder),
			}
		}
		writer.AddCode(code, binEntries)
	}

	for abbrev, entries := range abbrevEntries {
		sort.SliceStable(entries, func(i, j int) bool {
			if entries[i].weight != entries[j].weight {
				return entries[i].weight > entries[j].weight
			}
			return entries[i].naturalOrder < entries[j].naturalOrder
		})
		binEntries := make([]binformat.DictEntry, len(entries))
		for i, e := range entries {
			w := e.weight
			if norm != nil {
				w = norm.Normalize(w)
			}
			binEntries[i] = binformat.DictEntry{
				Text:   e.text,
				Weight: int32(w),
				Order:  int32(e.naturalOrder),
			}
		}
		writer.AddAbbrev(abbrev, binEntries)
	}

	if err := atomicWriteWdb(wdbPath, func(w io.Writer) error {
		return writer.Write(w)
	}); err != nil {
		return err
	}

	logger.Info("拼音词库转换完成", "codes", len(codeEntries), "abbrevs", len(abbrevEntries))
	return nil
}

// RimePinyinSourcePaths 返回拼音词库的所有源文件路径（用于缓存失效检测）
// mainDictPath 为主词库文件路径，自动从 import_tables 发现关联词库及补丁文件
func RimePinyinSourcePaths(mainDictPath string) []string {
	paths := []string{mainDictPath}
	dictDir := filepath.Dir(mainDictPath)

	importFiles := discoverRimePinyinFiles(mainDictPath)
	for _, name := range importFiles {
		p := filepath.Join(dictDir, name)
		if _, err := os.Stat(p); err == nil {
			paths = append(paths, p)
		}
	}

	// 包含补丁文件（补丁变更时触发缓存重建）
	// 将 import 文件名转换回 import_tables 名称格式（去掉 .dict.yaml 后缀）
	var importNames []string
	for _, f := range importFiles {
		importNames = append(importNames, strings.TrimSuffix(f, ".dict.yaml"))
	}
	paths = append(paths, FindPatchFiles(mainDictPath, importNames)...)

	return paths
}

// discoverRimePinyinFiles 从主词库的 import_tables 发现关联词库的相对路径
// 严格只加载 import_tables 中声明的词库，保留原始路径结构（如 "cn_dicts/8105.dict.yaml"）
func discoverRimePinyinFiles(mainDictPath string) []string {
	importNames := parseRimeImportTables(mainDictPath)

	var files []string
	for _, name := range importNames {
		// 保留原始路径: "cn_dicts/8105" → "cn_dicts/8105.dict.yaml"
		files = append(files, name+".dict.yaml")
	}

	return files
}

// ConvertUnigramToWdb 将 unigram.txt 转换为 unigram.wdb
func ConvertUnigramToWdb(txtPath, wdbPath string, logger *slog.Logger) error {
	logger.Info("转换 Unigram", "src", txtPath, "dst", wdbPath)

	file, err := os.Open(txtPath)
	if err != nil {
		return fmt.Errorf("打开 unigram 文件失败: %w", err)
	}
	defer file.Close()

	freqs := make(map[string]float64)
	var total float64

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}
		word := parts[0]
		freq, err := strconv.ParseFloat(parts[1], 64)
		if err != nil {
			continue
		}
		freqs[word] = freq
		total += freq
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("读取 unigram 文件失败: %w", err)
	}
	if total == 0 {
		return fmt.Errorf("unigram 文件为空")
	}

	writer := binformat.NewUnigramWriter()
	for word, freq := range freqs {
		logProb := math.Log(freq / total)
		writer.Add(word, logProb)
	}

	if err := atomicWriteWdb(wdbPath, func(w io.Writer) error {
		return writer.Write(w)
	}); err != nil {
		return err
	}

	logger.Info("Unigram 转换完成", "count", len(freqs))
	return nil
}

// ConvertRimeCodetableToWdb 将 rime 格式码表词库转换为 wdb 二进制格式
// mainDictPath 为主词库 .dict.yaml 文件路径，自动从其 YAML header 的
// import_tables 发现关联词库，并扫描同目录下同名前缀的额外词库文件。
// 遵循 RIME 标准：所有词库平等合并，按 weight 统一排序。
// 精确匹配优先于前缀匹配由引擎层 -2000000 降权保障，无需此处调整权重。
func ConvertRimeCodetableToWdb(mainDictPath, wdbPath string, logger *slog.Logger, normalizer ...*dict.WeightNormalizer) error {
	logger.Info("转换 rime 码表词库", "src", mainDictPath, "dst", wdbPath)

	dictDir := filepath.Dir(mainDictPath)
	codeEntries := make(map[string][]dictEntry)
	totalCount := 0
	globalOrder := 0

	// 1. 加载主词库
	count, mainHasWeight, err := loadRimeCodetableFile(mainDictPath, codeEntries, &globalOrder, logger)
	if err != nil {
		return fmt.Errorf("加载主词库失败: %w", err)
	}
	hasWeight := mainHasWeight
	logger.Info("加载词库", "name", filepath.Base(mainDictPath), "count", count)
	totalCount += count

	// 2. 发现关联词库：import_tables + 目录扫描
	importNames := discoverRimeCodetableImports(mainDictPath)
	for _, name := range importNames {
		path := filepath.Join(dictDir, name+".dict.yaml")
		if _, statErr := os.Stat(path); os.IsNotExist(statErr) {
			continue
		}
		c, fileHasWeight, loadErr := loadRimeCodetableFile(path, codeEntries, &globalOrder, logger)
		if loadErr != nil {
			logger.Warn("加载词库失败", "name", name, "error", loadErr)
			continue
		}
		if fileHasWeight {
			hasWeight = true
		}
		logger.Info("加载词库", "name", name, "count", c)
		totalCount += c
	}

	if totalCount == 0 {
		return fmt.Errorf("未加载到任何五笔词条")
	}

	// 3. 发现并应用词库补丁
	patchFiles := FindPatchFiles(mainDictPath, importNames)
	if len(patchFiles) > 0 {
		patch := LoadAndMergePatchFiles(patchFiles, logger)
		if !patch.IsEmpty() {
			added, modified, deleted := ApplyDictPatch(codeEntries, nil, patch, &globalOrder, logger)
			logger.Info("词库补丁已应用", "added", added, "modified", modified, "deleted", deleted)
			totalCount += added - deleted
		}
	}

	// 获取归一化器（可选）
	var norm *dict.WeightNormalizer
	if len(normalizer) > 0 {
		norm = normalizer[0]
	}

	writer := binformat.NewDictWriter()

	for code, entries := range codeEntries {
		sort.SliceStable(entries, func(i, j int) bool {
			if entries[i].weight != entries[j].weight {
				return entries[i].weight > entries[j].weight
			}
			return entries[i].naturalOrder < entries[j].naturalOrder
		})
		binEntries := make([]binformat.DictEntry, len(entries))
		for i, e := range entries {
			w := e.weight
			if norm != nil {
				w = norm.Normalize(w)
			}
			binEntries[i] = binformat.DictEntry{
				Text:   e.text,
				Weight: int32(w),
				Order:  int32(e.naturalOrder),
			}
		}
		writer.AddCode(code, binEntries)
	}

	// 生成元数据（从主词库文件名推导）
	mainName := strings.TrimSuffix(filepath.Base(mainDictPath), ".dict.yaml")
	meta := CodeTableMeta{
		Name:       mainName,
		Version:    "rime",
		CodeScheme: "五笔字型86版",
		CodeLength: 4,
		EntryCount: totalCount,
		HasWeight:  hasWeight,
	}
	metaJSON, err := json.Marshal(&meta)
	if err != nil {
		return fmt.Errorf("序列化 meta 失败: %w", err)
	}
	writer.SetMeta(metaJSON)

	if err := atomicWriteWdb(wdbPath, func(w io.Writer) error {
		return writer.Write(w)
	}); err != nil {
		return err
	}

	if err := writeMetaJSON(MetaPath(wdbPath), &meta); err != nil {
		logger.Warn("写入 sidecar meta.json 失败", "error", err)
	}

	logger.Info("rime 码表词库转换完成", "codes", len(codeEntries), "count", totalCount)
	return nil
}

// RimeCodetableSourcePaths 返回 rime 码表词库的所有源文件路径（用于缓存失效检测）
// mainDictPath 为主词库文件路径，自动发现关联词库及补丁文件
func RimeCodetableSourcePaths(mainDictPath string) []string {
	paths := []string{mainDictPath}
	dictDir := filepath.Dir(mainDictPath)

	importNames := discoverRimeCodetableImports(mainDictPath)
	for _, name := range importNames {
		p := filepath.Join(dictDir, name+".dict.yaml")
		if _, err := os.Stat(p); err == nil {
			paths = append(paths, p)
		}
	}

	// 包含补丁文件（补丁变更时触发缓存重建）
	paths = append(paths, FindPatchFiles(mainDictPath, importNames)...)

	return paths
}

// discoverRimeCodetableImports 从主词库 YAML header 的 import_tables 发现关联词库名称
// 严格只加载 import_tables 中声明的词库，不进行目录扫描，避免加载不合理的文件
func discoverRimeCodetableImports(mainDictPath string) []string {
	return parseRimeImportTables(mainDictPath)
}

// parseRimeImportTables 解析 rime .dict.yaml 文件 YAML header 中的 import_tables 列表
func parseRimeImportTables(path string) []string {
	file, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	inHeader := false
	inImportTables := false
	var tables []string

	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)

		if trimmed == "---" {
			inHeader = true
			continue
		}
		if trimmed == "..." {
			break
		}
		if !inHeader {
			continue
		}

		if strings.HasPrefix(trimmed, "import_tables:") {
			inImportTables = true
			continue
		}

		if inImportTables {
			if name, ok := strings.CutPrefix(trimmed, "- "); ok {
				// 移除行内注释
				if idx := strings.Index(name, "#"); idx >= 0 {
					name = strings.TrimSpace(name[:idx])
				}
				name = strings.TrimSpace(name)
				if name != "" {
					tables = append(tables, name)
				}
			} else if strings.HasPrefix(trimmed, "#") {
				// 跳过注释行（如被注释掉的 import 条目）
				continue
			} else if trimmed != "" {
				// 遇到非 import_tables 内容，结束解析
				inImportTables = false
			}
		}
	}

	return tables
}

// loadRimeCodetableFile 解析 rime 格式的码表 .dict.yaml 文件。
// 列顺序由 YAML header 的 columns 字段决定，默认为 text/code/weight。
//
// 权重策略基于词库自身的 sort 字段：
//   - sort: by_weight → 使用显式权重（权威词库，如主词库）
//   - sort: original  → 忽略显式权重，统一 weight=1（补充词库，不与主词库竞争）
func loadRimeCodetableFile(path string, codeEntries map[string][]dictEntry, globalOrder *int, logger *slog.Logger) (int, bool, error) {
	file, err := os.Open(path)
	if err != nil {
		return 0, false, err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	inHeader := true
	sortMode := ""
	// 列索引：默认 rime 标准顺序 text/code/weight
	colText, colCode, colWeight := 0, 1, 2
	inColumns := false
	var columnNames []string
	count := 0
	hasWeight := false

	for scanner.Scan() {
		line := scanner.Text()

		if inHeader {
			trimmed := strings.TrimSpace(line)
			if trimmed == "..." {
				// 解析完 header，根据收集到的 columns 列表确定索引
				if len(columnNames) > 0 {
					colText, colCode, colWeight = -1, -1, -1
					for i, name := range columnNames {
						switch name {
						case "text":
							colText = i
						case "code":
							colCode = i
						case "weight":
							colWeight = i
						}
					}
				}
				inHeader = false
				continue
			}
			// 提取 sort 字段
			if val, ok := strings.CutPrefix(trimmed, "sort:"); ok {
				if idx := strings.Index(val, "#"); idx >= 0 {
					val = val[:idx]
				}
				sortMode = strings.TrimSpace(val)
				inColumns = false
				continue
			}
			// 收集 columns 列表
			if strings.HasPrefix(trimmed, "columns:") {
				inColumns = true
				columnNames = nil
				continue
			}
			if inColumns {
				if name, ok := strings.CutPrefix(trimmed, "- "); ok {
					name = strings.TrimSpace(name)
					if idx := strings.Index(name, "#"); idx >= 0 {
						name = strings.TrimSpace(name[:idx])
					}
					if name != "" {
						columnNames = append(columnNames, name)
					}
				} else if trimmed != "" && !strings.HasPrefix(trimmed, "#") {
					inColumns = false
				}
			}
			continue
		}

		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")

		getCol := func(idx int) string {
			if idx < 0 || idx >= len(parts) {
				return ""
			}
			return strings.TrimSpace(parts[idx])
		}

		text := getCol(colText)
		code := getCol(colCode)

		if text == "" || code == "" {
			continue
		}

		// 权重策略：by_weight 使用原始权重，original 统一为 1
		weight := 1
		if sortMode == "by_weight" {
			if ws := getCol(colWeight); ws != "" {
				if w, err := strconv.Atoi(ws); err == nil && w > 0 {
					weight = w
					hasWeight = true
				}
			}
		}

		codeEntries[code] = append(codeEntries[code], dictEntry{
			text:         text,
			weight:       weight,
			naturalOrder: *globalOrder,
		})
		*globalOrder++
		count++
	}

	return count, hasWeight, scanner.Err()
}

// atomicWriteWdb 原子写入 wdb 文件：先写入临时文件，再 rename 到目标路径
// 防止进程被杀或并发写入导致目标文件被截断
func atomicWriteWdb(wdbPath string, writeFn func(w io.Writer) error) error {
	os.MkdirAll(filepath.Dir(wdbPath), 0755)

	tmpPath := wdbPath + ".tmp"
	f, err := os.Create(tmpPath)
	if err != nil {
		return fmt.Errorf("创建临时文件失败: %w", err)
	}

	bw := bufio.NewWriter(f)
	if err := writeFn(bw); err != nil {
		f.Close()
		os.Remove(tmpPath)
		return fmt.Errorf("写入 wdb 失败: %w", err)
	}
	if err := bw.Flush(); err != nil {
		f.Close()
		os.Remove(tmpPath)
		return fmt.Errorf("flush 失败: %w", err)
	}
	if err := f.Close(); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("关闭临时文件失败: %w", err)
	}

	// Windows 上 rename 目标文件已存在时会失败，需先删除
	os.Remove(wdbPath)
	if err := os.Rename(tmpPath, wdbPath); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("原子替换失败: %w", err)
	}
	return nil
}

// ConvertPinyinToWdat 将 Rime 拼音词库转换为 wdat (DAT) 格式
func ConvertPinyinToWdat(mainDictPath, wdatPath string, logger *slog.Logger, normalizer ...*dict.WeightNormalizer) error {
	logger.Info("转换拼音词库(DAT)", "src", mainDictPath, "dst", wdatPath)

	dictDir := filepath.Dir(mainDictPath)
	codeEntries := make(map[string][]dictEntry)
	abbrevEntries := make(map[string][]dictEntry)
	totalCount := 0
	wdatGlobalOrder := 0

	allFiles := discoverRimePinyinFiles(mainDictPath)
	for _, name := range allFiles {
		path := filepath.Join(dictDir, name)
		if _, err := os.Stat(path); os.IsNotExist(err) {
			continue
		}
		count, err := loadRimeFile(path, codeEntries, abbrevEntries, &wdatGlobalOrder, logger)
		if err != nil {
			logger.Warn("加载词库失败", "name", name, "error", err)
			continue
		}
		logger.Info("加载词库", "name", name, "count", count)
		totalCount += count
	}

	if totalCount == 0 {
		return fmt.Errorf("未加载到任何拼音词条")
	}

	// 发现并应用词库补丁
	var wdatImportNames []string
	for _, f := range allFiles {
		wdatImportNames = append(wdatImportNames, strings.TrimSuffix(f, ".dict.yaml"))
	}
	wdatPatchFiles := FindPatchFiles(mainDictPath, wdatImportNames)
	if len(wdatPatchFiles) > 0 {
		patch := LoadAndMergePatchFiles(wdatPatchFiles, logger)
		if !patch.IsEmpty() {
			added, modified, deleted := ApplyDictPatch(codeEntries, abbrevEntries, patch, &wdatGlobalOrder, logger)
			logger.Info("拼音词库(DAT)补丁已应用", "added", added, "modified", modified, "deleted", deleted)
			totalCount += added - deleted
		}
	}

	var norm *dict.WeightNormalizer
	if len(normalizer) > 0 {
		norm = normalizer[0]
	}

	writer := datformat.NewWdatWriter()

	for code, entries := range codeEntries {
		sort.SliceStable(entries, func(i, j int) bool {
			if entries[i].weight != entries[j].weight {
				return entries[i].weight > entries[j].weight
			}
			return entries[i].naturalOrder < entries[j].naturalOrder
		})
		wdatEntries := make([]datformat.WdatEntry, len(entries))
		for i, e := range entries {
			w := e.weight
			if norm != nil {
				w = norm.Normalize(w)
			}
			wdatEntries[i] = datformat.WdatEntry{
				Text:   e.text,
				Weight: int32(w),
			}
		}
		writer.AddCode(code, wdatEntries)
	}

	for abbrev, entries := range abbrevEntries {
		sort.SliceStable(entries, func(i, j int) bool {
			if entries[i].weight != entries[j].weight {
				return entries[i].weight > entries[j].weight
			}
			return entries[i].naturalOrder < entries[j].naturalOrder
		})
		wdatEntries := make([]datformat.WdatEntry, len(entries))
		for i, e := range entries {
			w := e.weight
			if norm != nil {
				w = norm.Normalize(w)
			}
			wdatEntries[i] = datformat.WdatEntry{
				Text:   e.text,
				Weight: int32(w),
			}
		}
		writer.AddAbbrev(abbrev, wdatEntries)
	}

	if err := atomicWriteWdb(wdatPath, func(w io.Writer) error {
		return writer.Write(w)
	}); err != nil {
		return err
	}

	logger.Info("拼音词库(DAT)转换完成", "codes", len(codeEntries), "abbrevs", len(abbrevEntries))
	return nil
}

// ---- 内部辅助 ----

type dictEntry struct {
	text         string
	weight       int
	naturalOrder int // 同编码下的原始顺序（0-based，按文件出现顺序）
}

func loadRimeFile(path string, codeEntries map[string][]dictEntry, abbrevEntries map[string][]dictEntry, globalOrder *int, logger *slog.Logger) (int, error) {
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

		if inHeader {
			if strings.TrimSpace(line) == "..." {
				inHeader = false
			}
			continue
		}

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
		if err != nil {
			continue
		}

		code := strings.ReplaceAll(pinyin, " ", "")
		order := *globalOrder
		*globalOrder++
		codeEntries[code] = append(codeEntries[code], dictEntry{
			text:         text,
			weight:       weight,
			naturalOrder: order,
		})

		// 构建简拼索引（2 字及以上）
		syllables := strings.Fields(pinyin)
		if len(syllables) >= 2 {
			var abbrevBuilder strings.Builder
			for _, s := range syllables {
				if len(s) == 0 {
					break
				}
				abbrevBuilder.WriteByte(s[0])
			}
			abbrev := abbrevBuilder.String()
			if abbrev != "" {
				abbrevEntries[abbrev] = append(abbrevEntries[abbrev], dictEntry{
					text:         text,
					weight:       weight,
					naturalOrder: order,
				})
			}
		}

		count++
	}

	return count, scanner.Err()
}
