package dict

import "math"

// NormalizedWeightMax 归一化后的权重上限
const NormalizedWeightMax = 10000

// WeightNormalizer 权重归一化器
// 将不同词库的原始权重映射到统一的 [0, NormalizedWeightMax] 区间
type WeightNormalizer struct {
	median int     // 原始权重中位数
	max    int     // 原始权重最大值
	min    int     // 原始权重最小值
	target int     // 中位映射目标值（默认 1000）
	useLog bool    // 是否使用对数映射
	logMax float64 // 预计算的 ln(max+1)
}

// NewWeightNormalizer 创建权重归一化器
//
//	mode: "linear" 分段线性映射 / "log" 对数映射
//	median: 原始权重中位数
//	max: 原始权重最大值
//	min: 原始权重最小值
//	target: 中位映射目标值（0 则使用默认值 1000）
func NewWeightNormalizer(mode string, median, max, min, target int) *WeightNormalizer {
	if target <= 0 {
		target = 1000
	}
	if min < 0 {
		min = 0
	}
	wn := &WeightNormalizer{
		median: median,
		max:    max,
		min:    min,
		target: target,
		useLog: mode == "log",
	}
	if wn.useLog && max > 0 {
		wn.logMax = math.Log(float64(max) + 1)
	}
	return wn
}

// Normalize 将原始权重映射到 [0, NormalizedWeightMax] 区间
func (wn *WeightNormalizer) Normalize(origWeight int) int {
	if wn.useLog {
		return wn.normalizeLog(origWeight)
	}
	return wn.normalizeLinear(origWeight)
}

// normalizeLog 对数映射
// W_norm = ln(W_orig + 1) / ln(W_max + 1) × NormalizedWeightMax
func (wn *WeightNormalizer) normalizeLog(origWeight int) int {
	if origWeight <= 0 {
		return 0
	}
	if wn.logMax <= 0 {
		return 0
	}
	norm := math.Log(float64(origWeight)+1) / wn.logMax * float64(NormalizedWeightMax)
	result := int(math.Round(norm))
	if result < 0 {
		return 0
	}
	if result > NormalizedWeightMax {
		return NormalizedWeightMax
	}
	return result
}

// normalizeLinear 分段线性映射
//
//	W_orig <= median: 映射到 [minTarget, target]
//	W_orig >  median: 映射到 [target, maxTarget]
//
// minTarget = target * min / median（如果 median > 0）
// maxTarget = 5000（系统高频词上限）
func (wn *WeightNormalizer) normalizeLinear(origWeight int) int {
	if origWeight <= 0 {
		return 0
	}

	median := wn.median
	max := wn.max
	min := wn.min
	target := wn.target

	if median <= 0 {
		median = 1
	}

	// 计算 min 端映射目标
	minTarget := 0
	if median > 0 && min > 0 {
		minTarget = target * min / median
	}
	// 系统高频词映射上限
	maxTarget := NormalizedWeightMax / 2 // 5000

	if origWeight <= min {
		return minTarget
	}
	if origWeight >= max {
		return maxTarget
	}

	var result float64
	if origWeight <= median {
		// 下段：[min, median] → [minTarget, target]
		if median == min {
			result = float64(target)
		} else {
			result = float64(minTarget) + float64(origWeight-min)/float64(median-min)*float64(target-minTarget)
		}
	} else {
		// 上段：[median, max] → [target, maxTarget]
		if max == median {
			result = float64(target)
		} else {
			result = float64(target) + float64(origWeight-median)/float64(max-median)*float64(maxTarget-target)
		}
	}

	r := int(math.Round(result))
	if r < 0 {
		return 0
	}
	if r > NormalizedWeightMax {
		return NormalizedWeightMax
	}
	return r
}
