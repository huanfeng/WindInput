// Package ui provides native Windows UI for candidate window
package ui

import "github.com/huanfeng/wind_input/internal/candidate"

// Candidate 是 candidate.Candidate 的类型别名，消除重复定义
type Candidate = candidate.Candidate

// CandidateRect represents the bounding rectangle of a candidate item
type CandidateRect struct {
	Index int     // Candidate index (0-based within current page)
	X     float64 // Left position
	Y     float64 // Top position
	W     float64 // Width
	H     float64 // Height
}

// RenderResult contains the rendered image and hit test information
type RenderResult struct {
	Rects        []CandidateRect // Bounding rectangles for each candidate
	PageUpRect   *CandidateRect  // Bounding rectangle for page up button
	PageDownRect *CandidateRect  // Bounding rectangle for page down button
}

// CandidateCallback defines callbacks for candidate window interactions
type CandidateCallback struct {
	OnSelect          func(index int)                     // Called when user clicks a candidate (index is 0-based within page)
	OnHoverChange     func(index, tooltipX, tooltipY int) // Called when hover state changes (-1 for no hover, with tooltip position below candidate)
	OnPageUp          func()                              // Called when user clicks the page up button
	OnPageDown        func()                              // Called when user clicks the page down button
	OnMoveUp          func(index int)                     // Called when user selects "Move Up" from context menu
	OnMoveDown        func(index int)                     // Called when user selects "Move Down" from context menu
	OnMoveTop         func(index int)                     // Called when user selects "Move to Top" from context menu
	OnDelete          func(index int)                     // Called when user selects "Delete" from context menu
	OnResetDefault    func(index int)                     // Called when user selects "Reset to Default" from context menu
	OnOpenSettings    func()                              // Called when user selects "Settings" from context menu
	OnAbout           func()                              // Called when user selects "About" from context menu
	OnShowUnifiedMenu func(screenX, screenY int)          // Called when user right-clicks blank area to show unified menu
}
