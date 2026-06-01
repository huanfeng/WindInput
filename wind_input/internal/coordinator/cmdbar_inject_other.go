//go:build !darwin

package coordinator

import "github.com/huanfeng/wind_input/internal/keyinject"

// cmdbar_inject_other.go — 非 darwin (Windows/Linux) 的按键注入实现。
// 直接走 internal/keyinject (Windows: user32.SendInput; 其它: stub)。
// 与 macOS 的 push→.app→CGEvent 通路 (cmdbar_inject_darwin.go) 区分。

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

func (cmdbarKeysService) Hold(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	return keyinject.Hold(c)
}

func (cmdbarKeysService) Release(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	return keyinject.Release(c)
}

func (cmdbarKeysService) TypeText(text string) error {
	return keyinject.TypeText(text)
}

// Paste 合成 Ctrl+V 粘贴当前剪贴板内容 (与历史行为一致)。
func (cmdbarClipService) Paste() error {
	c, err := keyinject.Parse("Ctrl+V")
	if err != nil {
		return err
	}
	return keyinject.Tap(c)
}
