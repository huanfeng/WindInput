//go:build darwin

package coordinator

import (
	"github.com/huanfeng/wind_input/internal/clipboard"
	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/keyinject"
	"github.com/huanfeng/wind_input/internal/uicmd"
)

// cmdbar_inject_darwin.go — macOS 的按键注入实现。
//
// Go 服务进程 (LaunchAgent) 无 GUI 事件上下文, 不能直接合成按键。改为把已用
// keyinject.Parse 解析的规范组合经 ui.Manager 下发 push 命令给 IMKit `.app`,
// 由 `.app` 用 CGEvent (tap/seq/hold/release) 或 client.insertText (type/paste)
// 实际执行。修饰键 win 在 .app 侧映射为 Command。
//
// keyinject.Parse 本身跨平台 (键名 / 修饰键规范化), 仅 Tap/Sequence 等"真正合成"
// 的入口在 darwin 是 stub —— 这里绕过 stub, 走 push 通路。

func (s cmdbarKeysService) Tap(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	s.c.uiManager.SendKeyTap(c.Key, c.Modifiers)
	return nil
}

func (s cmdbarKeysService) Sequence(combos ...string) error {
	cs := make([]uicmd.KeyCombo, 0, len(combos))
	for _, str := range combos {
		c, err := keyinject.Parse(str)
		if err != nil {
			return err
		}
		cs = append(cs, uicmd.KeyCombo{Key: c.Key, Modifiers: c.Modifiers})
	}
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	s.c.uiManager.SendKeySeq(cs)
	return nil
}

func (s cmdbarKeysService) Hold(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	s.c.uiManager.SendKeyHold(c.Key, c.Modifiers)
	return nil
}

func (s cmdbarKeysService) Release(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	s.c.uiManager.SendKeyRelease(c.Key, c.Modifiers)
	return nil
}

func (s cmdbarKeysService) TypeText(text string) error {
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	s.c.uiManager.SendKeyType(text)
	return nil
}

// Paste 读剪贴板文本, 经 .app client.insertText 上屏 (不模拟 Cmd+V, 无需辅助
// 功能授权)。剪贴板为空时静默返回。
func (s cmdbarClipService) Paste() error {
	if s.c == nil || s.c.uiManager == nil {
		return cmdbar.ErrServiceUnavailable
	}
	text, err := clipboard.GetText()
	if err != nil {
		if s.c.logger != nil {
			s.c.logger.Warn("cmdbar clip.paste darwin: GetText 失败", "error", err)
		}
		return err
	}
	if text == "" {
		return nil
	}
	s.c.uiManager.SendKeyType(text)
	return nil
}
