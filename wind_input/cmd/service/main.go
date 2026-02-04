package main

import (
	"context"
	"flag"
	"log/slog"
	"os"
	"path/filepath"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/config"
	"github.com/huanfeng/wind_input/internal/control"
	"github.com/huanfeng/wind_input/internal/coordinator"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
	"github.com/huanfeng/wind_input/internal/ui"
	pkgcontrol "github.com/huanfeng/wind_input/pkg/control"
)

const mutexName = "Global\\WindInputIMEService"

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
	// Set DPI awareness BEFORE any UI operations
	setDPIAwareness()

	// Parse command line arguments (these override config file settings)
	dictPath := flag.String("dict", "", "Dictionary file path (overrides config)")
	logLevel := flag.String("log", "", "Log level: debug, info, warn, error (overrides config)")
	saveDefaultConfig := flag.Bool("save-config", false, "Save default configuration and exit")
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
	if *dictPath != "" {
		cfg.Dictionary.SystemDict = *dictPath
	}

	// Check if another instance is already running
	if isPipeAlreadyExists() {
		// Use MessageBox to show error (since this might be started without console)
		user32 := windows.NewLazySystemDLL("user32.dll")
		messageBox := user32.NewProc("MessageBoxW")
		title, _ := windows.UTF16PtrFromString("WindInput IME Service")
		msg, _ := windows.UTF16PtrFromString("Another instance is already running.")
		messageBox.Call(0, uintptr(unsafe.Pointer(msg)), uintptr(unsafe.Pointer(title)), 0x40) // MB_ICONINFORMATION
		os.Exit(0)
	}

	// Create singleton mutex
	mutexHandle, ok := checkSingleton()
	if !ok {
		os.Exit(0)
	}
	defer windows.CloseHandle(mutexHandle)

	// Setup logging based on config
	var level slog.Level
	switch cfg.Advanced.LogLevel {
	case "debug":
		level = slog.LevelDebug
	case "info":
		level = slog.LevelInfo
	case "warn":
		level = slog.LevelWarn
	case "error":
		level = slog.LevelError
	default:
		level = slog.LevelInfo
	}

	// 创建基础的 stdout handler
	stdoutHandler := slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{
		Level: level,
	})

	// 创建文件日志 handler（日志文件在 %APPDATA%\WindInput\wind_input.log）
	var fileHandler slog.Handler
	logDir := filepath.Join(os.Getenv("APPDATA"), "WindInput")
	os.MkdirAll(logDir, 0755)
	logFilePath := filepath.Join(logDir, "wind_input.log")
	logFile, err := os.OpenFile(logFilePath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err == nil {
		fileHandler = slog.NewTextHandler(logFile, &slog.HandlerOptions{
			Level: level,
		})
	}

	// 创建 logger（stdout + file）
	var logger *slog.Logger
	if fileHandler != nil {
		logger = slog.New(newMultiHandler(stdoutHandler, fileHandler))
	} else {
		logger = slog.New(stdoutHandler)
	}
	slog.SetDefault(logger)

	logger.Info("WindInput IME Service starting...")

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

	// Initialize common chars table for filtering
	commonCharsPath := filepath.Join(exeDir, "dict", "common_chars.txt")
	dict.InitCommonCharsWithPath(commonCharsPath)
	logger.Info("Common chars table initialized", "path", commonCharsPath, "count", dict.GetCommonCharCount())

	// Create engine manager
	engineMgr := engine.NewManager()

	// Set paths for dynamic engine switching
	engineMgr.SetExeDir(exeDir)
	engineMgr.SetDictPaths(
		filepath.Join(exeDir, config.GetPinyinDictPath()),
		filepath.Join(exeDir, config.GetWubiDictPath()),
	)

	// Initialize DictManager (manages user dict, phrases, shadow rules)
	// Use config dir as data dir (same location as config.yaml)
	dataDir, err := config.GetConfigDir()
	if err != nil {
		logger.Warn("Failed to get config dir, using exe dir", "error", err)
		dataDir = exeDir
	}
	dictManager := dict.NewDictManager(dataDir)
	if err := dictManager.Initialize(); err != nil {
		logger.Warn("Failed to initialize dict manager", "error", err)
	} else {
		stats := dictManager.GetStats()
		logger.Info("DictManager initialized",
			"phrases", stats["phrases"],
			"commands", stats["commands"],
			"user_words", stats["user_words"],
			"shadow_rules", stats["shadow_rules"])
	}
	engineMgr.SetDictManager(dictManager)

	// Initialize engine based on config
	// 根据引擎类型选择正确的词库路径
	var fullDictPath string
	switch cfg.Engine.Type {
	case "wubi":
		fullDictPath = filepath.Join(exeDir, config.GetWubiDictPath())
	case "pinyin":
		fullDictPath = filepath.Join(exeDir, config.GetPinyinDictPath())
	default:
		fullDictPath = filepath.Join(exeDir, cfg.Dictionary.SystemDict)
	}
	logger.Info("Loading dictionary", "path", fullDictPath, "engine_type", cfg.Engine.Type)

	// 解析拼音配置
	pinyinConfig := &pinyin.Config{
		ShowWubiHint: cfg.Engine.Pinyin.ShowWubiHint,
		FilterMode:   cfg.Engine.FilterMode,
	}

	// 解析五笔配置（无论当前引擎类型，都需要配置以便动态切换）
	wubiConfig := &wubi.Config{
		MaxCodeLength:   4,
		AutoCommitAt4:   cfg.Engine.Wubi.AutoCommitAt4,
		ClearOnEmptyAt4: cfg.Engine.Wubi.ClearOnEmptyAt4,
		TopCodeCommit:   cfg.Engine.Wubi.TopCodeCommit,
		PunctCommit:     cfg.Engine.Wubi.PunctCommit,
		FilterMode:      cfg.Engine.FilterMode,
	}

	// 设置引擎配置（用于动态切换）
	engineMgr.SetPinyinConfig(pinyinConfig)
	engineMgr.SetWubiConfig(wubiConfig)

	// 初始化引擎
	engineConfig := &engine.EngineConfig{
		Type:         engine.EngineType(cfg.Engine.Type),
		DictPath:     fullDictPath,
		WubiDictPath: filepath.Join(exeDir, config.GetWubiDictPath()),
		PinyinConfig: pinyinConfig,
		WubiConfig:   wubiConfig,
	}

	if err := engineMgr.InitializeFromConfig(engineConfig); err != nil {
		logger.Warn("Failed to initialize engine from config, falling back to pinyin", "error", err)
		// 回退到拼音引擎
		engineConfig.Type = engine.EngineTypePinyin
		if err := engineMgr.InitializeFromConfig(engineConfig); err != nil {
			logger.Error("Failed to initialize fallback engine", "error", err)
			os.Exit(1)
		}
	}

	logger.Info("Engine initialized", "info", engineMgr.GetEngineInfo())

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

	// Create coordinator with Engine Manager, UI Manager and config
	coord := coordinator.NewCoordinator(engineMgr, uiManager, cfg, logger)

	// 创建控制管道服务端
	controlServer := control.NewServer(logger, dictManager)
	controlServer.SetReloadHandler(&reloadHandlerImpl{
		coord:  coord,
		cfg:    cfg,
		logger: logger,
	})
	controlServer.StartAsync()
	logger.Info("Control pipe server started", "pipe", pkgcontrol.PipeName)

	// Create Bridge IPC server (connects to C++)
	bridgeServer := bridge.NewServer(coord, logger)

	// Set bridge server on coordinator for state broadcasting
	coord.SetBridgeServer(bridgeServer)

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

		// Start new process with same arguments
		procAttr := &os.ProcAttr{
			Files: []*os.File{os.Stdin, os.Stdout, os.Stderr},
		}
		_, err = os.StartProcess(exePath, os.Args, procAttr)
		if err != nil {
			logger.Error("Failed to start new process", "error", err)
			return
		}

		logger.Info("New process started, exiting current process...")
		os.Exit(0)
	}()

	// Start Bridge server (blocks main thread)
	logger.Info("Starting Bridge IPC server...")
	if err := bridgeServer.Start(); err != nil {
		logger.Error("Bridge server failed", "error", err)
		os.Exit(1)
	}
}

// reloadHandlerImpl 实现 control.ReloadHandler 接口
type reloadHandlerImpl struct {
	coord  *coordinator.Coordinator
	cfg    *config.Config
	logger *slog.Logger
}

// ReloadConfig 重载配置
func (h *reloadHandlerImpl) ReloadConfig() error {
	newCfg, err := config.Load()
	if err != nil {
		return err
	}

	// 更新协调器的配置
	if h.coord != nil {
		// 更新引擎配置（包括引擎类型切换）
		h.coord.UpdateEngineConfig(&newCfg.Engine)
		// 更新快捷键配置
		h.coord.UpdateHotkeyConfig(&newCfg.Hotkeys)
		// 更新启动配置
		h.coord.UpdateStartupConfig(&newCfg.Startup)
		// 更新 UI 配置
		h.coord.UpdateUIConfig(&newCfg.UI)
		// 更新工具栏配置
		h.coord.UpdateToolbarConfig(&newCfg.Toolbar)
		// 更新输入配置
		h.coord.UpdateInputConfig(&newCfg.Input)
	}

	// 更新保存的配置引用
	*h.cfg = *newCfg

	h.logger.Info("Config reloaded successfully",
		"engineType", newCfg.Engine.Type,
		"toggleModeKeys", newCfg.Hotkeys.ToggleModeKeys)
	return nil
}

// GetStatus 获取服务状态
func (h *reloadHandlerImpl) GetStatus() *pkgcontrol.ServiceStatus {
	status := &pkgcontrol.ServiceStatus{
		Running: true,
	}

	if h.coord != nil {
		status.ChineseMode = h.coord.GetChineseMode()
		status.FullWidth = h.coord.GetFullWidth()
		status.ChinesePunct = h.coord.GetChinesePunctuation()
		status.EngineType = h.coord.GetCurrentEngineName()
	}

	return status
}

// multiHandler wraps multiple slog handlers
type multiHandler struct {
	handlers []slog.Handler
}

func newMultiHandler(handlers ...slog.Handler) *multiHandler {
	return &multiHandler{handlers: handlers}
}

func (h *multiHandler) Enabled(ctx context.Context, level slog.Level) bool {
	for _, handler := range h.handlers {
		if handler.Enabled(ctx, level) {
			return true
		}
	}
	return false
}

func (h *multiHandler) Handle(ctx context.Context, r slog.Record) error {
	for _, handler := range h.handlers {
		if handler.Enabled(ctx, r.Level) {
			handler.Handle(ctx, r)
		}
	}
	return nil
}

func (h *multiHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	newHandlers := make([]slog.Handler, len(h.handlers))
	for i, handler := range h.handlers {
		newHandlers[i] = handler.WithAttrs(attrs)
	}
	return newMultiHandler(newHandlers...)
}

func (h *multiHandler) WithGroup(name string) slog.Handler {
	newHandlers := make([]slog.Handler, len(h.handlers))
	for i, handler := range h.handlers {
		newHandlers[i] = handler.WithGroup(name)
	}
	return newMultiHandler(newHandlers...)
}
