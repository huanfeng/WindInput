//go:build windows

package systemfont

import (
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"

	"golang.org/x/sys/windows/registry"
)

// FontInfo describes a system-installed font family.
type FontInfo struct {
	Family      string `json:"family"`
	DisplayName string `json:"display_name"`
}

type catalog struct {
	fonts        []FontInfo
	families     map[string][]string
	displayNames map[string]string
}

var (
	catalogOnce sync.Once
	cached      catalog
	cachedErr   error

	// localizedDone is closed when async localized name resolution finishes.
	localizedDone  chan struct{}
	localizedFonts []FontInfo // populated by the background goroutine

	// localeAliasMu protects localeAliasMap and dwFamilyMap.
	// Both are written before localizedDone is closed; reads are safe after that.
	localeAliasMu sync.RWMutex

	// localeAliasMap maps normalizeKey(anyLocaleName) → font file paths.
	// Allows fonts to be found by any locale variant of their family name.
	localeAliasMap map[string][]string

	// dwFamilyMap maps normalizeKey(anyLocaleName) → DirectWrite-compatible
	// family name (nameID=1 for system locale). DirectWrite's FindFamilyName
	// uses nameID=1, which may differ from the Windows registry key (nameID=4).
	dwFamilyMap map[string]string
)

var styleSuffixes = []string{
	" ExtraBold",
	" Extra Light",
	" ExtraLight",
	" SemiBold",
	" Semibold",
	" Semi Light",
	" SemiLight",
	" Medium",
	" Regular",
	" Italic",
	" Oblique",
	" Condensed",
	" Narrow",
	" Light",
	" Black",
	" Bold",
	" Thin",
}

var fallbackFamilies = []FontInfo{
	{Family: "Microsoft YaHei", DisplayName: "Microsoft YaHei"},
	{Family: "Segoe UI", DisplayName: "Segoe UI"},
	{Family: "Segoe UI Symbol", DisplayName: "Segoe UI Symbol"},
	{Family: "Arial", DisplayName: "Arial"},
}

func systemFontsDir() string {
	winDir := os.Getenv("WINDIR")
	if winDir == "" {
		winDir = os.Getenv("SystemRoot")
	}
	if winDir == "" {
		winDir = "C:\\Windows"
	}
	return filepath.Join(winDir, "Fonts")
}

func normalizeKey(v string) string {
	return strings.ToLower(strings.Join(strings.Fields(strings.TrimSpace(v)), " "))
}

func trimRegistrySuffix(name string) string {
	name = strings.TrimSpace(name)
	for _, suffix := range []string{" (TrueType)", " (OpenType)", " (All res)"} {
		if strings.HasSuffix(strings.ToLower(name), strings.ToLower(suffix)) {
			name = strings.TrimSpace(name[:len(name)-len(suffix)])
			break
		}
	}
	return name
}

func trimStyleSuffix(name string) string {
	name = strings.TrimSpace(name)
	for {
		trimmed := name
		lower := strings.ToLower(name)
		for _, suffix := range styleSuffixes {
			if strings.HasSuffix(lower, strings.ToLower(suffix)) {
				trimmed = strings.TrimSpace(name[:len(name)-len(suffix)])
				break
			}
		}
		if trimmed == name {
			return name
		}
		name = trimmed
	}
}

func extractFamilies(displayName string) []string {
	raw := trimRegistrySuffix(displayName)
	if raw == "" {
		return nil
	}

	seen := make(map[string]struct{})
	var out []string

	add := func(name string) {
		name = strings.TrimSpace(name)
		if name == "" {
			return
		}
		key := normalizeKey(name)
		if _, ok := seen[key]; ok {
			return
		}
		seen[key] = struct{}{}
		out = append(out, name)
	}

	for _, part := range strings.Split(raw, "&") {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		add(trimStyleSuffix(part))
	}

	if len(out) == 0 {
		add(trimStyleSuffix(raw))
	}

	return out
}

func resolveFontPath(fileName string) string {
	fileName = strings.TrimSpace(fileName)
	if fileName == "" {
		return ""
	}
	if filepath.IsAbs(fileName) {
		return fileName
	}
	return filepath.Join(systemFontsDir(), fileName)
}

func appendUniquePath(paths []string, path string) []string {
	key := normalizeKey(path)
	for _, existing := range paths {
		if normalizeKey(existing) == key {
			return paths
		}
	}
	return append(paths, path)
}

func loadRegistryFonts(root registry.Key, path string, cat *catalog) error {
	key, err := registry.OpenKey(root, path, registry.READ)
	if err != nil {
		return err
	}
	defer key.Close()

	names, err := key.ReadValueNames(-1)
	if err != nil {
		return err
	}

	for _, name := range names {
		value, _, err := key.GetStringValue(name)
		if err != nil {
			continue
		}
		fontPath := resolveFontPath(value)
		for _, family := range extractFamilies(name) {
			fkey := normalizeKey(family)
			cat.families[fkey] = appendUniquePath(cat.families[fkey], fontPath)
			if _, ok := cat.displayNames[fkey]; !ok {
				cat.displayNames[fkey] = family
			}
		}
		raw := trimRegistrySuffix(name)
		for _, alias := range strings.Split(raw, "&") {
			alias = strings.TrimSpace(alias)
			if alias == "" {
				continue
			}
			akey := normalizeKey(alias)
			cat.families[akey] = appendUniquePath(cat.families[akey], fontPath)
		}
	}

	return nil
}

func ensureCatalog() error {
	catalogOnce.Do(func() {
		cached = catalog{
			families:     make(map[string][]string),
			displayNames: make(map[string]string),
		}

		paths := []struct {
			root registry.Key
			path string
		}{
			{registry.LOCAL_MACHINE, `SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts`},
			{registry.CURRENT_USER, `SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts`},
		}

		for _, item := range paths {
			_ = loadRegistryFonts(item.root, item.path, &cached)
		}

		seen := make(map[string]struct{})
		for key, paths := range cached.families {
			if len(paths) == 0 {
				continue
			}
			name := cached.displayNames[key]
			if name == "" {
				continue
			}
			seen[key] = struct{}{}
			cached.fonts = append(cached.fonts, FontInfo{
				Family:      name,
				DisplayName: name,
			})
		}

		if len(cached.fonts) == 0 {
			for _, fallback := range fallbackFamilies {
				key := normalizeKey(fallback.Family)
				if _, ok := seen[key]; ok {
					continue
				}
				cached.displayNames[key] = fallback.Family
				cached.fonts = append(cached.fonts, fallback)
			}
		}

		sort.Slice(cached.fonts, func(i, j int) bool {
			return strings.ToLower(cached.fonts[i].DisplayName) < strings.ToLower(cached.fonts[j].DisplayName)
		})

		// Resolve localized display names asynchronously by parsing font files'
		// name tables. This avoids slowing down IME startup (HasFamily/ResolveFile
		// don't need display names). List() blocks on this before returning.
		localizedDone = make(chan struct{})
		go resolveLocalizedDisplayNames()
	})
	return cachedErr
}

// resolveLocalizedDisplayNames reads each font file's name table to find
// Chinese localized family names, then rebuilds the font list with those names.
// It also builds localeAliasMap so that any locale variant of a font family
// name (e.g. the en-US nameID=1 vs the zh-CN nameID=1) resolves correctly.
func resolveLocalizedDisplayNames() {
	defer close(localizedDone)

	// Collect unique font paths per family key
	type entry struct {
		key  string
		path string
	}
	var entries []entry
	for key, paths := range cached.families {
		if len(paths) > 0 && cached.displayNames[key] != "" {
			entries = append(entries, entry{key: key, path: paths[0]})
		}
	}

	// Parse name tables: collect zh-CN display names, locale aliases, and DW family names.
	localNames := make(map[string]string, len(entries))
	alias := make(map[string][]string)
	dw := make(map[string]string)
	for _, e := range entries {
		data := readNameTableData(e.path)
		if data == nil {
			continue
		}
		zhName := parseChineseFamilyName(data)
		if zhName != "" {
			localNames[e.key] = zhName
		}

		paths := cached.families[e.key]
		if len(paths) == 0 {
			continue
		}

		allNames := parseAllFamilyNames(data)

		// The DirectWrite-compatible name is nameID-1 for the system locale.
		// Prefer zh-CN (parseChineseFamilyName), fall back to first nameID-1 entry.
		dwName := zhName
		if dwName == "" && len(allNames) > 0 {
			dwName = allNames[0]
		}

		if dwName != "" {
			// Map the registry key → DW name (covers the common case where user
			// picks the font from the UI and gets the registry key saved).
			dw[e.key] = dwName
		}

		// Index every locale family name that differs from the registry key.
		for _, name := range allNames {
			nk := normalizeKey(name)
			if nk != e.key {
				alias[nk] = appendUniquePath(alias[nk], paths[0])
			}
			// Also map every locale variant → DW name.
			if dwName != "" {
				dw[nk] = dwName
			}
		}
	}

	// Publish both maps before closing localizedDone so readers see them.
	localeAliasMu.Lock()
	localeAliasMap = alias
	dwFamilyMap = dw
	localeAliasMu.Unlock()

	if len(localNames) == 0 {
		return // no localized names found; List() will use cached.fonts as-is
	}

	// Build new font list with localized DisplayName
	fonts := make([]FontInfo, len(cached.fonts))
	copy(fonts, cached.fonts)
	for i := range fonts {
		key := normalizeKey(fonts[i].Family)
		if localName, ok := localNames[key]; ok {
			fonts[i].DisplayName = localName
		}
	}

	sort.Slice(fonts, func(i, j int) bool {
		return strings.ToLower(fonts[i].DisplayName) < strings.ToLower(fonts[j].DisplayName)
	})

	localizedFonts = fonts
}

// List returns installed system font families with localized display names.
// Blocks until async localized name resolution is complete.
func List() ([]FontInfo, error) {
	err := ensureCatalog()

	// Wait for localized names to be resolved
	<-localizedDone

	src := localizedFonts
	if src == nil {
		src = cached.fonts
	}
	fonts := make([]FontInfo, len(src))
	copy(fonts, src)
	return fonts, err
}

// HasFamily reports whether the family exists in the catalog.
// On a miss it waits for locale alias resolution to complete and retries,
// so fonts whose registry key name differs from their name-table family name
// (e.g. locale-specific variants) are still found correctly.
func HasFamily(family string) bool {
	_ = ensureCatalog()
	key := normalizeKey(family)
	if _, ok := cached.families[key]; ok {
		return true
	}
	// Primary lookup missed: wait for the alias map (built in background).
	if localizedDone != nil {
		<-localizedDone
	}
	localeAliasMu.RLock()
	_, ok := localeAliasMap[key]
	localeAliasMu.RUnlock()
	return ok
}

func isSingleFontFile(path string) bool {
	switch strings.ToLower(filepath.Ext(path)) {
	case ".ttf", ".otf":
		return true
	default:
		return false
	}
}

// ResolveFile returns a file path for a family name.
// When singleFontOnly is true, TTC collections are skipped.
// On a miss it waits for locale alias resolution and retries, so fonts whose
// registry key name differs from their name-table family name are still found.
func ResolveFile(family string, singleFontOnly bool) string {
	_ = ensureCatalog()
	key := normalizeKey(family)

	if path := firstAvailable(cached.families[key], singleFontOnly); path != "" {
		return path
	}
	// Primary lookup missed: wait for the alias map (built in background).
	if localizedDone != nil {
		<-localizedDone
	}
	localeAliasMu.RLock()
	aliasPaths := localeAliasMap[key]
	localeAliasMu.RUnlock()
	return firstAvailable(aliasPaths, singleFontOnly)
}

// firstAvailable returns the first path in paths that exists on disk.
func firstAvailable(paths []string, singleFontOnly bool) string {
	for _, path := range paths {
		if singleFontOnly && !isSingleFontFile(path) {
			continue
		}
		if _, err := os.Stat(path); err == nil {
			return path
		}
	}
	return ""
}

// ResolveDWFamily returns the DirectWrite-compatible family name (nameID=1)
// for a font identified by any of its registered or locale-specific names.
//
// Windows Font Registry keys are derived from nameID=4 (full name), whereas
// DirectWrite's IDWriteFontCollection::FindFamilyName uses nameID=1. For fonts
// where these differ (e.g. "浪漫雅圆字体" in the registry vs "浪漫雅圆+Sleek修改版"
// as nameID-1), passing the registry key to DirectWrite silently fails and falls
// back to an unrelated system font.
//
// Returns "" if the font is not installed or its name table cannot be read.
func ResolveDWFamily(family string) string {
	_ = ensureCatalog()
	key := normalizeKey(family)
	// Wait for the DW family map (built alongside localized display names).
	if localizedDone != nil {
		<-localizedDone
	}
	localeAliasMu.RLock()
	name := dwFamilyMap[key]
	localeAliasMu.RUnlock()
	return name
}
