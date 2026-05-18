// cmdbar_services.go — 命令直通车 (cmdbar) 的 Services 适配层。
// 把 wind_input 现有的 clipboard / keyinject / proc / dict / ui 模块封装成
// cmdbar 期望的细粒度接口, 由 coordinator 在创建时装配并注入到 EvalContext。
// 设计文档参考 docs/design/command-bar-design.md §3.4 / §7.4。
package coordinator

import (
	"fmt"

	"github.com/huanfeng/wind_input/internal/clipboard"
	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/keyinject"
	"github.com/huanfeng/wind_input/internal/proc"
	"github.com/huanfeng/wind_input/pkg/config"
)

// cmdbarClipService 实现 cmdbar.ClipboardService, 直接转发到内部 clipboard 包。
type cmdbarClipService struct{}

func (cmdbarClipService) SetText(text string) error { return clipboard.SetText(text) }
func (cmdbarClipService) GetText() (string, error)  { return clipboard.GetText() }

// cmdbarKeysService 实现 cmdbar.KeyInjector, 通过 keyinject.Parse 把
// combo 字符串转换为 Combo 后注入按键。
type cmdbarKeysService struct{}

func (cmdbarKeysService) Tap(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	return keyinject.Tap(c)
}

func (cmdbarKeysService) Sequence(combos ...string) error {
	cs := make([]keyinject.Combo, 0, len(combos))
	for _, s := range combos {
		c, err := keyinject.Parse(s)
		if err != nil {
			return err
		}
		cs = append(cs, c)
	}
	return keyinject.Sequence(cs...)
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
// Toggle 支持以下 target (P5):
//   - "cn-en"     切换中英模式 (等同工具栏点击)
//   - "fullshape" 切换全/半角
//   - "layout"    候选框横/纵布局互切
//   - "candwin"   隐藏/显示候选窗
//
// 未知 target 返回 error 而非 silent log, 方便用户在 wind_setting 试错。
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

// toggleCandidateLayoutForCmdbar 在横/纵候选布局之间互切 (layout target)。
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
}

// toggleCandidateWindowForCmdbar 隐藏/显示候选窗 (candwin target)。
// 真值由 uiManager.hideCandidateWindow 维护; hideUI 不影响该开关。
func (c *Coordinator) toggleCandidateWindowForCmdbar() {
	if c.uiManager == nil {
		return
	}
	c.uiManager.SetHideCandidateWindow(!c.uiManager.IsHideCandidateWindow())
}

// buildCmdbarServices 装配 cmdbar.Services, 由 NewCoordinator 在初始化阶段调用。
// SearchEngine 留 nil, 让 cmdbar 的 search() 走默认 URL 组装 + URLOpener 兜底。
func (c *Coordinator) buildCmdbarServices() *cmdbar.Services {
	return &cmdbar.Services{
		Clip: cmdbarClipService{},
		Keys: cmdbarKeysService{},
		Open: cmdbarOpenService{},
		Proc: cmdbarProcService{},
		Dict: cmdbarDictService{c: c},
		IME:  cmdbarIMEService{c: c},
		// Search: nil — 走默认 URL 组装即可
	}
}
