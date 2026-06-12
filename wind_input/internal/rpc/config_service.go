package rpc

import (
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// ConfigService 配置管理 RPC 服务
//
// 锁契约：cfgMu 守护 *cfg 的读写，由 Server 持有并与 ReloadHandler / SystemService /
// StatsService 共享。本服务的 Set/SetAll/Reset 在写锁内调用 configReloader.ApplyConfigUpdate，
// 所以 ApplyConfigUpdate 自身不会再次获取 cfgMu，避免 RWMutex 不可重入死锁。
type ConfigService struct {
	cfgMu          *sync.RWMutex
	cfg            *config.Config
	configReloader ConfigReloader
	broadcaster    *EventBroadcaster
	schemaMgr      SchemaOverrideResetter
	logger         *slog.Logger
	saveFn         func(*config.Config) error // 默认 config.Save，测试可替换为 no-op
}

// GetAll 获取完整配置
func (s *ConfigService) GetAll(args *rpcapi.Empty, reply *rpcapi.ConfigGetAllReply) error {
	s.cfgMu.RLock()
	cfgCopy := *s.cfg
	s.cfgMu.RUnlock()

	data, err := json.Marshal(&cfgCopy)
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}
	reply.Config = data
	return nil
}

// Get 按 key 获取配置项
func (s *ConfigService) Get(args *rpcapi.ConfigGetArgs, reply *rpcapi.ConfigGetReply) error {
	s.cfgMu.RLock()
	cfgCopy := *s.cfg
	s.cfgMu.RUnlock()

	reply.Values = make(map[string]any, len(args.Keys))
	for _, key := range args.Keys {
		section, path, err := resolveKeyPath(key)
		if err != nil {
			return err
		}
		sectionMap, err := getSectionMap(&cfgCopy, section)
		if err != nil {
			return err
		}
		val, err := getNestedKey(sectionMap, path)
		if err != nil {
			return fmt.Errorf("key %q: %w", key, err)
		}
		reply.Values[key] = val
	}
	return nil
}

// Set 设置配置项（校验+持久化+热更新）
func (s *ConfigService) Set(args *rpcapi.ConfigSetArgs, reply *rpcapi.ConfigSetReply) error {
	s.cfgMu.Lock()
	defer s.cfgMu.Unlock()

	newCfg := deepCopyConfig(s.cfg)
	changedSections := map[string]bool{}

	for _, item := range args.Items {
		section, path, err := resolveKeyPath(item.Key)
		if err != nil {
			return fmt.Errorf("invalid key %q: %w", item.Key, err)
		}
		sectionMap, err := getSectionMap(newCfg, section)
		if err != nil {
			return err
		}
		if err := setNestedKey(sectionMap, path, item.Value); err != nil {
			return fmt.Errorf("set key %q: %w", item.Key, err)
		}
		if err := setSectionFromMap(newCfg, section, sectionMap); err != nil {
			return fmt.Errorf("apply section %q: %w", section, err)
		}
		changedSections[section] = true
		reply.Applied = append(reply.Applied, item.Key)
	}

	config.ApplyConfigFallbacks(newCfg)

	if err := s.saveFn(newCfg); err != nil {
		return fmt.Errorf("save config: %w", err)
	}

	requiresRestart, err := s.configReloader.ApplyConfigUpdate(s.cfg, newCfg, changedSections)
	if err != nil {
		s.logger.Error("ApplyConfigUpdate failed", "error", err)
	}
	reply.RequiresRestart = requiresRestart

	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeConfig, Action: rpcapi.EventActionUpdate})
	return nil
}

// SetAll 设置完整配置（内部 diff 后走精准热更新）
func (s *ConfigService) SetAll(args *rpcapi.ConfigSetAllArgs, reply *rpcapi.ConfigSetAllReply) error {
	s.cfgMu.Lock()
	defer s.cfgMu.Unlock()

	newCfg := deepCopyConfig(s.cfg)
	if err := json.Unmarshal(args.Config, newCfg); err != nil {
		// 类型不匹配（*json.UnmarshalTypeError）时，encoding/json 仍会部分填充 newCfg，
		// 不应让单个字段类型错误导致整体保存失败。只记字段名元数据，继续用部分填充的 newCfg。
		var jsonTypeErr *json.UnmarshalTypeError
		if errors.As(err, &jsonTypeErr) {
			s.logger.Warn("保存配置时部分字段类型不匹配，已忽略该字段", "field", jsonTypeErr.Field)
		} else {
			// 其它错误（如 JSON 语法错误）无法部分解码，保持失败返回。
			return fmt.Errorf("unmarshal config: %w", err)
		}
	}

	// stats 配置由专用接口 Stats.UpdateConfig 独立管理，全局配置表单（前端 formData）
	// 并不包含 stats 字段。反序列化时缺失的 *bool 会被当作 null，从而把服务端已有的
	// track_english/enabled 覆盖为 nil（IsTrackEnglish() 又回退到默认 true），导致用户
	// 在统计页关闭的设置被全局保存冲掉。这里强制保留服务端现有 stats，使其只受专用接口影响。
	newCfg.Features.Stats = s.cfg.Features.Stats

	config.ApplyConfigFallbacks(newCfg)

	// 计算变更的 section
	changedSections := diffSections(s.cfg, newCfg)

	if err := s.saveFn(newCfg); err != nil {
		return fmt.Errorf("save config: %w", err)
	}

	requiresRestart, err := s.configReloader.ApplyConfigUpdate(s.cfg, newCfg, changedSections)
	if err != nil {
		s.logger.Error("ApplyConfigUpdate failed", "error", err)
	}
	reply.RequiresRestart = requiresRestart

	// 收集所有 key（用于 Applied 列表，这里简化为 section 级别）
	for sec := range changedSections {
		reply.Applied = append(reply.Applied, sec)
	}

	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeConfig, Action: rpcapi.EventActionUpdate})
	return nil
}

// GetDefaults 获取系统默认配置
func (s *ConfigService) GetDefaults(_ *rpcapi.Empty, reply *rpcapi.ConfigGetDefaultsReply) error {
	def := config.SystemDefaultConfig()
	data, err := json.Marshal(def)
	if err != nil {
		return fmt.Errorf("marshal defaults: %w", err)
	}
	reply.Config = data
	return nil
}

// Reset 重置指定 key 为默认值
// 注意：此方法不加 s.mu 锁，而是委托给 Set 完成原子化写入，避免 sync.Mutex 不可重入导致死锁。
func (s *ConfigService) Reset(args *rpcapi.ConfigResetArgs, reply *rpcapi.ConfigResetReply) error {
	def := config.SystemDefaultConfig()
	items := make([]rpcapi.ConfigSetItem, 0, len(args.Keys))
	for _, key := range args.Keys {
		section, path, err := resolveKeyPath(key)
		if err != nil {
			return err
		}
		defMap, err := getSectionMap(def, section)
		if err != nil {
			return err
		}
		val, err := getNestedKey(defMap, path)
		if err != nil {
			return fmt.Errorf("key %q not found in defaults: %w", key, err)
		}
		items = append(items, rpcapi.ConfigSetItem{Key: key, Value: val})
	}
	setReply := &rpcapi.ConfigSetReply{}
	if err := s.Set(&rpcapi.ConfigSetArgs{Items: items}, setReply); err != nil {
		return err
	}
	reply.Reset = setReply.Applied
	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeConfig, Action: rpcapi.EventActionReset})
	return nil
}

// GetSchemaOverride 获取方案覆盖配置
func (s *ConfigService) GetSchemaOverride(args *rpcapi.SchemaOverrideArgs, reply *rpcapi.SchemaOverrideReply) error {
	data, err := config.GetSchemaOverride(args.SchemaID)
	if err != nil {
		return err
	}
	reply.Data = data
	return nil
}

// SetSchemaOverride 设置方案覆盖配置
func (s *ConfigService) SetSchemaOverride(args *rpcapi.SchemaOverrideSetArgs, _ *rpcapi.Empty) error {
	if err := config.SetSchemaOverride(args.SchemaID, args.Data); err != nil {
		return err
	}
	return s.configReloader.ReloadConfig()
}

// DeleteSchemaOverride 只删除 Layer 3 override（保留 Layer 2 用户 schema 文件）
func (s *ConfigService) DeleteSchemaOverride(args *rpcapi.SchemaOverrideArgs, _ *rpcapi.Empty) error {
	if err := config.DeleteSchemaOverride(args.SchemaID); err != nil {
		return err
	}
	return s.configReloader.ReloadConfig()
}

// ResetSchemaOverride 删除 Layer 3 override + Layer 2 用户 schema diff 文件（如有内置方案）
func (s *ConfigService) ResetSchemaOverride(args *rpcapi.SchemaOverrideArgs, _ *rpcapi.Empty) error {
	if err := config.DeleteSchemaOverride(args.SchemaID); err != nil {
		return err
	}
	if s.schemaMgr != nil {
		if _, hasBuiltin := s.schemaMgr.GetBuiltinSchemaPath(args.SchemaID); hasBuiltin {
			if userPath, hasUser := s.schemaMgr.GetUserSchemaPath(args.SchemaID); hasUser {
				if err := os.Remove(userPath); err != nil && !os.IsNotExist(err) {
					s.logger.Warn("remove user schema file failed", "path", userPath, "error", err)
				}
			}
		}
	}
	return s.configReloader.ReloadConfig()
}

// SetActiveSchema 切换活跃方案（复用 Set 逻辑，原子修改+热更新）
func (s *ConfigService) SetActiveSchema(args *rpcapi.SetActiveSchemaArgs, _ *rpcapi.Empty) error {
	setReply := &rpcapi.ConfigSetReply{}
	return s.Set(&rpcapi.ConfigSetArgs{
		Items: []rpcapi.ConfigSetItem{{Key: "schema.active", Value: args.SchemaID}},
	}, setReply)
}

// ── 辅助函数 ──

// resolveKeyPath 将点号路径解析为 section 和子路径
// "ui.font_size" → section="ui", path=["font_size"]
// "input.auto_pair.chinese" → section="input", path=["auto_pair","chinese"]
func resolveKeyPath(key string) (section string, path []string, err error) {
	parts := strings.SplitN(key, ".", 2)
	if len(parts) < 2 || parts[0] == "" || parts[1] == "" {
		return "", nil, fmt.Errorf("key must be in section.field format, got %q", key)
	}
	section = parts[0]
	path = strings.Split(parts[1], ".")
	return section, path, nil
}

// getSectionMap 将 config struct 的某个 section 序列化为 map[string]any
func getSectionMap(cfg *config.Config, section string) (map[string]any, error) {
	var sectionVal any
	switch rpcapi.ConfigSection(section) {
	case rpcapi.ConfigSectionGeneral:
		sectionVal = cfg.General
	case rpcapi.ConfigSectionSchema:
		sectionVal = cfg.Schema
	case rpcapi.ConfigSectionHotkeys:
		sectionVal = cfg.Hotkeys
	case rpcapi.ConfigSectionInput:
		sectionVal = cfg.Input
	case rpcapi.ConfigSectionUI:
		sectionVal = cfg.UI
	case rpcapi.ConfigSectionFeatures:
		sectionVal = cfg.Features
	case rpcapi.ConfigSectionCompat:
		sectionVal = cfg.Compat
	case rpcapi.ConfigSectionDebug:
		sectionVal = cfg.Debug
	default:
		return nil, fmt.Errorf("unknown config section %q", section)
	}

	data, err := json.Marshal(sectionVal)
	if err != nil {
		return nil, fmt.Errorf("marshal section %q: %w", section, err)
	}
	var m map[string]any
	if err := json.Unmarshal(data, &m); err != nil {
		return nil, fmt.Errorf("unmarshal section %q: %w", section, err)
	}
	return m, nil
}

// setSectionFromMap 将修改后的 map 反序列化回 config struct 的对应 section。
//
// 注意：必须先将目标 section 清零再 Unmarshal，否则 encoding/json 对 map 类型字段
// 执行合并语义（JSON {} 不清空已有条目），导致删除 mappings 条目时旧数据残留。
// sectionMap 来自 getSectionMap（完整序列化整个 section），清零后 Unmarshal 不会丢失其他字段。
func setSectionFromMap(cfg *config.Config, section string, m map[string]any) error {
	data, err := json.Marshal(m)
	if err != nil {
		return fmt.Errorf("marshal section map %q: %w", section, err)
	}
	switch rpcapi.ConfigSection(section) {
	case rpcapi.ConfigSectionGeneral:
		cfg.General = config.GeneralConfig{}
		return json.Unmarshal(data, &cfg.General)
	case rpcapi.ConfigSectionSchema:
		cfg.Schema = config.SchemaConfig{}
		return json.Unmarshal(data, &cfg.Schema)
	case rpcapi.ConfigSectionHotkeys:
		cfg.Hotkeys = config.HotkeyConfig{}
		return json.Unmarshal(data, &cfg.Hotkeys)
	case rpcapi.ConfigSectionInput:
		cfg.Input = config.InputConfig{}
		return json.Unmarshal(data, &cfg.Input)
	case rpcapi.ConfigSectionUI:
		cfg.UI = config.UIConfig{}
		return json.Unmarshal(data, &cfg.UI)
	case rpcapi.ConfigSectionFeatures:
		cfg.Features = config.FeaturesConfig{}
		return json.Unmarshal(data, &cfg.Features)
	case rpcapi.ConfigSectionCompat:
		cfg.Compat = config.CompatConfig{}
		return json.Unmarshal(data, &cfg.Compat)
	case rpcapi.ConfigSectionDebug:
		cfg.Debug = config.DebugConfig{}
		return json.Unmarshal(data, &cfg.Debug)
	default:
		return fmt.Errorf("unknown config section %q", section)
	}
}

// getNestedKey 从 map 中按路径读取值，支持多级嵌套
func getNestedKey(m map[string]any, path []string) (any, error) {
	cur := any(m)
	for i, key := range path {
		curMap, ok := cur.(map[string]any)
		if !ok {
			return nil, fmt.Errorf("path segment %q: expected object, got %T", strings.Join(path[:i], "."), cur)
		}
		val, exists := curMap[key]
		if !exists {
			return nil, fmt.Errorf("key %q not found", key)
		}
		cur = val
	}
	return cur, nil
}

// setNestedKey 按路径设置 map 中的嵌套值，支持多级嵌套
func setNestedKey(m map[string]any, path []string, value any) error {
	if len(path) == 1 {
		m[path[0]] = value
		return nil
	}
	sub, exists := m[path[0]]
	if !exists {
		sub = map[string]any{}
	}
	subMap, ok := sub.(map[string]any)
	if !ok {
		subMap = map[string]any{}
	}
	if err := setNestedKey(subMap, path[1:], value); err != nil {
		return err
	}
	m[path[0]] = subMap
	return nil
}

// deepCopyConfig 通过 JSON roundtrip 深拷贝
func deepCopyConfig(cfg *config.Config) *config.Config {
	data, _ := json.Marshal(cfg)
	newCfg := &config.Config{}
	json.Unmarshal(data, newCfg)
	return newCfg
}

// diffSections 比较两个 config，返回发生变更的 section 集合
func diffSections(oldCfg, newCfg *config.Config) map[string]bool {
	sections := map[string]bool{}
	type sectionPair struct {
		name string
		old  any
		new  any
	}
	pairs := []sectionPair{
		{string(rpcapi.ConfigSectionGeneral), oldCfg.General, newCfg.General},
		{string(rpcapi.ConfigSectionSchema), oldCfg.Schema, newCfg.Schema},
		{string(rpcapi.ConfigSectionHotkeys), oldCfg.Hotkeys, newCfg.Hotkeys},
		{string(rpcapi.ConfigSectionInput), oldCfg.Input, newCfg.Input},
		{string(rpcapi.ConfigSectionUI), oldCfg.UI, newCfg.UI},
		{string(rpcapi.ConfigSectionFeatures), oldCfg.Features, newCfg.Features},
		{string(rpcapi.ConfigSectionCompat), oldCfg.Compat, newCfg.Compat},
		{string(rpcapi.ConfigSectionDebug), oldCfg.Debug, newCfg.Debug},
	}
	for _, p := range pairs {
		oldData, _ := json.Marshal(p.old)
		newData, _ := json.Marshal(p.new)
		if string(oldData) != string(newData) {
			sections[p.name] = true
		}
	}
	return sections
}
