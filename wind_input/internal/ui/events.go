package ui

// managerCommand 标识 UI 线程命令的类型。
type managerCommand string

const (
	cmdShow              managerCommand = "show"
	cmdHide              managerCommand = "hide"
	cmdMode              managerCommand = "mode"
	cmdStatus            managerCommand = "status"
	cmdStatusHide        managerCommand = "status_hide"
	cmdToolbarShow       managerCommand = "toolbar_show"
	cmdToolbarHide       managerCommand = "toolbar_hide"
	cmdToolbarUpdate     managerCommand = "toolbar_update"
	cmdSettings          managerCommand = "settings"
	cmdHideMenu          managerCommand = "hide_menu"
	cmdHideToolbarMenu   managerCommand = "hide_toolbar_menu"
	cmdShowUnifiedMenu   managerCommand = "show_unified_menu"
	cmdDPIChanged        managerCommand = "dpi_changed"
	cmdRegisterHotkeys   managerCommand = "register_hotkeys"
	cmdUnregisterHotkeys managerCommand = "unregister_hotkeys"
	cmdHideTooltip       managerCommand = "hide_tooltip"
)
