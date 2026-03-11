<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# include/ - Header Files

## Purpose

Public and internal header files defining interfaces, structures, and protocols for the TSF DLL. All headers use `#pragma once` guard and compile with C++17/MSVC.

## Key Files

| File | Description |
|------|-------------|
| `Globals.h` | Logging macros, COM utilities, global state (HINSTANCE, DLL ref count, GUIDs, named pipe names) |
| `BinaryProtocol.h` | Binary protocol definitions (5KB+ of structures, enums, command IDs); shared with Go service |
| `TextService.h` | CTextService class (main TSF text input processor, composition management, IPC coordination) |
| `KeyEventSink.h` | CKeyEventSink class (keyboard event capture, modifier state machine, barrier mechanism) |
| `IPCClient.h` | CIPCClient class (named pipe client, binary protocol, circuit breaker, async reader thread) |
| `ClassFactory.h` | CClassFactory class (COM class factory for CTextService instantiation) |
| `HotkeyManager.h` | CHotkeyManager class (hotkey whitelist, key classification, modifier tracking) |
| `LangBarItemButton.h` | CLangBarItemButton class (language bar UI, menu, thread-safe async updates) |
| `CaretEditSession.h` | CCaretEditSession class (TSF edit session for caret position retrieval) |
| `DisplayAttributeInfo.h` | CDisplayAttributeInfoInput, CDisplayAttributeProvider (composition text styling) |
| `Register.h` | RegisterServer, UnregisterServer functions (Windows registry integration) |

## Architecture Overview

**Dependency Graph:**
```
TextService.h
├── KeyEventSink.h
├── IPCClient.h
├── LangBarItemButton.h
├── CaretEditSession.h
├── HotkeyManager.h
├── Globals.h
└── BinaryProtocol.h

KeyEventSink.h
├── IPCClient.h
├── BinaryProtocol.h
└── Globals.h

IPCClient.h
├── BinaryProtocol.h
└── Globals.h

LangBarItemButton.h
└── Globals.h

HotkeyManager.h
├── BinaryProtocol.h
└── Globals.h
```

## Key Structures

### BinaryProtocol.h

**Protocol Header (8 bytes):**
```cpp
struct IpcHeader {
    uint16_t version;   // Protocol version (PROTOCOL_VERSION = 0x1001)
    uint16_t command;   // Command type (CMD_KEY_EVENT, CMD_COMMIT_REQUEST, etc.)
    uint32_t length;    // Payload length in bytes
};
```

**Key Event Payload (16 bytes):**
```cpp
struct KeyPayload {
    uint32_t keyCode;     // Virtual key code
    uint32_t scanCode;    // Scan code
    uint32_t modifiers;   // Modifier flags (KEYMOD_SHIFT, KEYMOD_CTRL, etc.)
    uint8_t  eventType;   // 0=KeyDown, 1=KeyUp
    uint8_t  toggles;     // Toggle state (TOGGLE_CAPSLOCK, etc.)
    uint16_t eventSeq;    // Monotonic event sequence
};
```

**Status Flags:**
```cpp
STATUS_CHINESE_MODE    = 0x0001  // Chinese/English mode
STATUS_FULL_WIDTH      = 0x0002  // Full-width/half-width
STATUS_CHINESE_PUNCT   = 0x0004  // Chinese/English punctuation
STATUS_TOOLBAR_VISIBLE = 0x0008  // Toolbar visibility
STATUS_CAPS_LOCK       = 0x0020  // CapsLock state
```

### Global State

**Globals.h declares:**
```cpp
extern HINSTANCE g_hInstance;           // DLL instance handle
extern LONG g_lServerLock;              // Server lock count (COM)
extern const CLSID c_clsidTextService;  // CLSID for COM factory
extern const GUID c_guidProfile;        // Profile GUID
extern const GUID c_guidLangBarItemButton; // Language bar item GUID
```

**Named Pipes:**
```cpp
PIPE_NAME      = L"\\\\.\\pipe\\wind_input"       // Main IPC pipe
PUSH_PIPE_NAME = L"\\\\.\\pipe\\wind_input_push"  // Async push pipe
```

## For AI Agents

### Working In This Directory

When adding or modifying headers:

1. **Keep headers minimal** - Move implementation to src/ files
2. **Document all public APIs** with inline comments explaining parameters and return values
3. **Maintain binary protocol compatibility** - Do not change struct sizes without coordination with Go service
4. **Use pack(1)** for network-protocol structs (all IpcHeader, *Payload, *Header structs)
5. **Define enums as enum class** (type-safe) unless binary compatibility requires plain enums
6. **Avoid iostream** - Use WIND_LOG_* macros instead

### Logging in Headers

Prefer declaration-only in headers; implementation in .cpp files. Use forward declares to avoid circular dependencies:

```cpp
// Good: forward declare and use in method signature
class CTextService;
class CIPCClient;

// Bad: include the full header (circular dependency risk)
#include "TextService.h"
```

### Protocol Evolution

If modifying BinaryProtocol.h:

1. **Increment PROTOCOL_VERSION** if making breaking changes
2. **Add new command IDs** before existing ones (e.g., 0x0F01 for batch)
3. **Extend payloads with new fields** only at the end (backward compatible)
4. **Document format changes** with comments showing byte offsets
5. **Update Go service** to recognize both old and new versions during transition

### Common Patterns

**Safe Interface Release:**
```cpp
// Prefer SafeRelease() template from Globals.h
ITfThreadMgr* pTm = ...;
SafeRelease(pTm);  // Calls Release() and sets to nullptr
```

**Safe Memory Deletion:**
```cpp
// Prefer SafeDelete() template from Globals.h
CKeyEventSink* pSink = new CKeyEventSink();
// ... use it ...
SafeDelete(pSink);  // Calls delete and sets to nullptr
```

**Logging with Levels:**
```cpp
WIND_LOG_ERROR_FMT(L"Failed to connect: %d", errorCode);
WIND_LOG_INFO(L"Key event received");
WIND_LOG_DEBUG_FMT(L"Modifiers: 0x%04X", modifiers);
```

## Dependencies

### Internal
- `Globals.h` included by all others (logging, COM utils)
- `BinaryProtocol.h` included by protocol-heavy files (IPCClient, KeyEventSink)

### External
- `<windows.h>` - Windows SDK basics
- `<msctf.h>` - TSF interfaces (ITfTextInputProcessor, ITfContext, etc.)
- `<ctfutb.h>` - Language bar interfaces (ITfLangBarItemButton)
- `<string>` - std::wstring, std::string
- `<vector>` - std::vector for dynamic arrays
- `<unordered_set>` - O(1) hotkey lookup

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
