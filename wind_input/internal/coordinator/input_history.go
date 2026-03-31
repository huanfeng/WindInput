package coordinator

// InputRecord 一条上屏记录
type InputRecord struct {
	Text     string // 上屏的文字（可能是单字或词）
	Code     string // 编码
	SchemaID string // 方案 ID
	ClientID uint32 // 客户端 ID
}

// InputHistory 输入历史记录器
// 按 clientID 隔离，追踪最近上屏的文字，用于加词推荐。
// 不持久化，仅内存保存。
type InputHistory struct {
	maxChars int                      // 每个 client 最多保留的字符数
	clients  map[uint32][]InputRecord // clientID -> records (newest last)
}

// NewInputHistory 创建 InputHistory 实例，maxChars<=0 时默认 20
func NewInputHistory(maxChars int) *InputHistory {
	if maxChars <= 0 {
		maxChars = 20
	}
	return &InputHistory{
		maxChars: maxChars,
		clients:  make(map[uint32][]InputRecord),
	}
}

// Record 记录一次上屏，空 text 忽略。记录后裁剪使总字符数 <= maxChars
func (h *InputHistory) Record(text, code, schemaID string, clientID uint32) {
	if text == "" {
		return
	}
	r := InputRecord{
		Text:     text,
		Code:     code,
		SchemaID: schemaID,
		ClientID: clientID,
	}
	h.clients[clientID] = append(h.clients[clientID], r)
	h.trimClient(clientID)
}

// trimClient 从最早记录开始移除，直到总 rune 数 <= maxChars
func (h *InputHistory) trimClient(clientID uint32) {
	records := h.clients[clientID]
	for h.charCountOf(records) > h.maxChars && len(records) > 0 {
		records = records[1:]
	}
	h.clients[clientID] = records
}

// charCountOf 计算一组记录的总 rune 数
func (h *InputHistory) charCountOf(records []InputRecord) int {
	total := 0
	for _, r := range records {
		total += len([]rune(r.Text))
	}
	return total
}

// GetRecentRecords 获取最近的记录，newest first
func (h *InputHistory) GetRecentRecords(limit int, clientID uint32) []InputRecord {
	records := h.clients[clientID]
	if len(records) == 0 {
		return nil
	}
	// reverse copy
	result := make([]InputRecord, 0, len(records))
	for i := len(records) - 1; i >= 0; i-- {
		result = append(result, records[i])
		if len(result) >= limit {
			break
		}
	}
	return result
}

// GetRecentChars 从最近输入提取 n 个字符，返回从早到晚排列
func (h *InputHistory) GetRecentChars(n int, clientID uint32) []rune {
	records := h.clients[clientID]
	if len(records) == 0 {
		return nil
	}

	// 从最新记录往回逐字符收集
	collected := make([]rune, 0, n)
	for i := len(records) - 1; i >= 0 && len(collected) < n; i-- {
		runes := []rune(records[i].Text)
		// 从该记录的末尾往前取
		for j := len(runes) - 1; j >= 0 && len(collected) < n; j-- {
			collected = append(collected, runes[j])
		}
	}

	// 反转为正序（从早到晚）
	for left, right := 0, len(collected)-1; left < right; left, right = left+1, right-1 {
		collected[left], collected[right] = collected[right], collected[left]
	}
	return collected
}

// ClearClient 清除指定客户端历史
func (h *InputHistory) ClearClient(clientID uint32) {
	delete(h.clients, clientID)
}

// CharCount 返回指定客户端总字符数
func (h *InputHistory) CharCount(clientID uint32) int {
	return h.charCountOf(h.clients[clientID])
}
