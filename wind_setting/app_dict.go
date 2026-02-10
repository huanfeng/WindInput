package main

import (
	"fmt"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/dictfile"

	"wind_setting/internal/editor"
)

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

// GetUserDictEngineType 获取当前用户词库对应的引擎类型
func (a *App) GetUserDictEngineType() string {
	cfg, err := config.Load()
	if err != nil {
		return "wubi"
	}
	return cfg.Engine.Type
}

// SwitchUserDictEngine 切换用户词库到指定引擎
func (a *App) SwitchUserDictEngine(engineType string) error {
	// 先保存当前词库
	if a.userDictEditor != nil {
		a.userDictEditor.Save()
		// 取消旧文件的监控
		a.fileWatcher.Unwatch(a.userDictEditor.GetFilePath())
	}

	// 创建新引擎类型的词库编辑器
	newEditor, err := editor.NewUserDictEditorForEngine(engineType)
	if err != nil {
		return fmt.Errorf("failed to create user dict editor: %w", err)
	}

	if err := newEditor.Load(); err != nil {
		return fmt.Errorf("failed to load user dict: %w", err)
	}

	a.userDictEditor = newEditor
	a.fileWatcher.Watch(a.userDictEditor.GetFilePath())
	return nil
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
