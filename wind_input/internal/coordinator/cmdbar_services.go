// cmdbar_services.go — 命令直通车 (cmdbar) 的 Services 适配层。
// 把 wind_input 现有的 clipboard / keyinject / proc / dict / ui / config 模块封装成
// cmdbar 期望的细粒度接口, 由 coordinator 在创建时装配并注入到 EvalContext。
// 设计文档参考 docs/design/command-bar-design.md §3.4 / §7.4。
package coordinator

import (
	"fmt"

	"github.com/huanfeng/wind_input/internal/clipboard"
	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/proc"
	"github.com/huanfeng/wind_input/pkg/config"
)

// cmdbarClipService 实现 cmdbar.ClipboardService。SetText/GetText 直接转发到
// 内部 clipboard 包 (跨平台); Paste 平台相关 (见 cmdbar_inject_{darwin,other}.go):
// Windows 合成 Ctrl+V, macOS 读剪贴板 + .app insertText。
type cmdbarClipService struct {
	c *Coordinator
}

func (cmdbarClipService) SetText(text string) error { return clipboard.SetText(text) }
func (cmdbarClipService) GetText() (string, error)  { return clipboard.GetText() }

// cmdbarKeysService 实现 cmdbar.KeyInjector。Tap/Sequence/Hold/Release/TypeText
// 平台相关 (见 cmdbar_inject_{darwin,other}.go): Windows 直接 keyinject.SendInput,
// macOS 经 ui.Manager 下发 push 命令给 .app 用 CGEvent 合成。
type cmdbarKeysService struct {
	c *Coordinator
}

// cmdbarOpenService 实现 cmdbar.URLOpener。
type cmdbarOpenService struct{}

func (cmdbarOpenService) Open(target string) error { return proc.Open(target) }

// cmdbarProcService 实现 cmdbar.ProcessRunner。
type cmdbarProcService struct{}

func (cmdbarProcService) Run(cmd string, args ...string) error { return proc.Run(cmd, args...) }
func (cmdbarProcService) Shell(cmdline string) error           { return proc.Shell(cmdline) }
func (cmdbarProcService) ShellEx(cmdline string, flags []string) error {
	return proc.ShellEx(cmdline, flags)
}

// cmdbarDictService 实现 cmdbar.DictService, 封装 engineMgr 的加词接口。
// code 为空时调 coordinator 的 calcWordCodeForCurrentSchema 计算编码。
type cmdbarDictService struct {
	c *Coordinator
}

func (s cmdbarDictService) AddWord(text, code string) error {
	if s.c == nil || s.c.engineMgr == nil {
		return cmdbar.ErrServiceUnavailable
	}
	dm := s.c.engineMgr.GetDictManager()
	if dm == nil {
		return cmdbar.ErrServiceUnavailable
	}
	if code == "" {
		// 用与"快捷加词"相同的编码生成路径, 保持行为一致。
		// 注意: 拼音方案下走拼音码生成 (与 updateAddWordCode 对齐)。
		if s.c.engineMgr.IsPinyinSchema() {
			code = s.c.engineMgr.GeneratePinyinCode(text)
		} else {
			code = s.c.calcWordCodeForCurrentSchema(text)
		}
	}
	if code == "" {
		return fmt.Errorf("cmdbar.dict.addword: cannot derive code for text")
	}
	if err := dm.AddUserWord(code, text, addWordMaxWeight); err != nil {
		return err
	}
	if s.c.eventNotifier != nil {
		schemaID := ""
		if s.c.engineMgr != nil {
			schemaID = s.c.engineMgr.GetCurrentSchemaID()
		}
		s.c.eventNotifier.NotifyUserDictAdd(schemaID)
	}
	return nil
}

// cmdbarIMEService 实现 cmdbar.IMEController。
// Toggle 支持以下 target：
//   - "cn-en"     切换中英模式（等同工具栏点击）
//   - "fullshape" 切换全/半角
//   - "layout"    候选框横/纵布局互切（持久化）
//   - "candwin"   隐藏/显示候选窗
//   - "s2t"       简入繁出开关（持久化）
//   - "preedit"   编码显示模式循环 top ↔ embedded（持久化）
//   - "toolbar"   工具栏显隐（持久化）
//
// 未知 target 返回 error 而非 silent log，方便用户在 wind_setting 试错。
type cmdbarIMEService struct {
	c *Coordinator
}

func (s cmdbarIMEService) Toggle(target string) error {
	if s.c == nil {
		return cmdbar.ErrServiceUnavailable
	}
	switch target {
	case "cn-en":
		s.c.toggleChineseModeForCmdbar()
		return nil
	case "fullshape":
		s.c.toggleFullWidthForCmdbar()
		return nil
	case "layout":
		s.c.toggleCandidateLayoutForCmdbar()
		return nil
	case "candwin":
		s.c.toggleCandidateWindowForCmdbar()
		return nil
	case "s2t":
		return s.c.toggleS2TForCmdbar()
	case "preedit":
		return s.c.togglePreeditModeForCmdbar()
	case "toolbar":
		s.c.toggleToolbarForCmdbar()
		return nil
	default:
		return fmt.Errorf("ime.toggle: unknown target %q", target)
	}
}

func (s cmdbarIMEService) OpenSetting(page string) error {
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	s.c.uiManager.OpenSettingsWithPage(page)
	return nil
}

func (s cmdbarIMEService) OpenSettingWeb(page string) error {
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	s.c.uiManager.OpenSettingsWebMode(page)
	return nil
}

func (s cmdbarIMEService) SetSchema(id string) error {
	if s.c == nil || s.c.engineMgr == nil {
		return cmdbar.ErrServiceUnavailable
	}
	return s.c.switchSchemaForCmdbar(id)
}

func (s cmdbarIMEService) ThemeCycle(dir string) (string, error) {
	if s.c == nil || s.c.uiManager == nil || s.c.config == nil || s.c.cfgMu == nil {
		return "", cmdbar.ErrServiceUnavailable
	}
	themes := s.c.uiManager.ListThemeIDs()
	if len(themes) == 0 {
		return "", fmt.Errorf("ime.theme_cycle: no themes available")
	}
	s.c.cfgMu.RLock()
	current := s.c.config.UI.Theme
	s.c.cfgMu.RUnlock()

	idx := 0
	for i, id := range themes {
		if id == current {
			idx = i
			break
		}
	}
	var next string
	if dir == "prev" {
		next = themes[(idx-1+len(themes))%len(themes)]
	} else {
		next = themes[(idx+1)%len(themes)]
	}
	if err := (cmdbarConfigService{c: s.c}).Set("ui.theme", next); err != nil {
		return "", fmt.Errorf("ime.theme_cycle: %w", err)
	}
	return next, nil
}

// toggleChineseModeForCmdbar 等同工具栏中英切换 (cn-en target)。
// 在 c.mu 锁内翻转 chineseMode + 联动标点 + 同步工具栏/状态。
func (c *Coordinator) toggleChineseModeForCmdbar() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.chineseMode = !c.chineseMode
	if c.punctFollowMode {
		c.chinesePunctuation = c.chineseMode
	}
	c.punctConverter.Reset()
	c.saveRuntimeState()
	c.updateStatusIndicator()
	c.broadcastState()
}

// toggleFullWidthForCmdbar 翻转全/半角状态 (fullshape target)。
func (c *Coordinator) toggleFullWidthForCmdbar() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.applyToggleFullWidth()
	c.broadcastState()
}

// toggleCandidateLayoutForCmdbar 在横/纵候选布局之间互切 (layout target)，并持久化。
func (c *Coordinator) toggleCandidateLayoutForCmdbar() {
	if c.uiManager == nil {
		return
	}
	cur := c.uiManager.GetCandidateLayout()
	next := config.LayoutHorizontal
	if cur == config.LayoutHorizontal {
		next = config.LayoutVertical
	}
	c.uiManager.SetCandidateLayout(next)
	go func() {
		if c.cfgMu == nil || c.config == nil {
			return
		}
		c.cfgMu.Lock()
		c.config.UI.CandidateLayout = next
		cfgCopy := c.config.Clone()
		c.cfgMu.Unlock()
		if err := config.Save(cfgCopy); err != nil {
			c.logger.Error("Failed to save layout config", "error", err)
		}
	}()
}

// toggleS2TForCmdbar 切换简入繁出开关 (s2t target)，行为与热键路径一致。
// 调用方不持 c.mu。
func (c *Coordinator) toggleS2TForCmdbar() error {
	if c.config == nil || c.s2tManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	c.mu.Lock()
	target := !c.config.S2T.Enabled
	c.config.S2T.Enabled = target
	c.reconfigureS2T(c.config.S2T)
	if c.hasPendingInput() {
		c.updateCandidates()
		c.showUI()
	}
	c.showS2TIndicator(target)
	c.mu.Unlock()
	c.persistS2TConfigAsync()
	return nil
}

// togglePreeditModeForCmdbar 在 top / embedded 编码显示模式之间循环切换 (preedit target)，并持久化。
func (c *Coordinator) togglePreeditModeForCmdbar() error {
	if c.uiManager == nil || c.config == nil || c.cfgMu == nil {
		return cmdbar.ErrServiceUnavailable
	}
	c.cfgMu.Lock()
	cur := c.config.UI.PreeditMode
	next := config.PreeditTop
	if cur == config.PreeditTop || cur == "" {
		next = config.PreeditEmbedded
	}
	c.config.UI.PreeditMode = next
	cfgCopy := c.config.Clone()
	c.cfgMu.Unlock()

	c.uiManager.SetPreeditMode(next)
	go func() {
		if err := config.Save(cfgCopy); err != nil {
			c.logger.Error("Failed to save preedit mode config", "error", err)
		}
	}()
	return nil
}

// toggleToolbarForCmdbar 切换工具栏显隐 (toolbar target)，行为与菜单操作一致。
// 调用方不持 c.mu。
func (c *Coordinator) toggleToolbarForCmdbar() {
	c.mu.Lock()
	c.toolbarVisible = !c.toolbarVisible
	if c.toolbarReducer != nil {
		reducer := c.toolbarReducer
		visible := c.toolbarVisible
		go reducer.sendCritical(toolbarEvent{kind: tevUserPreferenceChanged, visible: visible})
	}
	c.saveToolbarConfig()
	c.broadcastState()
	c.mu.Unlock()
}

// switchSchemaForCmdbar 切换输入方案并持久化，供 ime.schema() 调用。
// 调用方不持任何锁。
func (c *Coordinator) switchSchemaForCmdbar(id string) error {
	if err := c.engineMgr.SwitchToSchemaByID(id); err != nil {
		return fmt.Errorf("switch schema %q: %w", id, err)
	}
	notifier := c.eventNotifier
	go func() {
		if c.cfgMu != nil && c.config != nil {
			c.cfgMu.Lock()
			c.config.Schema.Active = id
			cfgCopy := c.config.Clone()
			c.cfgMu.Unlock()
			if err := config.Save(cfgCopy); err != nil {
				c.logger.Error("Failed to save schema config", "error", err)
			}
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
		}
	}()
	c.mu.Lock()
	c.broadcastState()
	c.mu.Unlock()
	return nil
}

// toggleCandidateWindowForCmdbar 隐藏/显示候选窗 (candwin target)。
// 真值由 uiManager.hideCandidateWindow 维护; hideUI 不影响该开关。
func (c *Coordinator) toggleCandidateWindowForCmdbar() {
	if c.uiManager == nil {
		return
	}
	c.uiManager.SetHideCandidateWindow(!c.uiManager.IsHideCandidateWindow())
}

// cmdbarConfigService 实现 cmdbar.ConfigService。
// Get/Set/Toggle 通过 pkg/config.Fields 注册表校验键路径，Set/Toggle 在修改后
// 按 section 调用对应的 Update*Config 热更新，再异步持久化到磁盘。
type cmdbarConfigService struct {
	c *Coordinator
}

func (s cmdbarConfigService) Get(key string) (string, error) {
	if s.c == nil || s.c.config == nil || s.c.cfgMu == nil {
		return "", cmdbar.ErrServiceUnavailable
	}
	f, ok := config.GetField(key)
	if !ok {
		return "", fmt.Errorf("config.get: unknown key %q", key)
	}
	s.c.cfgMu.RLock()
	val := f.Get(s.c.config)
	s.c.cfgMu.RUnlock()
	return val, nil
}

func (s cmdbarConfigService) Set(key, value string) error {
	if s.c == nil || s.c.config == nil || s.c.cfgMu == nil {
		return cmdbar.ErrServiceUnavailable
	}
	f, ok := config.GetField(key)
	if !ok {
		return fmt.Errorf("config.set: unknown key %q", key)
	}
	s.c.cfgMu.Lock()
	if err := f.Set(s.c.config, value); err != nil {
		s.c.cfgMu.Unlock()
		return err
	}
	cfgCopy := s.c.config.Clone()
	s.c.cfgMu.Unlock()

	s.applySection(key, cfgCopy)
	go func() {
		if err := config.Save(cfgCopy); err != nil {
			s.c.logger.Error("config.set: save failed", "key", key, "error", err)
		}
	}()
	return nil
}

func (s cmdbarConfigService) Toggle(key string) (string, error) {
	if s.c == nil || s.c.config == nil || s.c.cfgMu == nil {
		return "", cmdbar.ErrServiceUnavailable
	}
	f, ok := config.GetField(key)
	if !ok {
		return "", fmt.Errorf("config.toggle: unknown key %q", key)
	}
	s.c.cfgMu.Lock()
	next, err := f.ToggleValue(s.c.config)
	if err != nil {
		s.c.cfgMu.Unlock()
		return "", fmt.Errorf("config.toggle %q: %w", key, err)
	}
	if err := f.Set(s.c.config, next); err != nil {
		s.c.cfgMu.Unlock()
		return "", err
	}
	cfgCopy := s.c.config.Clone()
	s.c.cfgMu.Unlock()

	s.applySection(key, cfgCopy)
	go func() {
		if err := config.Save(cfgCopy); err != nil {
			s.c.logger.Error("config.toggle: save failed", "key", key, "error", err)
		}
	}()
	return next, nil
}

// applySection 根据 key 的顶层区段调用对应的 Update*Config 热更新。
// 注意：Update*Config 内部会获取 c.mu，调用此函数时不得持有 c.mu 或 cfgMu。
func (s cmdbarConfigService) applySection(key string, cfgCopy *config.Config) {
	c := s.c
	switch config.Section(key) {
	case "ui":
		c.UpdateUIConfig(&cfgCopy.UI)
	case "s2t":
		c.UpdateS2TConfig(&cfgCopy.S2T)
	case "input":
		c.UpdateInputConfig(&cfgCopy.Input)
	case "startup":
		c.UpdateStartupConfig(&cfgCopy.Startup)
	}
}

// buildCmdbarServices 装配 cmdbar.Services, 由 NewCoordinator 在初始化阶段调用。
// SearchEngine 留 nil, 让 cmdbar 的 search() 走默认 URL 组装 + URLOpener 兜底。
func (c *Coordinator) buildCmdbarServices() *cmdbar.Services {
	return &cmdbar.Services{
		Clip:   cmdbarClipService{c: c},
		Keys:   cmdbarKeysService{c: c},
		Open:   cmdbarOpenService{},
		Proc:   cmdbarProcService{},
		Dict:   cmdbarDictService{c: c},
		IME:    cmdbarIMEService{c: c},
		Config: cmdbarConfigService{c: c},
		// Search: nil — 走默认 URL 组装即可
	}
}
