// special_mode_decide.go — 特殊模式自动上屏判定（纯函数，无副作用，便于单测）。
package coordinator

import "github.com/huanfeng/wind_input/pkg/config"

// decideSpecialAutoCommit 判定当前是否应自动上屏。
//
//	strategy:    实例 auto_commit
//	fixedLength: 实例 fixed_length（仅 fixed_length 档用）
//	bufLen:      当前编码长度
//	candCount:   当前直接候选数（展开前的精确码候选条数）
//	hasLonger:   码表中是否存在以当前编码为前缀的更长编码
func decideSpecialAutoCommit(strategy string, fixedLength, bufLen, candCount int, hasLonger bool) bool {
	switch strategy {
	case config.SpecialAutoCommitPrefixFree:
		return candCount == 1 && !hasLonger
	case config.SpecialAutoCommitFixedLength:
		return fixedLength > 0 && bufLen >= fixedLength && candCount == 1
	default: // manual 及未知
		return false
	}
}
