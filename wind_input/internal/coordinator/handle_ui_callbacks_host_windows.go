//go:build windows

package coordinator

// 本文件集中 Windows host render（宿主进程代理渲染）模式下的鼠标交互导出包装。
// host render 时候选框是宿主进程内的 Band 分层窗口，鼠标消息在 DLL 的 _WndProc
// 命中测试后经 CmdCandidateSelect / CmdCandidateHover 异步上报，bridge server 类型
// 断言 candidateSelector / hostCandidateHoverHandler 调到此处。这些方法复用与本地
// 候选窗完全相同的 handleCandidateSelect / handleCandidateHoverChange / handlePageUp/
// Down 逻辑——host render 只是把「窗口在哪个进程」换了，选词/翻页/悬停语义不变。

// HandleCandidateSelect 处理 host 窗口的鼠标左键点选（DLL 经 CmdCandidateSelect 异步上报）。
// index 为页内 0-based 候选索引；负值是翻页按钮（-1=上页 -2=下页），与内嵌命中矩形
// 的约定一致。各分支均在独立 goroutine 中执行，避免阻塞 bridge dispatch；选词结果经
// push 管道交付活动客户端。
func (c *Coordinator) HandleCandidateSelect(index int) {
	c.logger.Info("Host render mouse select", "index", index)
	switch index {
	case -1:
		go func() {
			c.mu.Lock()
			defer c.mu.Unlock()
			c.handlePageUp()
		}()
	case -2:
		go func() {
			c.mu.Lock()
			defer c.mu.Unlock()
			c.handlePageDown()
		}()
	default:
		go c.handleCandidateSelect(index)
	}
}

// HandleCandidateScroll 处理 host 窗口的鼠标滚轮（DLL 经 CmdCandidateScroll 异步上报）。
// delta 为原始滚轮增量（WHEEL_DELTA=120 的整数倍，正=上滚 负=下滚）。
//
// 设计：标准版（本地候选窗）**没有**滚轮翻页，故此处**默认不做任何动作**，仅作为
// 统一接入点把事件交给 Go——后续若要支持滚轮（翻页/滚动选词等）应在此集中实现，
// 并受配置开关控制，避免在 DLL 侧硬编码默认行为。
func (c *Coordinator) HandleCandidateScroll(delta int) {
	c.logger.Debug("Host render mouse scroll (no-op by default)", "delta", delta)
	// TODO: 统一滚轮逻辑（配置开关 + 翻页/选词策略）后在此实现。
}

// HandleCandidateHoverAt 处理 host 窗口的鼠标悬停（DLL 经 CmdCandidateHover 异步上报）。
// 复用 handleCandidateHoverChange：内部 RefreshCandidates 会触发带高亮的重渲染并经
// SHM 重推宿主帧（高亮在所有 host 进程均生效），同时按 DLL 算好的屏幕锚点异步查询并
// 显示 tooltip。index<0 表示离开候选区（隐藏 tooltip）。
func (c *Coordinator) HandleCandidateHoverAt(index, tooltipX, tooltipBelowY, tooltipAboveY int) {
	// 解码 DLL 的悬停 index 编码（区别于点击/矩形的 -1/-2 翻页约定）：
	//   >=0 候选 | -1 无 | -2 上页按钮 | -3 下页按钮
	// 翻页按钮悬停时候选索引置 -1（不查 tooltip），并把 pageBtn 传给渲染器高亮按钮。
	pageBtn := ""
	candIdx := index
	switch index {
	case -2:
		pageBtn, candIdx = "up", -1
	case -3:
		pageBtn, candIdx = "down", -1
	}
	// 先同步 host 悬停目标到 UI manager，使紧接着的 RefreshCandidates 重渲染能高亮
	// 对应候选/翻页按钮（host 模式本地 window 的 hover 态恒为空，无法承载）。
	if c.uiManager != nil {
		c.uiManager.SetHostHover(candIdx, pageBtn)
	}
	go c.handleCandidateHoverChange(candIdx, tooltipX, tooltipBelowY, tooltipAboveY)
}
