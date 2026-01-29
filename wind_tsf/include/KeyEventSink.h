#pragma once

#include "Globals.h"
#include <string>

class CTextService;

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
    void ResetComposingState() { _isComposing = FALSE; }

private:
    LONG _refCount;
    CTextService* _pTextService;
    DWORD _dwKeySinkCookie;

    // State
    BOOL _isComposing;
    BOOL _hasCandidates;  // True if there are candidates to select
    BOOL _shiftPending;   // True if Shift was pressed alone (for mode toggle on release)
    WPARAM _pendingToggleKey;  // The toggle key that is pending (for release detection)

    // Helper methods
    int _GetModifierState();
    BOOL _IsKeyWeShouldHandle(WPARAM wParam);
    BOOL _IsPunctuationKey(WPARAM wParam);
    wchar_t _VirtualKeyToPunctuation(WPARAM wParam, BOOL shiftPressed);
    BOOL _SendKeyToService(WPARAM wParam);
    void _HandleServiceResponse();

    // Context state checking (for browser non-editable area detection)
    BOOL _IsContextReadOnly(ITfContext* pContext);
    BOOL _IsCurrentProcessBrowser();
};
