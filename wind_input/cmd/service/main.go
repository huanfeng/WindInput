package main

import (
	"flag"
	"os"
	"path/filepath"
	"runtime/debug"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/coordinator"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	imrpc "github.com/huanfeng/wind_input/internal/rpc"
	"github.com/huanfeng/wind_input/internal/schema"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
)

var mutexName = "Global\\WindInput" + buildvariant.Suffix() + "IMEService"

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

// showErrorMessageBox 显示错误弹框（MB_ICONERROR）
func showErrorMessageBox(message string) {
	user32 := windows.NewLazySystemDLL("user32.dll")
	messageBox := user32.NewProc("MessageBoxW")
	title, _ := windows.UTF16PtrFromString(buildvariant.DisplayName())
	msg, _ := windows.UTF16PtrFromString(message)
	messageBox.Call(0, uintptr(unsafe.Pointer(msg)), uintptr(unsafe.Pointer(title)), 0x10) // MB_ICONERROR
}

// DPI awareness constants
const (
	PROCESS_DPI_UNAWARE           = 0
	PROCESS_SYSTEM_DPI_AWARE      = 1
	PROCESS_PER_MONITOR_DPI_AWARE = 2
)

// setDPIAwareness sets the process DPI awareness to prevent UI blur
func setDPIAwareness() {
	// Try Windows 8.1+ API first (shcore.dll)
	shcore := syscall.NewLazyDLL("shcore.dll")
	setProcessDpiAwareness := shcore.NewProc("SetProcessDpiAwareness")
	if setProcessDpiAwareness.Find() == nil {
		setProcessDpiAwareness.Call(uintptr(PROCESS_PER_MONITOR_DPI_AWARE))
		return
	}

	// Fallback to Windows Vista+ API (user32.dll)
	user32 := syscall.NewLazyDLL("user32.dll")
	setProcessDPIAware := user32.NewProc("SetProcessDPIAware")
	if setProcessDPIAware.Find() == nil {
		setProcessDPIAware.Call()
	}
}

func checkSingleton() (windows.Handle, bool) {
	name, _ := windows.UTF16PtrFromString(mutexName)

	// Try to create a named mutex
	handle, err := windows.CreateMutex(nil, false, name)
	if err != nil {
		if err == windows.ERROR_ALREADY_EXISTS {
			// Another instance is already running
			if handle != 0 {
				windows.CloseHandle(handle)
			}
			return 0, false
		}
	}

	// Check if mutex already existed
	if handle != 0 {
		// Try to get ownership - if we can't, another instance has it
		event, _ := windows.WaitForSingleObject(handle, 0)
		if event == uint32(windows.WAIT_OBJECT_0) || event == uint32(windows.WAIT_ABANDONED) {
			// We got the mutex
			return handle, true
		}
		windows.CloseHandle(handle)
		return 0, false
	}

	return 0, false
}

// waitForPreviousExit waits for the previous instance to fully exit (pipe and mutex released)
// Used during restart to avoid "another instance already running" detection
func waitForPreviousExit() {
	const maxWait = 10 * time.Second
	const pollInterval = 100 * time.Millisecond

	deadline := time.Now().Add(maxWait)
	for time.Now().Before(deadline) {
		if !isPipeAlreadyExists() {
			// Pipe is gone, previous instance has exited
			// Wait a bit more for mutex to be released
			time.Sleep(pollInterval)
			return
		}
		time.Sleep(pollInterval)
	}
	// Timeout: proceed anyway, singleton check will handle it
}

// Also check if our pipe already exists (another way to detect running instance)
func isPipeAlreadyExists() bool {
	pipePath, _ := windows.UTF16PtrFromString(bridge.BridgePipeName)

	handle, err := windows.CreateFile(
		pipePath,
		windows.GENERIC_READ|windows.GENERIC_WRITE,
		0,
		nil,
		windows.OPEN_EXISTING,
		0,
		0,
	)

	if err == nil {
		// Pipe exists and we connected, another instance is running
		windows.CloseHandle(handle)
		return true
	}

	// ERROR_FILE_NOT_FOUND means pipe doesn't exist (no server running)
	// ERROR_PIPE_BUSY means pipe exists but busy (server running)
	if err == windows.ERROR_PIPE_BUSY {
		return true
	}

	return false
}

func main() {
	// 内存管理策略：降低内存波动
	// 软限制 150MB，超过后 GC 更频繁运行
	debug.SetMemoryLimit(150 * 1024 * 1024)
	// 降低 GOGC：默认 100 表示堆翻倍才 GC，改为 50 更频繁回收
	debug.SetGCPercent(50)

	// Set DPI awareness BEFORE any UI operations
	setDPIAwareness()

	// Initialize effective DPI with system DPI value
	ui.SetEffectiveDPI(ui.GetSystemDPI())

	// Parse command line arguments (these override config file settings)
	logLevel := flag.String("log", "", "Log level: debug, info, warn, error (overrides config)")
	saveDefaultConfig := flag.Bool("save-config", false, "Save default configuration and exit")
	isRestart := flag.Bool("restart", false, "Internal flag: wait for previous instance to exit before starting")
	flag.Parse()

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

	// Create singleton mutex
	mutexHandle, ok := checkSingleton()
	if !ok {
		os.Exit(0)
	}
	defer windows.CloseHandle(mutexHandle)

	// 初始化日志系统
	logger := setupLogger(cfg.Advanced.LogLevel)

	logger.Info(buildvariant.DisplayName() + " IME Service starting...")

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
	dict.InitCommonCharsWithPath(commonCharsPath)
	logger.Info("Common chars table initialized", "path", commonCharsPath, "count", dict.GetCommonCharCount())

	// Create engine manager
	engineMgr := engine.NewManager(logger)
	engineMgr.SetExeDir(dataRoot)

	// Initialize SchemaManager
	dataDir, err := config.GetConfigDir()
	if err != nil {
		logger.Warn("Failed to get config dir, using exe dir", "error", err)
		dataDir = exeDir
	}
	schemaMgr := schema.NewSchemaManager(dataRoot, dataDir, logger)
	if err := schemaMgr.LoadSchemas(); err != nil {
		logger.Error("Failed to load schemas", "error", err)
		showErrorMessageBox("输入方案加载失败，服务无法启动。\n\n原因：" + err.Error())
		os.Exit(1)
	}
	engineMgr.SetSchemaManager(schemaMgr)

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
		logger.Error("Failed to open store, falling back to file backend", "error", err)
	}

	if err := dictManager.Initialize(); err != nil {
		logger.Warn("Failed to initialize dict manager", "error", err)
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
		dictManager.SwitchSchemaFull(activeSchemaID, activeSchema.UserData.ShadowFile, activeSchema.UserData.UserDictFile,
			activeSchema.UserData.TempDictFile, activeSchema.Learning.TempMaxEntries, activeSchema.Learning.TempPromoteCount)
	}

	stats := dictManager.GetStats()
	logger.Info("DictManager initialized",
		"phrases", stats["phrases"],
		"commands", stats["commands"],
		"user_words", stats["user_words"],
		"shadow_rules", stats["shadow_rules"])

	// 创建并激活引擎
	if err := engineMgr.SwitchSchema(activeSchemaID); err != nil {
		logger.Warn("Failed to initialize engine, trying fallback", "schema", activeSchemaID, "error", err)
		// 尝试回退到其他方案
		fallbackOK := false
		for _, s := range schemaMgr.ListSchemas() {
			if s.ID != activeSchemaID {
				if err2 := engineMgr.SwitchSchema(s.ID); err2 == nil {
					activeSchemaID = s.ID
					schemaMgr.SetActive(s.ID)
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

	logger.Info("Engine initialized", "schema", activeSchemaID, "info", engineMgr.GetEngineInfo())

	// 从全局配置应用候选过滤模式（覆盖 schema 默认值）
	if cfg.Input.FilterMode != "" {
		engineMgr.UpdateFilterMode(cfg.Input.FilterMode)
	}

	// Create UI Manager (native Windows UI)
	uiManager := ui.NewManager(logger)

	// Start UI Manager in a separate goroutine (it has its own message loop)
	go func() {
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

	// 启动 RPC 服务端（统一 IPC 通道，供设置端使用）
	rpcServer := imrpc.NewServer(logger, dictManager, dictManager.GetStore())
	rpcServer.SetConfigReloader(coordinator.NewReloadHandler(coord, cfg, schemaMgr, engineMgr, dictManager, logger))
	rpcServer.SetStatusProvider(&statusAdapter{coord: coord, dm: dictManager})
	defer rpcServer.Stop()
	rpcServer.StartAsync()

	// Create Bridge IPC server (connects to C++)
	bridgeServer := bridge.NewServer(coord, logger)

	// Create host render manager for Band window proxy rendering (Start Menu etc.)
	hostRenderMgr := bridge.NewHostRenderManager(logger, cfg.Advanced.HostRenderProcesses)
	bridgeServer.SetHostRenderManager(hostRenderMgr)

	// Set bridge server on coordinator for state broadcasting
	coord.SetBridgeServer(bridgeServer)

	// Listen for exit requests in a separate goroutine
	go func() {
		<-coordinator.ExitRequested()
		logger.Info("Exit requested, shutting down...")
		os.Exit(0)
	}()

	// Listen for restart requests in a separate goroutine
	go func() {
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

	// Start Bridge server (blocks main thread)
	logger.Info("Starting Bridge IPC server...")
	if err := bridgeServer.Start(); err != nil {
		logger.Error("Bridge server failed", "error", err)
		os.Exit(1)
	}
}
