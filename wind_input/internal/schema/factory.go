package schema

import (
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/binformat"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
	"github.com/huanfeng/wind_input/internal/store"
)

// ErrAssetBuilding 表示方案所需的资源（如拼音 wdat 缓存）正在后台生成。
// 区别于"真正的加载失败"——上层可借此选择"显示准备中并等待"而非
// "切换到 fallback 方案"。所有 wrapping 都通过 fmt.Errorf("%w: ...", ErrAssetBuilding)
// 完成，调用方用 errors.Is(err, schema.ErrAssetBuilding) 判定。
var ErrAssetBuilding = errors.New("schema asset is being built")

// pinyinWdatBuildMu 保护以下三个共享状态：
//   - pinyinWdatBuilding：确保同一时刻最多一个后台 wdat 构建在运行
//   - pinyinWdatReadyCallbacks：等待构建完成的回调列表
var (
	pinyinWdatBuildMu        sync.Mutex
	pinyinWdatBuilding       bool
	pinyinWdatReadyCallbacks []func()
)

// IsPinyinWdatBuilding 报告拼音 wdat 是否正在后台生成。
func IsPinyinWdatBuilding() bool {
	pinyinWdatBuildMu.Lock()
	defer pinyinWdatBuildMu.Unlock()
	return pinyinWdatBuilding
}

// OnPinyinWdatReady 注册"拼音 wdat 后台生成完成"回调。
//   - 当前不在构建中：cb 同步立即调用（视作已就绪）；
//   - 正在构建：cb 加入队列，构建完成时（无论成功或失败）按注册顺序触发。
//
// 回调在构建 goroutine 中执行，调用方需自行处理与主流程的同步。
// 这一语义让"先查询状态再注册回调"的常见 race 自然消失：
// 调用方只需 OnPinyinWdatReady(reload)，无论当前状态如何都能拿到一次唤醒。
func OnPinyinWdatReady(cb func()) {
	if cb == nil {
		return
	}
	pinyinWdatBuildMu.Lock()
	if !pinyinWdatBuilding {
		pinyinWdatBuildMu.Unlock()
		cb()
		return
	}
	pinyinWdatReadyCallbacks = append(pinyinWdatReadyCallbacks, cb)
	pinyinWdatBuildMu.Unlock()
}

// startPinyinWdatBuildAsync 以后台协程异步构建拼音 wdat 缓存，若已在构建中则直接返回。
func startPinyinWdatBuildAsync(dictPath, wdatCachePath string, logger *slog.Logger, normalizer *dict.WeightNormalizer) {
	pinyinWdatBuildMu.Lock()
	if pinyinWdatBuilding {
		pinyinWdatBuildMu.Unlock()
		return
	}
	pinyinWdatBuilding = true
	pinyinWdatBuildMu.Unlock()
	go func() {
		defer func() {
			if r := recover(); r != nil {
				logger.Error("拼音 wdat 后台生成 goroutine 崩溃",
					"panic", r,
					"stack", string(debug.Stack()))
			}
			pinyinWdatBuildMu.Lock()
			pinyinWdatBuilding = false
			cbs := pinyinWdatReadyCallbacks
			pinyinWdatReadyCallbacks = nil
			pinyinWdatBuildMu.Unlock()
			for _, cb := range cbs {
				cb()
			}
		}()
		if err := dictcache.ConvertPinyinToWdat(dictPath, wdatCachePath, logger, normalizer); err != nil {
			logger.Warn("后台生成拼音 wdat 失败", "err", err)
		}
	}()
}

// EngineBundle 引擎创建结果（包含引擎实例和相关资源）
type EngineBundle struct {
	SchemaID string
	Engine   interface{} // *pinyin.Engine 或 *codetable.Engine 或 *mixed.Engine
	// SystemLayer 工厂在构建期间注册到 DictManager 的系统词库层。
	// 由 EngineManager 缓存到 systemLayers，方案切换时按缓存重新注册，
	// 避免依赖共享 CompositeDict 的当前状态（防止与并发切换发生竞态）。
	SystemLayer dict.DictLayer
	// ExtraLayers 该方案加载的所有附加码表层（codetable-extra-<schemaID>__<dictID>）。
	// 由 EngineManager 缓存到 systemExtras，方案切换时按缓存清理上一个方案的 extras，
	// 重新注册当前方案的 extras，确保不同方案的扩展词库相互隔离。
	ExtraLayers []dict.DictLayer
}

// SchemaResolver 方案解析器，用于混输引擎查找被引用的方案
type SchemaResolver func(schemaID string) *Schema

// EngineCreateOptions 引擎创建选项
type EngineCreateOptions struct {
	SkipReverseLookup  bool // 跳过反查码表加载（临时拼音模式下由 Manager 提供反向索引）
	UseIndependentDict bool // 使用独立的 CompositeDict，不注册到主 DictManager（临时拼音引擎避免污染混输主词库）
}

// CreateEngineFromSchema 根据 Schema 创建引擎实例并加载词库
func CreateEngineFromSchema(s *Schema, exeDir, dataDir string, dm *dict.DictManager, logger *slog.Logger, resolver SchemaResolver, opts ...EngineCreateOptions) (*EngineBundle, error) {
	var opt EngineCreateOptions
	if len(opts) > 0 {
		opt = opts[0]
	}
	switch s.Engine.Type {
	case EngineTypeCodeTable:
		return createCodeTableEngine(s, exeDir, dataDir, dm, logger)
	case EngineTypePinyin:
		return createPinyinEngine(s, exeDir, dataDir, dm, logger, opt)
	case EngineTypeMixed:
		return createMixedEngine(s, exeDir, dataDir, dm, logger, resolver)
	default:
		return nil, fmt.Errorf("不支持的引擎类型: %s", s.Engine.Type)
	}
}

// createCodeTableEngine 创建码表引擎（五笔等）
func createCodeTableEngine(s *Schema, exeDir, dataDir string, dm *dict.DictManager, logger *slog.Logger) (*EngineBundle, error) {
	spec := s.Engine.CodeTable
	if spec == nil {
		spec = &CodeTableSpec{
			MaxCodeLength:     4,
			TopCodeCommit:     true,
			PunctCommit:       true,
			ShowCodeHint:      true,
			CandidateSortMode: "natural",
		}
	}

	dedupCandidates := true
	if spec.DedupCandidates != nil {
		dedupCandidates = *spec.DedupCandidates
	}
	skipSingleCharFreq := true // 默认值：单字不自动调频
	if spec.SkipSingleCharFreq != nil {
		skipSingleCharFreq = *spec.SkipSingleCharFreq
	}
	config := &codetable.Config{
		MaxCodeLength: spec.MaxCodeLength,
		AutoCommitAtFull: func() bool {
			if spec.AutoCommitAtFull != nil {
				return *spec.AutoCommitAtFull
			}
			return spec.AutoCommitUnique
		}(),
		MinAutoCommitLen: spec.AutoCommitMinLen, // 0 由 LoadCodeTable 末尾兜底
		AutoCommitBlockOnPinyin: func() bool {
			if spec.AutoCommitBlockOnPinyin != nil {
				return *spec.AutoCommitBlockOnPinyin
			}
			return true
		}(),
		ClearOnEmptyAt4:    spec.ClearOnEmptyMax,
		TopCodeCommit:      spec.TopCodeCommit,
		PunctCommit:        spec.PunctCommit,
		ShowCodeHint:       spec.ShowCodeHint,
		SingleCodeInput:    spec.SingleCodeInput,
		SingleCodeComplete: spec.SingleCodeComplete,
		FilterMode:         s.Engine.FilterMode,
		CandidateSortMode:  spec.CandidateSortMode,
		DedupCandidates:    dedupCandidates,
		SkipSingleCharFreq: skipSingleCharFreq,
		LoadMode:           spec.LoadMode,
		PrefixMode:         spec.PrefixMode,
		BucketLimit:        spec.BucketLimit,
		WeightMode:         spec.WeightMode,
		CharsetPreference:  spec.CharsetPreference,
	}
	if spec.ShortCodeFirst != nil {
		config.ShortCodeFirst = *spec.ShortCodeFirst
	}

	// ProtectTopN 从 FreqSpec 读取，传入引擎 Config
	if s.Learning.Freq != nil {
		config.ProtectTopN = s.Learning.Freq.ProtectTopN
	}

	engine := codetable.NewEngine(config, logger)

	// 加载词库（主词库 + 所有已启用的附加词库）
	// dm 在此处尚未完成注册，附加词库需等主码表注册完成后再注册 system layer，
	// 故此处只加载主码表；附加词库在 dm 注册块之后处理。
	mainDictSpec := s.GetDefaultDictSpec()
	if mainDictSpec == nil {
		return nil, fmt.Errorf("方案 %s: 没有 default:true 的主词库", s.Schema.ID)
	}
	{
		srcPath := resolvePath(exeDir, dataDir, mainDictSpec.Path)
		cacheKey := s.Schema.ID + "_" + mainDictSpec.ID
		var norm *dict.WeightNormalizer
		if mainDictSpec.WeightSpec != nil {
			norm = mainDictSpec.WeightSpec.NewWeightNormalizer()
		}
		if mainDictSpec.WeightAsOrder {
			config.WeightAsOrder = true
		}
		if err := loadCodetable(engine, srcPath, mainDictSpec.Type, cacheKey, logger, norm); err != nil {
			return nil, fmt.Errorf("加载主码表失败: %w", err)
		}
		logger.Info("主码表加载成功", "schemaID", s.Schema.ID, "dictID", mainDictSpec.ID, "entryCount", engine.GetEntryCount())
	}

	// 注册码表为 CompositeDict 的 system layer + 设置 DictManager
	var ctSystemLayer dict.DictLayer
	if dm != nil {
		codeTable := engine.GetCodeTable()
		if codeTable != nil {
			systemLayer := dict.NewCodeTableLayer("codetable-system", dict.LayerTypeSystem, codeTable)
			dm.RegisterSystemLayer("codetable-system", systemLayer)
			ctSystemLayer = systemLayer
		}
		engine.SetDictManager(dm)
		// 排序模式不再由 dm 持有；引擎每次调用 composite.Search 时通过 SearchOptions 显式传入。

		// 注入 FreqHandler（调频）
		if s.Learning.IsFreqEnabled() {
			freqProfile := s.Learning.GetFreqProfile()
			dm.SetFreqProfile(freqProfile)
			freqHandler := dict.NewFreqHandler(dm.GetStore(), s.DataSchemaID())
			engine.SetFreqHandler(freqHandler)
		}

		// 注入 LearningStrategy（造词）
		// 码表引擎：auto_learn 或 auto_phrase 启用时使用码表自动造词
		// 学习层按 schema 自身的 dataSchemaID 绑定，避免预加载时被绑到当前活跃方案的 bucket
		if s.Learning.IsAutoPhraseEnabled() || s.Learning.IsAutoLearnEnabled() {
			autoPhrase := NewCodeTableLearningStrategy(&s.Learning, logger)
			if ul := dm.GetOrCreateStoreUserLayer(s.DataSchemaID()); ul != nil {
				autoPhrase.SetUserLayer(ul)
			}
			if tl := dm.GetOrCreateStoreTempLayer(s.DataSchemaID()); tl != nil {
				autoPhrase.SetTempLayer(tl)
			}
			autoPhrase.SetSystemChecker(dm)
			// 编码计算器：使用编码规则 + 码表反向索引（惰性构建）
			encoder := s.Encoder
			if encoder != nil && len(encoder.Rules) > 0 && engine.GetCodeTable() != nil {
				calc := NewEncoderWordCodeCalc(encoder.Rules, engine.GetCodeTable())
				autoPhrase.SetWordCodeCalculator(calc)
			}
			engine.SetLearningStrategy(autoPhrase)
		} else {
			engine.SetLearningStrategy(&ManualLearning{})
		}
	}

	// 加载附加词库（非 default 且 enabled 的词库条目）
	var extraLayers []dict.DictLayer
	if dm != nil {
		for _, dictSpec := range s.Dicts {
			if dictSpec.Default || !dictSpec.IsEnabled() {
				continue
			}
			srcPath := resolvePath(exeDir, dataDir, dictSpec.Path)
			cacheKey := s.Schema.ID + "_" + dictSpec.ID
			layer, err := loadExtraCodetable(dm, s.Schema.ID, srcPath, dictSpec, cacheKey, logger)
			if err != nil {
				logger.Warn("附加词库加载失败，跳过", "dictID", dictSpec.ID, "error", err)
				continue
			}
			if layer != nil {
				extraLayers = append(extraLayers, layer)
			}
		}
	}

	// 后台预生成拼音 wdat/unigram
	go func() {
		defer func() {
			if r := recover(); r != nil {
				logger.Error("拼音预生成后台任务 goroutine 崩溃",
					"panic", r,
					"stack", string(debug.Stack()))
			}
		}()
		preGeneratePinyinWdb(s, exeDir, dataDir, logger)
	}()

	// GC 释放临时内存
	go func() {
		runtime.GC()
		debug.FreeOSMemory()
	}()

	return &EngineBundle{
		SchemaID:    s.Schema.ID,
		Engine:      engine,
		SystemLayer: ctSystemLayer,
		ExtraLayers: extraLayers,
	}, nil
}

// createPinyinEngine 创建拼音引擎
func createPinyinEngine(s *Schema, exeDir, dataDir string, dm *dict.DictManager, logger *slog.Logger, opt EngineCreateOptions) (*EngineBundle, error) {
	spec := s.Engine.Pinyin
	if spec == nil {
		spec = &PinyinSpec{
			Scheme:          PinyinSchemeFull,
			ShowCodeHint:    true,
			UseSmartCompose: true,
		}
	}

	config := &pinyin.Config{
		ShowCodeHint:    spec.ShowCodeHint,
		FilterMode:      s.Engine.FilterMode,
		UseSmartCompose: spec.UseSmartCompose,
		CandidateOrder:  spec.CandidateOrder,
	}

	// 模糊音配置
	if spec.Fuzzy != nil && spec.Fuzzy.Enabled {
		config.Fuzzy = &pinyin.FuzzyConfig{
			ZhZ:     spec.Fuzzy.ZhZ,
			ChC:     spec.Fuzzy.ChC,
			ShS:     spec.Fuzzy.ShS,
			NL:      spec.Fuzzy.NL,
			FH:      spec.Fuzzy.FH,
			RL:      spec.Fuzzy.RL,
			AnAng:   spec.Fuzzy.AnAng,
			EnEng:   spec.Fuzzy.EnEng,
			InIng:   spec.Fuzzy.InIng,
			IanIang: spec.Fuzzy.IanIang,
			UanUang: spec.Fuzzy.UanUang,
		}
	}

	// 加载拼音词库
	pinyinDict := dict.NewPinyinDict(logger)

	dictSpec := s.GetDefaultDictSpec()
	if dictSpec != nil {
		dictPath := resolvePath(exeDir, dataDir, dictSpec.Path)
		var norm *dict.WeightNormalizer
		if dictSpec.WeightSpec != nil {
			norm = dictSpec.WeightSpec.NewWeightNormalizer()
		}
		if err := loadPinyinDict(pinyinDict, dictPath, logger, norm); err != nil {
			return nil, fmt.Errorf("加载拼音词库失败: %w", err)
		}
	}

	// 构建 CompositeDict
	var compositeDict *dict.CompositeDict
	var pySystemLayer dict.DictLayer
	if dm != nil && !opt.UseIndependentDict {
		systemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
		dm.RegisterSystemLayer("pinyin-system", systemLayer)
		compositeDict = dm.GetCompositeDict()
		pySystemLayer = systemLayer
		logger.Info("拼音引擎使用 CompositeDict")
	} else {
		// 无 DictManager 或要求独立词库时创建独立 CompositeDict（避免污染混输主词库）
		compositeDict = dict.NewCompositeDict()
		systemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
		compositeDict.AddLayer(systemLayer)
	}

	engine := pinyin.NewEngineWithConfig(compositeDict, config, logger)

	// 配置双拼转换器
	if spec.Scheme == PinyinSchemeShuangpin && spec.Shuangpin != nil {
		spScheme := shuangpin.Get(spec.Shuangpin.Layout)
		if spScheme != nil {
			engine.SetShuangpinConverter(shuangpin.NewConverter(spScheme))
			logger.Info("双拼模式", "layout", spScheme.ID, "name", spScheme.Name)
		} else {
			logger.Warn("未知的双拼方案，回退到全拼", "layout", spec.Shuangpin.Layout)
		}
	}

	// 加载 Unigram 语言模型
	if s.Learning.UnigramPath != "" {
		unigramTxtPath := resolvePath(exeDir, dataDir, s.Learning.UnigramPath)
		if err := loadUnigramModel(engine, unigramTxtPath, logger); err != nil {
			logger.Warn("加载 Unigram 模型失败", "err", err)
		}
	}

	// 反查/编码提示已统一由 Manager.ApplyCodeHintsToCandidates 注入（数据来自主码表方案的反向索引），
	// 拼音引擎不再加载独立的 reverse_lookup 词典（避免重复构建 *_reverse.wdb）。
	// schema yaml 中遗留的 role: reverse_lookup 字典项会在加载阶段被忽略。
	_ = opt.SkipReverseLookup // 保留字段供临时拼音入口使用（其他位置仍读取它）

	// 设置 DictManager
	if dm != nil {
		engine.SetDictManager(dm)

		// 注入 FreqHandler（调频）
		if s.Learning.IsFreqEnabled() {
			freqProfile := s.Learning.GetFreqProfile()
			dm.SetFreqProfile(freqProfile)
			freqHandler := dict.NewFreqHandler(dm.GetStore(), s.DataSchemaID())
			engine.SetFreqHandler(freqHandler)
		}

		// 注入 LearningStrategy（造词）
		// 按 schema 自身 dataSchemaID 绑定层，避免预加载时被绑到当前活跃方案
		userLayer := dm.GetOrCreateStoreUserLayer(s.DataSchemaID())
		learningStrategy := NewLearningStrategy(&s.Learning, userLayer)
		if al, ok := learningStrategy.(*AutoLearning); ok {
			if tl := dm.GetOrCreateStoreTempLayer(s.DataSchemaID()); tl != nil {
				al.SetTempLayer(tl)
			}
			al.SetSystemChecker(dm)
		}
		engine.SetLearningStrategy(learningStrategy)
	}

	// 加载用户词频（调频或造词启用时加载 Unigram 用户词频）
	if s.Learning.IsFreqEnabled() || s.Learning.IsAutoLearnEnabled() {
		if dm != nil && dm.GetStore() != nil {
			loadPinyinUserFreqs(engine, dm.GetStore(), s.DataSchemaID(), logger)
		}

	}

	return &EngineBundle{
		SchemaID:    s.Schema.ID,
		Engine:      engine,
		SystemLayer: pySystemLayer,
	}, nil
}

// --- 词库加载辅助函数（从 manager_init.go 迁移） ---

// loadPinyinDict 加载拼音词库（统一走 DAT/wdat 双数组 Trie，旧版 wdb 已废弃）。
func loadPinyinDict(pinyinDict *dict.PinyinDict, dictPath string, logger *slog.Logger, normalizer *dict.WeightNormalizer) error {
	dictDir := filepath.Dir(dictPath)
	srcPaths := dictcache.RimePinyinSourcePaths(dictPath)

	wdatInDir := filepath.Join(dictDir, "pinyin.wdat")
	if !dictcache.NeedsRegenerate(srcPaths, wdatInDir) {
		if err := pinyinDict.LoadDAT(wdatInDir); err == nil {
			logger.Info("拼音词库(预编译 wdat)加载成功", "entryCount", pinyinDict.EntryCount())
			return nil
		}
	}
	wdatCachePath := dictcache.WdatCachePath("pinyin")
	if dictcache.NeedsRegenerate(srcPaths, wdatCachePath) {
		// 缓存尚未就绪，异步构建，不阻塞当前调用方
		startPinyinWdatBuildAsync(dictPath, wdatCachePath, logger, normalizer)
		return fmt.Errorf("%w: 拼音 wdat 词库正在后台生成，请稍后切换到此方案", ErrAssetBuilding)
	}
	if err := pinyinDict.LoadDAT(wdatCachePath); err == nil {
		logger.Info("拼音词库(缓存 wdat)加载成功", "entryCount", pinyinDict.EntryCount())
		return nil
	}
	return fmt.Errorf("拼音 wdat 词库加载失败，缓存可能已损坏")
}

func loadUnigramModel(engine *pinyin.Engine, txtPath string, logger *slog.Logger) error {
	wdbPath := strings.TrimSuffix(txtPath, ".txt") + ".wdb"

	if _, err := os.Stat(wdbPath); err == nil {
		if !dictcache.NeedsRegenerate([]string{txtPath}, wdbPath) {
			bm, err := pinyin.NewBinaryUnigramModel(wdbPath)
			if err == nil {
				engine.SetUnigram(bm)
				logger.Info("Unigram 模型(预编译 wdb)加载成功", "size", bm.Size())
				return nil
			}
		}
	}

	wdbCachePath := dictcache.CachePath("unigram")
	if dictcache.NeedsRegenerate([]string{txtPath}, wdbCachePath) {
		if _, err := os.Stat(txtPath); err == nil {
			if err := dictcache.ConvertUnigramToWdb(txtPath, wdbCachePath, logger); err != nil {
				logger.Warn("转换 Unigram 到 wdb 失败", "err", err)
			}
		}
	}

	if _, err := os.Stat(wdbCachePath); err == nil {
		bm, err := pinyin.NewBinaryUnigramModel(wdbCachePath)
		if err == nil {
			engine.SetUnigram(bm)
			logger.Info("Unigram 模型(缓存 wdb)加载成功", "size", bm.Size())
			return nil
		}
		// 缓存文件可能损坏，删除后重新生成
		logger.Warn("缓存 Unigram 模型损坏，删除后重新生成", "path", wdbCachePath, "error", err)
		os.Remove(wdbCachePath)
		if _, statErr := os.Stat(txtPath); statErr == nil {
			if convErr := dictcache.ConvertUnigramToWdb(txtPath, wdbCachePath, logger); convErr == nil {
				if bm, err := pinyin.NewBinaryUnigramModel(wdbCachePath); err == nil {
					engine.SetUnigram(bm)
					logger.Info("Unigram 模型(重新生成)加载成功", "size", bm.Size())
					return nil
				}
			}
		}
	}

	return fmt.Errorf("Unigram 模型 wdb 不可用，智能组句功能将不可用")
}

func loadCodetable(engine *codetable.Engine, srcPath string, dictType DictType, cacheKey string, logger *slog.Logger, normalizer *dict.WeightNormalizer) error {
	var srcDir string
	var srcPaths []string

	if dictType == DictTypeRimeCodetable {
		// srcPath 是主词库 .dict.yaml 文件路径，自动发现关联词库
		srcDir = filepath.Dir(srcPath)
		srcPaths = dictcache.RimeCodetableSourcePaths(srcPath)
	} else {
		// 传统单文件码表格式
		srcDir = filepath.Dir(srcPath)
		srcPaths = []string{srcPath}
	}

	// 在加载入口列出所有发现的源文件，便于排查"应有的词库未被合并"问题
	logger.Info("码表源文件清单", "cacheKey", cacheKey, "type", dictType, "count", len(srcPaths), "files", srcPaths)

	wdbInDir := filepath.Join(srcDir, cacheKey+".wdb")
	if len(srcPaths) > 0 && !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if changed, recorded := dictcache.SourceListChanged(wdbInDir, srcPaths); changed {
			logger.Info("wdb 源文件清单已变化, 强制重建", "wdb", wdbInDir, "recorded", recorded, "current", srcPaths)
		} else if err := loadCodetableFromWdb(engine, wdbInDir); err == nil {
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath(cacheKey)
	regen := len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbCachePath)
	if !regen {
		if changed, recorded := dictcache.SourceListChanged(wdbCachePath, srcPaths); changed {
			logger.Info("wdb 缓存源文件清单已变化, 强制重建", "wdb", wdbCachePath, "recorded", recorded, "current", srcPaths)
			regen = true
		}
	}
	if regen {
		var convertErr error
		if dictType == DictTypeRimeCodetable {
			convertErr = dictcache.ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger, normalizer)
		} else {
			convertErr = dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath, logger)
		}
		if convertErr != nil {
			return fmt.Errorf("转换码表到 wdb 失败: %w", convertErr)
		}
	}

	if err := loadCodetableFromWdb(engine, wdbCachePath); err != nil {
		// 缓存文件可能损坏，删除后重新生成
		logger.Warn("缓存码表损坏，删除后重新生成", "path", wdbCachePath, "error", err)
		os.Remove(wdbCachePath)
		var convertErr error
		if dictType == DictTypeRimeCodetable {
			convertErr = dictcache.ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger, normalizer)
		} else {
			convertErr = dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath, logger)
		}
		if convertErr != nil {
			return fmt.Errorf("重新生成码表失败: %w", convertErr)
		}
		if err := loadCodetableFromWdb(engine, wdbCachePath); err != nil {
			return fmt.Errorf("加载重新生成的 %s.wdb 失败: %w", cacheKey, err)
		}
	}
	return nil
}

// ExtraLayerName 生成附加词库层名，使用 schemaID 前缀确保不同方案的扩展词库互相隔离。
// 形式：codetable-extra-<schemaID>__<dictID>
// 注意：分隔符使用 "__"，避免与 schemaID/dictID 内可能出现的单下划线发生歧义。
func ExtraLayerName(schemaID, dictID string) string {
	return "codetable-extra-" + schemaID + "__" + dictID
}

// loadExtraCodetable 加载附加词库为独立 CodeTable，注册为 DictManager 额外 system layer，
// 并返回注册到 CompositeDict 的 layer，供调用方缓存以便方案切换时清理/重挂。
// 附加词库加载失败为非致命错误：调用方记录警告后跳过，不影响主词库工作。
func loadExtraCodetable(dm *dict.DictManager, schemaID, srcPath string, spec DictSpec, cacheKey string, logger *slog.Logger) (dict.DictLayer, error) {
	if dm == nil {
		return nil, nil
	}

	srcDir := filepath.Dir(srcPath)
	var srcPaths []string
	if spec.Type == DictTypeRimeCodetable {
		srcPaths = dictcache.RimeCodetableSourcePaths(srcPath)
	} else {
		srcPaths = []string{srcPath}
	}

	logger.Info("附加词库源文件清单", "cacheKey", cacheKey, "type", spec.Type, "count", len(srcPaths))

	layerName := ExtraLayerName(schemaID, spec.ID)

	// 快捷路径：尝试加载源目录中的预编译 wdb（与 loadCodetable 行为对齐）
	wdbInDir := filepath.Join(srcDir, cacheKey+".wdb")
	if len(srcPaths) > 0 && !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if changed, recorded := dictcache.SourceListChanged(wdbInDir, srcPaths); changed {
			logger.Info("附加词库预编译 wdb 源文件清单已变化，跳过快捷路径", "wdb", wdbInDir, "recorded", recorded, "current", srcPaths)
		} else {
			ct := dict.NewCodeTable()
			if err := ct.LoadBinary(wdbInDir); err == nil {
				layer := dict.NewCodeTableLayer(layerName, dict.LayerTypeSystem, ct)
				dm.RegisterSystemLayer(layerName, layer)
				logger.Info("附加词库已注册(预编译 wdb)", "layer", layerName, "entryCount", ct.EntryCount())
				return layer, nil
			}
		}
	}

	wdbCachePath := dictcache.CachePath(cacheKey)
	regen := len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbCachePath)
	if !regen {
		if changed, recorded := dictcache.SourceListChanged(wdbCachePath, srcPaths); changed {
			logger.Info("附加词库缓存源文件清单已变化，强制重建", "cacheKey", cacheKey, "recorded", recorded, "current", srcPaths)
			regen = true
		}
	}
	if regen {
		var norm *dict.WeightNormalizer
		if spec.WeightSpec != nil {
			norm = spec.WeightSpec.NewWeightNormalizer()
		}
		var convertErr error
		if spec.Type == DictTypeRimeCodetable {
			convertErr = dictcache.ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger, norm)
		} else {
			convertErr = dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath, logger)
		}
		if convertErr != nil {
			return nil, fmt.Errorf("转换附加词库 %s 到 wdb 失败: %w", cacheKey, convertErr)
		}
	}

	ct := dict.NewCodeTable()
	if err := ct.LoadBinary(wdbCachePath); err != nil {
		// 缓存文件可能损坏，删除后重新生成
		logger.Warn("缓存附加词库损坏，删除后重新生成", "path", wdbCachePath, "error", err)
		os.Remove(wdbCachePath)
		var norm *dict.WeightNormalizer
		if spec.WeightSpec != nil {
			norm = spec.WeightSpec.NewWeightNormalizer()
		}
		var convertErr error
		if spec.Type == DictTypeRimeCodetable {
			convertErr = dictcache.ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger, norm)
		} else {
			convertErr = dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath, logger)
		}
		if convertErr != nil {
			return nil, fmt.Errorf("重新生成附加词库失败: %w", convertErr)
		}
		if err := ct.LoadBinary(wdbCachePath); err != nil {
			return nil, fmt.Errorf("加载重新生成的附加词库 %s 失败: %w", cacheKey, err)
		}
	}

	layer := dict.NewCodeTableLayer(layerName, dict.LayerTypeSystem, ct)
	dm.RegisterSystemLayer(layerName, layer)
	logger.Info("附加词库已注册", "layer", layerName, "entryCount", ct.EntryCount())
	return layer, nil
}

func loadCodetableFromWdb(engine *codetable.Engine, wdbPath string) error {
	if err := engine.LoadCodeTableBinary(wdbPath); err != nil {
		return err
	}

	// 从 wdb 内嵌 meta 段恢复 Header 信息
	reader, err := binformat.OpenDict(wdbPath)
	if err != nil {
		slog.Default().Warn("打开 wdb 读取 meta 失败", "err", err)
		return nil
	}
	defer reader.Close()

	meta, err := dictcache.LoadCodeTableMetaFromWdb(reader)
	if err != nil {
		slog.Default().Warn("加载码表 meta 失败", "err", err)
	} else {
		engine.RestoreCodeTableHeader(dict.CodeTableHeader{
			Name:          meta.Name,
			Version:       meta.Version,
			Author:        meta.Author,
			CodeScheme:    meta.CodeScheme,
			CodeLength:    meta.CodeLength,
			BWCodeLength:  meta.BWCodeLength,
			SpecialPrefix: meta.SpecialPrefix,
			PhraseRule:    meta.PhraseRule,
			HasWeight:     meta.HasWeight,
		})
	}
	return nil
}

// LoadCodetableForPinyinEngine 为拼音引擎加载码表反查（导出供热更新使用）
func LoadCodetableForPinyinEngine(engine *pinyin.Engine, srcPath string, dictType DictType, schemaID string, logger *slog.Logger) error {
	return loadCodetableForPinyin(engine, srcPath, dictType, schemaID, logger)
}

func loadCodetableForPinyin(engine *pinyin.Engine, srcPath string, dictType DictType, schemaID string, logger *slog.Logger) error {
	var srcDir string
	var srcPaths []string

	if dictType == DictTypeRimeCodetable {
		srcDir = filepath.Dir(srcPath)
		srcPaths = dictcache.RimeCodetableSourcePaths(srcPath)
	} else {
		srcDir = filepath.Dir(srcPath)
		srcPaths = []string{srcPath}
	}

	// 使用 _reverse 后缀避免与拼音词库缓存（CachePath(schemaID)）冲突
	reverseName := schemaID + "_reverse"
	wdbInDir := filepath.Join(srcDir, reverseName+".wdb")
	if len(srcPaths) > 0 && !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if err := engine.LoadCodeHintTableBinary(wdbInDir); err == nil {
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath(reverseName)
	if len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		var convertErr error
		if dictType == DictTypeRimeCodetable {
			convertErr = dictcache.ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger)
		} else {
			convertErr = dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath, logger)
		}
		if convertErr != nil {
			return fmt.Errorf("生成码表反查缓存失败: %w", convertErr)
		}
	}

	if err := engine.LoadCodeHintTableBinary(wdbCachePath); err == nil {
		return nil
	}

	return fmt.Errorf("码表反查 wdb 不可用")
}

// loadPinyinUserFreqs 从 Store 加载拼音用户词频
func loadPinyinUserFreqs(engine *pinyin.Engine, s *store.Store, schemaID string, logger *slog.Logger) {
	if engine.GetUnigram() == nil {
		return
	}
	if m := engine.GetUnigramModel(); m != nil {
		if err := m.LoadUserFreqsFromStore(s, schemaID); err != nil {
			logger.Warn("加载拼音用户词频失败", "error", err)
		} else if m.GetUserFreqs() != nil {
			logger.Info("用户词频加载成功", "entries", len(m.GetUserFreqs()))
		}
		return
	}
	if bm := engine.GetBinaryUnigramModel(); bm != nil {
		if err := bm.LoadUserFreqsFromStore(s, schemaID); err != nil {
			logger.Warn("加载拼音用户词频失败(binary)", "error", err)
		} else if bm.GetUserFreqs() != nil {
			logger.Info("用户词频加载成功(binary)", "entries", len(bm.GetUserFreqs()))
		}
	}
}

// SavePinyinUserFreqs 将拼音用户词频保存到 Store
func SavePinyinUserFreqs(engine *pinyin.Engine, s *store.Store, schemaID string) {
	if engine.GetUnigram() == nil {
		return
	}
	if m := engine.GetUnigramModel(); m != nil {
		if err := m.SaveUserFreqsToStore(s, schemaID); err != nil {
			slog.Error("保存拼音用户词频失败", "error", err)
		}
		return
	}
	if bm := engine.GetBinaryUnigramModel(); bm != nil {
		if err := bm.SaveUserFreqsToStore(s, schemaID); err != nil {
			slog.Error("保存拼音用户词频失败(binary)", "error", err)
		}
	}
}

func preGeneratePinyinWdb(s *Schema, exeDir, dataDir string, logger *slog.Logger) {
	// 查找拼音词库路径及归一化参数
	var pinyinDictPath string
	var norm *dict.WeightNormalizer
	for _, d := range s.Dicts {
		if d.Type == DictTypeRimePinyin {
			pinyinDictPath = resolvePath(exeDir, dataDir, d.Path)
			if d.WeightSpec != nil {
				norm = d.WeightSpec.NewWeightNormalizer()
			}
			break
		}
	}

	// 如果当前方案没有拼音词库，尝试默认路径
	if pinyinDictPath == "" {
		pinyinDictPath = resolvePath(exeDir, dataDir, "pinyin/rime_frost.dict.yaml")
	}

	// 主词库文件不存在时跳过预生成（路径解析失败的防御，避免把错误路径传入构建器）
	if _, err := os.Stat(pinyinDictPath); err != nil {
		logger.Debug("预生成拼音 wdat 跳过：主词库文件不存在", "path", pinyinDictPath)
		return
	}

	srcPaths := dictcache.RimePinyinSourcePaths(pinyinDictPath)

	// 预生成 wdat（供 dict_format=dat 的方案使用，避免首次切换时同步构建卡顿）
	// 通过 startPinyinWdatBuildAsync 确保全局最多一个构建协程在运行
	wdatCachePath := dictcache.WdatCachePath("pinyin")
	if dictcache.NeedsRegenerate(srcPaths, wdatCachePath) {
		startPinyinWdatBuildAsync(pinyinDictPath, wdatCachePath, logger, norm)
	}

	// 预生成 Unigram
	unigramTxtPath := resolvePath(exeDir, dataDir, "pinyin/unigram.txt")
	unigramWdbPath := strings.TrimSuffix(unigramTxtPath, ".txt") + ".wdb"
	unigramCachePath := dictcache.CachePath("unigram")

	if _, err := os.Stat(unigramWdbPath); err == nil {
		if !dictcache.NeedsRegenerate([]string{unigramTxtPath}, unigramWdbPath) {
			return
		}
	}
	if dictcache.NeedsRegenerate([]string{unigramTxtPath}, unigramCachePath) {
		if _, err := os.Stat(unigramTxtPath); err == nil {
			dictcache.ConvertUnigramToWdb(unigramTxtPath, unigramCachePath, logger)
		}
	}

	runtime.GC()
	debug.FreeOSMemory()
}

// ensureCodetableWdb 确保码表的 wdb 缓存文件已是最新，但不加载引擎、不注册词库层。
// 这是 loadCodetable 缓存判定逻辑的"只生成"版本：复刻其源文件发现与 NeedsRegenerate
// 判定，仅在缓存过期时触发 ConvertXxxToWdb，省去 mmap 加载与层注册的副作用。
// 用于启动 / 手动重建缓存时后台预生成所有已启用方案的缓存，消除首次切换方案的转换卡顿。
func ensureCodetableWdb(srcPath string, dictType DictType, cacheKey string, norm *dict.WeightNormalizer, logger *slog.Logger) error {
	if srcPath == "" {
		return nil
	}
	var srcPaths []string
	if dictType == DictTypeRimeCodetable {
		srcPaths = dictcache.RimeCodetableSourcePaths(srcPath)
	} else {
		srcPaths = []string{srcPath}
	}
	if len(srcPaths) == 0 {
		return nil
	}

	// 与 loadCodetable 一致：优先复用方案目录内的预编译 wdb，存在且不过期则无需生成缓存。
	srcDir := filepath.Dir(srcPath)
	wdbInDir := filepath.Join(srcDir, cacheKey+".wdb")
	if !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if changed, _ := dictcache.SourceListChanged(wdbInDir, srcPaths); !changed {
			return nil
		}
	}

	// 缓存目录内的 wdb 已是最新则跳过，否则转换重建。
	wdbCachePath := dictcache.CachePath(cacheKey)
	if !dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		if changed, _ := dictcache.SourceListChanged(wdbCachePath, srcPaths); !changed {
			return nil
		}
	}
	if dictType == DictTypeRimeCodetable {
		return dictcache.ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger, norm)
	}
	return dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath, logger)
}

// EnsureSchemaCacheFiles 预生成指定方案的全部词库缓存文件（.wdb/.wdat），但不构建引擎、
// 不注册词库层、不常驻内存。按引擎类型覆盖：
//   - 码表：主词库 + 所有已启用的附加词库的 wdb
//   - 拼音：wdat + unigram（经 preGeneratePinyinWdb，wdat 走全局单协程异步构建）
//   - 混输：码表（自身或主方案的 default 词库）+ 自身附加词库 + 拼音（自身或辅方案）
//
// 目的是在启动 / 手动重建缓存时，提前把所有已启用方案的缓存备好，消除用户首次切换方案
// 时的同步转换卡顿（rime yaml → 二进制，可达数百 ms~数秒）。失败仅记录警告、不中断其余。
func EnsureSchemaCacheFiles(s *Schema, exeDir, dataDir string, resolver SchemaResolver, logger *slog.Logger) {
	if s == nil {
		return
	}
	switch s.Engine.Type {
	case EngineTypeCodeTable:
		ensureCodetableSchemaCaches(s, exeDir, dataDir, logger)
	case EngineTypePinyin:
		preGeneratePinyinWdb(s, exeDir, dataDir, logger)
	case EngineTypeMixed:
		ensureMixedSchemaCaches(s, exeDir, dataDir, resolver, logger)
	}
}

// ensureExtraCodetableCaches 预生成方案所有"已启用的附加词库"的 wdb 缓存。
// cacheKey 约定与 loadExtraCodetable 调用处一致：schemaID + "_" + dictID。
func ensureExtraCodetableCaches(s *Schema, exeDir, dataDir string, logger *slog.Logger) {
	for i := range s.Dicts {
		dictSpec := s.Dicts[i]
		if dictSpec.Default || !dictSpec.IsEnabled() {
			continue
		}
		srcPath := resolvePath(exeDir, dataDir, dictSpec.Path)
		cacheKey := s.Schema.ID + "_" + dictSpec.ID
		var norm *dict.WeightNormalizer
		if dictSpec.WeightSpec != nil {
			norm = dictSpec.WeightSpec.NewWeightNormalizer()
		}
		if err := ensureCodetableWdb(srcPath, dictSpec.Type, cacheKey, norm, logger); err != nil {
			logger.Warn("预生成附加码表缓存失败", "schemaID", s.Schema.ID, "dictID", dictSpec.ID, "err", err)
		}
	}
}

// ensureCodetableSchemaCaches 预生成码表方案的主词库 + 附加词库 wdb 缓存。
func ensureCodetableSchemaCaches(s *Schema, exeDir, dataDir string, logger *slog.Logger) {
	if mainSpec := s.GetDefaultDictSpec(); mainSpec != nil {
		srcPath := resolvePath(exeDir, dataDir, mainSpec.Path)
		cacheKey := s.Schema.ID + "_" + mainSpec.ID
		var norm *dict.WeightNormalizer
		if mainSpec.WeightSpec != nil {
			norm = mainSpec.WeightSpec.NewWeightNormalizer()
		}
		if err := ensureCodetableWdb(srcPath, mainSpec.Type, cacheKey, norm, logger); err != nil {
			logger.Warn("预生成主码表缓存失败", "schemaID", s.Schema.ID, "dictID", mainSpec.ID, "err", err)
		}
	}
	ensureExtraCodetableCaches(s, exeDir, dataDir, logger)
}

// ensureMixedSchemaCaches 预生成混输方案的码表 + 附加词库 + 拼音缓存。
// 路径解析复刻 createMixedEngine：码表 default 词库优先取自身 Dicts、否则主方案；
// cacheKey 引用主方案时使用主方案 ID 以共享缓存。
func ensureMixedSchemaCaches(s *Schema, exeDir, dataDir string, resolver SchemaResolver, logger *slog.Logger) {
	mixedSpec := s.Engine.Mixed
	var primarySchema, secondarySchema *Schema
	if mixedSpec != nil && resolver != nil {
		if mixedSpec.PrimarySchema != "" {
			primarySchema = resolver(mixedSpec.PrimarySchema)
		}
		if mixedSpec.SecondarySchema != "" {
			secondarySchema = resolver(mixedSpec.SecondarySchema)
		}
	}

	// 码表 default 词库：优先混输自身，其次主方案
	var codetableDictSpec *DictSpec
	for i := range s.Dicts {
		if s.Dicts[i].Default {
			codetableDictSpec = &s.Dicts[i]
			break
		}
	}
	if codetableDictSpec == nil && primarySchema != nil {
		for i := range primarySchema.Dicts {
			if primarySchema.Dicts[i].Default {
				codetableDictSpec = &primarySchema.Dicts[i]
				break
			}
		}
	}
	codetableCacheID := s.Schema.ID
	if primarySchema != nil {
		codetableCacheID = primarySchema.Schema.ID
	}
	if codetableDictSpec != nil {
		if codetableDictSpec.ID != "" {
			codetableCacheID = codetableCacheID + "_" + codetableDictSpec.ID
		}
		srcPath := resolvePath(exeDir, dataDir, codetableDictSpec.Path)
		var norm *dict.WeightNormalizer
		if codetableDictSpec.WeightSpec != nil {
			norm = codetableDictSpec.WeightSpec.NewWeightNormalizer()
		}
		if err := ensureCodetableWdb(srcPath, codetableDictSpec.Type, codetableCacheID, norm, logger); err != nil {
			logger.Warn("预生成混输码表缓存失败", "schemaID", s.Schema.ID, "err", err)
		}
	}

	// 混输自身已启用的附加词库
	ensureExtraCodetableCaches(s, exeDir, dataDir, logger)

	// 拼音 wdat + unigram：辅方案含 rime pinyin 主词库时优先用辅方案，否则用混输自身
	pinyinSchema := s
	if secondarySchema != nil {
		pinyinSchema = secondarySchema
	}
	preGeneratePinyinWdb(pinyinSchema, exeDir, dataDir, logger)
}

// ResolveDictPath 解析相对路径为绝对路径（包内 resolvePath 的导出别名）
// 搜索顺序：exeDir → exeDir/schemas → dataDir → dataDir/schemas
// 这使得方案配置中的词库路径可以简写为相对于 schemas 目录的路径，
// 例如 "wubi86/wubi86_jidian.dict.yaml" 会从 schemas/wubi86/ 下查找。
func ResolveDictPath(exeDir, dataDir, path string) string {
	return resolvePath(exeDir, dataDir, path)
}

// resolvePath 解析相对路径为绝对路径
// 搜索顺序：exeDir → exeDir/schemas → dataDir → dataDir/schemas
// 这使得方案配置中的词库路径可以简写为相对于 schemas 目录的路径，
// 例如 "wubi86/wubi86_jidian.dict.yaml" 会从 schemas/wubi86/ 下查找。
func resolvePath(exeDir, dataDir, path string) string {
	if path == "" {
		return ""
	}
	if isAbsPath(path) {
		return path
	}
	// 按优先级依次查找
	searchDirs := make([]string, 0, 4)
	if exeDir != "" {
		searchDirs = append(searchDirs, exeDir, filepath.Join(exeDir, "schemas"))
	}
	if dataDir != "" {
		searchDirs = append(searchDirs, dataDir, filepath.Join(dataDir, "schemas"))
	}
	for _, dir := range searchDirs {
		candidate := filepath.Join(dir, path)
		if _, err := os.Stat(candidate); err == nil {
			return candidate
		}
	}
	// 都不存在时默认返回 exeDir/schemas 路径（用于错误提示）
	if exeDir != "" {
		return filepath.Join(exeDir, "schemas", path)
	}
	return path
}

// ReloadExtraDicts 根据方案 Dicts 配置动态加载/卸载附加词库层。
// 用于 dict enabled 状态变更后的热重载，不重建主词库。
// 返回该方案当前已启用并成功加载的所有附加层，调用方负责把这份列表同步回
// EngineManager.systemExtras，使得"切走 → 切回"路径仍能正确恢复 extras。
func ReloadExtraDicts(dm *dict.DictManager, s *Schema, exeDir, dataDir string, logger *slog.Logger) []dict.DictLayer {
	var layers []dict.DictLayer
	for _, dictSpec := range s.Dicts {
		if dictSpec.Default {
			continue
		}
		layerName := ExtraLayerName(s.Schema.ID, dictSpec.ID)
		if !dictSpec.IsEnabled() {
			dm.UnregisterSystemLayer(layerName)
			continue
		}
		srcPath := resolvePath(exeDir, dataDir, dictSpec.Path)
		cacheKey := s.Schema.ID + "_" + dictSpec.ID
		layer, err := loadExtraCodetable(dm, s.Schema.ID, srcPath, dictSpec, cacheKey, logger)
		if err != nil {
			logger.Warn("附加词库热重载失败", "dictID", dictSpec.ID, "error", err)
			continue
		}
		if layer != nil {
			layers = append(layers, layer)
		}
	}
	return layers
}

// createMixedEngine 创建混输引擎（五笔+拼音并行查询）
// 五笔引擎使用 DictManager 的主 CompositeDict（含 codetable-system 层），
// 拼音引擎使用独立的 CompositeDict（含 pinyin-system 层），避免交叉污染。
func createMixedEngine(s *Schema, exeDir, dataDir string, dm *dict.DictManager, logger *slog.Logger, resolver SchemaResolver) (*EngineBundle, error) {
	// === 1. 读取混输配置 ===
	mixedSpec := s.Engine.Mixed
	if mixedSpec == nil {
		mixedSpec = &MixedSpec{
			MinPinyinLength:      2,
			CodetableWeightBoost: 10000000,
			ShowSourceHint:       true,
		}
	}

	// === 解析引用方案 ===
	var primarySchema *Schema
	var secondarySchema *Schema

	if mixedSpec.PrimarySchema != "" && resolver != nil {
		primarySchema = resolver(mixedSpec.PrimarySchema)
		if primarySchema == nil {
			return nil, fmt.Errorf("混输：主方案 %q 不存在", mixedSpec.PrimarySchema)
		}
		logger.Info("混输：引用主方案", "primary", mixedSpec.PrimarySchema)
	}
	if mixedSpec.SecondarySchema != "" && resolver != nil {
		secondarySchema = resolver(mixedSpec.SecondarySchema)
		if secondarySchema == nil {
			return nil, fmt.Errorf("混输：拼音方案 %q 不存在", mixedSpec.SecondarySchema)
		}
		logger.Info("混输：引用拼音方案", "secondary", mixedSpec.SecondarySchema)
	}

	// === 2. 创建码表引擎 ===
	// 以主方案的码表配置为 base，再将混输方案自身的覆盖字段叠加其上。
	// 这样用户在 schema_overrides.yaml 中只写部分字段时，未写的字段沿用主方案值，
	// 而不会因 yaml.Unmarshal 的零值语义导致关键字段（如 MaxCodeLength）丢失。
	codeTableSpec := s.Engine.CodeTable
	if primarySchema != nil && primarySchema.Engine.CodeTable != nil {
		if codeTableSpec == nil {
			codeTableSpec = primarySchema.Engine.CodeTable
		} else {
			codeTableSpec = mergeCodeTableSpec(primarySchema.Engine.CodeTable, codeTableSpec)
		}
	}
	if codeTableSpec == nil {
		codeTableSpec = &CodeTableSpec{
			MaxCodeLength:     4,
			TopCodeCommit:     true,
			PunctCommit:       true,
			ShowCodeHint:      true,
			CandidateSortMode: "frequency",
		}
	}

	mixedDedupCandidates := true
	if codeTableSpec.DedupCandidates != nil {
		mixedDedupCandidates = *codeTableSpec.DedupCandidates
	}
	mixedSkipSingleCharFreq := true // 默认值：单字不自动调频
	if codeTableSpec.SkipSingleCharFreq != nil {
		mixedSkipSingleCharFreq = *codeTableSpec.SkipSingleCharFreq
	}
	codetableConfig := &codetable.Config{
		MaxCodeLength: codeTableSpec.MaxCodeLength,
		AutoCommitAtFull: func() bool {
			if codeTableSpec.AutoCommitAtFull != nil {
				return *codeTableSpec.AutoCommitAtFull
			}
			return codeTableSpec.AutoCommitUnique
		}(),
		MinAutoCommitLen: codeTableSpec.AutoCommitMinLen, // 0 由 LoadCodeTable 末尾兜底
		AutoCommitBlockOnPinyin: func() bool {
			if codeTableSpec.AutoCommitBlockOnPinyin != nil {
				return *codeTableSpec.AutoCommitBlockOnPinyin
			}
			return true
		}(),
		ClearOnEmptyAt4:    codeTableSpec.ClearOnEmptyMax,
		TopCodeCommit:      codeTableSpec.TopCodeCommit,
		PunctCommit:        codeTableSpec.PunctCommit,
		ShowCodeHint:       codeTableSpec.ShowCodeHint,
		SingleCodeInput:    codeTableSpec.SingleCodeInput,
		FilterMode:         s.Engine.FilterMode,
		CandidateSortMode:  codeTableSpec.CandidateSortMode,
		DedupCandidates:    mixedDedupCandidates,
		SkipShadow:         true, // 混输模式：Shadow 由 MixedEngine 合并后统一应用
		SkipSingleCharFreq: mixedSkipSingleCharFreq,
		LoadMode:           codeTableSpec.LoadMode,
		PrefixMode:         codeTableSpec.PrefixMode,
		BucketLimit:        codeTableSpec.BucketLimit,
		WeightMode:         codeTableSpec.WeightMode,
		CharsetPreference:  codeTableSpec.CharsetPreference,
	}
	if codeTableSpec.ShortCodeFirst != nil {
		codetableConfig.ShortCodeFirst = *codeTableSpec.ShortCodeFirst
	}

	// ProtectTopN 从 FreqSpec 读取
	if s.Learning.Freq != nil {
		codetableConfig.ProtectTopN = s.Learning.Freq.ProtectTopN
	}

	codetableEngine := codetable.NewEngine(codetableConfig, logger)

	// 加载码表（优先从混输方案的 Dicts 查找，其次从主方案）
	var codetableDictSpec *DictSpec
	for i := range s.Dicts {
		if s.Dicts[i].Default {
			codetableDictSpec = &s.Dicts[i]
			break
		}
	}
	if codetableDictSpec == nil && primarySchema != nil {
		for i := range primarySchema.Dicts {
			if primarySchema.Dicts[i].Default {
				codetableDictSpec = &primarySchema.Dicts[i]
				break
			}
		}
	}
	// wdb 缓存 key：引用主方案时使用主方案 ID，共享缓存
	codetableCacheID := s.Schema.ID
	if primarySchema != nil {
		codetableCacheID = primarySchema.Schema.ID
	}
	// 与多词库命名约定对齐：cacheKey = schemaID_dictID
	if codetableDictSpec != nil && codetableDictSpec.ID != "" {
		codetableCacheID = codetableCacheID + "_" + codetableDictSpec.ID
	} else if codetableDictSpec != nil {
		logger.Warn("混输：码表 DictSpec.ID 为空，cacheKey 未追加 dictID，可能与主码表引擎缓存冲突", "schemaID", s.Schema.ID, "cacheKey", codetableCacheID)
	}
	if codetableDictSpec != nil {
		srcPath := resolvePath(exeDir, dataDir, codetableDictSpec.Path)
		var codetableNorm *dict.WeightNormalizer
		if codetableDictSpec.WeightSpec != nil {
			codetableNorm = codetableDictSpec.WeightSpec.NewWeightNormalizer()
		}
		if codetableDictSpec.WeightAsOrder {
			codetableConfig.WeightAsOrder = true
		}
		if err := loadCodetable(codetableEngine, srcPath, codetableDictSpec.Type, codetableCacheID, logger, codetableNorm); err != nil {
			return nil, fmt.Errorf("混输：加载码表失败: %w", err)
		}
		logger.Info("混输：码表加载成功", "schemaID", s.Schema.ID, "cacheID", codetableCacheID, "entryCount", codetableEngine.GetEntryCount())
	}

	// 注册码表到 DictManager 的主 CompositeDict
	var mixedSystemLayer dict.DictLayer
	if dm != nil {
		codeTable := codetableEngine.GetCodeTable()
		if codeTable != nil {
			systemLayer := dict.NewCodeTableLayer("codetable-system", dict.LayerTypeSystem, codeTable)
			dm.RegisterSystemLayer("codetable-system", systemLayer)
			mixedSystemLayer = systemLayer
		}
		codetableEngine.SetDictManager(dm)
		// 排序模式不再由 dm 持有；引擎每次调用 composite.Search 时通过 SearchOptions 显式传入。

		// 注入码表引擎的 FreqHandler 和 LearningStrategy
		if s.Learning.IsFreqEnabled() {
			freqProfile := s.Learning.GetFreqProfile()
			dm.SetFreqProfile(freqProfile)
			codetableFreqHandler := dict.NewFreqHandler(dm.GetStore(), s.Schema.ID)
			codetableEngine.SetFreqHandler(codetableFreqHandler)
		}
		// 混输码表子引擎：auto_learn 或 auto_phrase 启用时使用码表自动造词
		// 按混输的 dataSchemaID（= 主方案 ID）绑定层，避免预加载时被绑到错误的活跃方案
		// 混输方案始终使用主方案的学习配置（混输本质是主码表 + 辅助拼音，不维护独立学习配置）
		codetableLearningSpec := &s.Learning
		if primarySchema != nil {
			codetableLearningSpec = &primarySchema.Learning
		}
		if codetableLearningSpec.IsAutoPhraseEnabled() || codetableLearningSpec.IsAutoLearnEnabled() {
			autoPhrase := NewCodeTableLearningStrategy(codetableLearningSpec, logger)
			if ul := dm.GetOrCreateStoreUserLayer(s.DataSchemaID()); ul != nil {
				autoPhrase.SetUserLayer(ul)
			}
			if tl := dm.GetOrCreateStoreTempLayer(s.DataSchemaID()); tl != nil {
				autoPhrase.SetTempLayer(tl)
			}
			autoPhrase.SetSystemChecker(dm)
			encoder := s.Encoder
			if encoder == nil && primarySchema != nil {
				encoder = primarySchema.Encoder
			}
			if encoder != nil && len(encoder.Rules) > 0 && codetableEngine.GetCodeTable() != nil {
				calc := NewEncoderWordCodeCalc(encoder.Rules, codetableEngine.GetCodeTable())
				autoPhrase.SetWordCodeCalculator(calc)
			}
			codetableEngine.SetLearningStrategy(autoPhrase)
		} else {
			codetableEngine.SetLearningStrategy(&ManualLearning{})
		}
	}

	// === 3. 创建拼音引擎（使用独立的 CompositeDict）===
	// 优先使用混输方案自身的拼音配置，其次从拼音方案继承
	pinyinSpec := s.Engine.Pinyin
	if pinyinSpec == nil && secondarySchema != nil {
		pinyinSpec = secondarySchema.Engine.Pinyin
	}
	if pinyinSpec == nil {
		pinyinSpec = &PinyinSpec{
			Scheme:          PinyinSchemeFull,
			ShowCodeHint:    true,
			UseSmartCompose: true,
		}
	}

	// 混输模式下默认关闭简拼匹配（减少噪声），用户可通过 enable_abbrev_match 开启
	skipAbbrev := true
	if mixedSpec.EnableAbbrevMatch != nil && *mixedSpec.EnableAbbrevMatch {
		skipAbbrev = false
	}
	pinyinConfig := &pinyin.Config{
		ShowCodeHint:    pinyinSpec.ShowCodeHint,
		FilterMode:      s.Engine.FilterMode,
		UseSmartCompose: pinyinSpec.UseSmartCompose,
		CandidateOrder:  pinyinSpec.CandidateOrder,
		SkipShadow:      true, // 混输模式：Shadow 由 MixedEngine 合并后统一应用
		SkipAbbrev:      skipAbbrev,
	}

	// 模糊音配置
	if pinyinSpec.Fuzzy != nil && pinyinSpec.Fuzzy.Enabled {
		pinyinConfig.Fuzzy = &pinyin.FuzzyConfig{
			ZhZ:   pinyinSpec.Fuzzy.ZhZ,
			ChC:   pinyinSpec.Fuzzy.ChC,
			ShS:   pinyinSpec.Fuzzy.ShS,
			NL:    pinyinSpec.Fuzzy.NL,
			FH:    pinyinSpec.Fuzzy.FH,
			RL:    pinyinSpec.Fuzzy.RL,
			AnAng: pinyinSpec.Fuzzy.AnAng,
			EnEng: pinyinSpec.Fuzzy.EnEng,
			InIng: pinyinSpec.Fuzzy.InIng,
		}
	}

	// 加载拼音词库（优先从混输方案查找，其次从拼音方案）
	pinyinDict := dict.NewPinyinDict(logger)
	var pinyinDictSpec *DictSpec
	for i := range s.Dicts {
		if s.Dicts[i].Type == DictTypeRimePinyin {
			pinyinDictSpec = &s.Dicts[i]
			break
		}
	}
	if pinyinDictSpec == nil && secondarySchema != nil {
		for i := range secondarySchema.Dicts {
			if secondarySchema.Dicts[i].Type == DictTypeRimePinyin {
				pinyinDictSpec = &secondarySchema.Dicts[i]
				break
			}
		}
	}
	if pinyinDictSpec != nil {
		dictPath := resolvePath(exeDir, dataDir, pinyinDictSpec.Path)
		var pinyinNorm *dict.WeightNormalizer
		if pinyinDictSpec.WeightSpec != nil {
			pinyinNorm = pinyinDictSpec.WeightSpec.NewWeightNormalizer()
		}
		if err := loadPinyinDict(pinyinDict, dictPath, logger, pinyinNorm); err != nil {
			return nil, fmt.Errorf("混输：加载拼音词库失败: %w", err)
		}
	}

	// 创建独立的 CompositeDict（仅包含拼音系统层，不污染五笔查询）
	pinyinCompositeDict := dict.NewCompositeDict()
	pinyinSystemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
	pinyinCompositeDict.AddLayer(pinyinSystemLayer)

	// 缓存拼音系统层到 engine manager（供临时拼音模式恢复使用）
	if dm != nil {
		dm.RegisterSystemLayer("pinyin-system", pinyinSystemLayer)
		// 立即从主 CompositeDict 移除拼音层，只保留在独立 dict 中
		if mainDict := dm.GetCompositeDict(); mainDict != nil {
			mainDict.RemoveLayer("pinyin-system")
		}
	}

	pinyinEngine := pinyin.NewEngineWithConfig(pinyinCompositeDict, pinyinConfig, logger)

	// 混输模式下的双拼转换器
	if pinyinSpec.Scheme == PinyinSchemeShuangpin && pinyinSpec.Shuangpin != nil {
		spScheme := shuangpin.Get(pinyinSpec.Shuangpin.Layout)
		if spScheme != nil {
			pinyinEngine.SetShuangpinConverter(shuangpin.NewConverter(spScheme))
			logger.Info("混输双拼模式", "layout", spScheme.ID, "name", spScheme.Name)
		}
	}

	// 加载 Unigram 语言模型（优先从混输方案，其次从拼音方案继承）
	unigramPath := s.Learning.UnigramPath
	if unigramPath == "" && secondarySchema != nil {
		unigramPath = secondarySchema.Learning.UnigramPath
	}
	if unigramPath != "" {
		unigramTxtPath := resolvePath(exeDir, dataDir, unigramPath)
		if err := loadUnigramModel(pinyinEngine, unigramTxtPath, logger); err != nil {
			logger.Warn("混输：加载 Unigram 模型失败", "err", err)
		}
	}

	// 混输模式下跳过拼音子引擎的反查码表加载：
	// 由 mixed.Engine.addCodeHintsFromCodetable() 直接使用主码表的反向索引，
	// 避免生成冗余的 _reverse.wdb 文件

	// 拼音子引擎的独立 dataSchemaID：
	// 优先使用次级方案（如 "pinyin"/"shuangpin"）的 DataSchemaID，回退 "pinyin"。
	// 这样混输下选拼音候选时学到的词只写入拼音独立 bucket，不会污染主码表（如 wubi86）的用户词库。
	pinyinDataSchemaID := "pinyin"
	if secondarySchema != nil {
		pinyinDataSchemaID = secondarySchema.DataSchemaID()
	}

	// 设置拼音引擎的 DictManager（用于用户词频学习）
	if dm != nil {
		pinyinEngine.SetDictManager(dm)

		// 注入拼音引擎的 FreqHandler 和 LearningStrategy（统一使用独立 pinyinDataSchemaID）
		if s.Learning.IsFreqEnabled() {
			pinyinFreqHandler := dict.NewFreqHandler(dm.GetStore(), pinyinDataSchemaID)
			pinyinEngine.SetFreqHandler(pinyinFreqHandler)
		}
		pinyinUserLayer := dm.GetOrCreateStoreUserLayer(pinyinDataSchemaID)
		pinyinLearning := NewLearningStrategy(&s.Learning, pinyinUserLayer)
		if al, ok := pinyinLearning.(*AutoLearning); ok {
			if tl := dm.GetOrCreateStoreTempLayer(pinyinDataSchemaID); tl != nil {
				// 拼音 temp layer 不是 activeStoreTemp，switchSchemaStore 不会覆盖它，
				// 必须显式 SetLimits，否则 promoteCount=0，LearnWord 永远返回 false。
				// 混输方案始终使用主方案的 temp limits（与 codetableLearningSpec 保持一致）。
				pinyinTempSpec := &s.Learning
				if primarySchema != nil {
					pinyinTempSpec = &primarySchema.Learning
				}
				tl.SetLimits(pinyinTempSpec.TempMaxEntries, pinyinTempSpec.TempPromoteCount)
				al.SetTempLayer(tl)
			}
		}
		pinyinEngine.SetLearningStrategy(pinyinLearning)
	}

	// 加载拼音用户词频
	if s.Learning.IsFreqEnabled() || s.Learning.IsAutoLearnEnabled() {
		if dm != nil && dm.GetStore() != nil {
			loadPinyinUserFreqs(pinyinEngine, dm.GetStore(), pinyinDataSchemaID, logger)
		}
	}

	// === 4. 创建混输引擎 ===
	pinyinOnlyOverflow := true // 默认超过码长仅查拼音
	if mixedSpec.PinyinOnlyOverflow != nil {
		pinyinOnlyOverflow = *mixedSpec.PinyinOnlyOverflow
	}
	mixedConfig := &mixed.Config{
		MinPinyinLength:      mixedSpec.MinPinyinLength,
		CodetableWeightBoost: mixedSpec.CodetableWeightBoost,
		ShowSourceHint:       mixedSpec.ShowSourceHint,
		PinyinOnlyOverflow:   pinyinOnlyOverflow,
	}
	if mixedConfig.MinPinyinLength <= 0 {
		mixedConfig.MinPinyinLength = 2
	}
	if mixedConfig.CodetableWeightBoost <= 0 {
		mixedConfig.CodetableWeightBoost = 10000000
	}

	mixedEngine := mixed.NewEngine(codetableEngine, pinyinEngine, mixedConfig, logger)

	// 设置 DictManager（用于合并后统一应用 Shadow 规则）
	if dm != nil {
		mixedEngine.SetDictManager(dm)
	}

	logger.Info("混输引擎创建成功", "schemaID", s.Schema.ID, "codetableEntries", codetableEngine.GetEntryCount(), "pinyinEntries", pinyinDict.EntryCount())

	// GC 释放临时内存
	go func() {
		runtime.GC()
		debug.FreeOSMemory()
	}()

	return &EngineBundle{
		SchemaID:    s.Schema.ID,
		Engine:      mixedEngine,
		SystemLayer: mixedSystemLayer,
	}, nil
}

func isAbsPath(path string) bool {
	if len(path) == 0 {
		return false
	}
	if len(path) >= 2 && path[1] == ':' {
		return true
	}
	if len(path) >= 2 && path[0] == '\\' && path[1] == '\\' {
		return true
	}
	return path[0] == '/'
}

// mergeCodeTableSpec 以 base 为基础，将 override 中的非零字段叠加其上，返回合并副本。
// 用于混输引擎：base 来自主方案（如 wubi86），override 来自混输方案的部分覆盖配置。
// 限制：plain bool 字段无法区分"显式 false"与"零值未设置"，始终沿用 base 的值。
func mergeCodeTableSpec(base, override *CodeTableSpec) *CodeTableSpec {
	merged := *base
	if override.MaxCodeLength > 0 {
		merged.MaxCodeLength = override.MaxCodeLength
	}
	if override.BucketLimit > 0 {
		merged.BucketLimit = override.BucketLimit
	}
	if override.CandidateSortMode != "" {
		merged.CandidateSortMode = override.CandidateSortMode
	}
	if override.LoadMode != "" {
		merged.LoadMode = override.LoadMode
	}
	if override.PrefixMode != "" {
		merged.PrefixMode = override.PrefixMode
	}
	if override.WeightMode != "" {
		merged.WeightMode = override.WeightMode
	}
	if override.CharsetPreference != "" {
		merged.CharsetPreference = override.CharsetPreference
	}
	if override.DedupCandidates != nil {
		merged.DedupCandidates = override.DedupCandidates
	}
	if override.ShortCodeFirst != nil {
		merged.ShortCodeFirst = override.ShortCodeFirst
	}
	if override.SkipSingleCharFreq != nil {
		merged.SkipSingleCharFreq = override.SkipSingleCharFreq
	}
	if override.TempPinyin != nil {
		merged.TempPinyin = override.TempPinyin
	}
	if override.ZKeyRepeat != nil {
		merged.ZKeyRepeat = override.ZKeyRepeat
	}
	return &merged
}
