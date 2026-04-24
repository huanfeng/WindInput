package dict

import (
	"fmt"
	"os"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/store"
	"gopkg.in/yaml.v3"
)

// PhraseLayer 短语层
// 加载系统短语和用户短语，支持变量模板展开（$Y, $MM, $DD 等）。
// 含变量的短语为"动态短语"，仅精确匹配（通过 SearchCommand），
// 不含变量的为"静态短语"，支持前缀搜索。
type PhraseLayer struct {
	mu                 sync.RWMutex
	name               string
	systemFilePath     string       // 系统短语文件（随程序打包，只读）
	systemUserFilePath string       // 用户目录的系统短语文件（同名覆盖，存在时替代系统文件）
	store              *store.Store // 持久化后端（user_data.db）

	// 静态短语（不含变量）: code -> []PhraseEntry，参与前缀搜索
	staticPhrases map[string][]PhraseEntry

	// 动态短语（含 $ 变量）: code -> []PhraseEntry，仅精确匹配
	dynamicPhrases map[string][]PhraseEntry

	// 数组组信息（texts 字段）: code -> PhraseGroup
	// 前缀搜索时返回组名候选而非展开字符
	phraseGroups map[string]PhraseGroup

	// 模板引擎
	templateEngine *TemplateEngine

	// 命令结果缓存（动态短语）
	cmdCache    map[string][]candidate.Candidate
	cmdCacheKey string
}

// PhraseEntry 短语条目
type PhraseEntry struct {
	Text     string // 输出文本（可含 $变量模板）
	Position int    // 候选位置
	IsSystem bool   // 是否来自系统短语
	Disabled bool   // 是否被禁用
}

// PhraseGroup 数组类型短语组的元数据（texts 字段的条目）
type PhraseGroup struct {
	Code     string // 完整编码（如 "zzbd"）
	Name     string // 显示名称（如 "标点符号"）
	Texts    string // 原始字符列表
	Position int    // 排序位置
	IsSystem bool   // 是否来自系统短语
	Disabled bool   // 是否被禁用
}

// PhrasesFileConfig 短语文件的 YAML 结构
type PhrasesFileConfig struct {
	Phrases []PhraseFileEntry `yaml:"phrases"`
}

// PhraseFileEntry 短语文件中的单条配置
type PhraseFileEntry struct {
	Code     string `yaml:"code"`
	Text     string `yaml:"text"`
	Texts    string `yaml:"texts,omitempty"` // 数组映射：每个字符展开为独立候选
	Name     string `yaml:"name,omitempty"`  // 组显示名称（用于 texts 类型的候选展示）
	Position int    `yaml:"position"`
	Disabled bool   `yaml:"disabled,omitempty"`
}

// NewPhraseLayer 创建短语层（测试用简化版，不绑定 Store）
func NewPhraseLayer(name string, systemPath string) *PhraseLayer {
	return NewPhraseLayerEx(name, systemPath, "", nil)
}

// NewPhraseLayerEx 创建短语层
// systemPath: 系统短语文件路径（程序目录，只读）
// systemUserPath: 用户目录的系统短语文件（同名覆盖，存在时替代 systemPath）
// s: 持久化后端（user_data.db），可为 nil（测试场景）
func NewPhraseLayerEx(name string, systemPath, systemUserPath string, s *store.Store) *PhraseLayer {
	return &PhraseLayer{
		name:               name,
		systemFilePath:     systemPath,
		systemUserFilePath: systemUserPath,
		store:              s,
		staticPhrases:      make(map[string][]PhraseEntry),
		dynamicPhrases:     make(map[string][]PhraseEntry),
		phraseGroups:       make(map[string]PhraseGroup),
		templateEngine:     GetTemplateEngine(),
		cmdCache:           make(map[string][]candidate.Candidate),
	}
}

// Name 返回层名称
func (pl *PhraseLayer) Name() string {
	return pl.name
}

// Type 返回层类型
func (pl *PhraseLayer) Type() LayerType {
	return LayerTypeLogic
}

// Search 精确查询静态短语（不含变量的短语）
func (pl *PhraseLayer) Search(code string, limit int) []candidate.Candidate {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	code = strings.ToLower(code)
	entries, ok := pl.staticPhrases[code]
	if !ok {
		return nil
	}

	results := make([]candidate.Candidate, 0, len(entries))
	for _, e := range entries {
		results = append(results, candidate.Candidate{
			Text:     e.Text,
			Code:     code,
			Weight:   positionToWeight(e.Position),
			IsCommon: true, // 短语由用户/系统配置，不应被 smart 过滤
		})
	}

	sortByPosition(results)

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// SearchCommand 查询动态短语（含变量的短语），展开模板后返回
func (pl *PhraseLayer) SearchCommand(code string, limit int) []candidate.Candidate {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)
	entries, ok := pl.dynamicPhrases[code]
	if !ok {
		return nil
	}

	// 使用缓存
	if cached, hit := pl.cmdCache[code]; hit {
		if limit > 0 && len(cached) > limit {
			return cached[:limit]
		}
		return cached
	}

	results := make([]candidate.Candidate, 0, len(entries))
	for _, e := range entries {
		expanded := pl.templateEngine.Expand(e.Text)
		results = append(results, candidate.Candidate{
			Text:           expanded,
			Code:           code,
			Weight:         positionToWeight(e.Position),
			IsCommand:      true,
			IsCommon:       true,   // 动态短语不应被 smart 过滤
			PhraseTemplate: e.Text, // 携带原始模板文本，用于右键菜单定位条目
		})
	}

	sortByPosition(results)
	pl.cmdCache[code] = results

	if limit > 0 && len(results) > limit {
		return results[:limit]
	}
	return results
}

// SearchPrefix 前缀查询（仅静态短语）
// 对 phraseGroups 中的条目，前缀搜索返回组名候选而非展开字符
func (pl *PhraseLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	prefix = strings.ToLower(prefix)
	var results []candidate.Candidate

	// 1. 处理 phraseGroups：返回组名候选
	for code, group := range pl.phraseGroups {
		if code != prefix && strings.HasPrefix(code, prefix) && !group.Disabled {
			displayName := group.Name
			if displayName == "" {
				displayName = code
			}
			results = append(results, candidate.Candidate{
				Text:      displayName,
				Code:      code,
				Weight:    positionToWeight(group.Position),
				Comment:   code[len(prefix):], // 显示编码后缀（如 zz→zzbd 显示 "bd"）
				IsCommon:  true,
				IsGroup:   true,
				GroupCode: code,
			})
		}
	}

	// 2. 处理普通静态短语（跳过 phraseGroups 已覆盖的编码）
	for code, entries := range pl.staticPhrases {
		if strings.HasPrefix(code, prefix) {
			if _, isGroup := pl.phraseGroups[code]; isGroup {
				continue // 此编码的字符级候选不参与前缀搜索
			}
			for _, e := range entries {
				results = append(results, candidate.Candidate{
					Text:     e.Text,
					Code:     code,
					Weight:   positionToWeight(e.Position),
					IsCommon: true,
				})
			}
		}
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// InvalidateCache 清除动态短语缓存
func (pl *PhraseLayer) InvalidateCache() {
	pl.mu.Lock()
	defer pl.mu.Unlock()
	pl.cmdCache = make(map[string][]candidate.Candidate)
}

// InvalidateCacheForInput 根据输入变化清除缓存
func (pl *PhraseLayer) InvalidateCacheForInput(input string) {
	pl.mu.Lock()
	defer pl.mu.Unlock()
	if pl.cmdCacheKey != input {
		pl.cmdCache = make(map[string][]candidate.Candidate)
		pl.cmdCacheKey = input
	}
}

// LoadFromStore loads phrases from the bbolt Store's Phrases bucket.
// This replaces file-based loading when Store backend is enabled.
func (pl *PhraseLayer) LoadFromStore(s *store.Store) error {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	// Clear existing data
	pl.staticPhrases = make(map[string][]PhraseEntry)
	pl.dynamicPhrases = make(map[string][]PhraseEntry)
	pl.phraseGroups = make(map[string]PhraseGroup)
	pl.cmdCache = make(map[string][]candidate.Candidate)
	pl.cmdCacheKey = ""

	records, err := s.GetAllPhrases()
	if err != nil {
		return fmt.Errorf("load phrases from store: %w", err)
	}

	for _, rec := range records {
		if !rec.Enabled {
			continue
		}

		code := strings.ToLower(rec.Code)
		position := rec.Position
		if position <= 0 {
			position = 1
		}

		switch rec.Type {
		case "array":
			pg := PhraseGroup{
				Code:     code,
				Name:     rec.Name,
				Texts:    rec.Texts,
				Position: position,
				IsSystem: rec.IsSystem,
			}
			pl.phraseGroups[code] = pg
			// Expand array characters into static phrases (same logic as loadFile)
			runes := []rune(rec.Texts)
			for idx, r := range runes {
				arrEntry := PhraseEntry{
					Text:     string(r),
					Position: position + idx,
					IsSystem: rec.IsSystem,
				}
				pl.staticPhrases[code] = append(pl.staticPhrases[code], arrEntry)
			}

		case "dynamic":
			entry := PhraseEntry{
				Text:     rec.Text,
				Position: position,
				IsSystem: rec.IsSystem,
			}
			pl.dynamicPhrases[code] = append(pl.dynamicPhrases[code], entry)

		default: // "static"
			entry := PhraseEntry{
				Text:     rec.Text,
				Position: position,
				IsSystem: rec.IsSystem,
			}
			pl.staticPhrases[code] = append(pl.staticPhrases[code], entry)
		}
	}

	return nil
}

// ParsePhraseYAMLFile reads a phrases YAML file and returns PhraseFileEntry slice.
func ParsePhraseYAMLFile(path string) ([]PhraseFileEntry, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var config PhrasesFileConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return nil, fmt.Errorf("parse phrases file %s: %w", path, err)
	}

	return config.Phrases, nil
}

// detectPhraseType determines the type string from a PhraseFileEntry.
func detectPhraseType(e PhraseFileEntry) string {
	if e.Texts != "" {
		return "array"
	}
	if HasVariable(e.Text) {
		return "dynamic"
	}
	return "static"
}

// GetPhraseCount 获取静态短语数量
func (pl *PhraseLayer) GetPhraseCount() int {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	count := 0
	for _, entries := range pl.staticPhrases {
		count += len(entries)
	}
	return count
}

// GetCommandCount 获取动态短语数量
func (pl *PhraseLayer) GetCommandCount() int {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	count := 0
	for _, entries := range pl.dynamicPhrases {
		count += len(entries)
	}
	return count
}

// ===== 辅助函数 =====

// positionToWeight 将位置转换为权重（position 1 → 最高权重）
func positionToWeight(position int) int {
	if position <= 0 {
		position = 1
	}
	return 10000 - position
}

// sortByPosition 按位置排序候选
func sortByPosition(candidates []candidate.Candidate) {
	sort.Slice(candidates, func(i, j int) bool {
		return candidates[i].Weight > candidates[j].Weight
	})
}

// ===== 右键菜单：短语位置调整 =====

// MovePhraseUp 在同一编码组内将短语前移一位（position 减小）
// templateText 为原始模板文本（如 "$Y-$MM-$DD"），用于精确定位条目
func (pl *PhraseLayer) MovePhraseUp(code, templateText string) error {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)
	entries := pl.getDynEntriesSorted(code)
	if entries == nil {
		entries = pl.getStatEntriesSorted(code)
	}
	if len(entries) < 2 {
		return nil
	}

	// 找到目标条目及其上方的条目
	targetIdx := -1
	for i, e := range entries {
		if e.Text == templateText {
			targetIdx = i
			break
		}
	}
	if targetIdx <= 0 { // 已在首位或未找到
		return nil
	}

	// 交换相邻两个条目的 position
	pl.swapEntryPositions(code, entries[targetIdx].Text, entries[targetIdx-1].Text)
	pl.clearCmdCache(code)

	return pl.savePositionOverrides(code, entries[targetIdx].Text, entries[targetIdx-1].Text)
}

// MovePhraseDown 在同一编码组内将短语后移一位（position 增大）
func (pl *PhraseLayer) MovePhraseDown(code, templateText string) error {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)
	entries := pl.getDynEntriesSorted(code)
	if entries == nil {
		entries = pl.getStatEntriesSorted(code)
	}
	if len(entries) < 2 {
		return nil
	}

	targetIdx := -1
	for i, e := range entries {
		if e.Text == templateText {
			targetIdx = i
			break
		}
	}
	if targetIdx < 0 || targetIdx >= len(entries)-1 { // 已在末位或未找到
		return nil
	}

	pl.swapEntryPositions(code, entries[targetIdx].Text, entries[targetIdx+1].Text)
	pl.clearCmdCache(code)

	return pl.savePositionOverrides(code, entries[targetIdx].Text, entries[targetIdx+1].Text)
}

// MovePhraseToTop 将短语移动到同一编码组的首位
func (pl *PhraseLayer) MovePhraseToTop(code, templateText string) error {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)
	entries := pl.getDynEntriesSorted(code)
	if entries == nil {
		entries = pl.getStatEntriesSorted(code)
	}
	if len(entries) < 2 {
		return nil
	}

	targetIdx := -1
	for i, e := range entries {
		if e.Text == templateText {
			targetIdx = i
			break
		}
	}
	if targetIdx <= 0 { // 已在首位或未找到
		return nil
	}

	// 与首位交换
	pl.swapEntryPositions(code, entries[targetIdx].Text, entries[0].Text)
	pl.clearCmdCache(code)

	return pl.savePositionOverrides(code, entries[targetIdx].Text, entries[0].Text)
}

// HasPhraseOverride 检查用户是否覆盖了指定短语的位置
func (pl *PhraseLayer) HasPhraseOverride(code, templateText string) bool {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	code = strings.ToLower(code)

	// 检查动态短语
	for _, e := range pl.dynamicPhrases[code] {
		if e.Text == templateText && !e.IsSystem {
			return true
		}
	}
	// 检查静态短语
	for _, e := range pl.staticPhrases[code] {
		if e.Text == templateText && !e.IsSystem {
			return true
		}
	}
	return false
}

// ResetPhraseOverride 移除用户对指定短语的位置覆盖，恢复系统默认
func (pl *PhraseLayer) ResetPhraseOverride(code, templateText string) error {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)

	// 从系统短语 YAML 中查找原始 position
	origPos := 0
	found := false
	for _, path := range []string{pl.systemUserFilePath, pl.systemFilePath} {
		if path == "" {
			continue
		}
		entries, err := ParsePhraseYAMLFile(path)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if strings.ToLower(e.Code) == code && e.Text == templateText {
				origPos = e.Position
				if origPos <= 0 {
					origPos = 1
				}
				found = true
				break
			}
		}
		if found {
			break
		}
	}

	if !found {
		return nil
	}

	// 恢复内存中的 position
	for _, entries := range []map[string][]PhraseEntry{pl.dynamicPhrases, pl.staticPhrases} {
		for i, e := range entries[code] {
			if e.Text == templateText {
				entries[code][i].Position = origPos
			}
		}
	}
	pl.clearCmdCache(code)

	// 同步到 Store
	if pl.store != nil {
		records, err := pl.store.GetPhrasesByCode(code)
		if err == nil {
			for _, rec := range records {
				if rec.Text == templateText {
					rec.Position = origPos
					_ = pl.store.UpdatePhrase(rec)
					break
				}
			}
		}
	}

	return nil
}

// ===== 内部辅助方法 =====

// getDynEntriesSorted 获取动态短语条目（按 position 升序）
func (pl *PhraseLayer) getDynEntriesSorted(code string) []PhraseEntry {
	entries, ok := pl.dynamicPhrases[code]
	if !ok || len(entries) == 0 {
		return nil
	}
	sorted := make([]PhraseEntry, len(entries))
	copy(sorted, entries)
	sort.Slice(sorted, func(i, j int) bool {
		return sorted[i].Position < sorted[j].Position
	})
	return sorted
}

// getStatEntriesSorted 获取静态短语条目（按 position 升序）
func (pl *PhraseLayer) getStatEntriesSorted(code string) []PhraseEntry {
	entries, ok := pl.staticPhrases[code]
	if !ok || len(entries) == 0 {
		return nil
	}
	sorted := make([]PhraseEntry, len(entries))
	copy(sorted, entries)
	sort.Slice(sorted, func(i, j int) bool {
		return sorted[i].Position < sorted[j].Position
	})
	return sorted
}

// swapEntryPositions 交换同一编码下两个条目的 position（内存中）
func (pl *PhraseLayer) swapEntryPositions(code, text1, text2 string) {
	// 先尝试动态短语
	if pl.swapInMap(pl.dynamicPhrases, code, text1, text2) {
		return
	}
	// 再尝试静态短语
	pl.swapInMap(pl.staticPhrases, code, text1, text2)
}

func (pl *PhraseLayer) swapInMap(m map[string][]PhraseEntry, code, text1, text2 string) bool {
	entries, ok := m[code]
	if !ok {
		return false
	}
	idx1, idx2 := -1, -1
	for i, e := range entries {
		if e.Text == text1 {
			idx1 = i
		}
		if e.Text == text2 {
			idx2 = i
		}
	}
	if idx1 < 0 || idx2 < 0 {
		return false
	}
	entries[idx1].Position, entries[idx2].Position = entries[idx2].Position, entries[idx1].Position
	return true
}

// clearCmdCache 清除指定编码的命令缓存
func (pl *PhraseLayer) clearCmdCache(code string) {
	delete(pl.cmdCache, code)
}

// savePositionOverrides 将两个条目的当前 position 持久化到 Store
func (pl *PhraseLayer) savePositionOverrides(code, text1, text2 string) error {
	if pl.store == nil {
		return nil
	}

	// 查找当前 position
	pos1, pos2 := 0, 0
	for _, entries := range []map[string][]PhraseEntry{pl.dynamicPhrases, pl.staticPhrases} {
		for _, e := range entries[code] {
			if e.Text == text1 {
				pos1 = e.Position
			}
			if e.Text == text2 {
				pos2 = e.Position
			}
		}
	}

	// 从 Store 读取并更新位置
	records, err := pl.store.GetPhrasesByCode(code)
	if err != nil {
		return fmt.Errorf("get phrases by code %q: %w", code, err)
	}
	for _, rec := range records {
		if rec.Text == text1 && rec.Position != pos1 {
			rec.Position = pos1
			if err := pl.store.UpdatePhrase(rec); err != nil {
				return fmt.Errorf("update phrase position: %w", err)
			}
		}
		if rec.Text == text2 && rec.Position != pos2 {
			rec.Position = pos2
			if err := pl.store.UpdatePhrase(rec); err != nil {
				return fmt.Errorf("update phrase position: %w", err)
			}
		}
	}
	return nil
}
