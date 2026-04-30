package main

import (
	"bufio"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"unicode"
)

// Entry 词典条目
type Entry struct {
	Text           string
	Code           string
	OrigWeight     int // jidian 原始优先级(10/20/30)；权重赋值后存放最终权重
	shortcodeLevel int // 0=普通词条, 1/2/3=简码级别（单字且码长≤3）
	origPos        int // 在 jidian 中的原始顺序，用于简码组内排序
}

// ── Unicode 过滤辅助 ───────────────────────────────────

var emojiTable = &unicode.RangeTable{
	R16: []unicode.Range16{
		{Lo: 0x2300, Hi: 0x23FF, Stride: 1},
		{Lo: 0x2600, Hi: 0x27BF, Stride: 1},
		{Lo: 0xFE00, Hi: 0xFE0F, Stride: 1},
	},
	R32: []unicode.Range32{
		{Lo: 0x1F000, Hi: 0x1F02F, Stride: 1},
		{Lo: 0x1F0A0, Hi: 0x1F0FF, Stride: 1},
		{Lo: 0x1F300, Hi: 0x1F9FF, Stride: 1},
		{Lo: 0x1FA00, Hi: 0x1FAFF, Stride: 1},
	},
}

var puaTable = &unicode.RangeTable{
	R16: []unicode.Range16{
		{Lo: 0xE000, Hi: 0xF8FF, Stride: 1},
	},
	R32: []unicode.Range32{
		{Lo: 0xF0000, Hi: 0xFFFFF, Stride: 1},
		{Lo: 0x100000, Hi: 0x10FFFF, Stride: 1},
	},
}

var cjkTable = &unicode.RangeTable{
	R16: []unicode.Range16{
		{Lo: 0x3400, Hi: 0x4DBF, Stride: 1},
		{Lo: 0x4E00, Hi: 0x9FFF, Stride: 1},
		{Lo: 0xF900, Hi: 0xFAFF, Stride: 1},
	},
	R32: []unicode.Range32{
		{Lo: 0x20000, Hi: 0x2A6DF, Stride: 1},
		{Lo: 0x2A700, Hi: 0x2CEAF, Stride: 1},
	},
}

func hasEmoji(s string) bool {
	for _, r := range s {
		if unicode.Is(emojiTable, r) {
			return true
		}
	}
	return false
}

func hasPUA(s string) bool {
	for _, r := range s {
		if unicode.Is(puaTable, r) {
			return true
		}
	}
	return false
}

func hasCJK(s string) bool {
	for _, r := range s {
		if unicode.Is(cjkTable, r) {
			return true
		}
	}
	return false
}

func isPureLatin(s string) bool {
	if s == "" {
		return false
	}
	for _, r := range s {
		if r > 0x7E || r < 0x20 {
			return false
		}
	}
	return true
}

func isValidCode(code string) bool {
	for _, c := range code {
		if c < 'a' || c > 'y' {
			return false
		}
	}
	return true
}

func sliceContains(ss []string, s string) bool {
	for _, x := range ss {
		if x == s {
			return true
		}
	}
	return false
}

// ── 过滤 ──────────────────────────────────────────────

func shouldKeep(e Entry, cfg *Config) (bool, string) {
	if cfg.DropZCode && strings.HasPrefix(e.Code, "z") {
		return false, "z_code"
	}
	if cfg.DropDollar && strings.HasPrefix(e.Text, "$") {
		return false, "dollar_prefix"
	}
	if cfg.MaxCodeLen > 0 && len(e.Code) > cfg.MaxCodeLen {
		return false, "code_too_long"
	}
	if !isValidCode(e.Code) {
		return false, "code_invalid_chars"
	}
	if cfg.MaxTextLen > 0 && len([]rune(e.Text)) > cfg.MaxTextLen {
		return false, "text_too_long"
	}
	if cfg.DropEmoji && hasEmoji(e.Text) {
		return false, "emoji"
	}
	if cfg.DropPUA && hasPUA(e.Text) {
		return false, "pua"
	}
	if cfg.DropPureLatin && isPureLatin(e.Text) {
		return false, "pure_latin"
	}
	if cfg.RequireCJK && !hasCJK(e.Text) {
		return false, "no_cjk"
	}
	for _, rule := range cfg.DropRules {
		reason := rule.Reason
		if reason == "" {
			reason = "manual_rule"
		}
		if rule.CodePrefix != "" && strings.HasPrefix(e.Code, rule.CodePrefix) {
			if !sliceContains(rule.ExceptCodes, e.Code) {
				return false, reason
			}
		} else if rule.Code != "" && e.Code == rule.Code {
			if !sliceContains(rule.ExceptCodes, e.Code) {
				return false, reason
			}
		}
	}
	return true, ""
}

// ── 简码权重分层 ──────────────────────────────────────

// assignShortcodeWeights 识别简码词条（单字且码长1-3），按分层配置赋予固定高权重。
// 同码同级的多个词条按 jidian 原始顺序递减，以保留原词库中的候选排列。
func assignShortcodeWeights(entries []Entry, cfg *Config) {
	if !cfg.Shortcodes.Enabled {
		return
	}
	for i, e := range entries {
		if len([]rune(e.Text)) == 1 && len(e.Code) >= 1 && len(e.Code) <= 3 {
			entries[i].shortcodeLevel = len(e.Code)
		}
	}

	type groupKey struct {
		level int
		code  string
	}
	groups := make(map[groupKey][]int)
	for i, e := range entries {
		if e.shortcodeLevel == 0 {
			continue
		}
		k := groupKey{e.shortcodeLevel, e.Code}
		groups[k] = append(groups[k], i)
	}

	for k, idxs := range groups {
		sort.Slice(idxs, func(a, b int) bool {
			return entries[idxs[a]].origPos < entries[idxs[b]].origPos
		})
		var base int
		switch k.level {
		case 1:
			base = cfg.Shortcodes.Level1Weight
		case 2:
			base = cfg.Shortcodes.Level2BaseWeight
		case 3:
			base = cfg.Shortcodes.Level3BaseWeight
		}
		for rank, idx := range idxs {
			entries[idx].OrigWeight = base - rank
		}
	}
}

func countShortcodeLevel(entries []Entry, level int) int {
	n := 0
	for _, e := range entries {
		if e.shortcodeLevel == level {
			n++
		}
	}
	return n
}

// ── 简码避让冲突分析 ───────────────────────────────────

type conflictEntry struct {
	ConflictType string // "level1_level2" / "level2_level3" / "level1_level3"
	Char         string
	ShortCode    string // 已能打出该字的较短编码
	LongCode     string // 同字占据首位的较长编码
}

// analyzeShortcodeConflicts 找出同一字在前缀关系的两个简码层级都占据首位的情况。
// 这类冲突意味着用户可以用更短的码打出该字，却同时也占据了较长码的首选位。
func analyzeShortcodeConflicts(entries []Entry) []conflictEntry {
	type best struct {
		text   string
		weight int
	}
	topByCode := make(map[string]best)
	for _, e := range entries {
		if e.shortcodeLevel == 0 {
			continue
		}
		if b, ok := topByCode[e.Code]; !ok || e.OrigWeight > b.weight {
			topByCode[e.Code] = best{e.Text, e.OrigWeight}
		}
	}

	var conflicts []conflictEntry
	for code, sc := range topByCode {
		clen := len(code)
		if clen < 2 {
			continue
		}
		for l := 1; l < clen; l++ {
			prefix := code[:l]
			if shorter, ok := topByCode[prefix]; ok && shorter.text == sc.text {
				conflicts = append(conflicts, conflictEntry{
					ConflictType: fmt.Sprintf("level%d_level%d", l, clen),
					Char:         sc.text,
					ShortCode:    prefix,
					LongCode:     code,
				})
			}
		}
	}
	sort.Slice(conflicts, func(i, j int) bool {
		if conflicts[i].ConflictType != conflicts[j].ConflictType {
			return conflicts[i].ConflictType < conflicts[j].ConflictType
		}
		return conflicts[i].LongCode < conflicts[j].LongCode
	})
	return conflicts
}

func writeConflictReport(path string, conflicts []conflictEntry) error {
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	bw := bufio.NewWriter(f)
	fmt.Fprintf(bw, "conflict_type\tchar\tshort_code\tlong_code\n")
	for _, c := range conflicts {
		fmt.Fprintf(bw, "%s\t%s\t%s\t%s\n", c.ConflictType, c.Char, c.ShortCode, c.LongCode)
	}
	return bw.Flush()
}

// ── 词频与权重 ────────────────────────────────────────

func loadUnigram(path string) (map[string]int64, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	freq := make(map[string]int64)
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 4*1024*1024), 4*1024*1024)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "\t", 2)
		if len(parts) < 2 {
			continue
		}
		// 支持整数和浮点频率
		v, err := strconv.ParseInt(strings.TrimSpace(parts[1]), 10, 64)
		if err != nil {
			if fv, err2 := strconv.ParseFloat(strings.TrimSpace(parts[1]), 64); err2 == nil {
				v = int64(fv)
			}
		}
		if v > 0 {
			freq[parts[0]] = v
		}
	}
	return freq, scanner.Err()
}

func computeMedianRawFreq(entries []Entry, unigram map[string]int64) float64 {
	freqs := make([]int64, 0, len(entries))
	for _, e := range entries {
		if f, ok := unigram[e.Text]; ok {
			freqs = append(freqs, f)
		}
	}
	if len(freqs) == 0 {
		return 1000
	}
	sort.Slice(freqs, func(i, j int) bool { return freqs[i] < freqs[j] })
	n := len(freqs)
	if n%2 == 1 {
		return float64(freqs[n/2])
	}
	return float64(freqs[n/2-1]+freqs[n/2]) / 2
}

func computeWeight(freq int64, logMedian float64, cfg *Config) int {
	if freq <= 0 || logMedian == 0 {
		return cfg.WeightMin
	}
	w := float64(cfg.TargetMedian) * math.Log10(float64(freq)+1) / logMedian
	return clampWeight(int(math.Round(w)), cfg)
}

func fallbackWeight(origWeight int, cfg *Config) int {
	if origWeight >= 30 {
		return cfg.Fallback.Priority30
	}
	if origWeight >= 20 {
		return cfg.Fallback.Priority20
	}
	return cfg.Fallback.Priority10
}

func clampWeight(w int, cfg *Config) int {
	if w < cfg.WeightMin {
		return cfg.WeightMin
	}
	if w > cfg.WeightMax {
		return cfg.WeightMax
	}
	return w
}

// ── jidian 解析 ───────────────────────────────────────

func parseJidian(path string) ([]Entry, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	inHeader := true
	colText, colCode, colWeight := 0, 1, 2
	inColumns := false
	var colNames []string
	var entries []Entry

	for scanner.Scan() {
		line := scanner.Text()
		if inHeader {
			trimmed := strings.TrimSpace(line)
			if trimmed == "..." {
				if len(colNames) > 0 {
					colText, colCode, colWeight = -1, -1, -1
					for i, name := range colNames {
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
			if strings.HasPrefix(trimmed, "columns:") {
				inColumns = true
				colNames = nil
				continue
			}
			if inColumns {
				if name, ok := strings.CutPrefix(trimmed, "- "); ok {
					if idx := strings.Index(name, "#"); idx >= 0 {
						name = name[:idx]
					}
					if name = strings.TrimSpace(name); name != "" {
						colNames = append(colNames, name)
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
		weight := 10
		if ws := getCol(colWeight); ws != "" {
			if w, err := strconv.Atoi(ws); err == nil && w > 0 {
				weight = w
			}
		}
		entries = append(entries, Entry{Text: text, Code: code, OrigWeight: weight, origPos: len(entries)})
	}
	return entries, scanner.Err()
}

// ── 主流程 ────────────────────────────────────────────

type droppedEntry struct {
	reason string
	e      Entry
}

func enrich(cfg *Config) error {
	// 1. 加载 unigram
	stat, _ := os.Stat(cfg.UnigramPath)
	sizeMB := 0
	if stat != nil {
		sizeMB = int(stat.Size() / 1024 / 1024)
	}
	fmt.Printf("[1/4] 加载 unigram.txt (%d MB)...\n", sizeMB)
	unigram, err := loadUnigram(cfg.UnigramPath)
	if err != nil {
		return fmt.Errorf("加载 unigram 失败: %w", err)
	}
	fmt.Printf("      加载完成: %d 条词频记录\n", len(unigram))

	// 2. 解析 jidian
	fmt.Printf("[2/4] 加载 jidian 词典...\n")
	jidianEntries, err := parseJidian(cfg.JidianPath)
	if err != nil {
		return fmt.Errorf("解析 jidian 失败: %w", err)
	}
	fmt.Printf("      %s: %d 条\n", cfg.JidianPath, len(jidianEntries))

	// 3. 过滤
	fmt.Printf("[3/4] 过滤 + 补充词频...\n")
	filterStats := make(map[string]int)
	var kept []Entry
	var dropped []droppedEntry
	for _, e := range jidianEntries {
		if ok, reason := shouldKeep(e, cfg); !ok {
			filterStats[reason]++
			dropped = append(dropped, droppedEntry{reason, e})
		} else {
			kept = append(kept, e)
		}
	}
	fmt.Printf("      保留: %d  过滤: %d\n", len(kept), len(dropped))
	// 按数量降序显示过滤原因
	type kv struct {
		k string
		v int
	}
	var sorted []kv
	for k, v := range filterStats {
		sorted = append(sorted, kv{k, v})
	}
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].v > sorted[j].v })
	for _, kv := range sorted {
		fmt.Printf("        - %s: %d\n", kv.k, kv.v)
	}

	// 识别简码并赋予分层权重（必须在 unigram 赋权之前完成）
	if cfg.Shortcodes.Enabled {
		assignShortcodeWeights(kept, cfg)
		fmt.Printf("      简码分层: 一级=%d  二级=%d  三级=%d\n",
			countShortcodeLevel(kept, 1), countShortcodeLevel(kept, 2), countShortcodeLevel(kept, 3))
	}

	// 计算归一化基准（仅基于 jidian 过滤后的词条）
	medianRaw := computeMedianRawFreq(kept, unigram)
	logMedian := math.Log10(medianRaw + 1)
	hit := 0
	for _, e := range kept {
		if _, ok := unigram[e.Text]; ok {
			hit++
		}
	}
	fmt.Printf("      unigram 命中: %d (%d%%)  未命中: %d\n", hit, hit*100/len(kept), len(kept)-hit)
	fmt.Printf("      中位原始频次: %.0f  (log10=%.3f)\n", medianRaw, logMedian)

	// 加载自定义词（可选）
	if cfg.CustomWordsPath != "" {
		if _, statErr := os.Stat(cfg.CustomWordsPath); statErr == nil {
			fmt.Printf("      加载自定义词表: %s\n", cfg.CustomWordsPath)
			charCodes := buildCharCodeMap(jidianEntries)
			customEntries, cerr := loadCustomWords(cfg.CustomWordsPath, charCodes, unigram, logMedian, cfg)
			if cerr != nil {
				fmt.Printf("      [警告] 自定义词表加载失败: %v\n", cerr)
			} else {
				fmt.Printf("      自定义词条: %d 条\n", len(customEntries))
				kept = append(kept, customEntries...)
			}
		}
	}

	// 普通词条权重上限：若启用简码分层则不能超过最低简码权重
	regularMax := cfg.WeightMax
	if cfg.Shortcodes.Enabled && cfg.RegularWeightMax > 0 && cfg.RegularWeightMax < regularMax {
		regularMax = cfg.RegularWeightMax
	}

	// 赋权重（简码词条已在 assignShortcodeWeights 中赋值，此处跳过）
	weightBuckets := make(map[string]int)
	for i, e := range kept {
		if e.shortcodeLevel > 0 {
			weightBuckets["简码"]++
			continue
		}
		isChar := len([]rune(e.Text)) == 1
		if freq, ok := unigram[e.Text]; ok {
			w := computeWeight(freq, logMedian, cfg)
			if isChar && cfg.CharBoostFactor != 1.0 {
				w = clampWeight(int(math.Round(float64(w)*cfg.CharBoostFactor)), cfg)
			}
			if w > regularMax {
				w = regularMax
			}
			kept[i].OrigWeight = w
			bucket := (w / 500) * 500
			key := fmt.Sprintf("%d-%d", bucket, bucket+499)
			weightBuckets[key]++
		} else {
			kept[i].OrigWeight = fallbackWeight(e.OrigWeight, cfg)
			weightBuckets["<200(生僻)"]++
		}
	}

	// 权重分布预览
	fmt.Printf("\n      权重分布预览:\n")
	type bkt struct {
		k  string
		lo int
	}
	var buckets []bkt
	for k := range weightBuckets {
		lo := 0
		if k != "<200(生僻)" {
			fmt.Sscanf(k, "%d", &lo)
		} else {
			lo = -1
		}
		buckets = append(buckets, bkt{k, lo})
	}
	sort.Slice(buckets, func(i, j int) bool { return buckets[i].lo < buckets[j].lo })
	for _, b := range buckets {
		cnt := weightBuckets[b.k]
		bar := strings.Repeat("█", cnt*30/len(kept))
		fmt.Printf("        %15s: %6d  %s\n", b.k, cnt, bar)
	}

	// 按编码升序、同码按权重降序排列
	sort.SliceStable(kept, func(i, j int) bool {
		if kept[i].Code != kept[j].Code {
			return kept[i].Code < kept[j].Code
		}
		return kept[i].OrigWeight > kept[j].OrigWeight
	})

	// 4. 写出
	fmt.Printf("\n[4/4] 写出到 %s ...\n", cfg.OutputPath)
	if err := writeRimeYAML(cfg.OutputPath, kept, cfg); err != nil {
		return fmt.Errorf("写出失败: %w", err)
	}
	stat2, _ := os.Stat(cfg.OutputPath)
	sizeKB := int64(0)
	if stat2 != nil {
		sizeKB = stat2.Size() / 1024
	}
	fmt.Printf("      完成: %d 条，%d KB\n", len(kept), sizeKB)

	// 简码避让冲突分析
	if cfg.Shortcodes.Enabled {
		conflicts := analyzeShortcodeConflicts(kept)
		fmt.Printf("      简码避让冲突: 共 %d 处\n", len(conflicts))
		if cfg.ConflictReportPath != "" {
			if err := writeConflictReport(cfg.ConflictReportPath, conflicts); err != nil {
				fmt.Printf("      [警告] 冲突报告写出失败: %v\n", err)
			} else {
				fmt.Printf("      冲突报告: %s\n", cfg.ConflictReportPath)
			}
		}
	}

	// 写过滤条目
	if len(dropped) > 0 {
		droppedPath := cfg.DroppedPath
		if droppedPath == "" {
			droppedPath = strings.Replace(cfg.OutputPath, ".dict.yaml", ".dict.filtered.tsv", 1)
		}
		writeDropped(droppedPath, dropped)
		fmt.Printf("      过滤条目已写出: %s\n", droppedPath)
	}

	fmt.Printf("\n✓ 完成\n")
	return nil
}
