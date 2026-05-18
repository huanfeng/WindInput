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

	// 数组组信息: code -> [PhraseGroup,...]
	// 同 code 可以有多个 group ($AA / $SS / 混合), 用户输入精确码命中时:
	//   - len==1: 直接展开为成员
	//   - len>=2: 经 coordinator collapse 路径展示为多 nav 让用户选
	// 前缀搜索时, 每个 group 都出 1 个 nav 候选。
	// 详见 docs/design/candidate-actions.md §5。
	phraseGroups map[string][]PhraseGroup

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

	// cmdbarArrayHook 由宿主装配, 用于处理 $SS 字符串数组短语。$SS 在
	// SearchCommand 精确码命中时被调用, 把 marker text 展开成 N 个元素
	// (含嵌入 $CC 元素的 display/actions 求值)。设计 §4.3。
	cmdbarArrayHook CmdbarArrayHook
}

// CmdbarArrayHook 由 coordinator 装配, 解析 $SS 字符串数组短语:
//   - value: 原 marker text (`$SS("name", ...)`)
//   - name: $SS 的 group display name (与静态 ParseSSGroupName 等价)
//   - elements: 每个元素的展开结果, 顺序保持 marker 内顺序
//   - groupModifiers: $SS 的 modifier map (含 marker syntax sugar 默认值 + 显式)
//   - ok: 该 value 是否真的被识别为 $SS (false 时 PhraseLayer 退回旧路径)
//   - err: 解析或求值失败
type CmdbarArrayHook func(value string) (name string, elements []CmdbarArrayElement, groupModifiers map[string]any, ok bool, err error)

// CmdbarArrayElement 是 $SS 内一个元素的展开结果。string lit 元素的 Actions 为空;
// 嵌入 $CC 元素的 Actions 是其动作链。ElementModifiers 是嵌入 $CC 自身的 modifiers
// (group prefix 由 groupModifiers["prefix"] 控制, 嵌入 $CC 禁用 prefix)。
type CmdbarArrayElement struct {
	Display          string
	Actions          []cmdbar.ResolvedAction
	ElementModifiers map[string]any
}

// SetCmdbarArrayHook 安装 $SS 数组 hook (允许为 nil, 等同卸载)。
func (pl *PhraseLayer) SetCmdbarArrayHook(h CmdbarArrayHook) {
	pl.mu.Lock()
	defer pl.mu.Unlock()
	pl.cmdbarArrayHook = h
	pl.cmdCache = make(map[string][]candidate.Candidate)
}

// CmdbarPhraseHook 由 coordinator 装配时注入到 PhraseLayer,
// 输入是短语 value 字符串 (如 `$CC("打开百度", open("https://baidu.com"))`)
// 输出:
//   - display: 候选显示文本
//   - actions: 选中触发的已解析动作列表 (含 Effect/Text 两种 Kind, 见 cmdbar.ResolvedAction)
//   - modifiers: options bag + marker syntax sugar 合并后的 modifier map
//     (含 prefix/expand/nav/async/scope 等键; 详见
//     docs/design/2026-05-16-cmdbar-followup.md §3.2)。candidate 进一步透传到
//     dict 层的前缀过滤 (替代旧 IsExactOnly 字符串扫描)。
//   - ok: 该 value 是否真的被 cmdbar 处理 (false 时调用方退回旧模板路径)
//   - err: 解析或求值失败 (调用方应记 WARN 后退化为字面量)
type CmdbarPhraseHook func(value string) (display string, actions []cmdbar.ResolvedAction, modifiers map[string]any, ok bool, err error)

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
	// GroupRawText 该 entry 所属 group 的原 PhraseRecord.Text (含 $AA/$SS marker)。
	// 空 = 该 entry 不属于任何 group (普通短语)。同 code 多 group 时, staticPhrases
	// 和 dynamicPhrases 把所有 group 的成员 append 在同一 slice 里, 用 GroupRawText
	// 反查归属 group, 让 collapse 能按 group 区分 nav。详见 docs/design/candidate-actions.md §5。
	GroupRawText string
}

// PhraseGroupKind 区分数组短语的元素粒度。
//   - PhraseGroupKindAA: $AA marker, 每元素是一个 rune (单字符候选)
//   - PhraseGroupKindSS: $SS marker, 每元素是一个字符串或嵌入的 $CC 命令
//
// 详见 docs/design/2026-05-16-cmdbar-followup.md §4.2 / §4.3。
type PhraseGroupKind string

const (
	PhraseGroupKindAA PhraseGroupKind = "aa"
	PhraseGroupKindSS PhraseGroupKind = "ss"
)

// PhraseGroup 数组类型短语组的元数据（texts 字段的条目）。
// Weight / Position 的语义与 PhraseEntry 一致 (参见其 doc); 字符组内
// 各元素共享 Weight, NaturalOrder 由展开位置决定。
//
// Kind 决定 SearchCommand 精确码命中后的展开路径:
//   - aa: 用 staticPhrases[code] 的字符级 entry 展开
//   - ss: 用 dynamicPhrases[code] 的原 marker text + ArrayHook 运行时展开
//     (元素可含嵌入 $CC, 需要 eval context)
type PhraseGroup struct {
	Code     string          // 完整编码（如 "zzbd"）
	Name     string          // 显示名称（如 "标点符号"）
	Texts    string          // 原始字符列表 (仅 aa 类型, ss 为空)
	Kind     PhraseGroupKind // 元素粒度: "aa" 或 "ss"
	Weight   int             // 排序权重 (0~10000)
	Position int             // 同 code 内手动调整后的顺序 (与 PhraseEntry.Position 一致语义)
	IsSystem bool            // 是否来自系统短语
	Disabled bool            // 是否被禁用
	// RawText group 原始 PhraseRecord.Text (含 $AA/$SS marker), nav/member 的
	// stable id 模板, Shadow pin / DisablePhrase 按 (code, RawText) 唯一定位。
	RawText string
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
		phraseGroups:       make(map[string][]PhraseGroup),
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

// PhraseCandidateID 为短语候选生成稳定 id (deterministic)。template 通常是
// PhraseEntry.Text (静态/动态短语模板) 或展开后的元素 (单字符/单元素),
// 详见 Candidate.ID 字段说明。
func PhraseCandidateID(code, template string) string {
	return "phrase:" + code + ":" + template
}

// Search 精确查询静态短语（不含变量的短语）。
//
// staticPhrases 同时承载两类 entry:
//   - 普通静态短语 (text=字面文本): IsCommand 由调用方按需标记, IsGroupMember=false
//   - $AA 字符组**展开后**的字符级 entry (text=单字符): IsCommand=true,
//     IsGroupMember=true — 因为 staticPhrases[code] 同时被 SearchCommand 路径
//     (情况 2b) 用于字符组精确匹配, 字段必须一致。
//
// 判断依据: code 在 pl.phraseGroups 里登记为 PhraseGroupKindAA → 字符组路径。
func (pl *PhraseLayer) Search(code string, limit int) []candidate.Candidate {
	pl.mu.RLock()
	defer pl.mu.RUnlock()

	code = strings.ToLower(code)
	entries, ok := pl.staticPhrases[code]
	if !ok {
		return nil
	}

	// 同 code 可能有多个 $AA group, 用 GroupRawText 反查归属 group。
	// 用 map 缓存避免每条 entry 都线性扫一次 groups slice。
	groups := pl.phraseGroups[code]
	groupByRaw := make(map[string]*PhraseGroup, len(groups))
	for i := range groups {
		if groups[i].Kind == PhraseGroupKindAA {
			groupByRaw[groups[i].RawText] = &groups[i]
		}
	}

	results := make([]candidate.Candidate, 0, len(entries))
	positions := make([]int, 0, len(entries))
	for _, e := range entries {
		cand := candidate.Candidate{
			Text:           e.Text,
			Code:           code,
			Weight:         resolvePhraseWeight(e.Weight),
			IsPhrase:       true, // 短语永远保留，但不计入 hasCommon 避免污染同编码码表字过滤
			PhraseTemplate: e.Text,
			ID:             PhraseCandidateID(code, e.Text),
		}
		// entry 属于某 AA group 时, 反查 group 取名字 + RawText 填到 candidate 上。
		// 让 coordinator collapse 路径能按 GroupTemplate 区分多 group。
		// TODO: 未来支持组内成员原地编辑 (允许在 IME 内改 chars 数组顺序)
		if g, ok := groupByRaw[e.GroupRawText]; ok {
			cand.IsCommand = true
			cand.IsGroupMember = true
			cand.GroupCode = code
			cand.GroupName = g.Name
			cand.GroupTemplate = g.RawText
		}
		results = append(results, cand)
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
//
// 锁策略 (double-checked locking, 2026-05-17 升级):
//   - 1st pass: 用 RLock 读 cmdCache, 命中直接返回, 让多个并发查询共享读路径不阻塞;
//   - 2nd pass: 缓存未命中, 升级到 Lock 执行展开 + 写 cache, 入锁后做 double-check
//     (避免 R→W 之间另一 goroutine 已写入, 重复展开);
//   - 内部辅助 (expandSSGroup/expandDynamicEntry) 注释要求"必须持有 pl.mu (Lock)"
//     的语义在此处仍然满足: 我们在 W-Lock 阶段调用它们。
func (pl *PhraseLayer) SearchCommand(code string, limit int) []candidate.Candidate {
	code = strings.ToLower(code)

	// 1st pass: R-Lock 读 cache, 让并发查询共享。
	pl.mu.RLock()
	if cached, hit := pl.cmdCache[code]; hit {
		pl.mu.RUnlock()
		if limit > 0 && len(cached) > limit {
			return cached[:limit]
		}
		return cached
	}
	pl.mu.RUnlock()

	// 2nd pass: W-Lock 计算并写 cache。
	pl.mu.Lock()
	defer pl.mu.Unlock()

	// double-check: R→W 间隙另一 goroutine 可能已经写入。
	if cached, hit := pl.cmdCache[code]; hit {
		if limit > 0 && len(cached) > limit {
			return cached[:limit]
		}
		return cached
	}

	// ── 情况 1：动态短语精确匹配（含 $ 变量, 或 $CC marker; 不含 $SS） ──
	// $SS 短语虽然也存在于 dynamicPhrases 中 (保留原 marker text), 但其展开
	// 走情况 2 的 ss 分支 (通过 cmdbarArrayHook), 所以这里跳过。
	if entries, ok := pl.dynamicPhrases[code]; ok {
		results := make([]candidate.Candidate, 0, len(entries))
		positions := make([]int, 0, len(entries))
		for _, e := range entries {
			if HasSSMarker(e.Text) {
				continue
			}
			cand := pl.expandDynamicEntry(code, e)
			results = append(results, cand)
			positions = append(positions, e.Position)
		}
		if len(results) > 0 {
			sortPhraseCandidates(results, positions)
			pl.cmdCache[code] = results
			if limit > 0 && len(results) > limit {
				return results[:limit]
			}
			return results
		}
		// 全部都是 $SS entry: 落到情况 2 由 ss 分支处理。
	}

	// ── 情况 2：字符组精确匹配 → 多 group 时返回所有成员让 coordinator collapse ──
	// 同 code 多 group ($AA/$SS 任意组合):
	//   - 全部成员候选 append 在一起返回, 每条 candidate 带自己的 GroupTemplate;
	//   - coordinator collapseGroupMembersIfMixed 自动按 GroupTemplate 区分各 group,
	//     生成多 nav (单 group 则直接展示成员)。
	// 单 group 行为不变 (直接展开成员)。
	groupsForCode := pl.phraseGroups[code]
	if len(groupsForCode) > 0 {
		var results []candidate.Candidate
		for _, group := range groupsForCode {
			if group.Disabled {
				continue
			}
			switch group.Kind {
			case PhraseGroupKindSS:
				// $SS: 通过 ArrayHook 运行时展开该 group
				results = append(results, pl.expandSSGroupSingle(group)...)
			case PhraseGroupKindAA:
				// $AA: 从 staticPhrases[code] 中按 GroupRawText 过滤出该 group 的字符
				// TODO: 未来支持组内成员原地编辑 (允许在 IME 内改 chars 数组顺序)
				groupWeight := resolvePhraseWeight(group.Weight)
				idx := 0
				for _, e := range pl.staticPhrases[code] {
					if e.GroupRawText != group.RawText {
						continue
					}
					results = append(results, candidate.Candidate{
						Text:           e.Text,
						Code:           code,
						Weight:         groupWeight,
						NaturalOrder:   idx,
						IsCommand:      true,
						IsPhrase:       true,
						IsGroupMember:  true,
						GroupCode:      code,
						GroupName:      group.Name,
						GroupTemplate:  group.RawText,
						PhraseTemplate: e.Text,
						ID:             PhraseCandidateID(code, e.Text),
					})
					idx++
				}
			}
		}
		if len(results) > 0 {
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
	}

	// ── 情况 3：字符组前缀匹配（如 zz → 返回 zzbd/zzsz... 导航候选） ──
	// 前缀长度 < 2 时不触发导航，避免单字符输入（如 "z"）引入噪音候选。
	if len(code) < 2 {
		return nil
	}
	var navResults []candidate.Candidate
	for groupCode, groupSlice := range pl.phraseGroups {
		if groupCode == code || !strings.HasPrefix(groupCode, code) {
			continue
		}
		// 同 code 下每个 group 各出 1 个 nav
		for _, group := range groupSlice {
			if group.Disabled {
				continue
			}
			displayName := group.Name
			if displayName == "" {
				displayName = groupCode
			}
			// nav id = phrase:<groupCode>:<group.RawText>, 详见 PhraseGroup.RawText 注释。
			navResults = append(navResults, candidate.Candidate{
				Text:           displayName,
				Code:           groupCode,
				Weight:         resolvePhraseWeight(group.Weight),
				Comment:        groupCode[len(code):],
				IsPhrase:       true,
				IsGroup:        true,
				GroupCode:      groupCode,
				GroupName:      displayName,
				GroupTemplate:  group.RawText,
				PhraseTemplate: group.RawText,
				ID:             PhraseCandidateID(groupCode, group.RawText),
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

	// 1. phraseGroups: 每个 group 出 1 个 nav 候选 (同 code 多 group 多 nav)。
	//    id 见 PhraseGroup.RawText 注释。
	for code, groupSlice := range pl.phraseGroups {
		if code == prefix || !strings.HasPrefix(code, prefix) {
			continue
		}
		for _, group := range groupSlice {
			if group.Disabled {
				continue
			}
			displayName := group.Name
			if displayName == "" {
				displayName = code
			}
			results = append(results, candidate.Candidate{
				Text:           displayName,
				Code:           code,
				Weight:         resolvePhraseWeight(group.Weight),
				Comment:        code[len(prefix):], // 显示编码后缀（如 zz→zzbd 显示 "bd"）
				IsPhrase:       true,
				IsGroup:        true,
				GroupCode:      code,
				GroupName:      displayName,
				GroupTemplate:  group.RawText,
				PhraseTemplate: group.RawText,
				ID:             PhraseCandidateID(code, group.RawText),
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
					Text:           e.Text,
					Code:           code,
					Weight:         resolvePhraseWeight(e.Weight),
					IsPhrase:       true,
					PhraseTemplate: e.Text,
					ID:             PhraseCandidateID(code, e.Text),
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
	pl.phraseGroups = make(map[string][]PhraseGroup)
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

		// 2026-05-16 简化: 不再 switch on rec.Type, 完全由 rec.Text 推断分类。
		// store 已删 Type/Texts/Name 字段, text 是单一信任源。
		switch {
		case HasSSMarker(rec.Text):
			// $SS 字符串数组: 元素 (含嵌入 $CC) 运行时通过 cmdbarArrayHook 展开
			ssName, ok := ParseSSGroupName(rec.Text)
			if !ok {
				ssName = code // 静态扫描失败时用 code 兜底, 运行时 hook 再判断
			}
			pg := PhraseGroup{
				Code:     code,
				Name:     ssName,
				Kind:     PhraseGroupKindSS,
				Weight:   rec.Weight,
				Position: position,
				IsSystem: rec.IsSystem,
				RawText:  rec.Text,
			}
			pl.phraseGroups[code] = append(pl.phraseGroups[code], pg)
			entry := PhraseEntry{
				Text:         rec.Text,
				Weight:       rec.Weight,
				Position:     position,
				IsSystem:     rec.IsSystem,
				GroupRawText: rec.Text, // 反查归属 group 用
			}
			pl.dynamicPhrases[code] = append(pl.dynamicPhrases[code], entry)

		case isAAMarker(rec.Text):
			// $AA 字符组: 解析 marker 得到 name + chars, 静态展开为字符级 entry
			name, chars, _ := ParseAAMarker(rec.Text)
			pg := PhraseGroup{
				Code:     code,
				Name:     name,
				Texts:    chars,
				Kind:     PhraseGroupKindAA,
				Weight:   rec.Weight,
				Position: position,
				IsSystem: rec.IsSystem,
				RawText:  rec.Text,
			}
			pl.phraseGroups[code] = append(pl.phraseGroups[code], pg)
			runes := []rune(chars)
			for idx, r := range runes {
				arrEntry := PhraseEntry{
					Text:         string(r),
					Weight:       rec.Weight,
					Position:     position + idx,
					IsSystem:     rec.IsSystem,
					GroupRawText: rec.Text, // 反查归属 group 用
				}
				pl.staticPhrases[code] = append(pl.staticPhrases[code], arrEntry)
			}

		case HasVariable(rec.Text):
			// 含 $ 的动态短语 ($CC marker / $Y/$M/$D 等模板变量)
			entry := PhraseEntry{
				Text:     rec.Text,
				Weight:   rec.Weight,
				Position: position,
				IsSystem: rec.IsSystem,
			}
			pl.dynamicPhrases[code] = append(pl.dynamicPhrases[code], entry)

		default:
			// 普通字面量短语
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

// isAAMarker 判定 text 是否为合法 $AA marker (能完整解析)。
// ParseAAMarker 已存在但返回三元组 (name, chars, ok); 这里包一个 bool 版本
// 用于 switch case 条件分支保持代码紧凑。
func isAAMarker(text string) bool {
	_, _, ok := ParseAAMarker(text)
	return ok
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

// (detectPhraseType 已删除, 2026-05-16: store 不再保存 type 字段, 分类
//  完全由 PhraseLayer.LoadFromStore 解析 Text 推断。)

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

// expandSSGroup 把 $SS 字符串数组 group 在精确码命中时展开成 N 个 candidate。
// 调用方必须持有 pl.mu (Lock 形态, 因为可能修改 cmdCache)。
//
// 实现细节:
//   - 同一 code 可能有多条 $SS entry (yaml 里同 code 多次声明), 每条 entry 用
//     自己的 weight (resolvePhraseWeight(entry.Weight)) 展开, 不共用 phraseGroup
//     的元数据 weight。最终 N 个候选用 candidate.Better 排序: 主键 entry weight
//     desc, 同 weight 内按 NaturalOrder asc (元素在 marker 内的顺序)。
//   - cmdbarArrayHook 为 nil 或返回 err/ok=false 时该 entry 不产生候选, 但其他
//     entry 继续展开。完全失败时返回空 slice (调用方上层会回退到模板/字面量)。
//   - PhraseTemplate 字段保留原 $SS marker text, 给 candidate adjustments
//     (右键 pin 等) 提供原始 entry 定位。
func (pl *PhraseLayer) expandSSGroupSingle(group PhraseGroup) []candidate.Candidate {
	if pl.cmdbarArrayHook == nil {
		slog.Warn("phrase: $SS group hit but no cmdbarArrayHook installed; group has 0 candidates",
			"code", group.Code)
		return nil
	}
	hookName, elements, modifiers, ok, err := pl.cmdbarArrayHook(group.RawText)
	if err != nil {
		slog.Warn("phrase: $SS array hook returned error, skipping entry",
			"code", group.Code, "valueLen", len(group.RawText))
		return nil
	}
	if !ok {
		return nil
	}
	// ArrayHook 返回的 name 与 PhraseGroup.Name (LoadFromStore 用 ParseSSGroupName 注入)
	// 等价, 静态字段更稳定。空时回落 hookName。
	effectiveName := group.Name
	if effectiveName == "" {
		effectiveName = hookName
	}
	entryWeight := resolvePhraseWeight(group.Weight)
	results := make([]candidate.Candidate, 0, len(elements))
	for i, elem := range elements {
		// $SS 每个元素 (string lit 或嵌入 $CC) 用其 raw display 作为 id 后缀,
		// 保证按元素粒度 pin / shadow。同 group 多元素的 id 互不冲突。
		// IsGroupMember=true: 顺序由 $SS marker 内的参数列表决定, 走"编辑短语"
		// 修改 yaml; 右键菜单 pin/delete/前移/置顶/恢复默认 全 disable。
		// TODO: 未来支持组内成员原地编辑 (允许在 IME 内改 $SS 数组顺序)
		cand := candidate.Candidate{
			Text:           elem.Display,
			Code:           group.Code,
			Weight:         entryWeight,
			NaturalOrder:   i,
			IsCommand:      true,
			IsPhrase:       true,
			IsGroupMember:  true,
			GroupCode:      group.Code,
			GroupName:      effectiveName,
			GroupTemplate:  group.RawText,
			PhraseTemplate: group.RawText,
			ID:             PhraseCandidateID(group.Code, elem.Display),
			DisplayText:    elem.Display,
			Actions:        elem.Actions,
			Modifiers:      modifiers, // group-level modifiers
		}
		results = append(results, cand)
	}
	return results
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
		ID:             PhraseCandidateID(code, e.Text),
	}
	if res.IsCommand {
		out.DisplayText = res.DisplayText
		out.Actions = res.Actions
		out.Modifiers = res.Modifiers
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

// ===== 短语位置调整已统一到 Shadow (R2, 2026-05-17) =====
//
// 旧 MovePhraseUp/MovePhraseDown/MovePhraseToTop/HasPhraseOverride/
// ResetPhraseOverride 已删除, 改由 coordinator 走 dm.PinWord / dm.DeleteWord
// 等 Shadow API, 短语候选 ID = PhraseCandidateID(code, template) 在
// ApplyShadowPins 内匹配 (动态短语跨日子稳定生效)。
//
// 这里不再保留 position swap / yaml fallback 路径 — 同一套机制 (Shadow)
// 覆盖码表 / 拼音 / 短语 / 命令直通车候选, 减少分支与状态不一致风险。

// clearCmdCache 清除指定编码的命令缓存
func (pl *PhraseLayer) clearCmdCache(code string) {
	delete(pl.cmdCache, code)
}
