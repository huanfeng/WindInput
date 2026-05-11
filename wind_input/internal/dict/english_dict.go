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
)

// EnglishDict 英文词库（优先使用 mmap wdb，首次自动从 Rime 文本构建）
type EnglishDict struct {
	logger     *slog.Logger
	wdbReader  *binformat.DictReader // 主路径：mmap 加载，不占堆内存
	trie       *Trie                 // 回退路径：wdb 不可用时使用
	seen       map[string]bool       // 仅在 Trie 加载时使用
	entryCount int
}

// NewEnglishDict 创建英文词库
func NewEnglishDict(logger *slog.Logger) *EnglishDict {
	if logger == nil {
		logger = slog.Default()
	}
	return &EnglishDict{logger: logger}
}

// LoadRimeDir 从目录加载英文词库
// wdbCachePath 指定 wdb 缓存文件路径（由调用方通过 dictcache.CachePath 提供）；
// 优先使用缓存的 wdb（mmap，不占堆），wdb 不存在或源文件更新时自动重建。
func (d *EnglishDict) LoadRimeDir(dirPath, wdbCachePath string) error {
	wdbPath := wdbCachePath
	candidateFiles := []string{
		filepath.Join(dirPath, "en.dict.yaml"),
		filepath.Join(dirPath, "en_ext.dict.yaml"),
	}

	var sourceFiles []string
	for _, f := range candidateFiles {
		if _, err := os.Stat(f); err == nil {
			sourceFiles = append(sourceFiles, f)
		}
	}
	if len(sourceFiles) == 0 {
		return fmt.Errorf("未找到任何英文词库文件（目录: %s）", dirPath)
	}

	if !isWdbFresh(wdbPath, sourceFiles) {
		count, err := d.buildWdb(sourceFiles, wdbPath)
		if err != nil {
			d.logger.Warn("构建英文 wdb 失败，回退到 Trie 加载", "error", err)
			return d.loadViaTrie(sourceFiles)
		}
		d.logger.Info("构建英文词库 wdb 完成", "path", wdbPath, "count", count)
	}

	reader, err := binformat.OpenDict(wdbPath)
	if err != nil {
		d.logger.Warn("打开英文 wdb 失败，回退到 Trie 加载", "path", wdbPath, "error", err)
		return d.loadViaTrie(sourceFiles)
	}

	d.wdbReader = reader
	d.entryCount = reader.EntryCount()
	d.logger.Info("加载英文词库 wdb", "path", wdbPath, "count", d.entryCount)
	return nil
}

// LoadRimeFile 加载单个 Rime dict.yaml 格式英文词库文件（使用 Trie，无 wdb 缓存）
func (d *EnglishDict) LoadRimeFile(path string) error {
	if d.trie == nil {
		d.trie = NewTrie()
		d.seen = make(map[string]bool)
		d.entryCount = 0
	}
	if d.seen == nil {
		d.seen = make(map[string]bool)
	}
	count, err := d.loadRimeFile(path)
	if err != nil {
		return err
	}
	d.entryCount = d.trie.EntryCount()
	d.logger.Info("加载英文词库完成", "path", path, "count", count)
	return nil
}

// isWdbFresh 检查 wdb 是否比所有源文件都新
func isWdbFresh(wdbPath string, sources []string) bool {
	wdbInfo, err := os.Stat(wdbPath)
	if err != nil {
		return false
	}
	wdbMtime := wdbInfo.ModTime()
	for _, src := range sources {
		info, err := os.Stat(src)
		if err != nil {
			continue
		}
		if info.ModTime().After(wdbMtime) {
			return false
		}
	}
	return true
}

// buildWdb 将 Rime 源文件解析并写出为 wdb 格式（原子写入）
func (d *EnglishDict) buildWdb(files []string, outputPath string) (int, error) {
	seen := make(map[string]bool)
	writer := binformat.NewDictWriter()
	total := 0

	for _, path := range files {
		count, err := collectRimeFileToWriter(path, seen, writer)
		if err != nil {
			d.logger.Warn("解析英文词库文件失败", "path", path, "error", err)
			continue
		}
		d.logger.Info("解析英文词库文件", "path", filepath.Base(path), "count", count)
		total += count
	}

	if total == 0 {
		return 0, fmt.Errorf("英文词库文件为空")
	}

	tmp := outputPath + ".tmp"
	f, err := os.Create(tmp)
	if err != nil {
		return 0, fmt.Errorf("创建 wdb 临时文件失败: %w", err)
	}

	bw := bufio.NewWriterSize(f, 256*1024)
	if err := writer.Write(bw); err != nil {
		f.Close()
		os.Remove(tmp)
		return 0, fmt.Errorf("写入 wdb 失败: %w", err)
	}
	if err := bw.Flush(); err != nil {
		f.Close()
		os.Remove(tmp)
		return 0, fmt.Errorf("刷新 wdb 缓冲失败: %w", err)
	}
	f.Close()

	if err := os.Rename(tmp, outputPath); err != nil {
		os.Remove(tmp)
		return 0, fmt.Errorf("重命名 wdb 失败: %w", err)
	}
	return total, nil
}

// collectRimeFileToWriter 解析单个 Rime 文件并将词条写入 DictWriter
// seen 跨文件共享，保证去重行为与 loadRimeFile 一致
func collectRimeFileToWriter(path string, seen map[string]bool, writer *binformat.DictWriter) (int, error) {
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
		var word string
		weight := 1

		switch len(parts) {
		case 1:
			word = strings.TrimSpace(parts[0])
		case 2:
			word = strings.TrimSpace(parts[0])
			if w, err := strconv.Atoi(strings.TrimSpace(parts[1])); err == nil && w > 0 {
				weight = w
			}
		default:
			word = strings.TrimSpace(parts[0])
			if w, err := strconv.Atoi(strings.TrimSpace(parts[len(parts)-1])); err == nil && w > 0 {
				weight = w
			}
		}

		if word == "" {
			continue
		}

		code := strings.ToLower(word)
		if seen[code] {
			continue
		}
		seen[code] = true

		writer.AddCode(code, []binformat.DictEntry{
			{Text: word, Weight: int32(weight)},
		})
		count++
	}

	return count, scanner.Err()
}

// loadViaTrie 回退路径：从源文件加载到 Trie（wdb 构建或打开失败时使用）
func (d *EnglishDict) loadViaTrie(files []string) error {
	d.trie = NewTrie()
	d.seen = make(map[string]bool)
	d.entryCount = 0

	loaded := 0
	for _, path := range files {
		count, err := d.loadRimeFile(path)
		if err != nil {
			d.logger.Warn("Trie 加载英文词库文件失败", "path", path, "error", err)
			continue
		}
		d.logger.Info("Trie 加载英文词库文件", "path", filepath.Base(path), "count", count)
		loaded++
		_ = count
	}

	if loaded == 0 {
		return fmt.Errorf("所有英文词库文件加载失败")
	}
	d.entryCount = d.trie.EntryCount()
	return nil
}

// loadRimeFile 解析单个 Rime dict.yaml 并插入 Trie（仅回退路径使用）
func (d *EnglishDict) loadRimeFile(path string) (int, error) {
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
		var word string
		weight := 1

		switch len(parts) {
		case 1:
			word = strings.TrimSpace(parts[0])
		case 2:
			word = strings.TrimSpace(parts[0])
			if w, err := strconv.Atoi(strings.TrimSpace(parts[1])); err == nil && w > 0 {
				weight = w
			}
		default:
			word = strings.TrimSpace(parts[0])
			if w, err := strconv.Atoi(strings.TrimSpace(parts[len(parts)-1])); err == nil && w > 0 {
				weight = w
			}
		}

		if word == "" {
			continue
		}

		code := strings.ToLower(word)
		if d.seen[code] {
			continue
		}
		d.seen[code] = true

		cand := candidate.Candidate{
			Text:   word,
			Code:   code,
			Weight: weight,
		}
		d.trie.Insert(code, cand)
		count++
	}

	return count, scanner.Err()
}

// EntryCount 返回词条数量
func (d *EnglishDict) EntryCount() int {
	return d.entryCount
}

// Lookup 精确查询（大小写不敏感）
func (d *EnglishDict) Lookup(word string) []candidate.Candidate {
	code := strings.ToLower(word)
	if d.wdbReader != nil {
		return d.wdbReader.Lookup(code)
	}
	if d.trie == nil {
		return nil
	}
	return d.trie.Search(code)
}

// LookupPrefix 前缀查询（大小写不敏感）
// 排序：精确匹配 > 短词优先 > 字母序
func (d *EnglishDict) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	prefixLower := strings.ToLower(prefix)

	var results []candidate.Candidate
	if d.wdbReader != nil {
		// limit=0 取全量后自行排序，保证英文排序语义（短词优先）
		results = d.wdbReader.LookupPrefix(prefixLower, 0)
	} else {
		if d.trie == nil {
			return nil
		}
		results = d.trie.SearchPrefix(prefixLower, 0)
	}

	sort.SliceStable(results, func(i, j int) bool {
		ci, cj := results[i], results[j]
		exactI := ci.Code == prefixLower
		exactJ := cj.Code == prefixLower
		if exactI != exactJ {
			return exactI
		}
		if len(ci.Code) != len(cj.Code) {
			return len(ci.Code) < len(cj.Code)
		}
		return ci.NaturalOrder < cj.NaturalOrder
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// Close 释放资源
func (d *EnglishDict) Close() error {
	if d.wdbReader != nil {
		err := d.wdbReader.Close()
		d.wdbReader = nil
		return err
	}
	d.trie = nil
	return nil
}

// EnglishDictLayer 将 EnglishDict 适配为 DictLayer
type EnglishDictLayer struct {
	name string
	dict *EnglishDict
}

// NewEnglishDictLayer 创建 EnglishDict 适配器
func NewEnglishDictLayer(name string, dict *EnglishDict) *EnglishDictLayer {
	return &EnglishDictLayer{
		name: name,
		dict: dict,
	}
}

// Name 返回层名称
func (l *EnglishDictLayer) Name() string {
	return l.name
}

// Type 返回层类型
func (l *EnglishDictLayer) Type() LayerType {
	return LayerTypeSystem
}

// Search 精确查询
func (l *EnglishDictLayer) Search(code string, limit int) []candidate.Candidate {
	results := l.dict.Lookup(code)
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// SearchPrefix 前缀查询
func (l *EnglishDictLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	return l.dict.LookupPrefix(prefix, limit)
}
