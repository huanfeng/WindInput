#pragma once

#include "Globals.h"
#include <string>

// Forward declarations
class CKeyEventSink;
class CIPCClient;
class CLangBarItemButton;
class CCaretEditSession;
class CDisplayAttributeProvider;
class CHotkeyManager;

class CTextService : public ITfTextInputProcessor,
                     public ITfThreadMgrEventSink,
                     public ITfCompositionSink,
                     public ITfDisplayAttributeProvider
{
    friend class CUpdateCompositionEditSession;
    friend class CEndCompositionEditSession;
    friend class CInsertAndComposeEditSession;
    friend class CInsertTextEditSession;
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

    // ITfCompositionSink
    STDMETHODIMP OnCompositionTerminated(TfEditCookie ecWrite, ITfComposition* pComposition);

    // ITfDisplayAttributeProvider
    STDMETHODIMP EnumDisplayAttributeInfo(IEnumTfDisplayAttributeInfo** ppEnum);
    STDMETHODIMP GetDisplayAttributeInfo(REFGUID guid, ITfDisplayAttributeInfo** ppInfo);

    // Get thread manager
    ITfThreadMgr* GetThreadMgr() { return _pThreadMgr; }

    // Get client ID
    TfClientId GetClientId() { return _tfClientId; }

    // Get IPC client
    CIPCClient* GetIPCClient() { return _pIPCClient; }

    // Get hotkey manager
    CHotkeyManager* GetHotkeyManager() { return _pHotkeyManager; }

    // Insert text into current context
    BOOL InsertText(const std::wstring& text);

    // Update composition text (Inline Composition)
    BOOL UpdateComposition(const std::wstring& text, int caretPos);

    // End current composition
    void EndComposition();

    // Insert text and start new composition (for top code commit)
    BOOL InsertTextAndStartComposition(const std::wstring& insertText, const std::wstring& newComposition);

    // Get and send caret position to Go Service
    BOOL GetCaretPosition(LONG* px, LONG* py, LONG* pHeight);
    void SendCaretPositionUpdate();

    // Get caret position using TSF APIs (more accurate for browsers)
    BOOL GetCaretPositionFromTSF(LONG* px, LONG* py, LONG* pHeight);

    // Input mode control
    void ToggleInputMode();
    void SetInputMode(BOOL bChineseMode);  // Set mode from service response (no IPC)
    BOOL IsChineseMode() { return _bChineseMode; }

    // Check if there's an active composition
    BOOL HasActiveComposition() { return _pComposition != nullptr; }

    // Check if last edit session was async (Weasel optimization)
    BOOL IsAsyncEdit() { return _asyncEdit; }
    void ClearAsyncEdit() { _asyncEdit = FALSE; }

    // Update language bar Caps Lock state
    void UpdateCapsLockState(BOOL bCapsLock);

    // Send menu command to Go service
    void SendMenuCommand(const char* command);

    // Send show context menu request to Go service (screen coordinates)
    void SendShowContextMenu(int screenX, int screenY);

    // Update full status from Go service response
    void UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock);

private:
    LONG _refCount;
    ITfThreadMgr* _pThreadMgr;
    TfClientId _tfClientId;
    DWORD _dwThreadMgrEventSinkCookie;

    // Components
    CKeyEventSink* _pKeyEventSink;
    CIPCClient* _pIPCClient;
    CLangBarItemButton* _pLangBarItemButton;
    CHotkeyManager* _pHotkeyManager;

    // Input mode state
    BOOL _bChineseMode;

    // Composition
    ITfComposition* _pComposition;
    std::wstring _lastCompositionText;  // Cache to skip redundant updates
    BOOL _asyncEdit;  // Track if last RequestEditSession returned TF_S_ASYNC (Weasel optimization)

    // Display Attribute
    TfGuidAtom _gaDisplayAttributeInput;

    BOOL _InitThreadMgrEventSink();
    void _UninitThreadMgrEventSink();

    BOOL _InitKeyEventSink();
    void _UninitKeyEventSink();

    BOOL _InitIPCClient();
    void _UninitIPCClient();

    BOOL _InitLangBarButton();
    void _UninitLangBarButton();

    BOOL _InitDisplayAttribute();
    void _UninitDisplayAttribute();

public:
    // Get display attribute GUID atom for composition
    TfGuidAtom GetDisplayAttributeInputAtom() { return _gaDisplayAttributeInput; }
};
