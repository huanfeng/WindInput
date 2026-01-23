package main

import (
	"context"
)

// App struct
type App struct {
	ctx context.Context
}

// NewApp creates a new App application struct
func NewApp() *App {
	return &App{}
}

// startup is called when the app starts
func (a *App) startup(ctx context.Context) {
	a.ctx = ctx
}

// GetAPIBaseURL returns the settings API base URL
func (a *App) GetAPIBaseURL() string {
	return "http://127.0.0.1:18923"
}
