// logging.go — 日志轮转与自定义格式化处理器
package main

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"golang.org/x/sys/windows"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
)

// rotatingWriter 实现日志文件轮转的 io.Writer
// 采用非持有句柄模式：每次写入时 Open→Write→Close，
// 文件在写入间隙无句柄占用，外部工具可随时截断或清空。
type rotatingWriter struct {
	mu       sync.Mutex
	filePath string
	maxSize  int64 // 单文件最大字节数
	maxFiles int   // 最大备份文件数
}

func newRotatingWriter(filePath string, maxSize int64, maxFiles int) (*rotatingWriter, error) {
	w := &rotatingWriter{
		filePath: filePath,
		maxSize:  maxSize,
		maxFiles: maxFiles,
	}

	// 检查已有文件大小，若超限则先轮转
	if info, err := os.Stat(filePath); err == nil {
		if info.Size() >= maxSize {
			w.rotateFiles()
		}
	}

	// 验证文件可写
	f, err := os.OpenFile(filePath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		return nil, fmt.Errorf("open log file: %w", err)
	}
	f.Close()

	return w, nil
}

func (w *rotatingWriter) Write(p []byte) (n int, err error) {
	w.mu.Lock()
	defer w.mu.Unlock()

	// 写入前检查文件大小，决定是否需要轮转
	if info, err := os.Stat(w.filePath); err == nil {
		if info.Size() >= w.maxSize {
			w.rotateFiles()
		}
	}

	f, err := os.OpenFile(w.filePath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		return 0, err
	}
	n, err = f.Write(p)
	f.Close()
	return n, err
}

// rotateFiles 移动备份链：3→删除, 2→3, 1→2, current→1
func (w *rotatingWriter) rotateFiles() {
	ext := filepath.Ext(w.filePath)
	base := w.filePath[:len(w.filePath)-len(ext)]

	// 删除最旧的备份
	oldest := fmt.Sprintf("%s.%d%s", base, w.maxFiles, ext)
	os.Remove(oldest)

	// 依次重命名：N-1→N, ..., 1→2
	for i := w.maxFiles - 1; i >= 1; i-- {
		src := fmt.Sprintf("%s.%d%s", base, i, ext)
		dst := fmt.Sprintf("%s.%d%s", base, i+1, ext)
		os.Rename(src, dst)
	}

	// 当前文件→.1
	first := fmt.Sprintf("%s.%d%s", base, 1, ext)
	os.Rename(w.filePath, first)
}

func (w *rotatingWriter) Close() error {
	return nil
}

// levelString 返回固定5字符宽度的日志级别字符串，便于对齐
func levelString(level slog.Level) string {
	switch {
	case level < slog.LevelInfo:
		return "DEBUG"
	case level < slog.LevelWarn:
		return "INFO "
	case level < slog.LevelError:
		return "WARN "
	default:
		return "ERROR"
	}
}

// formattedHandler 自定义格式化的 slog.Handler
// 输出格式: 2026-03-14 15:04:05.000 [INFO ] [PID:12345] message key=value ...
type formattedHandler struct {
	w     io.Writer
	mu    *sync.Mutex
	level slog.Level
	attrs []slog.Attr
	group string
	pid   string
}

func newFormattedHandler(w io.Writer, level slog.Level) *formattedHandler {
	pid := fmt.Sprintf("%6d", os.Getpid())
	return &formattedHandler{
		w:     w,
		mu:    &sync.Mutex{},
		level: level,
		pid:   pid,
	}
}

func (h *formattedHandler) Enabled(_ context.Context, level slog.Level) bool {
	return level >= h.level
}

func (h *formattedHandler) Handle(_ context.Context, r slog.Record) error {
	// 时间格式: 2006-01-02 15:04:05.000
	timeStr := r.Time.Format("2006-01-02 15:04:05.000")
	lvl := levelString(r.Level)

	var buf strings.Builder
	buf.WriteString(timeStr)
	buf.WriteString(" [")
	buf.WriteString(lvl)
	buf.WriteString("] [PID:")
	buf.WriteString(h.pid)
	buf.WriteString("] ")
	buf.WriteString(r.Message)

	// 先输出 handler 级别的预设属性
	for _, a := range h.attrs {
		buf.WriteByte(' ')
		writeAttr(&buf, h.group, a)
	}

	// 再输出 Record 级别的属性
	r.Attrs(func(a slog.Attr) bool {
		buf.WriteByte(' ')
		writeAttr(&buf, h.group, a)
		return true
	})

	buf.WriteByte('\n')

	h.mu.Lock()
	defer h.mu.Unlock()
	_, err := h.w.Write([]byte(buf.String()))
	return err
}

// writeAttr 将 slog.Attr 格式化为 key=value 写入 Builder
func writeAttr(buf *strings.Builder, group string, a slog.Attr) {
	key := a.Key
	if group != "" {
		key = group + "." + key
	}

	v := a.Value.Resolve()
	switch v.Kind() {
	case slog.KindGroup:
		for i, ga := range v.Group() {
			if i > 0 {
				buf.WriteByte(' ')
			}
			writeAttr(buf, key, ga)
		}
	default:
		buf.WriteString(key)
		buf.WriteByte('=')
		buf.WriteString(v.String())
	}
}

func (h *formattedHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	newAttrs := make([]slog.Attr, len(h.attrs)+len(attrs))
	copy(newAttrs, h.attrs)
	copy(newAttrs[len(h.attrs):], attrs)
	return &formattedHandler{
		w:     h.w,
		mu:    h.mu,
		level: h.level,
		attrs: newAttrs,
		group: h.group,
		pid:   h.pid,
	}
}

func (h *formattedHandler) WithGroup(name string) slog.Handler {
	newGroup := name
	if h.group != "" {
		newGroup = h.group + "." + name
	}
	return &formattedHandler{
		w:     h.w,
		mu:    h.mu,
		level: h.level,
		attrs: h.attrs,
		group: newGroup,
		pid:   h.pid,
	}
}

// discardHandler 丢弃所有日志（用于文件 handler 创建失败时的 fallback）
type discardHandler struct{}

func (discardHandler) Enabled(context.Context, slog.Level) bool  { return false }
func (discardHandler) Handle(context.Context, slog.Record) error { return nil }
func (discardHandler) WithAttrs([]slog.Attr) slog.Handler        { return discardHandler{} }
func (discardHandler) WithGroup(string) slog.Handler             { return discardHandler{} }

// 确保编译器检查接口实现
var _ slog.Handler = (*formattedHandler)(nil)
var _ slog.Handler = discardHandler{}

// redirectStderrToCrashLog 将 stderr（fd 2）重定向到 logs 目录下的 crash.log。
// 必须在 main() 最开始调用，以便捕获 Go runtime fatal（如 OOM）写到 stderr 的完整 stack trace。
// Go runtime 的 fatal 输出直接写底层 Windows STD_ERROR_HANDLE，不经过 os.Stderr 变量，
// 因此需同时调用 windows.SetStdHandle 和更新 os.Stderr。
// 正常运行时不向文件写入任何内容；发生崩溃时由 runtime 直接写入原始信息。
// 文件以 O_APPEND 打开，不持有排他锁，外部工具可并发读取。
func redirectStderrToCrashLog() {
	logDir, err := config.GetLogsDir()
	if err != nil {
		logDir = filepath.Join(os.TempDir(), buildvariant.AppName(), "logs")
	}
	os.MkdirAll(logDir, 0755)

	crashLogPath := filepath.Join(logDir, "crash.log")
	f, err := os.OpenFile(crashLogPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		return
	}

	windows.SetStdHandle(windows.STD_ERROR_HANDLE, windows.Handle(f.Fd()))
	os.Stderr = f
}

// setupLogger 初始化日志系统，返回配置好的 logger
// 日志文件位于 %LOCALAPPDATA%\WindInput\logs\wind_input.log
func setupLogger(levelStr string) *slog.Logger {
	var level slog.Level
	switch levelStr {
	case "debug":
		level = slog.LevelDebug
	case "warn":
		level = slog.LevelWarn
	case "error":
		level = slog.LevelError
	default:
		level = slog.LevelInfo
	}

	logDir, err := config.GetLogsDir()
	if err != nil {
		logDir = filepath.Join(os.TempDir(), buildvariant.AppName(), "logs")
	}
	os.MkdirAll(logDir, 0755)
	logFileName := "wind_input.log"
	if buildvariant.IsDebug() {
		logFileName = "wind_input_debug.log"
	}
	logFilePath := filepath.Join(logDir, logFileName)

	var handler slog.Handler
	rotWriter, err := newRotatingWriter(logFilePath, 5*1024*1024, 3) // 5MB, 3 backups
	if err == nil {
		handler = newFormattedHandler(rotWriter, level)
	} else {
		handler = discardHandler{}
	}

	logger := slog.New(handler)
	slog.SetDefault(logger)
	return logger
}
