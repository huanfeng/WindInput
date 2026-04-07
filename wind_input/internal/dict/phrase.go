package dict

import (
	"fmt"
	"os"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/fileutil"
	"gopkg.in/yaml.v3"
)

// PhraseLayer 短语层
// 加载系统短语和用户短语，支持变量模板展开（$Y, $MM, $DD 等）。
// 含变量的短语为"动态短语"，仅精确匹配（通过 SearchCommand），
// 不含变量的为"静态短语"，支持前缀搜索。
type PhraseLayer struct {
	mu                 sync.RWMutex
	name               string
	systemFilePath     string // 系统短语文件（随程序打包，只读）
	systemUserFilePath string // 用户目录的系统短语文件（同名覆盖，存在时替代系统文件）
	userFilePath       string // 用户短语文件（用户可编辑）

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

// NewPhraseLayer 创建短语层（兼容旧调用）
func NewPhraseLayer(name string, systemPath, userPath string) *PhraseLayer {
	return NewPhraseLayerEx(name, systemPath, "", userPath)
}

// NewPhraseLayerEx 创建短语层
// systemPath: 系统短语文件路径（程序目录，只读）
// systemUserPath: 用户目录的系统短语文件（同名覆盖，存在时替代 systemPath）
// userPath: 用户短语文件路径（可读写）
func NewPhraseLayerEx(name string, systemPath, systemUserPath, userPath string) *PhraseLayer {
	return &PhraseLayer{
		name:               name,
		systemFilePath:     systemPath,
		systemUserFilePath: systemUserPath,
		userFilePath:       userPath,
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

// Load 加载系统短语和用户短语
func (pl *PhraseLayer) Load() error {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	pl.staticPhrases = make(map[string][]PhraseEntry)
	pl.dynamicPhrases = make(map[string][]PhraseEntry)
	pl.phraseGroups = make(map[string]PhraseGroup)
	pl.cmdCache = make(map[string][]candidate.Candidate)

	// 1. 加载系统短语：优先用户目录的同名文件，不存在则用程序目录的原始文件
	systemLoaded := false
	if pl.systemUserFilePath != "" {
		if err := pl.loadFile(pl.systemUserFilePath, true); err == nil {
			systemLoaded = true
		} else if !os.IsNotExist(err) {
			return fmt.Errorf("failed to load user system phrases: %w", err)
		}
	}
	if !systemLoaded && pl.systemFilePath != "" {
		if err := pl.loadFile(pl.systemFilePath, true); err != nil {
			if !os.IsNotExist(err) {
				return fmt.Errorf("failed to load system phrases: %w", err)
			}
		}
	}

	// 2. 加载用户短语
	if pl.userFilePath != "" {
		if err := pl.loadFile(pl.userFilePath, false); err != nil {
			if !os.IsNotExist(err) {
				return fmt.Errorf("failed to load user phrases: %w", err)
			}
		}
	}

	return nil
}

// loadFile 从文件加载短语（需持有写锁）
func (pl *PhraseLayer) loadFile(path string, isSystem bool) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}

	var config PhrasesFileConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return fmt.Errorf("failed to parse phrases file %s: %w", path, err)
	}

	for _, p := range config.Phrases {
		code := strings.ToLower(p.Code)
		if code == "" || (p.Text == "" && p.Texts == "") {
			continue
		}

		position := p.Position
		if position <= 0 {
			position = 1
		}

		// texts 字段：注册为组并展开字符到精确匹配索引
		if p.Texts != "" {
			pl.phraseGroups[code] = PhraseGroup{
				Code:     code,
				Name:     p.Name,
				Texts:    p.Texts,
				Position: position,
				IsSystem: isSystem,
				Disabled: p.Disabled,
			}
			if p.Disabled {
				continue
			}
			runes := []rune(p.Texts)
			for idx, r := range runes {
				arrEntry := PhraseEntry{
					Text:     string(r),
					Position: position + idx,
					IsSystem: isSystem,
				}
				pl.staticPhrases[code] = append(pl.staticPhrases[code], arrEntry)
			}
			continue
		}

		// 用户文件可以覆盖系统短语（同 code+text 的条目）
		if !isSystem {
			phraseID := code + "||" + p.Text
			// 检查并移除系统短语中的同名条目（用户覆盖）
			pl.removeEntryByID(phraseID)
		}

		entry := PhraseEntry{
			Text:     p.Text,
			Position: position,
			IsSystem: isSystem,
			Disabled: p.Disabled,
		}

		// 被禁用的条目不加入查询索引（但仍可被前端读取）
		if p.Disabled {
			continue
		}

		// 含 $变量 的为动态短语，否则为静态短语
		if HasVariable(p.Text) {
			pl.dynamicPhrases[code] = append(pl.dynamicPhrases[code], entry)
		} else {
			pl.staticPhrases[code] = append(pl.staticPhrases[code], entry)
		}
	}

	return nil
}

// removeEntryByID 根据 "code||text" 从所有索引中移除条目（用于用户覆盖系统短语）
func (pl *PhraseLayer) removeEntryByID(id string) {
	for code, entries := range pl.staticPhrases {
		for i, e := range entries {
			if code+"||"+e.Text == id {
				pl.staticPhrases[code] = append(entries[:i], entries[i+1:]...)
				if len(pl.staticPhrases[code]) == 0 {
					delete(pl.staticPhrases, code)
				}
				return
			}
		}
	}
	for code, entries := range pl.dynamicPhrases {
		for i, e := range entries {
			if code+"||"+e.Text == id {
				pl.dynamicPhrases[code] = append(entries[:i], entries[i+1:]...)
				if len(pl.dynamicPhrases[code]) == 0 {
					delete(pl.dynamicPhrases, code)
				}
				return
			}
		}
	}
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

// GetUserFilePath 获取用户短语文件路径
func (pl *PhraseLayer) GetUserFilePath() string {
	return pl.userFilePath
}

// GetSystemFilePath 获取系统短语文件路径
func (pl *PhraseLayer) GetSystemFilePath() string {
	return pl.systemFilePath
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

	code = strings.ToLower(code)

	// 从用户文件中移除此条目
	removed := pl.removeUserOverride(code, templateText)
	pl.clearCmdCache(code)
	pl.mu.Unlock()

	if !removed {
		return nil
	}

	// 重新加载以恢复系统默认
	return pl.Load()
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

// removeUserOverride 从用户文件配置中移除指定条目的覆盖
func (pl *PhraseLayer) removeUserOverride(code, templateText string) bool {
	if pl.userFilePath == "" {
		return false
	}
	data, err := os.ReadFile(pl.userFilePath)
	if err != nil {
		return false
	}
	var config PhrasesFileConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return false
	}
	found := false
	filtered := config.Phrases[:0]
	for _, p := range config.Phrases {
		if strings.ToLower(p.Code) == code && p.Text == templateText {
			found = true
			continue
		}
		filtered = append(filtered, p)
	}
	if !found {
		return false
	}
	config.Phrases = filtered
	out, err := yaml.Marshal(&config)
	if err != nil {
		return false
	}
	_ = fileutil.AtomicWrite(pl.userFilePath, out, 0644)
	return true
}

// savePositionOverrides 将两个条目的当前 position 持久化到用户文件
func (pl *PhraseLayer) savePositionOverrides(code, text1, text2 string) error {
	if pl.userFilePath == "" {
		return fmt.Errorf("no user file path configured")
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

	// 加载用户文件
	var config PhrasesFileConfig
	data, err := os.ReadFile(pl.userFilePath)
	if err == nil {
		_ = yaml.Unmarshal(data, &config)
	}

	// 更新或添加条目
	updateOrAdd := func(text string, position int) {
		for i, p := range config.Phrases {
			if strings.ToLower(p.Code) == code && p.Text == text {
				config.Phrases[i].Position = position
				return
			}
		}
		config.Phrases = append(config.Phrases, PhraseFileEntry{
			Code:     code,
			Text:     text,
			Position: position,
		})
	}
	updateOrAdd(text1, pos1)
	updateOrAdd(text2, pos2)

	out, err := yaml.Marshal(&config)
	if err != nil {
		return fmt.Errorf("failed to marshal phrases: %w", err)
	}
	return fileutil.AtomicWrite(pl.userFilePath, out, 0644)
}
