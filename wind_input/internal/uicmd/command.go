package uicmd

// CommandType 是 UI 下行命令的类型标识。
// 命名规则: 模块.动作 (字符串形态), 二进制段位见 commands_id.go。
type CommandType uint16

// 下行命令 cmd id (0x06xx 段)
const (
	// --- 候选框 (0x0601 ~ 0x060F) ---
	CmdCandidatesShow     CommandType = 0x0601 // 显示候选框 (含候选词数据 + 锚点)
	CmdCandidatesHide     CommandType = 0x0602 // 隐藏候选框
	CmdCandidatesPosition CommandType = 0x0603 // 仅更新候选框位置 (光标移动跟随)
	CmdCandidatesMarkers  CommandType = 0x0604 // 模式标签 / accent 色等小标记
	CmdCandidatesConfig   CommandType = 0x0605 // 候选框布局/可见性配置
	CmdCandidatesPinState CommandType = 0x0606 // 固定位置应用记忆

	// --- 工具栏 (0x0610 ~ 0x061F) ---
	CmdToolbarShow   CommandType = 0x0610
	CmdToolbarHide   CommandType = 0x0611
	CmdToolbarUpdate CommandType = 0x0612

	// --- 状态/模式指示器 (0x0620 ~ 0x062F) ---
	CmdStatusShow   CommandType = 0x0620
	CmdStatusHide   CommandType = 0x0621
	CmdStatusConfig CommandType = 0x0622 // StatusWindowConfig 全量
	CmdModeShow     CommandType = 0x0623 // 短暂模式浮窗 (cmdMode)

	// --- Tooltip (0x0630 ~ 0x063F) ---
	CmdTooltipShow CommandType = 0x0630
	CmdTooltipHide CommandType = 0x0631

	// --- Toast (0x0640 ~ 0x064F) ---
	CmdToastShow CommandType = 0x0640
	CmdToastHide CommandType = 0x0641

	// --- 菜单 (0x0650 ~ 0x065F) ---
	CmdMenuShow          CommandType = 0x0650 // 统一右键菜单
	CmdMenuHide          CommandType = 0x0651
	CmdToolbarMenuHide   CommandType = 0x0652
	CmdCandidateMenuHide CommandType = 0x0653

	// --- 主题/配置 (0x0660 ~ 0x066F) ---
	CmdThemeApply   CommandType = 0x0660
	CmdConfigUpdate CommandType = 0x0661

	// --- 快捷键 (0x0670 ~ 0x067F) ---
	CmdHotkeysRegister   CommandType = 0x0670
	CmdHotkeysUnregister CommandType = 0x0671

	// --- 设置/其他 UI 杂项 (0x0680 ~ 0x068F) ---
	CmdSettingsOpen CommandType = 0x0680 // 打开设置窗口 (可选指定页面)
	CmdDPIChanged   CommandType = 0x0681 // DPI 变更通知 (Windows 专有; darwin 端忽略)
)

// Payload 是所有命令 payload 的标记接口。
// 私有方法 isPayload 确保只有本包内类型可实现, 避免外部错误传值。
type Payload interface {
	isPayload()
	// CommandType 返回该 payload 所对应的命令类型, 编码时用于校验 Command.Type 与 Payload 是否匹配。
	CommandType() CommandType
}

// Command 是下行命令信封。
//
// Session 用于"防止 stale 命令"——例如 input 已被清空后,
// 旧的 show 命令到达时通过比对 currentInputSession 丢弃。
// macOS 端 IMKit 也需要消费此字段, 用相同语义判定 stale。
type Command struct {
	Type    CommandType
	Session uint64
	Payload Payload
}

// NewCommand 构造一个命令。会断言 payload 的 CommandType() 与 typ 一致, 不一致直接 panic
// (编程错误, 应在开发期暴露)。
func NewCommand(typ CommandType, session uint64, payload Payload) Command {
	if payload != nil && payload.CommandType() != typ {
		panic("uicmd: command type mismatch: " + typ.String() + " vs payload " + payload.CommandType().String())
	}
	return Command{Type: typ, Session: session, Payload: payload}
}

// String 返回命令类型的可读名称, 便于日志。
func (t CommandType) String() string {
	if name, ok := commandNames[t]; ok {
		return name
	}
	return "uicmd.Unknown"
}

var commandNames = map[CommandType]string{
	CmdCandidatesShow:     "candidates.show",
	CmdCandidatesHide:     "candidates.hide",
	CmdCandidatesPosition: "candidates.position",
	CmdCandidatesMarkers:  "candidates.markers",
	CmdCandidatesConfig:   "candidates.config",
	CmdCandidatesPinState: "candidates.pin_state",
	CmdToolbarShow:        "toolbar.show",
	CmdToolbarHide:        "toolbar.hide",
	CmdToolbarUpdate:      "toolbar.update",
	CmdStatusShow:         "status.show",
	CmdStatusHide:         "status.hide",
	CmdStatusConfig:       "status.config",
	CmdModeShow:           "mode.show",
	CmdTooltipShow:        "tooltip.show",
	CmdTooltipHide:        "tooltip.hide",
	CmdToastShow:          "toast.show",
	CmdToastHide:          "toast.hide",
	CmdMenuShow:           "menu.show",
	CmdMenuHide:           "menu.hide",
	CmdToolbarMenuHide:    "menu.toolbar_hide",
	CmdCandidateMenuHide:  "menu.candidate_hide",
	CmdThemeApply:         "theme.apply",
	CmdConfigUpdate:       "config.update",
	CmdHotkeysRegister:    "hotkeys.register",
	CmdHotkeysUnregister:  "hotkeys.unregister",
	CmdSettingsOpen:       "settings.open",
	CmdDPIChanged:         "dpi.changed",
}
