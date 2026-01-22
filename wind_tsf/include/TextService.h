#pragma once

#include "Globals.h"
#include <string>

// Forward declarations
class CKeyEventSink;
class CIPCClient;
class CLangBarItemButton;

class CTextService : public ITfTextInputProcessor,
                     public ITfThreadMgrEventSink
{
public:
    CTextService();
    ~CTextService();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfTextInputProcessor
    STDMETHODIMP Activate(ITfThreadMgr* pThreadMgr, TfClientId tfClientId);
    STDMETHODIMP Deactivate();

    // ITfThreadMgrEventSink
    STDMETHODIMP OnInitDocumentMgr(ITfDocumentMgr* pDocMgr);
    STDMETHODIMP OnUninitDocumentMgr(ITfDocumentMgr* pDocMgr);
    STDMETHODIMP OnSetFocus(ITfDocumentMgr* pDocMgrFocus, ITfDocumentMgr* pDocMgrPrevFocus);
    STDMETHODIMP OnPushContext(ITfContext* pContext);
    STDMETHODIMP OnPopContext(ITfContext* pContext);

    // Get thread manager
    ITfThreadMgr* GetThreadMgr() { return _pThreadMgr; }

    // Get client ID
    TfClientId GetClientId() { return _tfClientId; }

    // Get IPC client
    CIPCClient* GetIPCClient() { return _pIPCClient; }

    // Insert text into current context
    BOOL InsertText(const std::wstring& text);

    // Get and send caret position to Go Service
    BOOL GetCaretPosition(LONG* px, LONG* py, LONG* pHeight);
    void SendCaretPositionUpdate();

    // Input mode control
    void ToggleInputMode();
    void SetInputMode(BOOL bChineseMode);  // Set mode from service response (no IPC)
    BOOL IsChineseMode() { return _bChineseMode; }

    // Update language bar Caps Lock state
    void UpdateCapsLockState(BOOL bCapsLock);

private:
    LONG _refCount;
    ITfThreadMgr* _pThreadMgr;
    TfClientId _tfClientId;
    DWORD _dwThreadMgrEventSinkCookie;

    // Components
    CKeyEventSink* _pKeyEventSink;
    CIPCClient* _pIPCClient;
    CLangBarItemButton* _pLangBarItemButton;

    // Input mode state
    BOOL _bChineseMode;

    BOOL _InitThreadMgrEventSink();
    void _UninitThreadMgrEventSink();

    BOOL _InitKeyEventSink();
    void _UninitKeyEventSink();

    BOOL _InitIPCClient();
    void _UninitIPCClient();

    BOOL _InitLangBarButton();
    void _UninitLangBarButton();
};
