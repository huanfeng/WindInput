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
	"github.com/huanfeng/wind_input/internal/coordinator"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
	"github.com/huanfeng/wind_input/internal/settings"
	"github.com/huanfeng/wind_input/internal/ui"
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

	// 先创建 settings server 以获取 LogHandler（稍后会注册完整的 services）
	tempLogger := slog.New(stdoutHandler)
	settingsServer := settings.NewServer(tempLogger)

	// 创建包含 LogHandler 的自定义 slog handler
	logHandler := settingsServer.GetLogHandler()
	customHandler := settings.NewSlogHandler(logHandler, stdoutHandler, level)
	// 如果有文件 handler，用 MultiHandler 包装
	var logger *slog.Logger
	if fileHandler != nil {
		logger = slog.New(newMultiHandler(customHandler, fileHandler))
	} else {
		logger = slog.New(customHandler)
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
		MaxCodeLength: 4,
		TopCodeCommit: cfg.Engine.Wubi.TopCodeCommit,
		PunctCommit:   cfg.Engine.Wubi.PunctCommit,
		FilterMode:    cfg.Engine.FilterMode,
	}
	// 解析自动上屏模式
	switch cfg.Engine.Wubi.AutoCommit {
	case "none":
		wubiConfig.AutoCommit = wubi.AutoCommitNone
	case "unique":
		wubiConfig.AutoCommit = wubi.AutoCommitUnique
	case "unique_at_4":
		wubiConfig.AutoCommit = wubi.AutoCommitUniqueAt4
	case "unique_full_match":
		wubiConfig.AutoCommit = wubi.AutoCommitUniqueFullMatch
	default:
		wubiConfig.AutoCommit = wubi.AutoCommitUniqueAt4
	}
	// 解析空码处理模式
	switch cfg.Engine.Wubi.EmptyCode {
	case "none":
		wubiConfig.EmptyCode = wubi.EmptyCodeNone
	case "clear":
		wubiConfig.EmptyCode = wubi.EmptyCodeClear
	case "clear_at_4":
		wubiConfig.EmptyCode = wubi.EmptyCodeClearAt4
	case "to_english":
		wubiConfig.EmptyCode = wubi.EmptyCodeToEnglish
	default:
		wubiConfig.EmptyCode = wubi.EmptyCodeClearAt4
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

	// 注册完整的 services 到 settings server
	settingsServer.RegisterServices(&settings.Services{
		Config:      cfg,
		EngineMgr:   engineMgr,
		Coordinator: coord,
		Logger:      logger,
		OnConfigSave: func(c *config.Config) error {
			return config.Save(c)
		},
	})

	settingsServer.StartAsync()
	logger.Info("Settings server started", "addr", settings.DefaultAddr)

	// Create Bridge IPC server (connects to C++)
	bridgeServer := bridge.NewServer(coord, logger)

	// Start Bridge server (blocks main thread)
	logger.Info("Starting Bridge IPC server...")
	if err := bridgeServer.Start(); err != nil {
		logger.Error("Bridge server failed", "error", err)
		os.Exit(1)
	}
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
