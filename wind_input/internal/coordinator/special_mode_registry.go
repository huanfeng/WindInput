// special_mode_registry.go — 引导键特殊模式实例注册表 + 码表懒加载。
// 设计见 docs/design/special-mode-codetable.md。
package coordinator

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sync"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
	"github.com/huanfeng/wind_input/pkg/config"
)

type specialModeInstance struct {
	cfg     config.SpecialModeConfig
	table   *dict.CodeTable
	loadErr error
}

type specialModeRegistry struct {
	mu        sync.Mutex
	instances []*specialModeInstance
	// schemasDirs 是码表文件的候选基目录（按优先级，靠前者覆盖）。
	// 通常为 [用户配置目录/schemas, 内置 dataRoot/schemas]，与 DiscoverSchemas 同源。
	schemasDirs []string
	logger      *slog.Logger
}

// newSpecialModeRegistry 校验配置、去重 id/触发键, 构造注册表。无效实例跳过+WARN。
// schemasDirs 为码表文件候选基目录（按优先级），ensureLoaded 取首个存在的解析路径。
func newSpecialModeRegistry(cfgs []config.SpecialModeConfig, schemasDirs []string, logger *slog.Logger) *specialModeRegistry {
	r := &specialModeRegistry{schemasDirs: schemasDirs, logger: logger}
	seenID := map[string]bool{}
	seenKey := map[string]string{}
	for _, c := range cfgs {
		if err := c.Validate(); err != nil {
			logger.Warn("special mode 配置无效，跳过", "err", err.Error())
			continue
		}
		if seenID[c.ID] {
			logger.Warn("special mode id 重复，跳过", "id", c.ID)
			continue
		}
		for _, k := range c.TriggerKeys {
			if owner, ok := seenKey[k]; ok {
				logger.Warn("special mode 触发键被占用", "key", k, "owner", owner, "skipped", c.ID)
			} else {
				seenKey[k] = c.ID
			}
		}
		seenID[c.ID] = true
		r.instances = append(r.instances, &specialModeInstance{cfg: c})
	}
	return r
}

func (r *specialModeRegistry) match(key string, keyCode int) string {
	for _, inst := range r.instances {
		if matchTriggerKeyInList(inst.cfg.TriggerKeys, key, keyCode) != "" {
			return inst.cfg.ID
		}
	}
	return ""
}

func (r *specialModeRegistry) get(id string) *specialModeInstance {
	for _, inst := range r.instances {
		if inst.cfg.ID == id {
			return inst
		}
	}
	return nil
}

// resolveTablePath 在候选基目录中解析码表文件，返回首个存在的绝对路径；
// 都不存在时返回首个候选拼接路径（供加载失败时的错误信息使用）。
func (r *specialModeRegistry) resolveTablePath(table string) string {
	var fallback string
	for i, base := range r.schemasDirs {
		p := filepath.Join(base, table)
		if i == 0 {
			fallback = p
		}
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	if fallback != "" {
		return fallback
	}
	return table
}

// ensureLoaded 懒加载实例码表(转 wdb + LoadBinary), 缓存到实例。
func (r *specialModeRegistry) ensureLoaded(inst *specialModeInstance) (*dict.CodeTable, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if inst.table != nil {
		return inst.table, nil
	}
	srcPath := r.resolveTablePath(inst.cfg.Table)
	cacheKey := "special-" + inst.cfg.ID
	wdbPath := dictcache.CachePath(cacheKey)
	srcPaths := dictcache.RimeCodetableSourcePaths(srcPath)
	if len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbPath) {
		if err := dictcache.ConvertRimeCodetableToWdb(srcPath, wdbPath, r.logger); err != nil {
			inst.loadErr = fmt.Errorf("转换特殊码表失败 %s: %w", inst.cfg.ID, err)
			return nil, inst.loadErr
		}
	}
	ct := dict.NewCodeTable()
	if err := ct.LoadBinary(wdbPath); err != nil {
		inst.loadErr = fmt.Errorf("加载特殊码表 wdb 失败 %s: %w", inst.cfg.ID, err)
		return nil, inst.loadErr
	}
	inst.table = ct
	r.logger.Info("特殊码表已加载", "id", inst.cfg.ID, "entries", ct.EntryCount())
	return ct, nil
}
