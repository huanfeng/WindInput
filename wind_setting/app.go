package main

import (
	"context"
	"fmt"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/control"
	"github.com/huanfeng/wind_input/pkg/dictfile"

	"wind_setting/internal/editor"
	"wind_setting/internal/filesync"
)

// App struct
type App struct {
	ctx context.Context

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

// startup is called when the app starts
func (a *App) startup(ctx context.Context) {
	a.ctx = ctx

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

// ========== 配置管理 ==========

// GetConfig 获取配置
func (a *App) GetConfig() (*config.Config, error) {
	if a.configEditor == nil {
		return nil, fmt.Errorf("config editor not initialized")
	}

	cfg := a.configEditor.GetConfig()
	if cfg == nil {
		return nil, fmt.Errorf("config not loaded")
	}

	return cfg, nil
}

// SaveConfig 保存配置
func (a *App) SaveConfig(cfg *config.Config) error {
	if a.configEditor == nil {
		return fmt.Errorf("config editor not initialized")
	}

	a.configEditor.SetConfig(cfg)
	if err := a.configEditor.Save(); err != nil {
		return err
	}

	// 更新文件监控状态
	a.fileWatcher.UpdateState(a.configEditor.GetFilePath())

	// 通知主程序重载
	go a.NotifyReload("config")

	return nil
}

// CheckConfigModified 检查配置是否被外部修改
func (a *App) CheckConfigModified() (bool, error) {
	if a.configEditor == nil {
		return false, nil
	}
	return a.configEditor.HasChanged()
}

// ReloadConfig 重新加载配置（丢弃本地修改）
func (a *App) ReloadConfig() error {
	if a.configEditor == nil {
		return fmt.Errorf("config editor not initialized")
	}
	return a.configEditor.Reload()
}

// ========== 短语管理 ==========

// PhraseItem 短语项（用于前端）
type PhraseItem struct {
	Code       string   `json:"code"`
	Text       string   `json:"text"`
	Candidates []string `json:"candidates,omitempty"`
	Type       string   `json:"type,omitempty"`
	Handler    string   `json:"handler,omitempty"`
	Weight     int      `json:"weight"`
}

// GetPhrases 获取所有短语
func (a *App) GetPhrases() ([]PhraseItem, error) {
	if a.phraseEditor == nil {
		return nil, fmt.Errorf("phrase editor not initialized")
	}

	cfg := a.phraseEditor.GetPhrases()
	if cfg == nil {
		return []PhraseItem{}, nil
	}

	items := make([]PhraseItem, len(cfg.Phrases))
	for i, p := range cfg.Phrases {
		items[i] = PhraseItem{
			Code:       p.Code,
			Text:       p.Text,
			Candidates: p.Candidates,
			Type:       p.Type,
			Handler:    p.Handler,
			Weight:     p.Weight,
		}
	}

	return items, nil
}

// SavePhrases 保存短语配置
func (a *App) SavePhrases(items []PhraseItem) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	// 转换为 PhrasesConfig
	cfg := &dictfile.PhrasesConfig{
		Phrases: make([]dictfile.PhraseConfig, len(items)),
	}
	for i, item := range items {
		cfg.Phrases[i] = dictfile.PhraseConfig{
			Code:       item.Code,
			Text:       item.Text,
			Candidates: item.Candidates,
			Type:       item.Type,
			Handler:    item.Handler,
			Weight:     item.Weight,
		}
	}

	// 设置数据
	a.phraseEditor.SetPhrases(cfg)

	if err := a.phraseEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.phraseEditor.GetFilePath())
	go a.NotifyReload("phrases")

	return nil
}

// AddPhrase 添加短语
func (a *App) AddPhrase(code, text string, weight int) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	a.phraseEditor.AddPhrase(code, text, weight)

	if err := a.phraseEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.phraseEditor.GetFilePath())
	go a.NotifyReload("phrases")

	return nil
}

// RemovePhrase 删除短语
func (a *App) RemovePhrase(code, text string) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	if !a.phraseEditor.RemovePhrase(code, text) {
		return fmt.Errorf("phrase not found")
	}

	if err := a.phraseEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.phraseEditor.GetFilePath())
	go a.NotifyReload("phrases")

	return nil
}

// CheckPhrasesModified 检查短语是否被外部修改
func (a *App) CheckPhrasesModified() (bool, error) {
	if a.phraseEditor == nil {
		return false, nil
	}
	return a.phraseEditor.HasChanged()
}

// ReloadPhrases 重新加载短语
func (a *App) ReloadPhrases() error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}
	return a.phraseEditor.Reload()
}

// ========== 用户词库管理 ==========

// UserWordItem 用户词条（用于前端）
type UserWordItem struct {
	Code      string `json:"code"`
	Text      string `json:"text"`
	Weight    int    `json:"weight"`
	CreatedAt string `json:"created_at"`
}

// GetUserDict 获取用户词库
func (a *App) GetUserDict() ([]UserWordItem, error) {
	if a.userDictEditor == nil {
		return nil, fmt.Errorf("user dict editor not initialized")
	}

	data := a.userDictEditor.GetUserDict()
	if data == nil {
		return []UserWordItem{}, nil
	}

	items := make([]UserWordItem, len(data.Words))
	for i, w := range data.Words {
		items[i] = UserWordItem{
			Code:      w.Code,
			Text:      w.Text,
			Weight:    w.Weight,
			CreatedAt: w.CreatedAt.Format(time.RFC3339),
		}
	}

	return items, nil
}

// AddUserWord 添加用户词条
func (a *App) AddUserWord(code, text string, weight int) error {
	if a.userDictEditor == nil {
		return fmt.Errorf("user dict editor not initialized")
	}

	a.userDictEditor.AddWord(code, text, weight)

	if err := a.userDictEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.userDictEditor.GetFilePath())
	go a.NotifyReload("userdict")

	return nil
}

// RemoveUserWord 删除用户词条
func (a *App) RemoveUserWord(code, text string) error {
	if a.userDictEditor == nil {
		return fmt.Errorf("user dict editor not initialized")
	}

	if !a.userDictEditor.RemoveWord(code, text) {
		return fmt.Errorf("word not found")
	}

	if err := a.userDictEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.userDictEditor.GetFilePath())
	go a.NotifyReload("userdict")

	return nil
}

// SearchUserDict 搜索用户词库
func (a *App) SearchUserDict(query string, limit int) ([]UserWordItem, error) {
	if a.userDictEditor == nil {
		return nil, fmt.Errorf("user dict editor not initialized")
	}

	words := a.userDictEditor.SearchWords(query, limit)
	items := make([]UserWordItem, len(words))
	for i, w := range words {
		items[i] = UserWordItem{
			Code:      w.Code,
			Text:      w.Text,
			Weight:    w.Weight,
			CreatedAt: w.CreatedAt.Format(time.RFC3339),
		}
	}

	return items, nil
}

// GetUserDictStats 获取用户词库统计
func (a *App) GetUserDictStats() map[string]int {
	stats := make(map[string]int)

	if a.userDictEditor != nil {
		stats["word_count"] = a.userDictEditor.GetWordCount()
	}
	if a.phraseEditor != nil {
		stats["phrase_count"] = a.phraseEditor.GetPhraseCount()
	}
	if a.shadowEditor != nil {
		stats["shadow_count"] = a.shadowEditor.GetRuleCount()
	}

	return stats
}

// CheckUserDictModified 检查用户词库是否被外部修改
func (a *App) CheckUserDictModified() (bool, error) {
	if a.userDictEditor == nil {
		return false, nil
	}
	return a.userDictEditor.HasChanged()
}

// ReloadUserDict 重新加载用户词库
func (a *App) ReloadUserDict() error {
	if a.userDictEditor == nil {
		return fmt.Errorf("user dict editor not initialized")
	}
	return a.userDictEditor.Reload()
}

// ========== Shadow 管理 ==========

// ShadowRuleItem Shadow 规则项（用于前端）
type ShadowRuleItem struct {
	Code   string `json:"code"`
	Word   string `json:"word"`
	Action string `json:"action"`
	Weight int    `json:"weight"`
}

// GetShadowRules 获取所有 Shadow 规则
func (a *App) GetShadowRules() ([]ShadowRuleItem, error) {
	if a.shadowEditor == nil {
		return nil, fmt.Errorf("shadow editor not initialized")
	}

	cfg := a.shadowEditor.GetShadowConfig()
	if cfg == nil {
		return []ShadowRuleItem{}, nil
	}

	var items []ShadowRuleItem
	for code, rules := range cfg.Rules {
		for _, r := range rules {
			items = append(items, ShadowRuleItem{
				Code:   code,
				Word:   r.Word,
				Action: r.Action,
				Weight: r.Weight,
			})
		}
	}

	return items, nil
}

// AddShadowRule 添加 Shadow 规则
func (a *App) AddShadowRule(code, word, action string, weight int) error {
	if a.shadowEditor == nil {
		return fmt.Errorf("shadow editor not initialized")
	}

	a.shadowEditor.AddRule(code, word, action, weight)

	if err := a.shadowEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.shadowEditor.GetFilePath())
	go a.NotifyReload("shadow")

	return nil
}

// RemoveShadowRule 删除 Shadow 规则
func (a *App) RemoveShadowRule(code, word string) error {
	if a.shadowEditor == nil {
		return fmt.Errorf("shadow editor not initialized")
	}

	if !a.shadowEditor.RemoveRule(code, word) {
		return fmt.Errorf("rule not found")
	}

	if err := a.shadowEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.shadowEditor.GetFilePath())
	go a.NotifyReload("shadow")

	return nil
}

// ========== 控制管道 ==========

// CheckServiceRunning 检查服务是否运行
func (a *App) CheckServiceRunning() (bool, error) {
	return a.controlClient.IsServiceRunning(), nil
}

// NotifyReload 通知服务重载
func (a *App) NotifyReload(target string) error {
	return a.controlClient.NotifyReload(target)
}

// GetServiceStatus 获取服务状态
func (a *App) GetServiceStatus() (*control.ServiceStatus, error) {
	return a.controlClient.GetStatus()
}

// ========== 文件变化检测 ==========

// FileChangeStatus 文件变化状态
type FileChangeStatus struct {
	ConfigChanged   bool `json:"config_changed"`
	PhrasesChanged  bool `json:"phrases_changed"`
	ShadowChanged   bool `json:"shadow_changed"`
	UserDictChanged bool `json:"userdict_changed"`
}

// CheckAllFilesModified 检查所有文件是否被外部修改
func (a *App) CheckAllFilesModified() (*FileChangeStatus, error) {
	status := &FileChangeStatus{}

	if changed, _ := a.CheckConfigModified(); changed {
		status.ConfigChanged = true
	}
	if changed, _ := a.CheckPhrasesModified(); changed {
		status.PhrasesChanged = true
	}
	if a.shadowEditor != nil {
		if changed, _ := a.shadowEditor.HasChanged(); changed {
			status.ShadowChanged = true
		}
	}
	if changed, _ := a.CheckUserDictModified(); changed {
		status.UserDictChanged = true
	}

	return status, nil
}

// ReloadAllFiles 重新加载所有文件
func (a *App) ReloadAllFiles() error {
	var lastErr error

	if err := a.ReloadConfig(); err != nil {
		lastErr = err
	}
	if err := a.ReloadPhrases(); err != nil {
		lastErr = err
	}
	if a.shadowEditor != nil {
		if err := a.shadowEditor.Reload(); err != nil {
			lastErr = err
		}
	}
	if err := a.ReloadUserDict(); err != nil {
		lastErr = err
	}

	return lastErr
}
