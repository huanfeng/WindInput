package main

import (
	"embed"
	"os"
	"path/filepath"
	"strings"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
	wailsWindows "github.com/wailsapp/wails/v2/pkg/options/windows"
	"golang.org/x/sys/windows"
)

//go:embed all:frontend/dist
var assets embed.FS

// validPages 是允许通过命令行参数指定的页面
var validPages = map[string]bool{
	"general":    true,
	"input":      true,
	"hotkey":     true,
	"appearance": true,
	"dictionary": true,
	"advanced":   true,
	"about":      true,
	"stats":      true,
	"add-word":   true,
}

// AddWordParams 加词对话框参数（通过命令行传入）
type AddWordParams struct {
	Text     string `json:"text"`
	Code     string `json:"code"`
	SchemaID string `json:"schema_id"`
}

// parseStartPage 从命令行参数中解析启动页面
// 支持两种格式: --page <name> 或 --<name>（如 --about）
func parseStartPage() string {
	args := os.Args[1:]
	for i, arg := range args {
		// 格式: --page <name> 或 --page=<name>
		if arg == "--page" && i+1 < len(args) {
			if page := args[i+1]; validPages[page] {
				return page
			}
		}
		if strings.HasPrefix(arg, "--page=") {
			if page := strings.TrimPrefix(arg, "--page="); validPages[page] {
				return page
			}
		}
		// 格式: --about, --dictionary 等
		if strings.HasPrefix(arg, "--") {
			page := strings.TrimPrefix(arg, "--")
			if validPages[page] {
				return page
			}
		}
	}
	return ""
}

// parseAddWordParams 从命令行参数中解析加词参数
func parseAddWordParams() AddWordParams {
	var params AddWordParams
	args := os.Args[1:]
	for i, arg := range args {
		if arg == "--text" && i+1 < len(args) {
			params.Text = args[i+1]
		} else if strings.HasPrefix(arg, "--text=") {
			params.Text = strings.TrimPrefix(arg, "--text=")
		} else if arg == "--code" && i+1 < len(args) {
			params.Code = args[i+1]
		} else if strings.HasPrefix(arg, "--code=") {
			params.Code = strings.TrimPrefix(arg, "--code=")
		} else if arg == "--schema" && i+1 < len(args) {
			params.SchemaID = args[i+1]
		} else if strings.HasPrefix(arg, "--schema=") {
			params.SchemaID = strings.TrimPrefix(arg, "--schema=")
		}
	}
	return params
}

func main() {
	// 解析启动页面参数（需在单例检查前解析，以便发送给已有实例）
	startPage := parseStartPage()

	// 解析加词参数（需在单例检查前解析，以便发送给已有实例）
	var addWordParams AddWordParams
	if startPage == "add-word" {
		addWordParams = parseAddWordParams()
	}

	// 单例检查：如果已有实例在运行，发送页面参数、激活其窗口并退出
	mutexHandle, ok := ensureSingleInstance(startPage, addWordParams)
	if !ok {
		return
	}
	defer windows.CloseHandle(mutexHandle)

	// Create an instance of the app structure
	app := NewApp()
	app.startPage = startPage
	app.addWordParams = addWordParams

	// 加词对话框模式使用较小的窗口
	winWidth := 800
	winHeight := 600
	minWidth := 600
	minHeight := 400
	if startPage == "add-word" {
		winWidth = 400
		winHeight = 450
		minWidth = 400
		minHeight = 400
	}

	// Create application with options
	err := wails.Run(&options.App{
		Title:     buildvariant.DisplayName() + " 设置",
		Width:     winWidth,
		Height:    winHeight,
		MinWidth:  minWidth,
		MinHeight: minHeight,
		AssetServer: &assetserver.Options{
			Assets: assets,
		},
		BackgroundColour: &options.RGBA{R: 255, G: 255, B: 255, A: 1},
		OnStartup:        app.startup,
		OnShutdown:       app.shutdown,
		Bind: []interface{}{
			app,
		},
		Windows: &wailsWindows.Options{
			WebviewIsTransparent: false,
			WindowIsTranslucent:  false,
			DisableWindowIcon:    false,
			WebviewUserDataPath:  filepath.Join(os.TempDir(), "wind_setting"),
		},
	})

	if err != nil {
		showNativeMessageBox(buildvariant.DisplayName()+" 设置 - 启动失败", err.Error())
	}
}
