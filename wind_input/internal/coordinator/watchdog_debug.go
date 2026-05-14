//go:build debug

package coordinator

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
)

const (
	watchdogInterval  = 30 * time.Second
	watchdogThreshold = 100 // goroutine 数量超过此值时触发告警并转储
)

// startGoroutineWatchdog 在 debug build 中启动 goroutine 数量监控。
// 当 goroutine 数量超过阈值时记录 ERROR 并将完整堆栈转储到诊断目录，
// 用于定位 goroutine 泄漏或死锁（如大量客户端请求 goroutine 阻塞在 c.mu）。
// 此 goroutine 不持有任何协调器锁，即使发生死锁时也能正常输出诊断信息。
func (c *Coordinator) startGoroutineWatchdog() {
	c.logger.Info("[watchdog] goroutine watchdog started (debug build)",
		"interval", watchdogInterval,
		"threshold", watchdogThreshold)

	go func() {
		ticker := time.NewTicker(watchdogInterval)
		defer ticker.Stop()

		for range ticker.C {
			n := runtime.NumGoroutine()
			if n < watchdogThreshold {
				c.logger.Debug("[watchdog] goroutine count OK", "count", n)
				continue
			}

			path := dumpGoroutines(n)
			c.logger.Error("[watchdog] goroutine count exceeded threshold — possible deadlock or goroutine leak",
				"count", n,
				"threshold", watchdogThreshold,
				"dump", path)
		}
	}()
}

func dumpGoroutines(count int) string {
	logsDir, err := config.GetLogsDir()
	if err != nil || logsDir == "" {
		logsDir = os.TempDir()
	}

	path := filepath.Join(logsDir, fmt.Sprintf("goroutine_watchdog_%s.txt", time.Now().Format("20060102_150405")))
	f, err := os.Create(path)
	if err != nil {
		return fmt.Sprintf("(dump failed: %v)", err)
	}
	defer f.Close()

	fmt.Fprintf(f, "goroutine count at dump time: %d\n\n", count)
	if err := pprof.Lookup("goroutine").WriteTo(f, 2); err != nil {
		return fmt.Sprintf("(pprof write failed: %v)", err)
	}
	return path
}
