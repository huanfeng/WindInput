package main

import (
	"context"

	"github.com/huanfeng/wind_input/pkg/control"

	"wind_setting/internal/editor"
	"wind_setting/internal/filesync"
)

// App struct
type App struct {
	ctx context.Context

	// 启动页面（通过命令行参数指定）
	startPage string

	// 编辑器
	configEditor   *editor.ConfigEditor
	phraseEditor   *editor.PhraseEditor
	shadowEditor   *editor.ShadowEditor
	userDictEditor *editor.UserDictEditor

	// 文件监控
	fileWatcher *filesync.FileWatcher

	// 控制管道客户端
	controlClient *control.Client
}

// NewApp creates a new App application struct
func NewApp() *App {
	return &App{
		controlClient: control.NewClient(),
	}
}

// GetStartPage 获取启动页面（供前端调用）
func (a *App) GetStartPage() string {
	return a.startPage
}

// GetVersion 获取应用版本号（供前端调用）
func (a *App) GetVersion() string {
	return version
}

// startup is called when the app starts
func (a *App) startup(ctx context.Context) {
	a.ctx = ctx

	// 启动 IPC 监听，接收其他实例的页面切换请求
	startIPCListener(ctx)

	// 初始化编辑器
	var err error

	a.configEditor, err = editor.NewConfigEditor()
	if err == nil {
		a.configEditor.Load()
	}

	a.phraseEditor, err = editor.NewPhraseEditor()
	if err == nil {
		a.phraseEditor.Load()
	}

	a.shadowEditor, err = editor.NewShadowEditor()
	if err == nil {
		a.shadowEditor.Load()
	}

	a.userDictEditor, err = editor.NewUserDictEditor()
	if err == nil {
		a.userDictEditor.Load()
	}

	// 初始化文件监控
	a.fileWatcher = filesync.NewFileWatcher()
	if a.configEditor != nil {
		a.fileWatcher.Watch(a.configEditor.GetFilePath())
	}
	if a.phraseEditor != nil {
		a.fileWatcher.Watch(a.phraseEditor.GetFilePath())
	}
	if a.shadowEditor != nil {
		a.fileWatcher.Watch(a.shadowEditor.GetFilePath())
	}
	if a.userDictEditor != nil {
		a.fileWatcher.Watch(a.userDictEditor.GetFilePath())
	}
}

// shutdown is called when the app is closing
func (a *App) shutdown(ctx context.Context) {
	if a.fileWatcher != nil {
		a.fileWatcher.Stop()
	}
}
