package ui

import (
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
)

// unified_menu_build.go — 统一菜单项构建 (平台无关)。
// 从 manager.go (Windows) 抽出, 让 darwin forwarder/manager 也能复用同一菜单结构。
// UnifiedMenu* 常量 / MenuItem / UnifiedMenuState 定义在 types_neutral.go。

func aboutText(version string) string {
	if version != "" && version != "dev" {
		return "关于 (" + version + ")"
	}
	return "关于"
}

// BuildUnifiedMenuItems constructs the unified menu item list.
func BuildUnifiedMenuItems(state UnifiedMenuState) []MenuItem {
	// Build schema submenu: 英文 + available schemas
	var schemaChildren []MenuItem
	schemaChildren = append(schemaChildren, MenuItem{
		ID:      UnifiedMenuSchemaEnglish,
		Text:    "英文",
		Checked: !state.ChineseMode,
	})
	if len(state.Schemas) > 0 {
		schemaChildren = append(schemaChildren, MenuItem{Separator: true})
		for i, s := range state.Schemas {
			schemaChildren = append(schemaChildren, MenuItem{
				ID:      UnifiedMenuSchemaBase + i,
				Text:    s.Name,
				Checked: state.ChineseMode && s.ID == state.CurrentSchemaID,
			})
		}
	}

	// Build filter mode submenu
	filterMode := state.CurrentFilterMode
	if filterMode == "" {
		filterMode = config.FilterSmart
	}
	filterChildren := []MenuItem{
		{ID: UnifiedMenuFilterModeBase, Text: "智能模式", Checked: filterMode == config.FilterSmart},
		{ID: UnifiedMenuFilterModeBase + 1, Text: "常用字", Checked: filterMode == config.FilterGeneral},
		{ID: UnifiedMenuFilterModeBase + 2, Text: "全部字符", Checked: filterMode == config.FilterGB18030},
	}

	// 简入繁出子菜单
	s2tVariant := state.S2TVariant
	if s2tVariant == "" {
		s2tVariant = config.S2TStandard
	}
	s2tChildren := []MenuItem{
		{ID: UnifiedMenuToggleS2T, Text: "启用", Checked: state.S2TEnabled},
		{Separator: true},
		{ID: UnifiedMenuS2TVariantBase, Text: "标准繁体", Checked: s2tVariant == config.S2TStandard},
		{ID: UnifiedMenuS2TVariantBase + 1, Text: "台湾繁体", Checked: s2tVariant == config.S2TTaiwan},
		{ID: UnifiedMenuS2TVariantBase + 2, Text: "台湾繁体（含词汇）", Checked: s2tVariant == config.S2TTaiwanPhrase},
		{ID: UnifiedMenuS2TVariantBase + 3, Text: "香港繁体", Checked: s2tVariant == config.S2THongKong},
	}

	items := []MenuItem{
		{Text: "输入方案", Children: schemaChildren},
		{Text: "检索范围", Children: filterChildren},
		{ID: UnifiedMenuToggleWidth, Text: "全角", Checked: state.FullWidth},
		{ID: UnifiedMenuTogglePunct, Text: "中文标点", Checked: state.ChinesePunct},
		{Text: "简入繁出", Children: s2tChildren},
	}
	if !state.OmitToolbarToggle {
		items = append(items,
			MenuItem{Separator: true},
			MenuItem{ID: UnifiedMenuToggleToolbar, Text: "显示工具栏", Checked: state.ToolbarVisible},
		)
	}

	// Build theme submenu if there are themes
	if len(state.Themes) > 0 {
		var themeChildren []MenuItem
		for i, t := range state.Themes {
			themeChildren = append(themeChildren, MenuItem{
				ID:      UnifiedMenuThemeBase + i,
				Text:    t.DisplayName,
				Checked: t.ID == state.CurrentThemeID,
			})
		}
		themeStyle := state.CurrentThemeStyle
		if themeStyle == "" {
			themeStyle = config.ThemeStyleSystem
		}
		themeChildren = append(themeChildren, MenuItem{Separator: true})
		themeChildren = append(themeChildren,
			MenuItem{ID: UnifiedMenuThemeStyleBase, Text: "跟随系统", Checked: themeStyle == config.ThemeStyleSystem},
			MenuItem{ID: UnifiedMenuThemeStyleBase + 1, Text: "亮色", Checked: themeStyle == config.ThemeStyleLight},
			MenuItem{ID: UnifiedMenuThemeStyleBase + 2, Text: "暗色", Checked: themeStyle == config.ThemeStyleDark},
		)
		items = append(items, MenuItem{Text: "主题", Children: themeChildren})
	}

	// Debug: 三级菜单测试
	if buildvariant.IsDebug() {
		testSubA := []MenuItem{
			{ID: UnifiedMenuTestBase, Text: "选项 A-1", Checked: true},
			{ID: UnifiedMenuTestBase + 1, Text: "选项 A-2"},
			{ID: UnifiedMenuTestBase + 2, Text: "选项 A-3"},
		}
		testSubB := []MenuItem{
			{ID: UnifiedMenuTestBase + 3, Text: "选项 B-1"},
			{ID: UnifiedMenuTestBase + 4, Text: "选项 B-2", Checked: true},
		}
		toastChildren := []MenuItem{
			{ID: UnifiedMenuTestToastInfo, Text: "Info（右下）"},
			{ID: UnifiedMenuTestToastSuccess, Text: "Success（右下）"},
			{ID: UnifiedMenuTestToastWarn, Text: "Warn（居中）"},
			{ID: UnifiedMenuTestToastError, Text: "Error（居中）"},
			{Separator: true},
			{ID: UnifiedMenuTestToastLongMessage, Text: "长文本 / 换行测试"},
		}
		testChildren := []MenuItem{
			{Text: "子菜单 A", Children: testSubA},
			{Text: "子菜单 B", Children: testSubB},
			{Separator: true},
			{ID: UnifiedMenuTestBase + 5, Text: "普通项"},
			{Separator: true},
			{Text: "Toast 通知", Children: toastChildren},
		}
		items = append(items, MenuItem{Text: "三级菜单测试", Children: testChildren})
	}

	// 服务控制项 (重载配置 / 重启服务): Win/darwin 共用, 底层 reload_config 与
	// resetAndResync 均为跨平台实现。放在「高级」块之前以保持与移植前一致的布局。
	items = append(items,
		MenuItem{Separator: true},
		MenuItem{ID: UnifiedMenuReloadConfig, Text: "重载配置"},
		MenuItem{ID: UnifiedMenuRestartService, Text: "重启服务"},
	)

	if !state.OmitAdvanced {
		processLabel := state.ActiveProcessName
		if processLabel == "" {
			processLabel = "当前应用"
		}
		advancedChildren := []MenuItem{
			{ID: UnifiedMenuSkipCaretPending, Text: "为 " + processLabel + " 启用即时候选", Checked: state.SkipCaretPending},
			{ID: UnifiedMenuPinCandidatePosition, Text: "为 " + processLabel + " 启用固定候选位置", Checked: state.PinCandidatePosition},
		}
		items = append(items,
			MenuItem{Separator: true},
			MenuItem{Text: "高级", Children: advancedChildren},
		)
	}
	items = append(items,
		MenuItem{Separator: true},
		MenuItem{ID: UnifiedMenuDictionary, Text: "词库管理..."},
		MenuItem{ID: UnifiedMenuSettings, Text: "设置..."},
		MenuItem{Separator: true},
		MenuItem{ID: UnifiedMenuAbout, Text: aboutText(state.Version)},
	)

	return items
}
