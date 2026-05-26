package ui

import "github.com/huanfeng/wind_input/internal/uicmd"

// uicmdItem 是 Manager 内部 channel 元素类型。
//
// 它包装了平台无关的 uicmd.Command 与若干"不可上 wire"的旁路字段:
//   - Candidates: doShowCandidates 需要的完整 candidate.Candidate (含 Action/Meta 等业务字段),
//     macOS forwarder 会改用 Cmd.Payload 中的 uicmd.Candidate (仅渲染字段) 序列化下发。
//   - Callback:   菜单点选回调函数指针; 跨进程通道用 SessionID 路由替代 (见 PR-1 AGENTS.md)。
//
// Windows 端 processOneCommand 消费 Cmd.Type 做 switch 分发, 用旁路字段给 do* 方法。
type uicmdItem struct {
	Cmd        uicmd.Command
	Candidates []Candidate       // 旁路: CmdCandidatesShow 时填充完整 candidate 切片
	Callback   func(int)         // 旁路: CmdMenuShow 时填充菜单回调
	MenuState  *UnifiedMenuState // 旁路: CmdMenuShow 时填充菜单状态 (Win 端走 BuildUnifiedMenuItems)
	// macOS forwarder 接入时, Cmd.Payload (uicmd.MenuShowPayload.Items) 由 menu_build 模块从 MenuState 转换填充
}

// toUICandidates 把 candidate 切片转换为 uicmd.Candidate 的 wire 镜像。
// 仅保留 IMKit 渲染需要的字段; Action/Meta/Pinyin 等业务字段不参与 wire。
func toUICandidates(in []Candidate) []uicmd.Candidate {
	if len(in) == 0 {
		return nil
	}
	out := make([]uicmd.Candidate, len(in))
	for i, c := range in {
		out[i] = uicmd.Candidate{
			Text:          c.Text,
			Code:          c.Code,
			Comment:       c.Comment,
			Index:         c.Index,
			IndexLabel:    c.IndexLabel,
			Source:        string(c.Source),
			IsCommon:      c.IsCommon,
			IsPhrase:      c.IsPhrase,
			IsCommand:     c.IsCommand,
			IsGroup:       c.IsGroup,
			IsGroupMember: c.IsGroupMember,
			HasShadow:     c.HasShadow,
		}
	}
	return out
}
