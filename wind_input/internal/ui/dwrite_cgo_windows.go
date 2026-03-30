//go:build windows

package ui

// CGO bridge for IDWriteTextRenderer::DrawGlyphRun callback.
//
// On Windows x64, non-variadic COM calls place float parameters exclusively
// in XMM registers. Go's syscall.NewCallback cannot reliably extract floats
// from XMM when parameters are declared as uintptr, and declaring them as
// float32 causes deadlocks in reentrant COM callback chains.
//
// This C trampoline solves the problem: the C compiler correctly receives
// floats from XMM registers, then passes them to the Go //export function
// via CGO's mature bridging mechanism. The C code is statically linked into
// the Go binary — no external DLL is produced.

/*
#include <stdint.h>

// Forward declaration of the Go callback (defined below via //export).
extern uintptr_t goDrawGlyphRunBridge(
    uintptr_t thisPtr,
    uintptr_t clientCtx,
    float baselineOriginX,
    float baselineOriginY,
    uintptr_t measuringMode,
    uintptr_t glyphRun,
    uintptr_t glyphRunDescription,
    uintptr_t clientDrawingEffect
);

// C trampoline — receives the COM callback with proper float params.
// On Windows x64 there is only one calling convention, so no __stdcall needed.
static uintptr_t cDrawGlyphRunTrampoline(
    void* thisPtr,
    void* clientCtx,
    float baselineOriginX,
    float baselineOriginY,
    uint32_t measuringMode,
    void* glyphRun,
    void* glyphRunDescription,
    void* clientDrawingEffect)
{
    return goDrawGlyphRunBridge(
        (uintptr_t)thisPtr,
        (uintptr_t)clientCtx,
        baselineOriginX,
        baselineOriginY,
        (uintptr_t)measuringMode,
        (uintptr_t)glyphRun,
        (uintptr_t)glyphRunDescription,
        (uintptr_t)clientDrawingEffect
    );
}

// Returns the C function pointer for use in the COM vtable.
static uintptr_t getDrawGlyphRunTrampoline() {
    return (uintptr_t)cDrawGlyphRunTrampoline;
}
*/
import "C"

import (
	"math"
	"syscall"
	"unsafe"
)

//export goDrawGlyphRunBridge
func goDrawGlyphRunBridge(
	thisPtr C.uintptr_t,
	clientCtx C.uintptr_t,
	baselineOriginX C.float,
	baselineOriginY C.float,
	measuringMode C.uintptr_t,
	glyphRun C.uintptr_t,
	glyphRunDescription C.uintptr_t,
	clientDrawingEffect C.uintptr_t,
) C.uintptr_t {
	// Convert C.uintptr_t → Go pointer via //export bridge.
	// The thisPtr is a valid goTextRenderer* passed through COM vtable.
	addr := uintptr(thisPtr)
	tr := (*goTextRenderer)(*((*unsafe.Pointer)(unsafe.Pointer(&addr))))
	if tr.bitmapTarget == nil {
		return 0 // S_OK
	}

	xBits := uintptr(math.Float32bits(float32(baselineOriginX)))
	yBits := uintptr(math.Float32bits(float32(baselineOriginY)))

	// Call IDWriteBitmapRenderTarget::DrawGlyphRun (vtable index 3).
	vtbl := *(*unsafe.Pointer)(tr.bitmapTarget)
	methodPtr := *(*uintptr)(unsafe.Add(vtbl, unsafe.Sizeof(uintptr(0))*uintptr(dwBmpVtDrawGlyphRun)))
	var blackBoxRect RECT
	syscall.SyscallN(methodPtr,
		uintptr(tr.bitmapTarget),
		xBits,
		yBits,
		uintptr(measuringMode),
		uintptr(glyphRun),
		uintptr(tr.renderParams),
		uintptr(tr.textColor),
		uintptr(unsafe.Pointer(&blackBoxRect)),
	)
	return 0 // S_OK
}

// dwCGODrawGlyphRunCallback returns the C trampoline function pointer
// for use in the IDWriteTextRenderer COM vtable.
func dwCGODrawGlyphRunCallback() uintptr {
	return uintptr(C.getDrawGlyphRunTrampoline())
}
