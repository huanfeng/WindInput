// Package keyinject sends synthesized keyboard input to the foreground
// window via the Win32 SendInput API. It is used by the command-bar
// `key.tap` / `key.seq` action functions.
//
// The token names accepted by Parse are intentionally kept in sync with
// pkg/keys/keys.go (the project's canonical key-name registry). When
// adding new token names here, also update pkg/keys so consumers stay
// consistent — see docs/design/enum-constraint.md.
package keyinject

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
)

// Combo describes one keystroke: a primary key plus zero or more
// modifier keys held while the key is pressed.
type Combo struct {
	Key       string   // canonical key name, e.g. "enter", "a", "f1", "/"
	Modifiers []string // subset of {"ctrl","shift","alt","win"}, lower-cased
}

// String returns the canonical "Ctrl+Shift+Enter"-style form of c, used
// for diagnostics. The output is not guaranteed to round-trip through
// Parse beyond Parse's documented forgiveness on case / aliases.
func (c Combo) String() string {
	var parts []string
	for _, m := range c.Modifiers {
		parts = append(parts, titleASCII(m))
	}
	parts = append(parts, c.Key)
	return strings.Join(parts, "+")
}

// titleASCII upper-cases the first byte of s if it is an ASCII letter.
// Used only for diagnostic display of modifier names ("ctrl" → "Ctrl").
func titleASCII(s string) string {
	if s == "" {
		return s
	}
	b := []byte(s)
	if b[0] >= 'a' && b[0] <= 'z' {
		b[0] -= 32
	}
	return string(b)
}

// ErrEmptyCombo is returned by Parse for empty / whitespace-only input.
var ErrEmptyCombo = errors.New("keyinject: empty combo")

// Parse converts a textual combo such as "Ctrl+Shift+End" into a Combo.
// Token matching is case-insensitive, and `+` is the separator. The
// primary key must be the last segment. Modifier order is canonicalised
// to ctrl < shift < alt < win regardless of the input order.
func Parse(s string) (Combo, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return Combo{}, ErrEmptyCombo
	}
	rawParts := strings.Split(s, "+")
	// Trim each segment.
	for i := range rawParts {
		rawParts[i] = strings.TrimSpace(rawParts[i])
		if rawParts[i] == "" {
			return Combo{}, fmt.Errorf("keyinject: empty segment in %q", s)
		}
	}
	// Last segment is the primary key.
	keyTok := strings.ToLower(rawParts[len(rawParts)-1])
	key, ok := normalizeKey(keyTok)
	if !ok {
		return Combo{}, fmt.Errorf("keyinject: unknown key %q", rawParts[len(rawParts)-1])
	}
	// Preceding segments are modifiers.
	seen := map[string]struct{}{}
	for _, m := range rawParts[:len(rawParts)-1] {
		ml := strings.ToLower(m)
		mn, ok := normalizeMod(ml)
		if !ok {
			return Combo{}, fmt.Errorf("keyinject: unknown modifier %q", m)
		}
		seen[mn] = struct{}{}
	}
	// Stable canonical order.
	var mods []string
	for _, m := range []string{"ctrl", "shift", "alt", "win"} {
		if _, ok := seen[m]; ok {
			mods = append(mods, m)
		}
	}
	return Combo{Key: key, Modifiers: mods}, nil
}

func normalizeMod(s string) (string, bool) {
	switch s {
	case "ctrl", "control":
		return "ctrl", true
	case "shift":
		return "shift", true
	case "alt", "menu":
		return "alt", true
	case "win", "super", "meta", "cmd":
		return "win", true
	}
	return "", false
}

// keyAliases maps lower-case textual key names to a canonical key name
// that vkFromName understands. Stays aligned with pkg/keys/keys.go.
var keyAliases = map[string]string{
	// punctuation char ↔ canonical
	"`": "grave", "grave": "grave", "backtick": "grave", "~": "grave",
	";": "semicolon", "semicolon": "semicolon",
	"'": "quote", "quote": "quote",
	",": "comma", "comma": "comma",
	".": "period", "period": "period",
	"-": "minus", "minus": "minus",
	"=": "equal", "equal": "equal", "plus": "equal",
	"[": "lbracket", "lbracket": "lbracket", "open_bracket": "lbracket",
	"]": "rbracket", "rbracket": "rbracket", "close_bracket": "rbracket",
	"\\": "backslash", "backslash": "backslash",
	"/": "slash", "slash": "slash",

	// control keys
	"space": "space",
	"tab":   "tab",
	"enter": "enter", "return": "enter",
	"backspace": "backspace", "back": "backspace",
	"escape": "escape", "esc": "escape",
	"pageup": "pageup", "page_up": "pageup", "prior": "pageup",
	"pagedown": "pagedown", "page_down": "pagedown", "next": "pagedown",
	"delete": "delete", "del": "delete",
	"insert": "insert", "ins": "insert",
	"home": "home",
	"end":  "end",
	"up":   "up", "arrowup": "up",
	"down": "down", "arrowdown": "down",
	"left": "left", "arrowleft": "left",
	"right": "right", "arrowright": "right",
	"capslock":    "capslock",
	"printscreen": "printscreen", "prtsc": "printscreen",
	"scrolllock": "scrolllock",
	"pause":      "pause",
}

// normalizeKey resolves an input token to its canonical key name. It
// accepts:
//   - letters a..z (case-insensitive)
//   - digits 0..9
//   - f1..f24
//   - aliases from keyAliases above
//   - vk:<hex|dec> — raw Windows virtual-key code, e.g. "vk:0x5D" or "vk:93"
func normalizeKey(s string) (string, bool) {
	if s == "" {
		return "", false
	}
	// vk:<number> — raw Virtual Key code passthrough.
	// "vk:0x5D" 按 16 进制解析; 其它形式按 10 进制解析 (如 "vk:93")。
	if strings.HasPrefix(s, "vk:") {
		rest := s[3:]
		var num uint64
		var err error
		if strings.HasPrefix(rest, "0x") || strings.HasPrefix(rest, "0X") {
			num, err = strconv.ParseUint(rest[2:], 16, 16)
		} else {
			num, err = strconv.ParseUint(rest, 10, 16)
		}
		if err != nil || num == 0 || num > 0xFF {
			return "", false
		}
		return fmt.Sprintf("vk:%02x", num), true
	}
	// f1..f24
	if (len(s) == 2 || len(s) == 3) && (s[0] == 'f' || s[0] == 'F') {
		// validate digits
		n := 0
		for i := 1; i < len(s); i++ {
			c := s[i]
			if c < '0' || c > '9' {
				n = -1
				break
			}
			n = n*10 + int(c-'0')
		}
		if n >= 1 && n <= 24 {
			return fmt.Sprintf("f%d", n), true
		}
	}
	if len(s) == 1 {
		c := s[0]
		switch {
		case c >= 'a' && c <= 'z':
			return s, true
		case c >= 'A' && c <= 'Z':
			return strings.ToLower(s), true
		case c >= '0' && c <= '9':
			return s, true
		}
	}
	if v, ok := keyAliases[s]; ok {
		return v, true
	}
	return "", false
}
