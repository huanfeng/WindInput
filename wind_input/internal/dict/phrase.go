package dict

import (
	"fmt"
	"log/slog"
	"os"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/cmdbar"
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

	// cmdbarHook 由宿主（coordinator）注入，用于将包含 "$CC(" 的短语
	// 交给命令直通车解析。短语 value 不含 "$CC(" 时仍走旧的 templateEngine
	// 路径，保持完全兼容。
	//   ok == false: hook 未注入或当前短语不需要走 cmdbar (回退旧路径)
	//   err != nil:  解析或求值失败, 调用方应记 WARN 后退化为字面量
	//   返回的 actions 直接挂到 Candidate.Actions, 闭包形式由 hook 自行构造
	cmdbarHook CmdbarPhraseHook
}

// CmdbarPhraseHook 由 coordinator 装配时注入到 PhraseLayer,
// 输入是短语 value 字符串 (如 `$CC("打开百度", open("https://baidu.com"))`)
// 输出: display 候选显示文本, actions 选中触发的已解析动作列表 (含 Effect/Text
// 两种 Kind, 见 cmdbar.ResolvedAction), ok 表示该 value 是否真的被 cmdbar
// 处理 (false 时调用方退回旧模板路径)。
type CmdbarPhraseHook func(value string) (display string, actions []cmdbar.ResolvedAction, ok bool, err error)

// SetCmdbarHook 安装命令直通车 hook (允许为 nil, 等同卸载)。
func (pl *PhraseLayer) SetCmdbarHook(h CmdbarPhraseHook) {
	pl.mu.Lock()
	defer pl.mu.Unlock()
	pl.cmdbarHook = h
	// hook 变化会影响动态短语候选, 清缓存
	pl.cmdCache = make(map[string][]candidate.Candidate)
}

// PhraseEntry 短语条目
//
// 2026-05-16 后权重模型 (docs/design/2026-05-16-cmdbar-followup.md §2):
//   - Weight: 跨编码全局优先级 (0~10000); resolvePhraseWeight 把 0 / 负数
//     都映射为 1000 (短语 tier 中位); 显式 0 由 file entry 层 (Weight=*0)
//     表达"禁用排序权重"。
//   - Position: 同编码组内的 tie-break (升序; 0 = 未手动调整, 已调整优先于
//     未调整); 不再被 fallback 映射为 weight。也用于 MovePhraseUp/Down/ToTop。
type PhraseEntry struct {
	Text     string // 输出文本（可含 $变量模板）
	Weight   int    // 候选权重 (0~10000)
	Position int    // 同 code 内手动调整后的相对顺序 (升序; 0 = 未调整)
	IsSystem bool   // 是否来自系统短语
	Disabled bool   // 是否被禁用
}

// PhraseGroup 数组类型短语组的元数据（texts 字段的条目）。
// Weight / Position 的语义与 PhraseEntry 一致 (参见其 doc); 字符组内
// 各字符共享 Weight, NaturalOrder 由展开位置决定。
type PhraseGroup struct {
	Code     string // 完整编码（如 "zzbd"）
	Name     string // 显示名称（如 "标点符号"）
	Texts    string // 原始字符列表
	Weight   int    // 排序权重 (0~10000)
	Position int    // 同 code 内手动调整后的顺序 (与 PhraseEntry.Position 一致语义)
	IsSystem bool   // 是否来自系统短语
	Disabled bool   // 是否被禁用
}

// PhrasesFileConfig 短语文件的 YAML 结构
type PhrasesFileConfig struct {
	Phrases []PhraseFileEntry `yaml:"phrases"`
}

// PhraseFileEntry 短语文件中的单条配置。
//
// 字符组短语 (一编码展开为 N 个独立字符候选) 改用 Text 字段携带
// $AA("name", "chars") marker 表达, 不再使用 yaml 端的 texts/name 双字段。
// 详见 internal/dict/aa_marker.go 与
// docs/design/2026-05-12-command-bar-design.md §3.7。
type PhraseFileEntry struct {
	Code string `yaml:"code"`
	Text string `yaml:"text"`
	// Weight 显式权重 (0~10000), 优先级高于 Position。
	// 用 *int 以区分"未设置"和"显式设置为 0"。
	Weight   *int `yaml:"weight,omitempty"`
	Position int  `yaml:"position,omitempty"`
	Disabled bool `yaml:"disabled,omitempty"`
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
	positions := make([]int, 0, len(entries))
	for _, e := range entries {
		results = append(results, candidate.Candidate{
			Text:     e.Text,
			Code:     code,
			Weight:   resolvePhraseWeight(e.Weight),
			IsPhrase: true, // 短语永远保留，但不计入 hasCommon 避免污染同编码码表字过滤
		})
		positions = append(positions, e.Position)
	}

	sortPhraseCandidates(results, positions)

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// SearchCommand 查询动态短语和字符组短语，展开后返回。
// 支持三种情况：
//  1. 精确匹配动态短语（含 $ 变量，如 date/time/uuid）
//  2. 精确匹配字符组（texts 字段，如 zzbd → 标点字符列表）
//  3. 字符组前缀匹配（如 zz → 返回 zzbd/zzsz... 导航候选供二级展开）
func (pl *PhraseLayer) SearchCommand(code string, limit int) []candidate.Candidate {
	pl.mu.Lock()
	defer pl.mu.Unlock()

	code = strings.ToLower(code)

	// 缓存命中
	if cached, hit := pl.cmdCache[code]; hit {
		if limit > 0 && len(cached) > limit {
			return cached[:limit]
		}
		return cached
	}

	// ── 情况 1：动态短语精确匹配（含 $ 变量） ──
	if entries, ok := pl.dynamicPhrases[code]; ok {
		results := make([]candidate.Candidate, 0, len(entries))
		positions := make([]int, 0, len(entries))
		for _, e := range entries {
			cand := pl.expandDynamicEntry(code, e)
			results = append(results, cand)
			positions = append(positions, e.Position)
		}
		sortPhraseCandidates(results, positions)
		pl.cmdCache[code] = results
		if limit > 0 && len(results) > limit {
			return results[:limit]
		}
		return results
	}

	// ── 情况 2：字符组精确匹配（texts 字段，如 zzbd → 展开为字符列表） ──
	if group, ok := pl.phraseGroups[code]; ok && !group.Disabled {
		entries := pl.staticPhrases[code]
		// 字符组内所有字符共享 group 的 weight, 用 NaturalOrder = 字符在
		// chars 字符串中的下标做 tie-break, 保证按数组顺序排列。
		// 权重统一取自 group 字段, 不再用每个 char entry 的 e.Position。
		groupWeight := resolvePhraseWeight(group.Weight)
		results := make([]candidate.Candidate, 0, len(entries))
		for i, e := range entries {
			_ = e.Position // 字符级 entry 的 position 仅记录展开顺序, 不参与权重计算
			results = append(results, candidate.Candidate{
				Text:         e.Text,
				Code:         code,
				Weight:       groupWeight,
				NaturalOrder: i,
				IsCommand:    true,
				IsPhrase:     true,
			})
		}
		// 同权重场景下用 NaturalOrder 做 tie-break, 保证字符按 chars 数组顺序。
		sort.Slice(results, func(i, j int) bool {
			return candidate.Better(results[i], results[j])
		})
		pl.cmdCache[code] = results
		if limit > 0 && len(results) > limit {
			return results[:limit]
		}
		return results
	}

	// ── 情况 3：字符组前缀匹配（如 zz → 返回 zzbd/zzsz... 导航候选） ──
	// 前缀长度 < 2 时不触发导航，避免单字符输入（如 "z"）引入噪音候选。
	if len(code) < 2 {
		return nil
	}
	var navResults []candidate.Candidate
	for groupCode, group := range pl.phraseGroups {
		if !group.Disabled && groupCode != code && strings.HasPrefix(groupCode, code) {
			displayName := group.Name
			if displayName == "" {
				displayName = groupCode
			}
			navResults = append(navResults, candidate.Candidate{
				Text:      displayName,
				Code:      groupCode,
				Weight:    resolvePhraseWeight(group.Weight),
				Comment:   groupCode[len(code):],
				IsPhrase:  true,
				IsGroup:   true,
				GroupCode: groupCode,
			})
		}
	}

	if len(navResults) > 0 {
		sort.Slice(navResults, func(i, j int) bool {
			return candidate.Better(navResults[i], navResults[j])
		})
		pl.cmdCache[code] = navResults
		if limit > 0 && len(navResults) > limit {
			return navResults[:limit]
		}
		return navResults
	}

	return nil
}

// SearchPrefix 前缀查询。
//   - phraseGroups: 返回组名候选 (Comment 显示编码后缀)
//   - 静态短语: 直接候选
//   - 动态短语 (含 $CC marker): 展开为可执行命令候选, 末尾 filterCmdbarExactOnly
//     再过滤 $CC( 仅精确条目, 留下 $CC1(
//
// 普通 $X 模板动态短语 (如 date → $Y-$M-$D) 不在前缀路径出现, 维持精确匹配
// 语义 (通过 SearchCommand 触发)。
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
				Weight:    resolvePhraseWeight(group.Weight),
				Comment:   code[len(prefix):], // 显示编码后缀（如 zz→zzbd 显示 "bd"）
				IsPhrase:  true,
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
					Weight:   resolvePhraseWeight(e.Weight),
					IsPhrase: true,
				})
			}
		}
	}

	// 3. 处理动态短语中的命令直通车 ($CC/$CC1)。仅扫含 cmdbar marker 的条目,
	//    避免普通 $X 模板 (date/time 等) 污染前缀候选。末尾的 filterCmdbarExactOnly
	//    会把 $CC( 过滤掉, 只留 $CC1( 实际参与前缀展开。
	for dynCode, entries := range pl.dynamicPhrases {
		if dynCode == prefix || !strings.HasPrefix(dynCode, prefix) {
			continue
		}
		for _, e := range entries {
			if !HasCmdbarMarker(e.Text) {
				continue
			}
			cand := pl.expandDynamicEntry(dynCode, e)
			if cand.Comment == "" {
				cand.Comment = dynCode[len(prefix):]
			}
			results = append(results, cand)
		}
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	// 过滤 cmdbar 仅精确条目 ($CC(), 保留 $CC1(
	results = filterCmdbarExactOnly(results)

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
			// 字符组短语: 优先解析 Text 字段中的 $AA("name", "chars") marker,
			// 兼容回退到旧的 Texts/Name 字段 (migration 已把 bbolt 中旧记录改写,
			// 这里的回退仅服务于尚未走过 migration 的内存路径如测试种子)。
			name, chars, ok := ParseAAMarker(rec.Text)
			if !ok {
				name = rec.Name
				chars = rec.Texts
			}
			pg := PhraseGroup{
				Code:     code,
				Name:     name,
				Texts:    chars,
				Weight:   rec.Weight,
				Position: position,
				IsSystem: rec.IsSystem,
			}
			pl.phraseGroups[code] = pg
			// 字符级 entry 共享 group 的权重, 字符内 NaturalOrder 由 SearchCommand
			// 在展开时按数组下标分配, 这里 Position 仅记录原始展开顺序。
			runes := []rune(chars)
			for idx, r := range runes {
				arrEntry := PhraseEntry{
					Text:     string(r),
					Weight:   rec.Weight,
					Position: position + idx,
					IsSystem: rec.IsSystem,
				}
				pl.staticPhrases[code] = append(pl.staticPhrases[code], arrEntry)
			}

		case "dynamic":
			entry := PhraseEntry{
				Text:     rec.Text,
				Weight:   rec.Weight,
				Position: position,
				IsSystem: rec.IsSystem,
			}
			pl.dynamicPhrases[code] = append(pl.dynamicPhrases[code], entry)

		default: // "static"
			entry := PhraseEntry{
				Text:     rec.Text,
				Weight:   rec.Weight,
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
// 字符组短语通过 Text 字段的 $AA("name", "chars") marker 识别 (array)。
func detectPhraseType(e PhraseFileEntry) string {
	if _, _, ok := ParseAAMarker(e.Text); ok {
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

// expandDynamicEntry 把一条动态短语条目展开成候选。
// 含 cmdbar marker ($CC( 或 $CC1() 且已注入 cmdbarHook 时走命令直通车;
// 否则保持旧 templateEngine 行为。
// hook 报错时不阻断输入流, 退化为字面量短语并记 WARN (不带 value 内容)。
// 调用方需持有 pl.mu (RLock 或 Lock 形态, 仅读字段不修改)。
func (pl *PhraseLayer) expandDynamicEntry(code string, e PhraseEntry) candidate.Candidate {
	value := e.Text
	ve := ValueExpander{Hook: pl.cmdbarHook, TemplateEngine: pl.templateEngine}

	// 含 $CC 但 hook 返回错: ValueExpander 会降级为字面量 (IsCommand=true)。
	// PhraseLayer 仍负责记 WARN, 因为短语数量有限, 日志不会爆。
	res := ve.Expand(value)
	if res.IsCommand && len(res.Actions) == 0 && res.DisplayText == "" && pl.cmdbarHook != nil && HasCmdbarMarker(value) {
		// IsCommand=true && Actions=nil && DisplayText="" 仅在 hook 报错时出现。
		// (hook 成功时即使 Actions=nil, DisplayText 也非空; ok=false 时 IsCommand=false)
		slog.Warn("phrase: cmdbar hook returned error, falling back to literal",
			"code", code, "valueLen", len(value))
	}

	out := candidate.Candidate{
		Text:           res.Text,
		Code:           code,
		Weight:         resolvePhraseWeight(e.Weight),
		IsCommand:      true,
		IsPhrase:       true,
		PhraseTemplate: e.Text,
	}
	if res.IsCommand {
		out.DisplayText = res.DisplayText
		out.Actions = res.Actions
	}
	return out
}

// ===== 辅助函数 =====

// resolvePhraseWeight 计算短语候选的最终权重 (0~10000)。
//
// 设计 (docs/design/2026-05-16-cmdbar-followup.md §2):
//   - weight 与 position 各司其职: weight 表跨编码全局优先级,
//     position 表同编码组内的手动调整顺序; position 不再被换算为 weight。
//   - 旧 `10000 - position` fallback 公式被删除 — 那是导致旧 yaml 中
//     `position: 1` 被放大为 weight=9999 的根因 (短语过度靠前)。
//
// 优先级:
//  1. 显式 weight > 0 → 直接使用 (clamp 到 NormalizedWeightMax)
//  2. weight <= 0     → 默认 1000 (短语 tier 的中位)
//
// position 由 sortPhraseCandidates 作为同 code tie-break 使用, 不影响 weight。
func resolvePhraseWeight(weight int) int {
	if weight <= 0 {
		return 1000
	}
	if weight > NormalizedWeightMax {
		return NormalizedWeightMax
	}
	return weight
}

// resolveWeightFromFileEntry 把 yaml 解析出的 PhraseFileEntry 转成
// 最终生效的权重整数 (0~10000)。
//
// PhraseFileEntry.Weight 是 *int 以区分"未在 yaml 写"和"显式 weight: 0":
//   - nil          → 默认 1000
//   - *Weight == 0 → 0 (用户主动禁用排序权重)
//   - *Weight  > 0 → resolvePhraseWeight 处理 (clamp)
//   - *Weight  < 0 → 0
func resolveWeightFromFileEntry(e PhraseFileEntry) int {
	if e.Weight == nil {
		return 1000
	}
	if *e.Weight <= 0 {
		return 0
	}
	return resolvePhraseWeight(*e.Weight)
}

// sortPhraseCandidates 按短语 tier 内部规则排序:
//   - 主键: Weight 降序
//   - 同 weight: position 升序; 但 position == 0 视为"未手动调整",
//     已调整 (position > 0) 优先于未调整, 多个已调整按 position 升序。
//
// 旧名 sortByPosition 保留为本函数的别名供已有调用点透明替换。
func sortPhraseCandidates(candidates []candidate.Candidate, positions []int) {
	if len(positions) != len(candidates) {
		// 长度不一致时退化为纯 weight 排序 (调用方未传 positions)。
		sort.SliceStable(candidates, func(i, j int) bool {
			return candidates[i].Weight > candidates[j].Weight
		})
		return
	}
	type pair struct {
		cand candidate.Candidate
		pos  int
	}
	pairs := make([]pair, len(candidates))
	for i := range candidates {
		pairs[i] = pair{cand: candidates[i], pos: positions[i]}
	}
	sort.SliceStable(pairs, func(i, j int) bool {
		if pairs[i].cand.Weight != pairs[j].cand.Weight {
			return pairs[i].cand.Weight > pairs[j].cand.Weight
		}
		// position == 0 视为未调整, 排在已调整之后。
		iAdj, jAdj := pairs[i].pos > 0, pairs[j].pos > 0
		if iAdj != jAdj {
			return iAdj
		}
		if iAdj { // 都已调整
			return pairs[i].pos < pairs[j].pos
		}
		return false // 都未调整, 保持稳定
	})
	for i := range pairs {
		candidates[i] = pairs[i].cand
	}
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
