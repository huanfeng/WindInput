package ui

import (
	"os"
	"path/filepath"
	"strings"
	"sync"

	ggtext "github.com/gogpu/gg/text"
	"github.com/huanfeng/wind_input/pkg/systemfont"
)

// GDI font weight constants (Windows LOGFONT.lfWeight values).
// 这些值直接映射到 Windows LOGFONT.lfWeight，便于配置层和原生渲染层统一。
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
// GDI / DirectWrite use the general system font chain, while gogpu/gg text
// uses a separate TTF/OTF-only chain because TTC collections are unsupported.
type FontConfig struct {
	// PrimaryFont stores the configured primary font spec.
	// Current usage prefers system font family names; explicit file paths are only
	// retained for internal fallback and gg/text resolution.
	PrimaryFont string
	// SystemFonts lists system fonts in priority order for fallback.
	// When a font lacks certain glyphs, subsequent fonts in the list are tried.
	// GDI / DirectWrite use this general Windows font chain directly.
	SystemFonts []string
	// UserFonts holds user-configured additional fonts (prepended before SystemFonts).
	// Reserved for future use: users can configure preferred fonts via config file.
	UserFonts []string

	// GDIFontWeight controls the font weight for GDI rendering (candidate box).
	// Valid range: 100 (thin) to 900 (heavy). Common values:
	//   400 = Normal, 500 = Medium (default), 600 = SemiBold, 700 = Bold
	// Note: GDI weight 400 and 500 look nearly identical; 600 is the minimum
	// for visibly bolder text. Menu components use their own weight setting.
	GDIFontWeight int
	// GDIFontScale controls the font size multiplier for GDI rendering.
	// Default 1.0 means lfHeight = -fontSize (character height = fontSize pixels).
	// Values > 1.0 produce larger text (e.g., 1.15 makes GDI text ~15% larger).
	// Useful for matching visual size between GDI and gg/text backends.
	GDIFontScale float64
}

// defaultSystemFontNames lists font file names (relative to system Fonts directory).
// Ordered by priority: CJK-capable fonts first, then symbol/Latin fonts.
// TTC entries are allowed here because GDI / DirectWrite can handle them.
var defaultSystemFontNames = []string{
	"msyh.ttc",     // Microsoft YaHei (best CJK + Latin coverage)
	"segoeui.ttf",  // Segoe UI (Latin, UI symbols)
	"seguisym.ttf", // Segoe UI Symbol (✓, ▸, and other symbols)
	"arial.ttf",    // Arial (Latin fallback)
}

// defaultTextPrimaryFontNames is the TTF/OTF-only primary chain for gogpu/gg text.
// It mirrors the original intent of "pick a CJK-capable UI font first", but avoids TTC.
var defaultTextPrimaryFontNames = []string{
	"simhei.ttf",  // SimHei
	"Deng.ttf",    // DengXian Regular
	"Dengb.ttf",   // DengXian Bold
	"simkai.ttf",  // KaiTi
	"simfang.ttf", // FangSong
}

// defaultTextFallbackFontNames is ordered for common IME UI fallback cases.
// Symbol fonts come first so menu glyphs like ✓ / ▸ do not trigger loading the
// entire CJK fallback chain before reaching Segoe UI Symbol.
var defaultTextFallbackFontNames = []string{
	"seguisym.ttf", // Segoe UI Symbol
	"segoeui.ttf",  // Segoe UI
	"arial.ttf",    // Arial
	"simhei.ttf",   // SimHei
	"Deng.ttf",     // DengXian Regular
	"Dengb.ttf",    // DengXian Bold
	"simkai.ttf",   // KaiTi
	"simfang.ttf",  // FangSong
}

// getSystemFontsDir returns the system Fonts directory path.
// Uses WINDIR environment variable to avoid hardcoding "C:\\Windows".
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

// buildFontPaths converts file names into absolute paths under the system font dir.
func buildFontPaths(names []string) []string {
	fontsDir := getSystemFontsDir()
	fonts := make([]string, len(names))
	for i, name := range names {
		fonts[i] = filepath.Join(fontsDir, name)
	}
	return fonts
}

func buildDefaultSystemFonts() []string {
	return buildFontPaths(defaultSystemFontNames)
}

func buildDefaultTextPrimaryFonts() []string {
	return buildFontPaths(defaultTextPrimaryFontNames)
}

func buildDefaultTextFallbackFonts() []string {
	return buildFontPaths(defaultTextFallbackFontNames)
}

// isGGTextCompatibleFont reports whether gogpu/gg text can load the file directly.
// 当前只接受单字体文件；TTC collection 仍留给 GDI / DirectWrite 路径处理。
func isGGTextCompatibleFont(path string) bool {
	switch strings.ToLower(filepath.Ext(path)) {
	case ".ttf", ".otf":
		return true
	default:
		return false
	}
}

func availableFont(path string) bool {
	if path == "" {
		return false
	}
	_, err := os.Stat(path)
	return err == nil
}

func resolveConfiguredFontPath(spec string, singleFontOnly bool) string {
	if availableFont(spec) {
		if !singleFontOnly || isGGTextCompatibleFont(spec) {
			return spec
		}
	}
	if path := systemfont.ResolveFile(spec, singleFontOnly); path != "" {
		return path
	}
	return ""
}

func appendUnique(paths []string, seen map[string]struct{}, path string) []string {
	if path == "" {
		return paths
	}
	key := strings.ToLower(path)
	if _, ok := seen[key]; ok {
		return paths
	}
	seen[key] = struct{}{}
	return append(paths, path)
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

// textPrimaryFonts returns a deduplicated primary font list for gogpu/gg text rendering.
// Only TTF / OTF entries are returned, because this path must never select a
// TTC collection such as msyh.ttc.
func (fc *FontConfig) textPrimaryFonts() []string {
	seen := make(map[string]struct{})
	fonts := make([]string, 0, len(fc.UserFonts)+len(fc.SystemFonts)+len(defaultTextPrimaryFontNames))

	for _, path := range fc.UserFonts {
		if isGGTextCompatibleFont(path) {
			fonts = appendUnique(fonts, seen, path)
		}
	}
	for _, path := range buildDefaultTextPrimaryFonts() {
		fonts = appendUnique(fonts, seen, path)
	}
	for _, path := range fc.SystemFonts {
		if isGGTextCompatibleFont(path) {
			fonts = appendUnique(fonts, seen, path)
		}
	}

	return fonts
}

// textFallbackFonts returns a deduplicated fallback chain for gogpu/gg text rendering.
// The order is intentionally different from primary selection: common UI symbol
// fonts are placed first to avoid eagerly parsing multiple large CJK fonts when
// rendering menu markers such as ✓ and ▸.
func (fc *FontConfig) textFallbackFonts() []string {
	seen := make(map[string]struct{})
	fonts := make([]string, 0, len(fc.UserFonts)+len(fc.SystemFonts)+len(defaultTextFallbackFontNames))

	for _, path := range fc.UserFonts {
		if isGGTextCompatibleFont(path) {
			fonts = appendUnique(fonts, seen, path)
		}
	}
	for _, path := range buildDefaultTextFallbackFonts() {
		fonts = appendUnique(fonts, seen, path)
	}
	for _, path := range fc.SystemFonts {
		if isGGTextCompatibleFont(path) {
			fonts = appendUnique(fonts, seen, path)
		}
	}

	return fonts
}

// ResolvePrimaryFont returns the first available font path.
// Search order: PrimaryFont → UserFonts → SystemFonts.
// Native Windows backends may still resolve to TTC fonts here.
func (fc *FontConfig) ResolvePrimaryFont() string {
	if path := resolveConfiguredFontPath(fc.PrimaryFont, false); path != "" {
		return path
	}
	for _, path := range fc.allFonts() {
		if availableFont(path) {
			return path
		}
	}
	return ""
}

// ResolvePrimaryFontFamily returns the preferred system font family for native rendering.
func (fc *FontConfig) ResolvePrimaryFontFamily() string {
	if family := strings.TrimSpace(fc.PrimaryFont); family != "" {
		if systemfont.HasFamily(family) {
			// DirectWrite uses nameID-1 for font lookup, which may differ from
			// the Windows registry key (derived from nameID-4). Resolve to the
			// nameID-1 name so DirectWrite can actually find the font.
			if dwName := systemfont.ResolveDWFamily(family); dwName != "" {
				return dwName
			}
			return family
		}
		if path := resolveConfiguredFontPath(family, false); path != "" {
			return FontSpecToName(path)
		}
	}
	if resolved := fc.ResolvePrimaryFont(); resolved != "" {
		return FontSpecToName(resolved)
	}
	return "Microsoft YaHei"
}

// ResolveTextPrimaryFont returns the first available TTF / OTF path for gogpu/gg text.
// If PrimaryFont points to an unsupported TTC file, this intentionally falls back
// to the dedicated TTF-only chain instead of failing hard.
func (fc *FontConfig) ResolveTextPrimaryFont() string {
	if path := resolveConfiguredFontPath(fc.PrimaryFont, true); path != "" {
		return path
	}
	for _, path := range fc.textPrimaryFonts() {
		if availableFont(path) {
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
		if path != primary && availableFont(path) {
			fallbacks = append(fallbacks, path)
		}
	}
	return fallbacks
}

// GetTextFallbackFonts returns all available TTF / OTF fonts after the gg/text primary.
// The list is ordered to handle common UI symbols cheaply before trying larger
// CJK fallback fonts.
func (fc *FontConfig) GetTextFallbackFonts() []string {
	primary := fc.ResolveTextPrimaryFont()
	var fallbacks []string
	for _, path := range fc.textFallbackFonts() {
		if path != primary && availableFont(path) {
			fallbacks = append(fallbacks, path)
		}
	}
	return fallbacks
}

// SetPrimaryFont sets the configured primary font family/spec.
func (fc *FontConfig) SetPrimaryFont(font string) {
	fc.PrimaryFont = font
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
// All UI components share parsed gg/text FontSource instances to avoid loading
// the same font file multiple times. Each font file is read and parsed only once,
// regardless of how many components use it.
var (
	globalFontSourcesMu sync.Mutex
	globalFontSources   map[string]*ggtext.FontSource
)

func GetSharedFontSource(path string) (*ggtext.FontSource, error) {
	globalFontSourcesMu.Lock()
	defer globalFontSourcesMu.Unlock()

	if globalFontSources == nil {
		globalFontSources = make(map[string]*ggtext.FontSource)
	}
	if source, ok := globalFontSources[path]; ok {
		return source, nil
	}

	source, err := ggtext.NewFontSourceFromFile(path)
	if err != nil {
		return nil, err
	}
	globalFontSources[path] = source
	return source, nil
}

// fallbackFontEntry holds a fallback font path.
// The actual FontSource is still loaded lazily through fontCache.getFace(),
// so merely enabling freetype mode does not parse the entire fallback chain.
type fallbackFontEntry struct {
	path string
}

// GetSharedFallbackFonts returns fallback font entries for the given paths.
// Unsupported or missing fonts are skipped, but the actual font file is not
// parsed here; parsing stays lazy and only happens when a glyph lookup needs it.
func GetSharedFallbackFonts(fallbackPaths []string) []fallbackFontEntry {
	var entries []fallbackFontEntry
	for _, path := range fallbackPaths {
		if availableFont(path) {
			entries = append(entries, fallbackFontEntry{path: path})
		}
	}
	return entries
}
