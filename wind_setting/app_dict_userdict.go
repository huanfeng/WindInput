package main

import (
	"bufio"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// ========== 用户词库管理 ==========

// UserWordItem 用户词条（用于前端）
type UserWordItem struct {
	Code      string `json:"code"`
	Text      string `json:"text"`
	Weight    int    `json:"weight"`
	CreatedAt string `json:"created_at"`
}

// ImportExportResult 导入导出操作结果
type ImportExportResult struct {
	Cancelled bool   `json:"cancelled"`
	Count     int    `json:"count"`
	Total     int    `json:"total,omitempty"`
	Path      string `json:"path,omitempty"`
}

// convertWordEntries 将 RPC WordEntry 转换为前端 UserWordItem
func convertWordEntries(words []rpcapi.WordEntry) []UserWordItem {
	items := make([]UserWordItem, len(words))
	for i, w := range words {
		createdAt := ""
		if w.CreatedAt != 0 {
			createdAt = time.Unix(w.CreatedAt, 0).Format(time.RFC3339)
		}
		items[i] = UserWordItem{
			Code:      w.Code,
			Text:      w.Text,
			Weight:    w.Weight,
			CreatedAt: createdAt,
		}
	}
	return items
}

// GetUserDict 获取用户词库
func (a *App) GetUserDict() ([]UserWordItem, error) {
	reply, err := a.rpcClient.DictSearch("", "", "", 0, 0)
	if err != nil {
		return nil, fmt.Errorf("获取用户词库失败: %w", err)
	}
	return convertWordEntries(reply.Words), nil
}

// AddUserWord 添加用户词条
func (a *App) AddUserWord(code, text string, weight int) error {
	return a.rpcClient.DictAdd("", code, text, weight)
}

// RemoveUserWord 删除用户词条
func (a *App) RemoveUserWord(code, text string) error {
	return a.rpcClient.DictRemove("", code, text)
}

// SearchUserDict 搜索用户词库
func (a *App) SearchUserDict(query string, limit int) ([]UserWordItem, error) {
	reply, err := a.rpcClient.DictSearch("", query, "", limit, 0)
	if err != nil {
		return nil, fmt.Errorf("搜索用户词库失败: %w", err)
	}
	return convertWordEntries(reply.Words), nil
}

// GetUserDictStats 获取用户词库统计
func (a *App) GetUserDictStats() map[string]int {
	stats := make(map[string]int)

	if rpcStats, err := a.rpcClient.DictGetStats(); err == nil {
		for k, v := range rpcStats {
			stats[k] = v
		}
	}

	// 短语数量通过 RPC 获取
	if phraseReply, err := a.rpcClient.PhraseList(); err == nil {
		stats["phrase_count"] = len(phraseReply.Phrases)
	}

	return stats
}

// CheckUserDictModified 检查用户词库是否被外部修改
func (a *App) CheckUserDictModified() (bool, error) {
	return false, nil
}

// ReloadUserDict 重新加载用户词库
func (a *App) ReloadUserDict() error {
	return nil
}

// GetUserDictSchemaID 获取当前用户词库对应的方案 ID
func (a *App) GetUserDictSchemaID() string {
	cfg, err := config.Load()
	if err != nil {
		return "wubi86"
	}
	if cfg.Schema.Active != "" {
		return cfg.Schema.Active
	}
	if len(cfg.Schema.Available) > 0 {
		return cfg.Schema.Available[0]
	}
	return "wubi86"
}

// SwitchUserDictSchema 切换用户词库到指定方案
func (a *App) SwitchUserDictSchema(schemaID string) error {
	return nil
}

// ImportUserDict 从文件导入用户词库
func (a *App) ImportUserDict() (*ImportExportResult, error) {
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

	words, err := parseTSVFile(path)
	if err != nil {
		return nil, fmt.Errorf("导入失败: %w", err)
	}

	count, err := a.rpcClient.DictBatchAdd("", words)
	if err != nil {
		return nil, fmt.Errorf("导入失败: %w", err)
	}

	return &ImportExportResult{
		Count: count,
	}, nil
}

// ExportUserDict 导出用户词库到文件
func (a *App) ExportUserDict() (*ImportExportResult, error) {
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

	reply, err := a.rpcClient.DictSearch("", "", "", 0, 0)
	if err != nil {
		return nil, fmt.Errorf("获取词库失败: %w", err)
	}

	if err := writeTSVFile(path, reply.Words); err != nil {
		return nil, fmt.Errorf("导出失败: %w", err)
	}

	return &ImportExportResult{
		Count: len(reply.Words),
		Path:  path,
	}, nil
}

// ========== 导入导出（按方案） ==========

// ImportUserDictForSchema 导入指定方案的用户词库
func (a *App) ImportUserDictForSchema(schemaID string) (*ImportExportResult, error) {
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

	words, err := parseTSVFile(path)
	if err != nil {
		return nil, fmt.Errorf("导入失败: %w", err)
	}

	count, err := a.rpcClient.DictBatchAdd(schemaID, words)
	if err != nil {
		return nil, fmt.Errorf("导入失败: %w", err)
	}

	return &ImportExportResult{
		Count: count,
	}, nil
}

// ExportUserDictForSchema 导出指定方案的用户词库
func (a *App) ExportUserDictForSchema(schemaID string) (*ImportExportResult, error) {
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

	reply, err := a.rpcClient.DictSearch(schemaID, "", "", 0, 0)
	if err != nil {
		return nil, fmt.Errorf("获取词库失败: %w", err)
	}

	if err := writeTSVFile(path, reply.Words); err != nil {
		return nil, fmt.Errorf("导出失败: %w", err)
	}

	return &ImportExportResult{
		Count: len(reply.Words),
		Path:  path,
	}, nil
}

// ========== TSV 文件解析/写入 ==========

// parseTSVFile 解析 TSV 格式的词库文件
func parseTSVFile(path string) ([]rpcapi.WordEntry, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var words []rpcapi.WordEntry
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}

		entry := rpcapi.WordEntry{
			Code: parts[0],
			Text: parts[1],
		}

		if len(parts) >= 3 {
			if w, err := strconv.Atoi(parts[2]); err == nil {
				entry.Weight = w
			}
		}
		if len(parts) >= 4 {
			if ts, err := strconv.ParseInt(parts[3], 10, 64); err == nil {
				entry.CreatedAt = ts
			}
		}
		if len(parts) >= 5 {
			if c, err := strconv.Atoi(parts[4]); err == nil {
				entry.Count = c
			}
		}

		words = append(words, entry)
	}

	return words, scanner.Err()
}

// writeTSVFile 将词条写入 TSV 格式文件
func writeTSVFile(path string, words []rpcapi.WordEntry) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()

	w := bufio.NewWriter(f)
	_, _ = fmt.Fprintln(w, "# code\ttext\tweight\ttimestamp\tcount")

	for _, entry := range words {
		_, _ = fmt.Fprintf(w, "%s\t%s\t%d\t%d\t%d\n",
			entry.Code, entry.Text, entry.Weight, entry.CreatedAt, entry.Count)
	}

	return w.Flush()
}
