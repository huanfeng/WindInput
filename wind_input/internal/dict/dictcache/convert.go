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
	"runtime"
	"sync/atomic"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/binformat"
	"github.com/huanfeng/wind_input/internal/dict/datformat"
	"github.com/huanfeng/wind_input/pkg/sysinfo"
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
	// Sources 记录生成此 wdb 时实际使用的源文件路径列表（已排序、绝对路径）。
	// 用于 NeedsRegenerateBySources 检测：源文件清单变化（例如新增/删除 import_tables）
	// 即使每个文件 mtime 都早于 wdb，也需重建缓存。
	Sources []string `json:"sources,omitempty"`
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
	if sysinfo.LowMemoryMode() {
		// 传统单文件码表：GetEntries 返回的是 CodeTable 内部 map 引用，
		// 不能在此 delete；仅启用 writer 内部的省内存释放路径即可。
		writer.SetLowMemory(true)
		logger.Info("低内存模式：码表 wdb 采用省内存生成", "availMB", sysinfo.AvailablePhysicalMB())
	}
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

	logger.Info("码表转换完成", "codes", len(entries))
	return nil
}

// normalizeSources 返回排序、去重后的绝对路径列表，作为 wdb meta 的 Sources 字段
// 与 NeedsRegenerateBySources 检测的稳定基线。无法 Abs 的路径保留原样。
func normalizeSources(paths []string) []string {
	if len(paths) == 0 {
		return nil
	}
	seen := make(map[string]struct{}, len(paths))
	out := make([]string, 0, len(paths))
	for _, p := range paths {
		if p == "" {
			continue
		}
		abs, err := filepath.Abs(p)
		if err != nil {
			abs = p
		}
		abs = filepath.Clean(abs)
		if _, ok := seen[abs]; ok {
			continue
		}
		seen[abs] = struct{}{}
		out = append(out, abs)
	}
	sort.Strings(out)
	return out
}

// SourceListChanged 比较当前发现的源文件列表与 wdb meta 中记录的列表，
// 若不同则返回 true 并指明差异。对于不携带 Sources 字段的旧 wdb 视为未变化（兼容）。
func SourceListChanged(wdbPath string, currentSources []string) (changed bool, recorded []string) {
	reader, err := binformat.OpenDict(wdbPath)
	if err != nil {
		return false, nil
	}
	defer reader.Close()
	meta, err := LoadCodeTableMetaFromWdb(reader)
	if err != nil || meta == nil || len(meta.Sources) == 0 {
		return false, nil
	}
	want := normalizeSources(currentSources)
	if len(want) != len(meta.Sources) {
		return true, meta.Sources
	}
	for i := range want {
		if want[i] != meta.Sources[i] {
			return true, meta.Sources
		}
	}
	return false, meta.Sources
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

// RimePinyinSourcePaths 返回拼音词库的所有源文件路径（用于缓存失效检测）
// mainDictPath 为主词库文件路径，自动从 import_tables 发现关联词库及补丁文件
func RimePinyinSourcePaths(mainDictPath string) []string {
	paths := dictFilesFor(mainDictPath)
	dictDir := filepath.Dir(mainDictPath)

	importFiles := discoverRimePinyinFiles(mainDictPath)
	for _, name := range importFiles {
		p := filepath.Join(dictDir, name)
		if _, err := os.Stat(p); err == nil {
			paths = append(paths, dictFilesFor(p)...)
		}
	}

	// 包含补丁文件（补丁变更时触发缓存重建）
	// 将 import 文件名转换回 import_tables 名称格式（去掉词库后缀）
	var importNames []string
	for _, f := range importFiles {
		importNames = append(importNames, dictStem(f))
	}
	paths = append(paths, FindPatchFiles(mainDictPath, importNames)...)

	return paths
}

// discoverRimePinyinFiles 从主词库的 import_tables 发现关联词库的相对路径
// 严格只加载 import_tables 中声明的词库，保留原始路径结构（如 "cn_dicts/8105.dict.yaml"）。
// 兄弟词库扩展名跟随主词库格式（split→.dict.toml，rime→.dict.yaml）。
func discoverRimePinyinFiles(mainDictPath string) []string {
	hdr, _ := ReadDictHeader(mainDictPath)
	suffix := dictSuffixOf(mainDictPath)

	var files []string
	for _, name := range hdr.ImportTables {
		// 保留原始路径: "cn_dicts/8105" → "cn_dicts/8105.dict.yaml"（或 .dict.toml）
		files = append(files, name+suffix)
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
	importSuffix := dictSuffixOf(mainDictPath)
	for _, name := range importNames {
		path := filepath.Join(dictDir, name+importSuffix)
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

	lowMem := sysinfo.LowMemoryMode()
	writer := binformat.NewDictWriter()
	if lowMem {
		writer.SetLowMemory(true)
		logger.Info("低内存模式：rime 码表 wdb 采用省内存生成", "availMB", sysinfo.AvailablePhysicalMB())
	}
	codesCount := len(codeEntries)

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
		// 省内存：该编码已转入 writer，释放源 map 项（range 中 delete 安全）。
		if lowMem {
			delete(codeEntries, code)
		}
	}
	if lowMem {
		codeEntries = nil
		runtime.GC()
	}

	// 生成元数据（从主词库文件名推导）
	mainName := dictStem(filepath.Base(mainDictPath))
	meta := CodeTableMeta{
		Name:       mainName,
		Version:    "rime",
		CodeScheme: "五笔字型86版",
		CodeLength: 4,
		EntryCount: totalCount,
		HasWeight:  hasWeight,
		Sources:    normalizeSources(RimeCodetableSourcePaths(mainDictPath)),
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

	logger.Info("rime 码表词库转换完成", "codes", codesCount, "count", totalCount)
	return nil
}

// RimeCodetableSourcePaths 返回 rime 码表词库的所有源文件路径（用于缓存失效检测）
// mainDictPath 为主词库文件路径，自动发现关联词库及补丁文件
func RimeCodetableSourcePaths(mainDictPath string) []string {
	paths := dictFilesFor(mainDictPath)
	dictDir := filepath.Dir(mainDictPath)

	importNames := discoverRimeCodetableImports(mainDictPath)
	importSuffix := dictSuffixOf(mainDictPath)
	for _, name := range importNames {
		p := filepath.Join(dictDir, name+importSuffix)
		if _, err := os.Stat(p); err == nil {
			paths = append(paths, dictFilesFor(p)...)
		}
	}

	// 包含补丁文件（补丁变更时触发缓存重建）
	paths = append(paths, FindPatchFiles(mainDictPath, importNames)...)

	return paths
}

// discoverRimeCodetableImports 从主词库头的 import_tables 发现关联词库名称。
// 严格只加载 import_tables 中声明的词库，不进行目录扫描，避免加载不合理的文件。
func discoverRimeCodetableImports(mainDictPath string) []string {
	hdr, _ := ReadDictHeader(mainDictPath)
	return hdr.ImportTables
}

// loadRimeCodetableFile 解析 rime 格式的码表词库（.dict.yaml 或 split .dict.toml+.dict.tsv）。
// 头/体来源经 OpenDictSource 解耦：列顺序由头的 columns 决定（缺省 text/code/weight），
// 数据体逐行制表符分隔解析，两种磁盘格式共用同一段体解析逻辑、零行为差异。
//
// 权重策略基于词库自身的 sort 字段：
//   - sort: by_weight → 使用显式权重（权威词库，如主词库）
//   - sort: original  → 忽略显式权重，统一 weight=1（补充词库，不与主词库竞争）
func loadRimeCodetableFile(path string, codeEntries map[string][]dictEntry, globalOrder *int, logger *slog.Logger) (int, bool, error) {
	hdr, body, err := OpenDictSource(path)
	if err != nil {
		return 0, false, err
	}
	defer body.Close()

	sortMode := strings.TrimSpace(hdr.Sort)
	// 列索引：默认 rime 标准顺序 text/code/weight；header 显式声明 columns 时按名定位。
	colText, colCode, colWeight := 0, 1, 2
	if len(hdr.Columns) > 0 {
		colText, colCode, colWeight = -1, -1, -1
		for i, name := range hdr.Columns {
			switch strings.TrimSpace(name) {
			case "text":
				colText = i
			case "code":
				colCode = i
			case "weight":
				colWeight = i
			}
		}
	}

	scanner := bufio.NewScanner(body)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)
	count := 0
	hasWeight := false

	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
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
// 防止进程被杀或并发写入导致目标文件被截断。
//
// Windows 上若目标文件正被本进程 mmap 持有（典型场景：切换方案触发重建，
// 但旧引擎仍缓存在 engine.Manager 中持锁），rename 会以 "Access is denied" 失败。
// 替换前调用 binformat.CloseReadersForPath 强制释放本进程内所有同路径 reader，
// 让 rename 得以成功；被强制关闭的 reader 在查询时安全返回空结果（见 binformat/registry.go）。
// atomicWriteSeq 为 atomicWriteWdb 的临时文件名提供进程内唯一序号，避免多个
// goroutine 并发写同一目标 wdb 时争用同一个 .tmp 文件导致内容交错损坏（典型场景：
// 启动 / 重建缓存时多方案预生成同时触发 unigram 等全局共享缓存的转换）。
var atomicWriteSeq atomic.Uint64

func atomicWriteWdb(wdbPath string, writeFn func(w io.Writer) error) error {
	os.MkdirAll(filepath.Dir(wdbPath), 0755)

	tmpPath := fmt.Sprintf("%s.tmp.%d", wdbPath, atomicWriteSeq.Add(1))
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

	// 释放本进程内对目标路径的 mmap，避免 Windows 上 rename 因自身持锁失败。
	if closed := binformat.CloseReadersForPath(wdbPath); closed > 0 {
		slog.Info("替换 wdb 前已释放本进程内 mmap reader", "path", wdbPath, "closed", closed)
	}

	// os.Rename 在 Windows 上等价于 MoveFileEx(MOVEFILE_REPLACE_EXISTING)，
	// 目标存在时直接覆盖；无需先 Remove（先 Remove 反而会引入 pending-delete 竞态）。
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
		wdatImportNames = append(wdatImportNames, dictStem(f))
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

	lowMem := sysinfo.LowMemoryMode()
	writer := datformat.NewWdatWriter()
	if lowMem {
		writer.SetLowMemory(true)
		logger.Info("低内存模式：拼音 wdat 采用省内存生成", "availMB", sysinfo.AvailablePhysicalMB())
	}
	codesCount := len(codeEntries)
	abbrevsCount := len(abbrevEntries)

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
		// 省内存：该编码已转入 writer，释放源 map 项（range 中 delete 安全）。
		if lowMem {
			delete(codeEntries, code)
		}
	}
	if lowMem {
		codeEntries = nil
		runtime.GC()
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
		// 省内存：该简拼已转入 writer，释放源 map 项（range 中 delete 安全）。
		if lowMem {
			delete(abbrevEntries, abbrev)
		}
	}
	if lowMem {
		abbrevEntries = nil
		runtime.GC()
	}

	if err := atomicWriteWdb(wdatPath, func(w io.Writer) error {
		return writer.Write(w)
	}); err != nil {
		return err
	}

	logger.Info("拼音词库(DAT)转换完成", "codes", codesCount, "abbrevs", abbrevsCount)
	return nil
}

// ---- 内部辅助 ----

type dictEntry struct {
	text         string
	weight       int
	naturalOrder int // 同编码下的原始顺序（0-based，按文件出现顺序）
}

// loadRimeFile 解析 rime 拼音词库（.dict.yaml 或 split .dict.toml+.dict.tsv）。
// 拼音词库固定 text/code/weight 列序（不读 header columns），头/体经 OpenDictSource
// 解耦后体解析逻辑两种格式共用。
func loadRimeFile(path string, codeEntries map[string][]dictEntry, abbrevEntries map[string][]dictEntry, globalOrder *int, logger *slog.Logger) (int, error) {
	_, body, err := OpenDictSource(path)
	if err != nil {
		return 0, err
	}
	defer body.Close()

	scanner := bufio.NewScanner(body)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	count := 0

	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
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
