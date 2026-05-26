package main

import (
	"errors"
	"flag"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/coordinator"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	imrpc "github.com/huanfeng/wind_input/internal/rpc"
	"github.com/huanfeng/wind_input/internal/schema"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/encoding"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// mutexName / showErrorMessageBox / setDPIAwareness / checkSingleton /
// waitForPreviousExit / isPipeAlreadyExists 已迁至 main_{windows,darwin}.go,
// 接口签名跨平台对齐 (checkSingleton 改为返回 release func() 而非平台特定句柄)。

// statusAdapter 将 coordinator 和 dictManager 适配为 rpc.StatusProvider 接口
type statusAdapter struct {
	coord *coordinator.Coordinator
	dm    *dict.DictManager
}

func (a *statusAdapter) GetSchemaID() string   { return a.dm.GetActiveSchemaID() }
func (a *statusAdapter) GetEngineType() string { return a.coord.GetCurrentEngineName() }
func (a *statusAdapter) IsChineseMode() bool   { return a.coord.GetChineseMode() }
func (a *statusAdapter) IsFullWidth() bool     { return a.coord.GetFullWidth() }
func (a *statusAdapter) IsChinesePunct() bool  { return a.coord.GetChinesePunctuation() }

// rpcEventNotifier 把 coordinator.EventNotifier 调用转发到 RPC 事件广播器，
// 让 coordinator 旁路 RPC 路径（热键切换方案、快捷加词等）也能广播事件给设置端订阅者
type rpcEventNotifier struct {
	broadcaster *imrpc.EventBroadcaster
}

func (n *rpcEventNotifier) NotifyConfigUpdate() {
	n.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeConfig, Action: rpcapi.EventActionUpdate})
}

func (n *rpcEventNotifier) NotifyUserDictAdd(schemaID string) {
	n.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeUserDict, SchemaID: schemaID, Action: rpcapi.EventActionAdd})
}

// pinyinCodeGenAdapter 适配 engine.Manager 为 rpc.PinyinCodeGenerator 和 rpc.SchemaIDMapper 接口
type pinyinCodeGenAdapter struct {
	engineMgr *engine.Manager
}

func (a *pinyinCodeGenAdapter) GeneratePinyinCode(word string) string {
	// 当前 active 方案可能是码表（PinyinEngine 未加载），需 lazy init 后再生成。
	// 与 BatchEncode 路径保持一致：拼音 user dict 加词不依赖当前 active 引擎类型。
	_ = a.engineMgr.EnsurePinyinLoaded()
	return a.engineMgr.GeneratePinyinCode(word)
}

func (a *pinyinCodeGenAdapter) DataSchemaID(schemaID string) string {
	return a.engineMgr.DataSchemaID(schemaID)
}

// batchEncoderAdapter 适配 engine.Manager 为 rpc.BatchEncoder 接口
type batchEncoderAdapter struct {
	engineMgr *engine.Manager
}

func (a *batchEncoderAdapter) BatchEncode(schemaID string, words []string) []rpcapi.EncodeResultItem {
	if a.engineMgr.IsSchemaTypePinyin(schemaID) {
		// 用户可能正在使用码表方案，拼音引擎尚未加载，先确保加载
		_ = a.engineMgr.EnsurePinyinLoaded()
		items := make([]rpcapi.EncodeResultItem, len(words))
		for i, w := range words {
			code := a.engineMgr.GeneratePinyinCode(w)
			if code == "" {
				items[i] = rpcapi.EncodeResultItem{Word: w, Status: "error", Error: "无法生成拼音编码"}
			} else {
				items[i] = rpcapi.EncodeResultItem{Word: w, Code: code, Status: "ok"}
			}
		}
		return items
	}

	reverseIndex := a.engineMgr.GetReverseIndex()
	schemaRules := a.engineMgr.GetEncoderRulesForSchema(schemaID)

	encRules := make([]encoding.SchemaEncoderRule, len(schemaRules))
	for i, sr := range schemaRules {
		encRules[i] = encoding.SchemaEncoderRule{
			LengthEqual:   sr.LengthEqual,
			LengthInRange: sr.LengthInRange,
			Formula:       sr.Formula,
		}
	}
	rules := encoding.ConvertSchemaRules(encRules)

	encoder := encoding.NewReverseEncoder(reverseIndex, rules)
	results := encoder.EncodeBatch(words)

	items := make([]rpcapi.EncodeResultItem, len(results))
	for i, r := range results {
		items[i] = rpcapi.EncodeResultItem{
			Word:   r.Word,
			Code:   r.Code,
			Status: string(r.Status),
			Error:  r.Error,
		}
	}
	return items
}

// showErrorMessageBox / setDPIAwareness / checkSingleton / waitForPreviousExit /
// isPipeAlreadyExists 已迁至 main_{windows,darwin}.go。
//
// checkSingleton 签名跨平台对齐为 (release func(), ok bool), 让 main() 用
// `defer release()` 释放平台特定资源 (Win mutex / darwin flock-on-pidfile)。

// recoverPanic 捕获 goroutine 中未处理的 panic，将其写入日志后以非零码退出。
// 用于替代默认行为（panic 信息仅输出到 stderr，当进程由 TSF DLL 启动时 stderr 通常被丢弃）。
func recoverPanic(logger *slog.Logger, component string) {
	if r := recover(); r != nil {
		logger.Error("goroutine panic",
			"component", component,
			"panic", r,
			"stack", string(debug.Stack()))
		os.Exit(1)
	}
}

func main() {
	redirectStderrToCrashLog()

	// 内存管理策略：
	// 启动阶段不设内存上限：wdat 等大型词库的首次构建（~650K 条目排序+二进制写入）
	// 峰值内存可达 300-400MB，若此时 SetMemoryLimit 已生效会触发 Go 运行时
	// "runtime: out of memory" fatal error（无法被 recover 捕获），导致服务静默崩溃。
	// 待大型数据处理完成后，由 applyMemoryConstraints 强制 GC 并恢复 300MB 软限制。
	//
	// SetMemoryLimit 只管理 Go heap（Private Bytes），不影响 mmap 词典文件（OS 文件缓存页）。
	// GOGC 恢复默认值 100：堆增长翻倍才触发 GC，避免过于频繁的 GC 造成 CPU 持续占用。
	debug.SetGCPercent(100)

	// Set DPI awareness BEFORE any UI operations
	setDPIAwareness()

	// Initialize effective DPI with system DPI value
	ui.SetEffectiveDPI(ui.GetSystemDPI())

	// Parse command line arguments (these override config file settings)
	logLevel := flag.String("log", "", "Log level: debug, info, warn, error (overrides config)")
	saveDefaultConfig := flag.Bool("save-config", false, "Save default configuration and exit")
	isRestart := flag.Bool("restart", false, "Internal flag: wait for previous instance to exit before starting")
	testCrash := flag.Bool("test-crash", false, "触发测试 panic，验证 crash.log 重定向是否生效（测试用途）")
	flag.Parse()

	// 测试 crash.log：触发 panic，其 stack trace 应写入 crash.log
	if *testCrash {
		panic("test crash: verifying crash.log redirect")
	}

	// Load configuration
	cfg, err := config.Load()
	if err != nil {
		// Can't log yet, just print to stderr
		os.Stderr.WriteString("Warning: failed to load config: " + err.Error() + "\n")
	}

	// Handle --save-config flag
	if *saveDefaultConfig {
		if err := config.SaveDefault(); err != nil {
			os.Stderr.WriteString("Failed to save config: " + err.Error() + "\n")
			os.Exit(1)
		}
		configPath, _ := config.GetConfigPath()
		os.Stdout.WriteString("Default configuration saved to: " + configPath + "\n")
		os.Exit(0)
	}

	// Command line overrides config
	if *logLevel != "" {
		cfg.Advanced.LogLevel = *logLevel
	}
	// If restarting, wait for previous instance to fully exit
	if *isRestart {
		waitForPreviousExit()
	}

	// Check if another instance is already running (silently exit, no popup)
	if isPipeAlreadyExists() {
		os.Exit(0)
	}

	// Create singleton mutex / pidfile lock (平台特定)
	releaseSingleton, ok := checkSingleton()
	if !ok {
		os.Exit(0)
	}
	defer releaseSingleton()

	// 初始化日志系统
	logger := setupLogger(cfg.Advanced.LogLevel)

	logger.Info(buildvariant.DisplayName()+" IME Service starting", "version", version)

	// Log config location
	if configPath, err := config.GetConfigPath(); err == nil {
		logger.Info("Configuration", "path", configPath)
	}

	// Get executable directory
	exePath, err := os.Executable()
	if err != nil {
		logger.Error("Failed to get executable path", "error", err)
		os.Exit(1)
	}
	exeDir := filepath.Dir(exePath)

	// Program data directory (exeDir/data)
	dataRoot := config.GetDataDir(exeDir)

	// Initialize common chars table for filtering
	commonCharsPath := filepath.Join(dataRoot, "schemas", "common_chars.txt")
	if err := dict.InitCommonCharsWithPath(commonCharsPath); err != nil {
		// 文件缺失不致命：内置约 189 字 fallback 已足以维持基础过滤；但必须告警，
		// 否则下次再被 INFO "count=189" 误导很难定位（首次安装/杀软隔离都会触发）
		logger.Warn("Common chars file unreadable, falling back to builtin minimal set", "path", commonCharsPath, "error", err)
	}
	logger.Info("Common chars table initialized", "path", commonCharsPath, "count", dict.GetCommonCharCount())

	// Early bridge server startup: create named pipe BEFORE heavy initialization.
	// On first install, wdb generation can take seconds. Without early pipe startup,
	// any TSF client (e.g., Notepad) would block in OnSetFocus waiting for the pipe.
	// DeferredHandler returns safe defaults (PassThrough keys, "…" icon) until ready.
	deferredHandler := bridge.NewDeferredHandler(logger)
	bridgeServer := bridge.NewServer(deferredHandler, logger)
	hostRenderMgr := bridge.NewHostRenderManager(logger, cfg.Advanced.HostRenderProcesses)
	bridgeServer.SetHostRenderManager(hostRenderMgr)

	go func() {
		defer recoverPanic(logger, "bridge-server")
		logger.Info("Starting Bridge IPC server (early)...")
		if err := bridgeServer.Start(); err != nil {
			logger.Error("Bridge server failed", "error", err)
			os.Exit(1)
		}
	}()

	// Create engine manager
	engineMgr := engine.NewManager(logger)
	engineMgr.SetDataRoot(dataRoot)

	// Initialize SchemaManager
	dataDir, err := config.GetConfigDir()
	if err != nil {
		logger.Warn("Failed to get config dir, using exe dir", "error", err)
		dataDir = exeDir
	}
	// 确保用户数据目录存在（首次安装时该目录尚未创建，bbolt 等组件需要写入）
	if err := os.MkdirAll(dataDir, 0755); err != nil {
		logger.Error("Failed to create user data dir", "path", dataDir, "error", err)
	}
	schemaMgr := schema.NewSchemaManager(dataRoot, dataDir, logger)
	if err := schemaMgr.LoadSchemas(); err != nil {
		logger.Error("Failed to load schemas", "error", err)
		showErrorMessageBox("输入方案加载失败，服务无法启动。\n\n原因：" + err.Error())
		os.Exit(1)
	}
	engineMgr.SetSchemaManager(schemaMgr)

	// 设置主码表 / 主拼音方案：拼音的反查由主码表派生，码表的临时拼音指向主拼音方案。
	// 空字符串表示自动推断（按 SchemaManager 列出顺序选第一个匹配类型）。
	engineMgr.SetPrimarySchemas(cfg.Schema.PrimaryCodetable, cfg.Schema.PrimaryPinyin)

	// Initialize DictManager (manages user dict, phrases, shadow rules)
	dictManager := dict.NewDictManager(dataDir, dataRoot, logger)
	defer func() {
		engineMgr.SaveUserFreqs()
		dictManager.Close()
		logger.Info("DictManager closed, user data saved")
	}()

	// 启用 bbolt Store 后端（用户词库、词频、Shadow 统一存储）
	dbPath := filepath.Join(dataDir, "user_data.db")
	if err := dictManager.OpenStore(dbPath); err != nil {
		logger.Error("Failed to open bbolt store, user data features will be unavailable", "path", dbPath, "error", err)
	}

	if err := dictManager.Initialize(); err != nil {
		logger.Warn("Failed to initialize dict manager", "error", err)
	}
	if pl := dictManager.GetPhraseLayer(); pl != nil {
		pl.SetMinPrefixLength(cfg.Input.Phrase.MinPrefixLength)
	}
	engineMgr.SetDictManager(dictManager)

	// 确定活跃方案 ID
	activeSchemaID := cfg.Schema.Active
	if activeSchemaID == "" {
		if len(cfg.Schema.Available) > 0 {
			activeSchemaID = cfg.Schema.Available[0]
		} else {
			activeSchemaID = "wubi86"
		}
	}

	// 通过 Schema 驱动引擎创建和词库切换
	if err := schemaMgr.SetActive(activeSchemaID); err != nil {
		logger.Warn("Active schema not found, using first available", "schema", activeSchemaID, "error", err)
		schemas := schemaMgr.ListSchemas()
		if len(schemas) > 0 {
			activeSchemaID = schemas[0].ID
			schemaMgr.SetActive(activeSchemaID)
		}
	}

	activeSchema := schemaMgr.GetActiveSchema()
	if activeSchema != nil {
		// 切换 DictManager 的用户数据层
		dictManager.SwitchSchemaFull(activeSchemaID, activeSchema.DataSchemaID(),
			activeSchema.Learning.TempMaxEntries, activeSchema.Learning.TempPromoteCount)
	}

	stats := dictManager.GetStats()
	logger.Info("DictManager initialized",
		"phrases", stats["phrases"],
		"commands", stats["commands"],
		"user_words", stats["user_words"],
		"shadow_rules", stats["shadow_rules"])

	// 创建并激活引擎
	// pendingWdatActiveID 非空表示活跃方案因 wdat 后台生成中而暂未激活，
	// 等 coord 创建之后再注册 OnPinyinWdatReady，让回调能拿到 coord 引用
	// 调用 NotifySchemaActivated 触发 toolbar / TSF 状态同步与就绪提示。
	var pendingWdatActiveID string
	if err := engineMgr.SwitchSchema(activeSchemaID); err != nil {
		if errors.Is(err, schema.ErrAssetBuilding) {
			// 资源后台生成中：不切换到其它方案（用户的方案才是其习惯所在），
			// 期间引擎为 nil，按键路径在 engineMgr.ConvertEx 中对 nil 引擎
			// 短路返回空候选——不影响进程稳定性。
			logger.Info("活跃方案资源准备中，等待后台生成完成后激活，期间不可输入",
				"schema", activeSchemaID, "reason", err.Error())
			pendingWdatActiveID = activeSchemaID
		} else {
			logger.Warn("Failed to initialize engine, trying fallback",
				"schema", activeSchemaID, "error", err)
			// 真正的加载失败：保留原 fallback 行为，避免服务空跑
			fallbackOK := false
			for _, s := range schemaMgr.ListSchemas() {
				if s.ID != activeSchemaID {
					if err2 := engineMgr.SwitchSchema(s.ID); err2 == nil {
						activeSchemaID = s.ID
						schemaMgr.SetActive(s.ID)
						cfg.Schema.Active = s.ID // 同步到内存配置，使 RPC ConfigGetAll 返回正确的活跃方案
						fallbackOK = true
						break
					}
				}
			}
			if !fallbackOK {
				logger.Error("All engines failed to initialize")
				showErrorMessageBox("输入法引擎初始化失败，服务无法启动。\n\n原因：" + err.Error())
				os.Exit(1)
			}
		}
	}

	logger.Info("Engine initialized", "schema", activeSchemaID, "info", engineMgr.GetEngineInfo())

	// 注入学习策略（造词 + 调频），必须在 DictManager 和引擎均就绪后执行
	if activeSchema != nil {
		engineMgr.UpdateLearningConfig(&activeSchema.Learning)
	}

	// 从全局配置应用候选过滤模式（覆盖 schema 默认值）
	if cfg.Input.FilterMode != "" {
		engineMgr.UpdateFilterMode(cfg.Input.FilterMode)
	}

	// Create UI Manager (native Windows UI)
	uiManager := ui.NewManager(logger)

	// Start UI Manager in a separate goroutine (it has its own message loop)
	go func() {
		defer recoverPanic(logger, "ui-manager")
		logger.Info("Starting UI Manager...")
		if err := uiManager.Start(); err != nil {
			logger.Error("UI Manager failed", "error", err)
		}
	}()

	// Wait for UI to be ready
	logger.Info("Waiting for UI Manager to be ready...")
	uiManager.WaitReady()
	logger.Info("UI Manager is ready")

	// Load app compatibility rules
	appCompat := config.LoadAppCompat()
	logger.Info("App compatibility rules loaded", "count", len(appCompat.Apps))

	// Create coordinator with Engine Manager, UI Manager and config
	coord := coordinator.NewCoordinator(engineMgr, uiManager, cfg, appCompat, logger)
	coord.SetVersion(version)

	// applyMemoryConstraints 在大型数据处理完成后调用：强制 GC 释放构建期临时内存，
	// 然后设置 300MB 运行时软限制。
	applyMemoryConstraints := func() {
		runtime.GC()
		debug.FreeOSMemory()
		debug.SetMemoryLimit(300 * 1024 * 1024)
		logger.Info("运行时内存限制已生效", "heapLimit", "300MB")
	}

	// 启动期若活跃方案因 wdat 后台生成而未激活，等就绪回调触发后再激活，
	// 并通过 coord 推送状态到 toolbar/TSF 客户端、显示"<方案>已就绪"指示器。
	if pendingWdatActiveID != "" {
		desiredActiveID := pendingWdatActiveID
		schema.OnPinyinWdatReady(func() {
			defer recoverPanic(logger, "wdat-ready-retry")
			if err := engineMgr.SwitchSchema(desiredActiveID); err != nil {
				logger.Warn("拼音 wdat 就绪后激活用户方案失败",
					"schema", desiredActiveID, "error", err)
				applyMemoryConstraints()
				return
			}
			schemaMgr.SetActive(desiredActiveID)
			var displayName string
			if s := schemaMgr.GetSchema(desiredActiveID); s != nil {
				displayName = s.Schema.Name
				dictManager.SwitchSchemaFull(desiredActiveID, s.DataSchemaID(),
					s.Learning.TempMaxEntries, s.Learning.TempPromoteCount)
				engineMgr.UpdateLearningConfig(&s.Learning)
			}
			coord.NotifySchemaActivated(displayName)
			logger.Info("拼音 wdat 就绪，用户方案已激活", "schema", desiredActiveID)
			applyMemoryConstraints()
		})
	} else {
		// 无 wdat 等待（方案已正常激活）：若有后台 preGeneratePinyinWdb 任务在构建
		// wdat，等其完成后收紧内存；若无后台构建，OnPinyinWdatReady 立即触发。
		schema.OnPinyinWdatReady(func() {
			applyMemoryConstraints()
		})
	}

	// 初始化输入统计采集器（配置存储在 bbolt 中，始终创建）
	if st := dictManager.GetStore(); st != nil {
		statCollector := store.NewStatCollector(st, logger)
		coord.SetStatCollector(statCollector)
		defer statCollector.Close()
		logger.Info("Input statistics collector started")
	}

	// 启动 RPC 服务端（统一 IPC 通道，供设置端使用）
	rpcServer := imrpc.NewServer(logger, dictManager, dictManager.GetStore())
	rpcServer.SetConfig(cfg)
	rpcServer.SetConfigReloader(coordinator.NewReloadHandler(coord, cfg, rpcServer.CfgMu(), schemaMgr, engineMgr, dictManager, logger))
	// 注入共享 cfgMu，使 coordinator 的 save* goroutine 与 RPC 路径使用同一把锁
	coord.SetCfgMu(rpcServer.CfgMu())
	// 注入事件通知器：热键切换方案、快捷加词等旁路 RPC 路径需主动广播事件给设置端订阅者
	coord.SetEventNotifier(&rpcEventNotifier{broadcaster: rpcServer.Broadcaster()})
	rpcServer.SetSchemaOverrideResetter(schemaMgr)
	rpcServer.SetStatusProvider(&statusAdapter{coord: coord, dm: dictManager})
	rpcServer.SetBatchEncoder(&batchEncoderAdapter{engineMgr: engineMgr})
	pinyinAdapter := &pinyinCodeGenAdapter{engineMgr: engineMgr}
	rpcServer.SetPinyinCodeGenerator(pinyinAdapter)
	rpcServer.SetSchemaIDMapper(pinyinAdapter)
	if sc := coord.GetStatCollector(); sc != nil {
		rpcServer.SetStatCollector(sc)
	}
	defer rpcServer.Stop()
	rpcServer.StartAsync()

	// Wire up coordinator to bridge server and mark service as ready.
	// From this point on, DeferredHandler delegates all requests to the real coordinator.
	coord.SetBridgeServer(bridgeServer)
	deferredHandler.SetReady(coord)
	logger.Info("Service initialization complete, bridge handler is now ready")

	// Push current state to any TSF clients that connected during initialization.
	// Without this, clients would show "…" until the next focus change.
	bridgeServer.PushStateToActiveClient(coord.BuildCurrentStatus())

	// Listen for exit requests in a separate goroutine
	go func() {
		defer recoverPanic(logger, "exit-handler")
		<-coordinator.ExitRequested()
		logger.Info("Exit requested, shutting down...")
		os.Exit(0)
	}()

	// Listen for restart requests in a separate goroutine
	go func() {
		defer recoverPanic(logger, "restart-handler")
		<-coordinator.RestartRequested()
		logger.Info("Restart requested, starting new process...")

		// Get current executable path
		exePath, err := os.Executable()
		if err != nil {
			logger.Error("Failed to get executable path for restart", "error", err)
			return
		}

		// Build args: preserve original args but add --restart flag
		// so the new process knows to wait for us to exit
		args := append([]string{exePath}, os.Args[1:]...)
		hasRestart := false
		for _, arg := range args {
			if arg == "--restart" || arg == "-restart" {
				hasRestart = true
				break
			}
		}
		if !hasRestart {
			args = append(args, "--restart")
		}

		// Start new process with --restart flag
		procAttr := &os.ProcAttr{
			Files: []*os.File{os.Stdin, os.Stdout, os.Stderr},
		}
		_, err = os.StartProcess(exePath, args, procAttr)
		if err != nil {
			logger.Error("Failed to start new process", "error", err)
			return
		}

		logger.Info("New process started with --restart flag, exiting current process...")
		os.Exit(0)
	}()

	// Block main thread forever (exit/restart goroutines handle shutdown via os.Exit)
	select {}
}
