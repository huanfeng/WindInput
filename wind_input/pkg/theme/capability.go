package theme

import "encoding/json"

// capability.go — 主题能力声明 schema（Capability Manifest）。
//
// 权威矩阵：每个 view 对每个能力维度的支持状态（三态）。前后端共享单一数据源——
// 编辑器据此显示/灰显/隐藏控件；引擎据此明确渲染/忽略；文档从它生成。
// 设计见 docs/design/theme-capability-schema.md。
//
// 维护纪律（类 AGENTS.md）：改渲染消费时同步对应格子；把格子从 reserved/unsupported
// 转 supported 必须同时落地真实渲染（不得空转声明）。TestCapabilities_WellFormed 校验
// 状态/键/view 名合法（防 typo）；TestCapabilitiesJSON 守护导出 JSON 不漂移。

// CapabilityStatus 能力三态。
type CapabilityStatus string

const (
	CapSupported   CapabilityStatus = "supported"   // 已渲染消费（真能力）
	CapReserved    CapabilityStatus = "reserved"    // schema 有、渲染未实现（假字段，如 gradient/blur）
	CapUnsupported CapabilityStatus = "unsupported" // 该 view 概念上不支持（如 status 无交互状态）
)

// 能力维度键（用户可感知的能力单元粒度；白名单见 capabilityKeys）。
const (
	CapPadding            = "padding"
	CapMargin             = "margin"
	CapBorder             = "border"
	CapBackgroundColor    = "background_color"
	CapTextColor          = "text_color"
	CapBackgroundImage    = "background_image"
	CapBackgroundGradient = "background_gradient"
	CapBackgroundShape    = "background_shape"
	CapFont               = "font"
	CapStateSelected      = "state_selected"
	CapStateHover         = "state_hover"
	CapStateDisabled      = "state_disabled"
	CapLayers             = "layers"
	CapShadowOffset       = "shadow_offset"
	CapShadowBlurSpread   = "shadow_blur_spread"
	CapLineSpacing        = "line_spacing"
	CapColGap             = "col_gap"
	CapTitleGap           = "title_gap"
	CapItemSpacing        = "item_spacing"
	CapBandGap            = "band_gap"
	CapRowGap             = "row_gap"
	CapIndexLabels        = "index_labels"
	CapAccentBar          = "accent_bar"
	CapFooterArrowImage   = "footer_arrow_image"
	CapPager              = "pager"
	CapModeStates         = "mode_states"
)

// capabilityKeys 能力键白名单。
var capabilityKeys = map[string]bool{
	CapPadding: true, CapMargin: true, CapBorder: true,
	CapBackgroundColor: true, CapTextColor: true,
	CapBackgroundImage: true, CapBackgroundGradient: true, CapBackgroundShape: true,
	CapFont:          true,
	CapStateSelected: true, CapStateHover: true, CapStateDisabled: true,
	CapLayers: true, CapShadowOffset: true, CapShadowBlurSpread: true,
	CapLineSpacing: true, CapColGap: true, CapTitleGap: true,
	CapItemSpacing: true, CapBandGap: true, CapRowGap: true,
	CapIndexLabels: true, CapAccentBar: true, CapFooterArrowImage: true,
	CapPager: true, CapModeStates: true,
}

// viewSubjects view 主体白名单（候选窗 + 其它窗口）。
var viewSubjects = map[string]bool{
	"window": true, "preedit_bar": true, "candidate_list": true, "item": true,
	"index": true, "text": true, "comment": true, "accent_bar": true,
	"footer_bar": true, "mode_label": true,
	"status": true, "tooltip": true, "toast": true,
	"menu.root": true, "menu.item": true, "menu.separator": true, "toolbar": true,
}

// ViewCapability 一个 view 的能力声明。caps 中未列出的键 = 隐式 unsupported（该能力对该 view 不适用）。
type ViewCapability struct {
	View string                      `json:"view"`
	Caps map[string]CapabilityStatus `json:"caps"`
}

// capabilityManifest 导出 JSON 的根结构。
type capabilityManifest struct {
	Version int              `json:"version"`
	Views   []ViewCapability `json:"views"`
}

// capabilityVersion 能力声明版本（维度键/语义变更时升版）。
const capabilityVersion = 1

// ThemeCapabilities 权威能力矩阵（顺序即导出 JSON 的 view 顺序）。
//
// 仅列出对该 view 有意义的能力键：supported/reserved，以及"值得显式传达故意不做"的
// unsupported（如候选项 state_disabled 无业务触发源）；纯粹不适用的能力隐式省略。
var ThemeCapabilities = []ViewCapability{
	// ---- 候选窗 ----
	{"window", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapBackgroundImage: CapSupported, CapLayers: CapSupported,
		CapShadowOffset: CapSupported, CapShadowBlurSpread: CapReserved,
		CapBackgroundGradient: CapSupported,
	}},
	{"preedit_bar", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapBackgroundImage: CapSupported, CapLayers: CapSupported,
		CapTextColor: CapSupported, CapFont: CapSupported,
		CapBackgroundGradient: CapSupported,
	}},
	{"candidate_list", map[string]CapabilityStatus{
		CapItemSpacing: CapSupported, CapBandGap: CapSupported, CapRowGap: CapSupported,
		CapBackgroundColor: CapReserved, // ViewNode 可配，但列表 View 当前不绘制底色
	}},
	{"item", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapBackgroundImage: CapSupported, CapLayers: CapSupported,
		CapStateSelected: CapSupported, CapStateHover: CapSupported,
		CapStateDisabled:      CapUnsupported, // 候选项无禁用业务语义（Candidate 无 disabled 字段）
		CapBackgroundGradient: CapSupported,
	}},
	{"index", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapTextColor: CapSupported, CapFont: CapSupported,
		CapBackgroundShape: CapSupported, CapIndexLabels: CapSupported,
		CapStateSelected: CapSupported, CapStateHover: CapSupported,
		CapStateDisabled:      CapUnsupported,
		CapBackgroundGradient: CapSupported,
	}},
	{"text", map[string]CapabilityStatus{
		CapMargin: CapSupported, CapTextColor: CapSupported, CapFont: CapSupported,
		CapStateSelected: CapSupported, CapStateHover: CapSupported, CapStateDisabled: CapUnsupported,
	}},
	{"comment", map[string]CapabilityStatus{
		CapMargin: CapSupported, CapTextColor: CapSupported, CapFont: CapSupported,
		CapStateSelected: CapSupported, CapStateHover: CapSupported, CapStateDisabled: CapUnsupported,
	}},
	{"accent_bar", map[string]CapabilityStatus{
		CapAccentBar: CapSupported, CapBackgroundColor: CapSupported,
	}},
	{"footer_bar", map[string]CapabilityStatus{
		CapFont: CapSupported, CapTextColor: CapSupported,
		CapFooterArrowImage: CapSupported, CapPager: CapSupported,
		CapPadding: CapSupported, // 翻页箭头左右 padding（复用 footer_bar.padding）
	}},
	{"mode_label", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapMargin: CapSupported,
		CapFont: CapSupported, CapTextColor: CapSupported,
	}},
	// ---- 其它窗口 ----
	{"status", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapTextColor: CapSupported, CapFont: CapSupported,
		CapBackgroundImage: CapSupported, CapLayers: CapSupported,
		CapStateHover:         CapUnsupported, // 瞬时提示窗，无交互状态
		CapBackgroundGradient: CapSupported,
	}},
	{"tooltip", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapTextColor: CapSupported, CapFont: CapSupported,
		CapLineSpacing: CapSupported, CapColGap: CapSupported,
		CapBackgroundImage: CapSupported, CapLayers: CapSupported,
		CapStateHover:         CapUnsupported,
		CapBackgroundGradient: CapSupported,
	}},
	{"toast", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapTextColor: CapSupported, CapFont: CapSupported,
		CapLineSpacing: CapSupported, CapTitleGap: CapSupported,
		CapBackgroundImage: CapSupported, CapLayers: CapSupported,
		CapStateHover:         CapUnsupported,
		CapBackgroundGradient: CapSupported,
	}},
	{"menu.root", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapBackgroundImage: CapSupported, CapLayers: CapSupported,
		CapBackgroundGradient: CapSupported,
	}},
	{"menu.item", map[string]CapabilityStatus{
		CapPadding: CapSupported, CapBorder: CapSupported,
		CapBackgroundColor: CapSupported, CapTextColor: CapSupported, CapFont: CapSupported,
		CapStateHover: CapSupported, CapStateDisabled: CapSupported,
	}},
	{"menu.separator", map[string]CapabilityStatus{
		CapBackgroundColor: CapSupported, // 作分隔线色
	}},
	{"toolbar", map[string]CapabilityStatus{
		CapBackgroundColor: CapSupported, CapBorder: CapSupported, CapFont: CapSupported,
		CapPadding: CapSupported, CapModeStates: CapSupported,
		CapStateHover:    CapUnsupported, // 切片4 完整 View 化延后：仅 mode 中/英切换
		CapStateDisabled: CapUnsupported,
	}},
}

// MarshalCapabilities 把权威矩阵序列化为稳定 JSON（map 键按字母序，确定性）。
func MarshalCapabilities() ([]byte, error) {
	return json.MarshalIndent(capabilityManifest{
		Version: capabilityVersion,
		Views:   ThemeCapabilities,
	}, "", "  ")
}
