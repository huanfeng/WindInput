#pragma once

#include "Globals.h"
#include <string>
#include <cstdint>

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
    void ResetComposingState() { _isComposing = FALSE; _hasCandidates = FALSE; }

private:
    LONG _refCount;
    CTextService* _pTextService;
    DWORD _dwKeySinkCookie;

    // State
    BOOL _isComposing;
    BOOL _hasCandidates;         // True if there are candidates to select
    uint32_t _pendingKeyUpKey;   // Key code of pending KeyUp toggle key
    uint32_t _pendingKeyUpModifiers; // Modifiers when KeyDown was pressed

    // Helper methods
    BOOL _IsMatchingKeyUp(WPARAM wParam, uint32_t pendingKey);
    BOOL _SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType);
    void _HandleServiceResponse();

    // Context state checking (for browser non-editable area detection)
    BOOL _IsContextReadOnly(ITfContext* pContext);
};
