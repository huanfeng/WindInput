// Package ui provides native Windows UI for candidate window
package ui

// Candidate represents a candidate word
type Candidate struct {
	Text           string
	Index          int
	Comment        string
	Weight         int
	IsCommand      bool
	ConsumedLength int // 该候选消耗的输入长度（拼音部分上屏用）
}

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
	Rects []CandidateRect // Bounding rectangles for each candidate
}

// CandidateCallback defines callbacks for candidate window interactions
type CandidateCallback struct {
	OnSelect       func(index int)                     // Called when user clicks a candidate (index is 0-based within page)
	OnHoverChange  func(index, tooltipX, tooltipY int) // Called when hover state changes (-1 for no hover, with tooltip position below candidate)
	OnMoveUp       func(index int)                     // Called when user selects "Move Up" from context menu
	OnMoveDown     func(index int)                     // Called when user selects "Move Down" from context menu
	OnMoveTop      func(index int)                     // Called when user selects "Move to Top" from context menu
	OnDelete       func(index int)                     // Called when user selects "Delete" from context menu
	OnOpenSettings func()                              // Called when user selects "Settings" from context menu
}
