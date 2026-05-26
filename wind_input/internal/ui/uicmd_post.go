package ui

import (
	"image/color"

	"github.com/huanfeng/wind_input/internal/uicmd"
)

// uicmd_post.go 集中 Manager 投递 uicmd.Command 的辅助方法, 与各 setter 配合使用。
//
// 设计要点:
//   - postCmd / postCmdItem 是非阻塞投递, channel 满时丢弃 (与现有 setter 的 select-default 一致语义)。
//   - SnapshotXxx 方法从 Manager 当前完整状态构造一份"全量快照" payload, 供 setter 末尾调用,
//     也供未来 macOS forwarder 在 IMKit 连接时做初次状态同步。
//   - 这些 snapshot 命令在 Windows 端 processOneCommand 中走 no-op 分支
//     (state 已被 sync setter 应用), 只对未来 forwarder 有意义。

// postCmd 投递一个不含旁路字段的 uicmd.Command (常用于无旁路的简单命令)。
func (m *Manager) postCmd(cmd uicmd.Command) {
	m.postCmdItem(uicmdItem{Cmd: cmd})
}

// postCmdItem 非阻塞投递一个完整的 uicmdItem (含旁路字段)。
// channel 满时丢弃, 仅打 debug 日志 — 与历史 setter 的 select-default 行为一致。
func (m *Manager) postCmdItem(item uicmdItem) {
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Debug("UI command channel full, dropping snapshot", "type", item.Cmd.Type.String())
	}
}

// ============================================================================
// Snapshot builders
// ============================================================================

// snapshotCandidatesMarkers 构造 CmdCandidatesMarkers 的全量快照。
// 读取 Manager 当前的 isPinyinMode / isQuickInputMode / modeLabel / modeAccentColor。
func (m *Manager) snapshotCandidatesMarkers() uicmd.Command {
	m.mu.Lock()
	payload := uicmd.CandidatesMarkersPayload{
		IsPinyinMode:     m.isPinyinMode,
		IsQuickInputMode: m.isQuickInputMode,
		ModeLabel:        m.modeLabel,
		AccentColor:      toUIColorPtr(m.modeAccentColor),
	}
	m.mu.Unlock()
	return uicmd.NewCommand(uicmd.CmdCandidatesMarkers, 0, payload)
}

// snapshotCandidatesConfig 构造 CmdCandidatesConfig 全量快照。
// 注意: FontSize/FontFamily 当前未在 Manager 层追踪 (由 renderer 内部持有),
// 留空; PR-3 后续可补 renderer.GetFontSize/GetFontFamily 后填充。
func (m *Manager) snapshotCandidatesConfig() uicmd.Command {
	m.mu.Lock()
	payload := uicmd.CandidatesConfigPayload{
		HideCandidateWindow: m.hideCandidateWindow,
		HidePreedit:         m.hidePreedit,
		PreeditMode:         uicmd.PreeditMode(m.preeditMode),
		PagerDisplayMode:    uicmd.PagerDisplayMode(m.pagerDisplayMode),
		CmdbarPrefix:        m.cmdbarPrefix,
		MaxCandidateChars:   m.maxCandidateChars,
	}
	m.mu.Unlock()
	if m.renderer != nil {
		payload.Layout = uicmd.CandidateLayout(m.renderer.GetLayout())
	}
	return uicmd.NewCommand(uicmd.CmdCandidatesConfig, 0, payload)
}

// snapshotCandidatesPinState 构造 CmdCandidatesPinState 全量快照。
// 拷贝 appPinPositions map 防止后续修改影响快照。
func (m *Manager) snapshotCandidatesPinState() uicmd.Command {
	m.mu.Lock()
	enabled := m.appPinEnabled
	var positions map[string][2]int
	if len(m.appPinPositions) > 0 {
		positions = make(map[string][2]int, len(m.appPinPositions))
		for k, v := range m.appPinPositions {
			positions[k] = v
		}
	}
	m.mu.Unlock()
	return uicmd.NewCommand(uicmd.CmdCandidatesPinState, 0, uicmd.CandidatesPinStatePayload{
		Enabled:            enabled,
		PositionsByMonitor: positions,
	})
}

// toUIColorPtr 把 image/color.Color 转换为 *uicmd.Color。nil → nil。
func toUIColorPtr(c color.Color) *uicmd.Color {
	if c == nil {
		return nil
	}
	r, g, b, a := c.RGBA()
	return &uicmd.Color{
		R: uint8(r >> 8),
		G: uint8(g >> 8),
		B: uint8(b >> 8),
		A: uint8(a >> 8),
	}
}
