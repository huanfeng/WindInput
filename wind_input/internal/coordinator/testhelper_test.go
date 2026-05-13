// testhelper_test.go — Coordinator routing 测试脚手架（首版骨架）
//
// 设计原则
//
// Coordinator 在生产环境通过 NewCoordinator 构建, 含磁盘 IO / 主题加载 /
// hotkey 编译 / tooltip 服务等大量副作用. 测试里我们绕开 NewCoordinator,
// 直接 &Coordinator{...} 设置最小字段, 用 functional option 按需注入.
//
// 现状（首版）
//
// 当前骨架仅覆盖"不触达 engineMgr / uiManager / dictManager"的最浅路由
// （如英文模式直通）. 想要测的 z 键混合决策、临时拼音回退等路径都依赖
// 真实 engine.Manager + 拼音 schema, 后续提交会增量补 fixture.
//
// 想加新选项时记得检查 HandleKeyEvent 早期分支对 c.config 字段的依赖,
// 否则 nil deref 会比较隐蔽.
package coordinator

import (
	"log/slog"
	"strings"
	"sync"
	"testing"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/schema"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/pkg/config"
)

// testCoordinator 封装一个最小化构造的 *Coordinator + *testing.T,
// 提供 pressKey / 断言便利方法.
type testCoordinator struct {
	*Coordinator
	t *testing.T
}

// testOption 是 newTestCoordinator 的函数式选项.
type testOption func(*Coordinator)

// newTestCoordinator 构造一个能在英文/中文直通路径上不 panic 的 Coordinator.
//
// 默认值:
//   - logger: 丢弃
//   - config: 全零值（无 hotkeys, 无 auto-pair, 无 quick input/temp pinyin）
//   - chineseMode: true
//   - candidatesPerPage: 9
//   - mu / cfgMu: 可用
//   - punctConverter / inputHistory: 已初始化
//
// 任何依赖 engineMgr / uiManager 的路径都会在调用方 nil deref;
// 待对应 fixture 补全后再加 with* 选项.
func newTestCoordinator(t *testing.T, opts ...testOption) *testCoordinator {
	t.Helper()
	c := &Coordinator{
		logger:            slog.New(slog.DiscardHandler),
		config:            &config.Config{},
		cfgMu:             new(sync.RWMutex),
		chineseMode:       true,
		candidatesPerPage: 9,
		currentPage:       1,
		totalPages:        1,
		punctConverter:    transform.NewPunctuationConverter(),
		inputHistory:      NewInputHistory(20),
	}
	for _, opt := range opts {
		opt(c)
	}
	return &testCoordinator{Coordinator: c, t: t}
}

// withChineseMode 覆盖默认的中文模式标志.
func withChineseMode(chinese bool) testOption {
	return func(c *Coordinator) { c.chineseMode = chinese }
}

// withConfig 整体替换 config（便于测试 hotkeys / quick input / temp pinyin 配置）.
func withConfig(cfg *config.Config) testOption {
	return func(c *Coordinator) { c.config = cfg }
}

// ── engine.Manager fixture（最小化） ─────────────────────────────────────────
//
// withEngineMgr 给 testCoordinator 装上一个真实的 engine.Manager, 内部仅挂载
// 用 stubDictLayer 模拟的码表/短语层, 不调用 EnsurePinyinLoaded 之类的重路径.
// 当前仅够 Manager.HasPrefix / GetDictManager 这类纯查询使用; 想要触发
// EnsurePinyinLoaded / SwitchSchema 之类的真实引擎构建仍需进一步 fixture.

// stubDictLayer 是 dict.DictLayer 的最小化测试实现, 用 map 模拟码表 / 短语层.
// 仅实现 Search / SearchPrefix, 不支持权重 / 排序.
type stubDictLayer struct {
	name    string
	layerTy dict.LayerType
	entries map[string][]string // code -> texts
}

func (s *stubDictLayer) Name() string         { return s.name }
func (s *stubDictLayer) Type() dict.LayerType { return s.layerTy }

func (s *stubDictLayer) Search(code string, limit int) []candidate.Candidate {
	code = strings.ToLower(code)
	texts, ok := s.entries[code]
	if !ok {
		return nil
	}
	return makeCandidates(code, texts, limit)
}

func (s *stubDictLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	var results []candidate.Candidate
	for code, texts := range s.entries {
		if !strings.HasPrefix(code, prefix) {
			continue
		}
		results = append(results, makeCandidates(code, texts, 0)...)
		if limit > 0 && len(results) >= limit {
			return results[:limit]
		}
	}
	return results
}

func makeCandidates(code string, texts []string, limit int) []candidate.Candidate {
	if limit > 0 && len(texts) > limit {
		texts = texts[:limit]
	}
	out := make([]candidate.Candidate, 0, len(texts))
	for _, t := range texts {
		out = append(out, candidate.Candidate{Text: t, Code: code, Weight: 1})
	}
	return out
}

// engineFixture 聚合 testEngine 构造期间需要的可变状态, 通过 engineOption 修改.
type engineFixture struct {
	codetableEntries map[string][]string
}

type engineOption func(*engineFixture)

// withCodetableEntry 在 stub 码表层里塞一条 code -> text 映射.
// 多次调用会累积. 同一 code 可对应多个 text.
func withCodetableEntry(code, text string) engineOption {
	return func(f *engineFixture) {
		if f.codetableEntries == nil {
			f.codetableEntries = make(map[string][]string)
		}
		f.codetableEntries[code] = append(f.codetableEntries[code], text)
	}
}

// withEngineMgr 装上最小 engine.Manager (含一个 stub 系统词库层).
// 不构造任何真实引擎实例, 调用 SwitchSchema / EnsurePinyinLoaded 等会失败.
func withEngineMgr(opts ...engineOption) testOption {
	return func(c *Coordinator) {
		fx := &engineFixture{}
		for _, opt := range opts {
			opt(fx)
		}
		logger := slog.New(slog.DiscardHandler)
		dm := dict.NewDictManager("", "", logger)
		layer := &stubDictLayer{
			name:    "stub-codetable",
			layerTy: dict.LayerTypeSystem,
			entries: fx.codetableEntries,
		}
		dm.RegisterSystemLayer(layer.Name(), layer)

		em := engine.NewManager(logger)
		em.SetDictManager(dm)

		c.engineMgr = em
	}
}

// withZHybridSchema 让 testCoordinator 进入"z 临时拼音触发 + 可选 Z 键重复"
// 状态. 自动:
//  1. 如未挂载 engineMgr, 用 withEngineMgr 默认参数补一个;
//  2. 注入一个码表 schema, TempPinyin 启用, ZKeyRepeat = zRepeat;
//  3. 把 "z" 加入 config.Input.TempPinyin.TriggerKeys.
//
// 不构造任何拼音引擎实例; EnsurePinyinLoaded / ConvertWithPinyin 仍会失败,
// 仅适合测试 isZKeyHybridMode / isTempPinyinZTrigger / HasPrefix 等纯查询.
func withZHybridSchema(zRepeat bool) testOption {
	return func(c *Coordinator) {
		if c.engineMgr == nil {
			withEngineMgr()(c)
		}
		// 用局部变量取地址, 避免外层 zRepeat 形参被 schema 持有指针引用
		zRepeatVal := zRepeat
		sm := schema.NewSchemaManager("", "", slog.New(slog.DiscardHandler))
		const id = "test-codetable"
		sm.InjectSchemaForTest(id, &schema.Schema{
			Schema: schema.SchemaInfo{ID: id, Name: "Test Codetable"},
			Engine: schema.EngineSpec{
				Type: schema.EngineTypeCodeTable,
				CodeTable: &schema.CodeTableSpec{
					TempPinyin: &schema.TempPinyinSpec{Enabled: true},
					ZKeyRepeat: &zRepeatVal,
				},
			},
		})
		sm.SetActiveForTest(id)
		c.engineMgr.SetSchemaManager(sm)
		c.engineMgr.SetCurrentIDForTest(id)

		if c.config == nil {
			c.config = &config.Config{}
		}
		c.config.Input.TempPinyin.TriggerKeys = append(
			c.config.Input.TempPinyin.TriggerKeys, "z",
		)
	}
}

// pressKey 用最常见参数构造 KeyEventData 并调用 HandleKeyEvent.
// key 应为单字符（如 "a"）或 keys.Key* 字符串.
// 对单字符字母, KeyCode 自动取 ASCII 大写值（与 Win32 VK_A..VK_Z 对齐）.
func (h *testCoordinator) pressKey(key string) *bridge.KeyEventResult {
	h.t.Helper()
	kc := 0
	if len(key) == 1 {
		ch := key[0]
		switch {
		case ch >= 'a' && ch <= 'z':
			kc = int(ch - 'a' + 'A')
		case ch >= 'A' && ch <= 'Z':
			kc = int(ch)
		default:
			kc = int(ch)
		}
	}
	return h.HandleKeyEvent(bridge.KeyEventData{
		Key:     key,
		KeyCode: kc,
	})
}
