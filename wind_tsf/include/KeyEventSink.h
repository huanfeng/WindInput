#pragma once

#include "Globals.h"
#include "IPCClient.h"
#include <string>
#include <cstdint>
#include <map>
#include <set>
#include <vector>
#include <utility>

class CTextService;

// Lightweight pair engine for English auto-pair (C++ side, for English mode)
class PairEngine {
public:
    struct Entry { wchar_t left; wchar_t right; };

    void SetEnabled(bool enabled) { _enabled = enabled; }
    bool IsEnabled() const { return _enabled; }

    void SetPairs(const std::vector<std::pair<wchar_t, wchar_t>>& pairs) {
        _pairMap.clear();
        _rightSet.clear();
        for (auto& p : pairs) {
            _pairMap[p.first] = p.second;
            _rightSet.insert(p.second);
        }
        _stack.clear();
    }

    bool IsLeft(wchar_t ch) const { return _pairMap.count(ch) > 0; }
    bool IsRight(wchar_t ch) const { return _rightSet.count(ch) > 0; }
    wchar_t GetRight(wchar_t left) const {
        auto it = _pairMap.find(left);
        return it != _pairMap.end() ? it->second : 0;
    }

    void Push(wchar_t left, wchar_t right) { _stack.push_back({left, right}); }
    bool Peek(Entry& entry) const {
        if (_stack.empty()) return false;
        entry = _stack.back();
        return true;
    }
    bool Pop(Entry& entry) {
        if (_stack.empty()) return false;
        entry = _stack.back();
        _stack.pop_back();
        return true;
    }
    void Clear() { _stack.clear(); }
    bool IsEmpty() const { return _stack.empty(); }

private:
    std::map<wchar_t, wchar_t> _pairMap;
    std::set<wchar_t> _rightSet;
    std::vector<Entry> _stack;
    bool _enabled = false;
};

class CKeyEventSink : public ITfKeyEventSink
{
public:
    CKeyEventSink(CTextService* pTextService);
    ~CKeyEventSink();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfKeyEventSink
    STDMETHODIMP OnSetFocus(BOOL fForeground);
    STDMETHODIMP OnTestKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnTestKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnPreservedKey(ITfContext* pContext, REFGUID rguid, BOOL* pfEaten);

    // Initialize/Uninitialize
    BOOL Initialize();
    void Uninitialize();

    // Reset composing state (called when focus is lost or input field changes)
    // 注意: _lastPassthroughDigit 不在此清零。它是跨 IME 会话的上下文信号，
    // 用于 Excel/WPS cell-select(按数字直通) → cell-edit(按标点) 这种焦点切换
    // 场景的数字后智能标点判断。残留由按键事件路径（_SendKeyToService 非智能
    // 标点目标键清零）和光标 Y 跨行检测兜底，不应在 IME 会话状态重置时一起清。
    void ResetComposingState() { _isComposing = FALSE; _hasCandidates = FALSE; _skipKeyCount = 0; _pendingPairAction = {}; _englishPairEngine.Clear(); }

    // Handle config sync from Go service (called from async reader thread)
    void OnSyncConfig(const std::string& key, const std::vector<uint8_t>& value);

    // Called when composition is unexpectedly terminated by the application
    // This resets state and notifies Go service to clear input buffer
    void OnCompositionUnexpectedlyTerminated();

private:
    LONG _refCount;
    CTextService* _pTextService;
    DWORD _dwKeySinkCookie;

    // State
    BOOL _isComposing;
    BOOL _hasCandidates;         // True if there are candidates to select
    WCHAR _lastPassthroughDigit; // Last digit key that passed through (for smart punct fallback in apps where TSF can't read text)
    uint32_t _pendingKeyUpKey;   // Key code of pending KeyUp toggle key
    uint32_t _pendingKeyUpModifiers; // Modifiers when KeyDown was pressed
    DWORD    _pendingKeyDownTime;    // GetTickCount() when toggle key was pressed down

    // Maximum duration (ms) for a toggle key press to count as a "tap"
    // Long presses beyond this threshold will NOT trigger mode toggle
    static constexpr DWORD TOGGLE_TAP_THRESHOLD_MS = 500;

    // Guard against accidental re-toggle when modifier is used as combination key
    // (e.g., Shift tap to toggle, then immediately Shift+A for uppercase)
    // After a toggle, subsequent toggles are ignored until a non-modifier key is pressed
    // or TOGGLE_GUARD_MS has elapsed.
    static constexpr DWORD TOGGLE_GUARD_MS = 300;

    DWORD _lastToggleExecuteTime;   // GetTickCount() when last toggle was executed
    BOOL  _anyKeyAfterToggle;       // TRUE if any non-modifier key was pressed after last toggle

    // ========================================================================
    // Modifier key state machine (replaces GetAsyncKeyState for consistency)
    // ========================================================================
    uint32_t _modsState;         // Current modifier state (maintained by KeyDown/KeyUp)
    uint16_t _eventSeq;          // Monotonic event sequence number

    // State machine update methods
    void _UpdateModsOnKeyDown(WPARAM vk);
    void _UpdateModsOnKeyUp(WPARAM vk);
    uint32_t _GetModsSnapshot() const { return _modsState; }
    uint8_t _GetTogglesSnapshot() const;
    uint16_t _GetNextEventSeq() { return _eventSeq++; }

    // Sync state from Go response
    void _SyncStateFromResponse(uint32_t statusFlags);

    // ========================================================================
    // Barrier mechanism for async commit requests
    // ========================================================================
    struct PendingBarrier
    {
        uint16_t barrierSeq;
        std::wstring composition;  // Composition at request time
        DWORD requestTime;         // GetTickCount() at request
        bool waiting;
    };

    uint16_t _nextBarrierSeq;
    PendingBarrier _pendingCommit;

    // Barrier timeout (if Go doesn't respond, fallback handling)
    static constexpr DWORD BARRIER_TIMEOUT_MS = 500;

    BOOL _SendCommitRequest(uint16_t barrierSeq, uint16_t triggerKey, uint32_t mods, const std::string& inputBuffer);
    void _HandleCommitResult(uint16_t barrierSeq, const std::wstring& text, const std::wstring& newComp, bool modeChanged, bool chineseMode);
    void _CheckBarrierTimeout();

    // ========================================================================
    // Helper methods
    // ========================================================================
    BOOL _IsMatchingKeyUp(WPARAM wParam, uint32_t pendingKey);
    BOOL _SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType);
    BOOL _HandleServiceResponse(); // Returns TRUE if key was handled, FALSE to pass through

    // Context state checking (for browser non-editable area detection)
    BOOL _IsContextReadOnly(ITfContext* pContext);

    // ========================================================================
    // Auto-pair key simulation (deferred + skip list approach)
    // ========================================================================
    void _SimulatePairKey(WORD vk);
    static bool _AreModifiersHeld();

    // Pending auto-pair action (deferred until modifiers released)
    struct PendingPairAction {
        WORD vk = 0;
        int count = 0;
        bool active = false;
    };
    PendingPairAction _pendingPairAction;

    // English auto-pair engine (handles pairing in English mode)
    PairEngine _englishPairEngine;

    // Skip list: SendInput keys generated by auto-pair that should bypass IME processing
    static constexpr int MAX_SKIP_KEYS = 16;
    WORD _skipKeys[MAX_SKIP_KEYS] = {};
    int _skipKeyCount = 0;
    void _PushSkipKey(WORD vk);
    BOOL _TryConsumeSkipKey(WPARAM wParam);
};
