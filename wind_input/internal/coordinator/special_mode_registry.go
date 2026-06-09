// special_mode_registry.go — 引导键特殊模式实例注册表 + 码表懒加载。
// 设计见 docs/design/special-mode-codetable.md。
package coordinator

import (
	"fmt"
	"log/slog"
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
	mu         sync.Mutex
	instances  []*specialModeInstance
	schemasDir string
	logger     *slog.Logger
}

// newSpecialModeRegistry 校验配置、去重 id/触发键, 构造注册表。无效实例跳过+WARN。
func newSpecialModeRegistry(cfgs []config.SpecialModeConfig, schemasDir string, logger *slog.Logger) *specialModeRegistry {
	r := &specialModeRegistry{schemasDir: schemasDir, logger: logger}
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

// ensureLoaded 懒加载实例码表(转 wdb + LoadBinary), 缓存到实例。
func (r *specialModeRegistry) ensureLoaded(inst *specialModeInstance) (*dict.CodeTable, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if inst.table != nil {
		return inst.table, nil
	}
	srcPath := filepath.Join(r.schemasDir, inst.cfg.Table)
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
