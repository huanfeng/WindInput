package main

import (
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
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
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
		cfg.General.LogLevel = *logLevel
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
	switch cfg.General.LogLevel {
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

	logger := slog.New(slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{
		Level: level,
	}))
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

	// Load dictionary
	d := dict.NewSimpleDict()
	fullDictPath := filepath.Join(exeDir, cfg.Dictionary.SystemDict)
	logger.Info("Loading dictionary", "path", fullDictPath)

	if err := d.Load(fullDictPath); err != nil {
		logger.Warn("Failed to load dictionary, using test data", "error", err)
		// Add test data
		d.AddEntry("ni", "你", 100)
		d.AddEntry("ni", "泥", 20)
		d.AddEntry("ni", "尼", 10)
		d.AddEntry("hao", "好", 100)
		d.AddEntry("hao", "号", 50)
		d.AddEntry("hao", "浩", 30)
		d.AddEntry("shi", "是", 100)
		d.AddEntry("shi", "事", 80)
		d.AddEntry("shi", "时", 70)
		d.AddEntry("wo", "我", 100)
		d.AddEntry("wo", "握", 30)
		d.AddEntry("de", "的", 100)
		d.AddEntry("de", "得", 80)
		d.AddEntry("da", "大", 100)
		d.AddEntry("da", "打", 80)
		d.AddEntry("xiao", "小", 100)
		d.AddEntry("xiao", "笑", 80)
		d.AddEntry("zhong", "中", 100)
		d.AddEntry("zhong", "钟", 80)
		d.AddEntry("guo", "国", 100)
		d.AddEntry("guo", "过", 80)
		d.AddEntry("ren", "人", 100)
		d.AddEntry("ren", "任", 80)
		d.AddPhrase([]string{"ni", "hao"}, "你好", 150)
		d.AddPhrase([]string{"zhong", "guo"}, "中国", 150)
		d.AddPhrase([]string{"zhong", "guo", "ren"}, "中国人", 200)
	}

	// Create pinyin engine
	eng := pinyin.NewEngine(d)

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

	// Create coordinator with UI Manager and config
	coord := coordinator.NewCoordinator(eng, uiManager, cfg, logger)

	// Create Bridge IPC server (connects to C++)
	bridgeServer := bridge.NewServer(coord, logger)

	// Start Bridge server (blocks main thread)
	logger.Info("Starting Bridge IPC server...")
	if err := bridgeServer.Start(); err != nil {
		logger.Error("Bridge server failed", "error", err)
		os.Exit(1)
	}
}
