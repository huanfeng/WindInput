package main

import (
	"embed"
	"os"
	"strings"

	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
	"github.com/wailsapp/wails/v2/pkg/options/windows"
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

func main() {
	// 解析启动页面参数
	startPage := parseStartPage()

	// Create an instance of the app structure
	app := NewApp()
	app.startPage = startPage

	// Create application with options
	err := wails.Run(&options.App{
		Title:     "清风输入法 设置",
		Width:     800,
		Height:    600,
		MinWidth:  600,
		MinHeight: 400,
		AssetServer: &assetserver.Options{
			Assets: assets,
		},
		BackgroundColour: &options.RGBA{R: 255, G: 255, B: 255, A: 1},
		OnStartup:        app.startup,
		OnShutdown:       app.shutdown,
		Bind: []interface{}{
			app,
		},
		Windows: &windows.Options{
			WebviewIsTransparent: false,
			WindowIsTranslucent:  false,
			DisableWindowIcon:    false,
		},
	})

	if err != nil {
		println("Error:", err.Error())
	}
}
