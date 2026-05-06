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

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
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
		MaxCodeLength:      spec.MaxCodeLength,
		AutoCommitAt4:      spec.AutoCommitUnique,
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

	// 加载主码表
	dictSpec := s.GetDefaultDictSpec()
	if dictSpec != nil {
		srcPath := resolvePath(exeDir, dataDir, dictSpec.Path)
		var norm *dict.WeightNormalizer
		if dictSpec.WeightSpec != nil {
			norm = dictSpec.WeightSpec.NewWeightNormalizer()
		}
		if dictSpec.WeightAsOrder {
			config.WeightAsOrder = true
		}
		if err := loadCodetable(engine, srcPath, dictSpec.Type, s.Schema.ID, logger, norm); err != nil {
			return nil, fmt.Errorf("加载码表失败: %w", err)
		}
		logger.Info("码表加载成功", "schemaID", s.Schema.ID, "entryCount", engine.GetEntryCount())
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
		// 同步排序模式到 CompositeDict，避免启动时使用默认的词频排序
		dm.SetSortMode(candidate.CandidateSortMode(spec.CandidateSortMode))

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

	// 后台预生成拼音 wdb
	go preGeneratePinyinWdb(s, exeDir, dataDir, logger)

	// GC 释放临时内存
	go func() {
		runtime.GC()
		debug.FreeOSMemory()
	}()

	return &EngineBundle{
		SchemaID:    s.Schema.ID,
		Engine:      engine,
		SystemLayer: ctSystemLayer,
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
		if err := loadPinyinDict(pinyinDict, dictPath, logger, norm, spec.DictFormat); err != nil {
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

func loadPinyinDict(pinyinDict *dict.PinyinDict, dictPath string, logger *slog.Logger, normalizer *dict.WeightNormalizer, dictFormat DictFormat) error {
	if dictFormat == "" {
		dictFormat = DictFormatDAT
	}
	dictDir := filepath.Dir(dictPath)
	srcPaths := dictcache.RimePinyinSourcePaths(dictPath)

	// DAT 模式
	if dictFormat == DictFormatDAT {
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

	// 原有 wdb 流程
	wdbInDir := filepath.Join(dictDir, "pinyin.wdb")
	if !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if err := pinyinDict.LoadBinary(wdbInDir); err == nil {
			logger.Info("拼音词库(预编译 wdb)加载成功", "entryCount", pinyinDict.EntryCount())
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath("pinyin")
	if dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		if err := dictcache.ConvertPinyinToWdb(dictPath, wdbCachePath, logger, normalizer); err != nil {
			if _, statErr := os.Stat(wdbInDir); statErr == nil {
				if err := pinyinDict.LoadBinary(wdbInDir); err == nil {
					return nil
				}
			}
			return fmt.Errorf("无法加载拼音词库: %w", err)
		}
	}

	if err := pinyinDict.LoadBinary(wdbCachePath); err != nil {
		// 缓存文件可能损坏（截断），删除后重新生成
		logger.Warn("缓存拼音词库损坏，删除后重新生成", "path", wdbCachePath, "error", err)
		os.Remove(wdbCachePath)
		if err := dictcache.ConvertPinyinToWdb(dictPath, wdbCachePath, logger, normalizer); err != nil {
			return fmt.Errorf("重新生成拼音词库失败: %w", err)
		}
		if err := pinyinDict.LoadBinary(wdbCachePath); err != nil {
			return fmt.Errorf("加载重新生成的拼音词库失败: %w", err)
		}
	}
	logger.Info("拼音词库(缓存 wdb)加载成功", "entryCount", pinyinDict.EntryCount())
	return nil
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

func loadCodetable(engine *codetable.Engine, srcPath string, dictType DictType, schemaID string, logger *slog.Logger, normalizer *dict.WeightNormalizer) error {
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

	wdbInDir := filepath.Join(srcDir, schemaID+".wdb")
	if len(srcPaths) > 0 && !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if err := loadCodetableFromWdb(engine, wdbInDir); err == nil {
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath(schemaID)
	if len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
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
			return fmt.Errorf("加载重新生成的 %s.wdb 失败: %w", schemaID, err)
		}
	}
	return nil
}

func loadCodetableFromWdb(engine *codetable.Engine, wdbPath string) error {
	if err := engine.LoadCodeTableBinary(wdbPath); err != nil {
		return err
	}

	// 从 sidecar meta.json 恢复 Header 信息
	meta, err := dictcache.LoadCodeTableMeta(wdbPath)
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
		pinyinDictPath = resolvePath(exeDir, dataDir, "pinyin/rime_ice.dict.yaml")
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
	// 优先使用混输方案自身的码表配置，其次从主方案继承
	codeTableSpec := s.Engine.CodeTable
	if codeTableSpec == nil && primarySchema != nil {
		codeTableSpec = primarySchema.Engine.CodeTable
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
		MaxCodeLength:      codeTableSpec.MaxCodeLength,
		AutoCommitAt4:      codeTableSpec.AutoCommitUnique,
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
		dm.SetSortMode(candidate.CandidateSortMode(codeTableSpec.CandidateSortMode))

		// 注入码表引擎的 FreqHandler 和 LearningStrategy
		if s.Learning.IsFreqEnabled() {
			freqProfile := s.Learning.GetFreqProfile()
			dm.SetFreqProfile(freqProfile)
			codetableFreqHandler := dict.NewFreqHandler(dm.GetStore(), s.Schema.ID)
			codetableEngine.SetFreqHandler(codetableFreqHandler)
		}
		// 混输码表子引擎：auto_learn 或 auto_phrase 启用时使用码表自动造词
		// 按混输的 dataSchemaID（= 主方案 ID）绑定层，避免预加载时被绑到错误的活跃方案
		if s.Learning.IsAutoPhraseEnabled() || s.Learning.IsAutoLearnEnabled() {
			autoPhrase := NewCodeTableLearningStrategy(&s.Learning, logger)
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
		if err := loadPinyinDict(pinyinDict, dictPath, logger, pinyinNorm, pinyinSpec.DictFormat); err != nil {
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
