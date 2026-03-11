package ui

import (
	"os"
	"path/filepath"
	"sync"

	"github.com/golang/freetype/truetype"
)

// GDI font weight constants (Windows LOGFONT.lfWeight values)
const (
	FontWeightThin       = 100
	FontWeightExtraLight = 200
	FontWeightLight      = 300
	FontWeightNormal     = 400 // Default
	FontWeightMedium     = 500
	FontWeightSemiBold   = 600
	FontWeightBold       = 700
)

// FontConfig holds centralized font configuration for all UI components.
// Instead of each component maintaining its own hardcoded font list,
// all components share this configuration for consistent font management.
type FontConfig struct {
	// PrimaryFont is the user-configured font file path (may be empty for auto-detection)
	PrimaryFont string
	// SystemFonts lists system fonts in priority order for fallback.
	// When a font lacks certain glyphs, subsequent fonts in the list are tried.
	SystemFonts []string
	// UserFonts holds user-configured additional fonts (prepended before SystemFonts).
	// Reserved for future use: users can configure preferred fonts via config file.
	UserFonts []string

	// GDIFontWeight controls the font weight for GDI rendering.
	// Valid range: 100 (thin) to 900 (heavy). Common values:
	//   400 = Normal (default), 500 = Medium, 600 = SemiBold, 700 = Bold
	// Higher values produce thicker/heavier strokes.
	GDIFontWeight int

	// GDIFontScale controls the font size multiplier for GDI rendering.
	// Default 1.0 means lfHeight = -fontSize (character height = fontSize pixels).
	// Values > 1.0 produce larger text (e.g., 1.15 makes GDI text ~15% larger).
	// Useful for matching visual size between GDI and FreeType backends.
	GDIFontScale float64
}

// defaultSystemFontNames lists font file names (relative to system Fonts directory).
// Ordered by priority: CJK-capable fonts first, then symbol/Latin fonts.
var defaultSystemFontNames = []string{
	"msyh.ttc",     // Microsoft YaHei (best CJK + Latin coverage)
	"segoeui.ttf",  // Segoe UI (Latin, UI symbols)
	"seguisym.ttf", // Segoe UI Symbol (✓, ▸, and other symbols)
	"arial.ttf",    // Arial (Latin fallback)
}

// getSystemFontsDir returns the system Fonts directory path.
// Uses WINDIR environment variable to avoid hardcoding "C:\Windows".
func getSystemFontsDir() string {
	winDir := os.Getenv("WINDIR")
	if winDir == "" {
		// Fallback: try SystemRoot (always set on Windows)
		winDir = os.Getenv("SystemRoot")
	}
	if winDir == "" {
		// Last resort fallback
		winDir = "C:\\Windows"
	}
	return filepath.Join(winDir, "Fonts")
}

// buildDefaultSystemFonts constructs full paths from font file names and system Fonts directory.
func buildDefaultSystemFonts() []string {
	fontsDir := getSystemFontsDir()
	fonts := make([]string, len(defaultSystemFontNames))
	for i, name := range defaultSystemFontNames {
		fonts[i] = filepath.Join(fontsDir, name)
	}
	return fonts
}

// NewFontConfig creates a FontConfig with the default system font chain.
func NewFontConfig() *FontConfig {
	return &FontConfig{
		SystemFonts:   buildDefaultSystemFonts(),
		GDIFontWeight: FontWeightMedium,
		GDIFontScale:  1.0,
	}
}

// SetUserFonts sets user-configured fonts that take priority over system fonts.
// These are prepended before SystemFonts when resolving the primary font.
// Reserved for future config file integration.
func (fc *FontConfig) SetUserFonts(fonts []string) {
	fc.UserFonts = fonts
}

// allFonts returns the combined font list: UserFonts first, then SystemFonts.
func (fc *FontConfig) allFonts() []string {
	if len(fc.UserFonts) == 0 {
		return fc.SystemFonts
	}
	combined := make([]string, 0, len(fc.UserFonts)+len(fc.SystemFonts))
	combined = append(combined, fc.UserFonts...)
	combined = append(combined, fc.SystemFonts...)
	return combined
}

// ResolvePrimaryFont returns the first available font path.
// Search order: PrimaryFont → UserFonts → SystemFonts.
func (fc *FontConfig) ResolvePrimaryFont() string {
	if fc.PrimaryFont != "" {
		if _, err := os.Stat(fc.PrimaryFont); err == nil {
			return fc.PrimaryFont
		}
	}
	for _, path := range fc.allFonts() {
		if _, err := os.Stat(path); err == nil {
			return path
		}
	}
	return ""
}

// GetFallbackFonts returns all available fonts after the primary,
// in priority order, for fallback rendering of missing glyphs.
func (fc *FontConfig) GetFallbackFonts() []string {
	primary := fc.ResolvePrimaryFont()
	var fallbacks []string
	for _, path := range fc.allFonts() {
		if path != primary {
			if _, err := os.Stat(path); err == nil {
				fallbacks = append(fallbacks, path)
			}
		}
	}
	return fallbacks
}

// SetPrimaryFont sets a user-configured primary font.
func (fc *FontConfig) SetPrimaryFont(fontPath string) {
	fc.PrimaryFont = fontPath
}

// SetGDIFontWeight sets the GDI font weight (100-900).
// Common values: 400=Normal, 500=Medium, 600=SemiBold, 700=Bold.
func (fc *FontConfig) SetGDIFontWeight(weight int) {
	if weight < 100 {
		weight = 100
	}
	if weight > 900 {
		weight = 900
	}
	fc.GDIFontWeight = weight
}

// SetGDIFontScale sets the GDI font size multiplier (0.5-2.0).
func (fc *FontConfig) SetGDIFontScale(scale float64) {
	if scale < 0.5 {
		scale = 0.5
	}
	if scale > 2.0 {
		scale = 2.0
	}
	fc.GDIFontScale = scale
}

// GetEffectiveGDIWeight returns the GDI font weight, defaulting to 400 if unset.
func (fc *FontConfig) GetEffectiveGDIWeight() int {
	if fc.GDIFontWeight <= 0 {
		return FontWeightNormal
	}
	return fc.GDIFontWeight
}

// GetEffectiveGDIScale returns the GDI font scale, defaulting to 1.0 if unset.
func (fc *FontConfig) GetEffectiveGDIScale() float64 {
	if fc.GDIFontScale <= 0 {
		return 1.0
	}
	return fc.GDIFontScale
}

// --- Global font registry (package-level singleton) ---
// All UI components share parsed truetype.Font instances to avoid loading
// the same font file multiple times. Each font file (e.g., msyh.ttc ~25MB)
// is read and parsed only once, regardless of how many components use it.

var (
	globalFontsMu sync.Mutex
	globalFonts   map[string]*truetype.Font // path -> parsed font
)

// GetSharedFont returns a shared parsed truetype.Font for the given path.
// The font is loaded and parsed only once; subsequent calls return the cached instance.
// This is used by both fontCache (primary font) and freeTypeDrawer (fallback fonts)
// to ensure font data is not duplicated in memory.
func GetSharedFont(path string) (*truetype.Font, error) {
	globalFontsMu.Lock()
	defer globalFontsMu.Unlock()

	if globalFonts == nil {
		globalFonts = make(map[string]*truetype.Font)
	}

	if f, ok := globalFonts[path]; ok {
		return f, nil
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	f, err := truetype.Parse(data)
	if err != nil {
		return nil, err
	}
	globalFonts[path] = f
	return f, nil
}

// fallbackFontEntry holds a parsed font and its source path.
type fallbackFontEntry struct {
	font *truetype.Font
	path string
}

// GetSharedFallbackFonts returns shared fallback font entries for the given paths.
// Each font is loaded via GetSharedFont (global cache), so no duplication occurs.
func GetSharedFallbackFonts(fallbackPaths []string) []fallbackFontEntry {
	var entries []fallbackFontEntry
	for _, path := range fallbackPaths {
		f, err := GetSharedFont(path)
		if err != nil {
			continue
		}
		entries = append(entries, fallbackFontEntry{font: f, path: path})
	}
	return entries
}
