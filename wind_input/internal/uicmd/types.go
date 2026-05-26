package uicmd

// Candidate 是 internal/candidate.Candidate 的"渲染镜像", 只含渲染端需要的字段。
//
// 不直接复用 candidate.Candidate 的原因:
//  1. candidate.Candidate 含 Actions (cmdbar.ResolvedAction) 等业务字段, 序列化代价大且无 UI 用途。
//  2. 跨进程边界 (macOS IMKit) 需要稳定的 wire 形态, 不应受业务字段变动影响。
//
// 与 candidate.Candidate 的对齐由 ui 层转换函数维护 (见 internal/ui 中 ToUICandidate)。
type Candidate struct {
	Text          string // 候选文字
	Code          string // 编码 (右键菜单显示用)
	Comment       string // 注释 (反查编码、PUA 提示等)
	Index         int    // 显示序号 (1-9/0)
	IndexLabel    string // 自定义序号标签, 非空时覆盖 Index 数字
	Source        string // 候选来源: ""/"codetable"/"pinyin"/"english"/"phrase"
	IsCommon      bool   // 通用规范汉字
	IsPhrase      bool   // 短语候选
	IsCommand     bool   // 命令候选 (uuid/date/time 等)
	IsGroup       bool   // 组候选 (展开二级)
	IsGroupMember bool   // 组成员 (右键禁用大部分操作)
	HasShadow     bool   // 存在 Shadow 修改 (右键"恢复默认"用)
}

// Color 是 RGBA 颜色的 wire 形态。
// 与 image/color.RGBA 等价但跨语言/跨进程序列化更稳定。
type Color struct {
	R, G, B, A uint8
}

// CandidateLayout 候选框排版方向. 镜像 pkg/config.CandidateLayout 取值。
type CandidateLayout string

const (
	CandidateLayoutHorizontal CandidateLayout = "horizontal"
	CandidateLayoutVertical   CandidateLayout = "vertical"
)

// PreeditMode 编码区显示模式. 镜像 pkg/config.PreeditMode。
type PreeditMode string

// PagerDisplayMode 翻页器显示模式. 镜像 pkg/config.PagerDisplayMode。
type PagerDisplayMode string

// ThemeStyle 主题风格. 镜像 pkg/config.ThemeStyle。
type ThemeStyle string

const (
	ThemeStyleSystem ThemeStyle = "system"
	ThemeStyleLight  ThemeStyle = "light"
	ThemeStyleDark   ThemeStyle = "dark"
)

// ToastLevel Toast 级别. 镜像 internal/ui.ToastLevel。
type ToastLevel string

const (
	ToastInfo    ToastLevel = "info"
	ToastSuccess ToastLevel = "success"
	ToastWarn    ToastLevel = "warn"
	ToastError   ToastLevel = "error"
)

// ToastPosition Toast 位置. 镜像 internal/ui.ToastPosition。
type ToastPosition string

const (
	ToastBottomRight ToastPosition = "bottom_right"
	ToastCenter      ToastPosition = "center"
)

// StatusDisplayMode 状态指示器显示模式. 镜像 internal/ui.StatusDisplayMode。
type StatusDisplayMode string

// StatusPositionMode 状态指示器位置模式. 镜像 internal/ui.StatusPositionMode。
type StatusPositionMode string

// HotkeyEntry 全局快捷键项 (镜像 internal/ui.GlobalHotkeyEntry 的 wire 形态)。
type HotkeyEntry struct {
	ID      int32  // 注册 ID (Win 上用作 RegisterHotKey 的 id)
	Mods    uint32 // 修饰键位掩码 (语义与底层平台一致, 由调用方保证)
	KeyCode uint32 // 主键码
	Command string // 触发后回传的命令名
}
