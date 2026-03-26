package main

import (
	"fmt"
	"path/filepath"
	"time"

	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/dictfile"

	"wind_setting/internal/editor"
)

// ========== 短语管理 ==========

// PhraseItem 短语项（用于前端）
type PhraseItem struct {
	Code     string `json:"code"`
	Text     string `json:"text"`
	Position int    `json:"position"`
	IsSystem bool   `json:"is_system"` // 是否为系统短语
	Disabled bool   `json:"disabled"`  // 是否被禁用
}

// GetPhrases 获取所有短语（用户短语）
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
			Code:     p.Code,
			Text:     p.Text,
			Position: p.Position,
		}
	}

	return items, nil
}

// GetSystemPhrases 获取系统短语（合并用户覆盖状态）
func (a *App) GetSystemPhrases() ([]PhraseItem, error) {
	if a.systemPhraseEditor == nil {
		return []PhraseItem{}, nil
	}

	cfg := a.systemPhraseEditor.GetPhrases()
	if cfg == nil {
		return []PhraseItem{}, nil
	}

	// 构建用户覆盖索引（code+text → PhraseConfig）
	overrides := make(map[string]dictfile.PhraseConfig)
	if a.phraseEditor != nil {
		userCfg := a.phraseEditor.GetPhrases()
		if userCfg != nil {
			for _, p := range userCfg.Phrases {
				overrides[p.Code+"||"+p.Text] = p
			}
		}
	}

	items := make([]PhraseItem, len(cfg.Phrases))
	for i, p := range cfg.Phrases {
		item := PhraseItem{
			Code:     p.Code,
			Text:     p.Text,
			Position: p.Position,
			IsSystem: true,
		}
		// 应用用户覆盖
		if ov, ok := overrides[p.Code+"||"+p.Text]; ok {
			item.Position = ov.Position
			item.Disabled = ov.Disabled
		}
		items[i] = item
	}

	return items, nil
}

// SavePhrases 保存用户短语配置
func (a *App) SavePhrases(items []PhraseItem) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	cfg := &dictfile.PhrasesConfig{
		Phrases: make([]dictfile.PhraseConfig, len(items)),
	}
	for i, item := range items {
		cfg.Phrases[i] = dictfile.PhraseConfig{
			Code:     item.Code,
			Text:     item.Text,
			Position: item.Position,
		}
	}

	a.phraseEditor.SetPhrases(cfg)

	if err := a.phraseEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.phraseEditor.GetFilePath())
	go a.NotifyReload("phrases")

	return nil
}

// AddPhrase 添加用户短语
func (a *App) AddPhrase(code, text string, position int) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	a.phraseEditor.AddPhrase(code, text, position)

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

// UpdatePhrase 编辑用户短语（修改 code/text/position）
func (a *App) UpdatePhrase(oldCode, oldText, newCode, newText string, newPosition int) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	// 移除旧条目
	a.phraseEditor.RemovePhrase(oldCode, oldText)
	// 添加新条目
	a.phraseEditor.AddPhrase(newCode, newText, newPosition)

	if err := a.phraseEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.phraseEditor.GetFilePath())
	go a.NotifyReload("phrases")
	return nil
}

// OverrideSystemPhrase 覆盖系统短语（保存到用户短语文件，不修改系统文件）
// 可用于修改 position 或禁用系统短语
func (a *App) OverrideSystemPhrase(code, text string, position int, disabled bool) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	cfg := a.phraseEditor.GetPhrases()
	if cfg == nil {
		cfg = &dictfile.PhrasesConfig{}
	}

	// 查找是否已有覆盖
	found := false
	for i, p := range cfg.Phrases {
		if p.Code == code && p.Text == text {
			cfg.Phrases[i].Position = position
			cfg.Phrases[i].Disabled = disabled
			found = true
			break
		}
	}

	if !found {
		cfg.Phrases = append(cfg.Phrases, dictfile.PhraseConfig{
			Code:     code,
			Text:     text,
			Position: position,
			Disabled: disabled,
		})
	}

	a.phraseEditor.SetPhrases(cfg)

	if err := a.phraseEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.phraseEditor.GetFilePath())
	go a.NotifyReload("phrases")
	return nil
}

// RemoveSystemPhraseOverride 移除对系统短语的覆盖（恢复为系统默认）
func (a *App) RemoveSystemPhraseOverride(code, text string) error {
	if a.phraseEditor == nil {
		return fmt.Errorf("phrase editor not initialized")
	}

	// 从用户短语文件中移除同 code+text 的覆盖
	a.phraseEditor.RemovePhrase(code, text)

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

// GetUserDictSchemaID 获取当前用户词库对应的方案 ID
func (a *App) GetUserDictSchemaID() string {
	cfg, err := config.Load()
	if err != nil {
		return "wubi86"
	}
	return cfg.Schema.Active
}

// SwitchUserDictSchema 切换用户词库到指定方案
func (a *App) SwitchUserDictSchema(schemaID string) error {
	// 先保存当前词库
	if a.userDictEditor != nil {
		a.userDictEditor.Save()
		// 取消旧文件的监控
		a.fileWatcher.Unwatch(a.userDictEditor.GetFilePath())
	}

	// 创建新方案的词库编辑器
	newEditor, err := editor.NewUserDictEditorForSchema(schemaID)
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

// ========== 用户词库导入导出 ==========

// ImportExportResult 导入导出操作结果
type ImportExportResult struct {
	Cancelled bool   `json:"cancelled"`
	Count     int    `json:"count"`
	Total     int    `json:"total,omitempty"`
	Path      string `json:"path,omitempty"`
}

// ImportUserDict 从文件导入用户词库
func (a *App) ImportUserDict() (*ImportExportResult, error) {
	if a.userDictEditor == nil {
		return nil, fmt.Errorf("user dict editor not initialized")
	}

	path, err := wailsRuntime.OpenFileDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title: "导入用户词库",
		Filters: []wailsRuntime.FileFilter{
			{
				DisplayName: "词库文件 (*.txt)",
				Pattern:     "*.txt",
			},
			{
				DisplayName: "所有文件 (*.*)",
				Pattern:     "*.*",
			},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开文件对话框失败: %w", err)
	}

	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	count, err := a.userDictEditor.ImportFromFile(path)
	if err != nil {
		return nil, fmt.Errorf("导入失败: %w", err)
	}

	if err := a.userDictEditor.Save(); err != nil {
		return nil, fmt.Errorf("保存失败: %w", err)
	}

	a.fileWatcher.UpdateState(a.userDictEditor.GetFilePath())
	go a.NotifyReload("userdict")

	return &ImportExportResult{
		Count: count,
		Total: a.userDictEditor.GetWordCount(),
	}, nil
}

// ExportUserDict 导出用户词库到文件
func (a *App) ExportUserDict() (*ImportExportResult, error) {
	if a.userDictEditor == nil {
		return nil, fmt.Errorf("user dict editor not initialized")
	}

	defaultFilename := fmt.Sprintf("user_dict_%s.txt", time.Now().Format("20060102"))

	path, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出用户词库",
		DefaultFilename: defaultFilename,
		Filters: []wailsRuntime.FileFilter{
			{
				DisplayName: "词库文件 (*.txt)",
				Pattern:     "*.txt",
			},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开保存对话框失败: %w", err)
	}

	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	if err := a.userDictEditor.ExportToFile(path); err != nil {
		return nil, fmt.Errorf("导出失败: %w", err)
	}

	return &ImportExportResult{
		Count: a.userDictEditor.GetWordCount(),
		Path:  path,
	}, nil
}

// ========== Shadow 管理 ==========

// ShadowRuleItem Shadow 规则项（用于前端）
type ShadowRuleItem struct {
	Code     string `json:"code"`
	Word     string `json:"word"`
	Type     string `json:"type"`     // "pin" 或 "delete"
	Position int    `json:"position"` // 仅 pin 有效
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
	for code, cc := range cfg.Rules {
		for _, p := range cc.Pinned {
			items = append(items, ShadowRuleItem{
				Code:     code,
				Word:     p.Word,
				Type:     "pin",
				Position: p.Position,
			})
		}
		for _, d := range cc.Deleted {
			items = append(items, ShadowRuleItem{
				Code: code,
				Word: d,
				Type: "delete",
			})
		}
	}

	return items, nil
}

// PinShadowWord 固定词到指定位置
func (a *App) PinShadowWord(code, word string, position int) error {
	if a.shadowEditor == nil {
		return fmt.Errorf("shadow editor not initialized")
	}

	a.shadowEditor.PinWord(code, word, position)

	if err := a.shadowEditor.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(a.shadowEditor.GetFilePath())
	go a.NotifyReload("shadow")

	return nil
}

// DeleteShadowWord 隐藏词条
func (a *App) DeleteShadowWord(code, word string) error {
	if a.shadowEditor == nil {
		return fmt.Errorf("shadow editor not initialized")
	}

	a.shadowEditor.DeleteWord(code, word)

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

// ========== 按方案操作词库（左右分栏 UI） ==========

// SchemaDictStats 方案词库统计信息
type SchemaDictStats struct {
	SchemaID      string `json:"schema_id"`
	SchemaName    string `json:"schema_name"`
	IconLabel     string `json:"icon_label"`
	WordCount     int    `json:"word_count"`
	ShadowCount   int    `json:"shadow_count"`
	TempWordCount int    `json:"temp_word_count"`
}

// getOrCreateSchemaUserDictEditor 获取或创建指定方案的用户词库编辑器
func (a *App) getOrCreateSchemaUserDictEditor(schemaID string) (*editor.UserDictEditor, error) {
	if ed, ok := a.schemaUserDicts[schemaID]; ok {
		return ed, nil
	}

	cfg, err := a.GetSchemaConfig(schemaID)
	if err != nil {
		return nil, fmt.Errorf("获取方案配置失败: %w", err)
	}

	if cfg.UserData.UserDictFile == "" {
		return nil, fmt.Errorf("方案 %s 未配置用户词库文件", schemaID)
	}

	configDir, err := config.GetConfigDir()
	if err != nil {
		return nil, fmt.Errorf("获取配置目录失败: %w", err)
	}

	path := filepath.Join(configDir, cfg.UserData.UserDictFile)
	ed := editor.NewUserDictEditorWithPath(path)
	ed.Load() // 文件不存在是正常的

	a.schemaUserDicts[schemaID] = ed
	return ed, nil
}

// getOrCreateSchemaShadowEditor 获取或创建指定方案的 Shadow 编辑器
func (a *App) getOrCreateSchemaShadowEditor(schemaID string) (*editor.ShadowEditor, error) {
	if ed, ok := a.schemaShadows[schemaID]; ok {
		return ed, nil
	}

	cfg, err := a.GetSchemaConfig(schemaID)
	if err != nil {
		return nil, fmt.Errorf("获取方案配置失败: %w", err)
	}

	if cfg.UserData.ShadowFile == "" {
		return nil, fmt.Errorf("方案 %s 未配置 Shadow 文件", schemaID)
	}

	configDir, err := config.GetConfigDir()
	if err != nil {
		return nil, fmt.Errorf("获取配置目录失败: %w", err)
	}

	path := filepath.Join(configDir, cfg.UserData.ShadowFile)
	ed := editor.NewShadowEditorWithPath(path)
	ed.Load() // 文件不存在是正常的

	a.schemaShadows[schemaID] = ed
	return ed, nil
}

// GetEnabledSchemasWithDictStats 获取所有已启用方案及其词库统计
func (a *App) GetEnabledSchemasWithDictStats() ([]SchemaDictStats, error) {
	cfg, err := config.Load()
	if err != nil {
		return nil, fmt.Errorf("加载配置失败: %w", err)
	}

	schemas, err := a.GetAvailableSchemas()
	if err != nil {
		return nil, err
	}

	// 建立 ID→SchemaInfo 映射
	schemaMap := make(map[string]SchemaInfo)
	for _, s := range schemas {
		schemaMap[s.ID] = s
	}

	var result []SchemaDictStats
	for _, schemaID := range cfg.Schema.Available {
		info, ok := schemaMap[schemaID]
		if !ok {
			continue
		}

		stats := SchemaDictStats{
			SchemaID:   schemaID,
			SchemaName: info.Name,
			IconLabel:  info.IconLabel,
		}

		if ud, err := a.getOrCreateSchemaUserDictEditor(schemaID); err == nil {
			stats.WordCount = ud.GetWordCount()
		}

		if sd, err := a.getOrCreateSchemaShadowEditor(schemaID); err == nil {
			stats.ShadowCount = sd.GetRuleCount()
		}

		if td, err := a.getOrCreateSchemaTempDictEditor(schemaID); err == nil {
			stats.TempWordCount = td.GetWordCount()
		}

		result = append(result, stats)
	}

	return result, nil
}

// GetUserDictBySchema 获取指定方案的用户词库
func (a *App) GetUserDictBySchema(schemaID string) ([]UserWordItem, error) {
	ed, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return nil, err
	}

	data := ed.GetUserDict()
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

// AddUserWordForSchema 向指定方案添加用户词条
func (a *App) AddUserWordForSchema(schemaID, code, text string, weight int) error {
	ed, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return err
	}

	ed.AddWord(code, text, weight)

	if err := ed.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("userdict")
	return nil
}

// RemoveUserWordForSchema 从指定方案删除用户词条
func (a *App) RemoveUserWordForSchema(schemaID, code, text string) error {
	ed, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return err
	}

	if !ed.RemoveWord(code, text) {
		return fmt.Errorf("word not found")
	}

	if err := ed.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("userdict")
	return nil
}

// SearchUserDictBySchema 搜索指定方案的用户词库
func (a *App) SearchUserDictBySchema(schemaID, query string, limit int) ([]UserWordItem, error) {
	ed, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return nil, err
	}

	words := ed.SearchWords(query, limit)
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

// ImportUserDictForSchema 导入指定方案的用户词库
func (a *App) ImportUserDictForSchema(schemaID string) (*ImportExportResult, error) {
	ed, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return nil, err
	}

	path, err := wailsRuntime.OpenFileDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title: "导入用户词库",
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "词库文件 (*.txt)", Pattern: "*.txt"},
			{DisplayName: "所有文件 (*.*)", Pattern: "*.*"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开文件对话框失败: %w", err)
	}
	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	count, err := ed.ImportFromFile(path)
	if err != nil {
		return nil, fmt.Errorf("导入失败: %w", err)
	}

	if err := ed.Save(); err != nil {
		return nil, fmt.Errorf("保存失败: %w", err)
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("userdict")

	return &ImportExportResult{
		Count: count,
		Total: ed.GetWordCount(),
	}, nil
}

// ExportUserDictForSchema 导出指定方案的用户词库
func (a *App) ExportUserDictForSchema(schemaID string) (*ImportExportResult, error) {
	ed, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return nil, err
	}

	defaultFilename := fmt.Sprintf("user_dict_%s_%s.txt", schemaID, time.Now().Format("20060102"))
	path, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出用户词库",
		DefaultFilename: defaultFilename,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "词库文件 (*.txt)", Pattern: "*.txt"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开保存对话框失败: %w", err)
	}
	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	if err := ed.ExportToFile(path); err != nil {
		return nil, fmt.Errorf("导出失败: %w", err)
	}

	return &ImportExportResult{
		Count: ed.GetWordCount(),
		Path:  path,
	}, nil
}

// GetShadowBySchema 获取指定方案的 Shadow 规则
func (a *App) GetShadowBySchema(schemaID string) ([]ShadowRuleItem, error) {
	ed, err := a.getOrCreateSchemaShadowEditor(schemaID)
	if err != nil {
		return nil, err
	}

	cfg := ed.GetShadowConfig()
	if cfg == nil {
		return []ShadowRuleItem{}, nil
	}

	var items []ShadowRuleItem
	for code, cc := range cfg.Rules {
		for _, p := range cc.Pinned {
			items = append(items, ShadowRuleItem{
				Code:     code,
				Word:     p.Word,
				Type:     "pin",
				Position: p.Position,
			})
		}
		for _, d := range cc.Deleted {
			items = append(items, ShadowRuleItem{
				Code: code,
				Word: d,
				Type: "delete",
			})
		}
	}

	return items, nil
}

// PinShadowWordForSchema 在指定方案中固定词到指定位置
func (a *App) PinShadowWordForSchema(schemaID, code, word string, position int) error {
	ed, err := a.getOrCreateSchemaShadowEditor(schemaID)
	if err != nil {
		return err
	}

	ed.PinWord(code, word, position)

	if err := ed.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("shadow")
	return nil
}

// DeleteShadowWordForSchema 在指定方案中隐藏词条
func (a *App) DeleteShadowWordForSchema(schemaID, code, word string) error {
	ed, err := a.getOrCreateSchemaShadowEditor(schemaID)
	if err != nil {
		return err
	}

	ed.DeleteWord(code, word)

	if err := ed.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("shadow")
	return nil
}

// ========== 临时词库管理（按方案） ==========

// TempWordItem 临时词条（前端展示用）
type TempWordItem struct {
	Code   string `json:"code"`
	Text   string `json:"text"`
	Weight int    `json:"weight"`
	Count  int    `json:"count"` // 选择次数
}

// getOrCreateSchemaTempDictEditor 获取或创建指定方案的临时词库编辑器
func (a *App) getOrCreateSchemaTempDictEditor(schemaID string) (*editor.UserDictEditor, error) {
	if ed, ok := a.schemaTempDicts[schemaID]; ok {
		return ed, nil
	}

	cfg, err := a.GetSchemaConfig(schemaID)
	if err != nil {
		return nil, fmt.Errorf("获取方案配置失败: %w", err)
	}

	tempFile := cfg.UserData.TempDictFile
	if tempFile == "" {
		// 默认临时词库文件名
		tempFile = "temp_words_" + schemaID + ".txt"
	}

	configDir, err := config.GetConfigDir()
	if err != nil {
		return nil, fmt.Errorf("获取配置目录失败: %w", err)
	}

	path := filepath.Join(configDir, tempFile)
	ed := editor.NewUserDictEditorWithPath(path)
	ed.Load() // 文件不存在是正常的

	a.schemaTempDicts[schemaID] = ed
	return ed, nil
}

// GetTempDictBySchema 获取指定方案的临时词库
func (a *App) GetTempDictBySchema(schemaID string) ([]TempWordItem, error) {
	ed, err := a.getOrCreateSchemaTempDictEditor(schemaID)
	if err != nil {
		return nil, err
	}

	data := ed.GetUserDict()
	if data == nil {
		return []TempWordItem{}, nil
	}

	items := make([]TempWordItem, len(data.Words))
	for i, w := range data.Words {
		items[i] = TempWordItem{
			Code:   w.Code,
			Text:   w.Text,
			Weight: w.Weight,
			Count:  0, // dictfile.UserWord 没有 Count 字段，暂为 0
		}
	}
	return items, nil
}

// ClearTempDictForSchema 清空指定方案的临时词库
func (a *App) ClearTempDictForSchema(schemaID string) (int, error) {
	ed, err := a.getOrCreateSchemaTempDictEditor(schemaID)
	if err != nil {
		return 0, err
	}

	data := ed.GetUserDict()
	count := 0
	if data != nil {
		count = len(data.Words)
	}

	// 清空并保存
	ed.SetUserDict(nil)
	if err := ed.Save(); err != nil {
		return 0, err
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("tempdict")
	return count, nil
}

// PromoteTempWordForSchema 将临时词条晋升到用户词库
func (a *App) PromoteTempWordForSchema(schemaID, code, text string) error {
	tempEd, err := a.getOrCreateSchemaTempDictEditor(schemaID)
	if err != nil {
		return err
	}

	userEd, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return err
	}

	// 查找词条获取权重
	data := tempEd.GetUserDict()
	if data == nil {
		return fmt.Errorf("临时词库为空")
	}

	weight := 100
	for _, w := range data.Words {
		if w.Code == code && w.Text == text {
			weight = w.Weight
			break
		}
	}

	// 添加到用户词库
	userEd.AddWord(code, text, weight)
	if err := userEd.Save(); err != nil {
		return fmt.Errorf("保存用户词库失败: %w", err)
	}

	// 从临时词库移除
	tempEd.RemoveWord(code, text)
	if err := tempEd.Save(); err != nil {
		return fmt.Errorf("保存临时词库失败: %w", err)
	}

	a.fileWatcher.UpdateState(userEd.GetFilePath())
	a.fileWatcher.UpdateState(tempEd.GetFilePath())
	go a.NotifyReload("userdict")
	go a.NotifyReload("tempdict")
	return nil
}

// PromoteAllTempWordsForSchema 将所有临时词条晋升到用户词库
func (a *App) PromoteAllTempWordsForSchema(schemaID string) (int, error) {
	tempEd, err := a.getOrCreateSchemaTempDictEditor(schemaID)
	if err != nil {
		return 0, err
	}

	userEd, err := a.getOrCreateSchemaUserDictEditor(schemaID)
	if err != nil {
		return 0, err
	}

	data := tempEd.GetUserDict()
	if data == nil || len(data.Words) == 0 {
		return 0, nil
	}

	count := 0
	for _, w := range data.Words {
		userEd.AddWord(w.Code, w.Text, w.Weight)
		count++
	}

	if err := userEd.Save(); err != nil {
		return 0, fmt.Errorf("保存用户词库失败: %w", err)
	}

	// 清空临时词库
	ed, _ := a.getOrCreateSchemaTempDictEditor(schemaID)
	ed.SetUserDict(nil)
	if err := ed.Save(); err != nil {
		return count, fmt.Errorf("清空临时词库失败: %w", err)
	}

	go a.NotifyReload("userdict")
	go a.NotifyReload("tempdict")
	return count, nil
}

// RemoveTempWordForSchema 从临时词库删除词条
func (a *App) RemoveTempWordForSchema(schemaID, code, text string) error {
	ed, err := a.getOrCreateSchemaTempDictEditor(schemaID)
	if err != nil {
		return err
	}

	if !ed.RemoveWord(code, text) {
		return fmt.Errorf("word not found")
	}

	if err := ed.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("tempdict")
	return nil
}

// RemoveShadowRuleForSchema 在指定方案中删除规则
func (a *App) RemoveShadowRuleForSchema(schemaID, code, word string) error {
	ed, err := a.getOrCreateSchemaShadowEditor(schemaID)
	if err != nil {
		return err
	}

	if !ed.RemoveRule(code, word) {
		return fmt.Errorf("rule not found")
	}

	if err := ed.Save(); err != nil {
		return err
	}

	a.fileWatcher.UpdateState(ed.GetFilePath())
	go a.NotifyReload("shadow")
	return nil
}
