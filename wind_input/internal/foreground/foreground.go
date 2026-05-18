// Package foreground reads the currently active foreground window's
// process basename and title via Win32 APIs. Non-Windows builds return
// empty strings so callers (cmdbar app() / title()) degrade gracefully.
//
// Design context: docs/design/command-bar-design.md §4.3.
package foreground
