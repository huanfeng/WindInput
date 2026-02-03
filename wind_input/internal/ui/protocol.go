// Package ui provides native Windows UI for candidate window
package ui

// Candidate represents a candidate word
type Candidate struct {
	Text    string
	Index   int
	Comment string
	Weight  int
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
	OnSelect      func(index int) // Called when user clicks a candidate (index is 0-based within page)
	OnHoverChange func(index int) // Called when hover state changes (-1 for no hover)
	OnContextMenu func(index int) // Called when user right-clicks a candidate
}
