#include "DisplayAttributeInfo.h"

// ============================================================================
// CDisplayAttributeInfoInput
// ============================================================================

CDisplayAttributeInfoInput::CDisplayAttributeInfoInput()
    : _refCount(1)
{
    // Initialize default display attribute for input/composition
    // This shows text with a solid underline, typical for IME composition
    ZeroMemory(&_displayAttribute, sizeof(_displayAttribute));

    // Text color: use system default (TF_CT_NONE means no change)
    _displayAttribute.crText.type = TF_CT_NONE;

    // Background color: use system default
    _displayAttribute.crBk.type = TF_CT_NONE;

    // Underline style: dotted line for composition
    _displayAttribute.lsStyle = TF_LS_DOT;

    // Bold underline for visibility
    _displayAttribute.fBoldLine = FALSE;

    // Underline color: use text color
    _displayAttribute.crLine.type = TF_CT_NONE;

    // Attribute effects
    _displayAttribute.bAttr = TF_ATTR_INPUT;
}

CDisplayAttributeInfoInput::~CDisplayAttributeInfoInput()
{
}

STDAPI CDisplayAttributeInfoInput::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfDisplayAttributeInfo))
    {
        *ppvObj = (ITfDisplayAttributeInfo*)this;
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CDisplayAttributeInfoInput::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CDisplayAttributeInfoInput::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);
    if (cr == 0)
    {
        delete this;
    }
    return cr;
}

STDAPI CDisplayAttributeInfoInput::GetGUID(GUID* pguid)
{
    if (pguid == nullptr)
        return E_INVALIDARG;

    *pguid = c_guidDisplayAttributeInput;
    return S_OK;
}

STDAPI CDisplayAttributeInfoInput::GetDescription(BSTR* pbstrDesc)
{
    if (pbstrDesc == nullptr)
        return E_INVALIDARG;

    *pbstrDesc = SysAllocString(L"WindInput Input Attribute");
    return (*pbstrDesc != nullptr) ? S_OK : E_OUTOFMEMORY;
}

STDAPI CDisplayAttributeInfoInput::GetAttributeInfo(TF_DISPLAYATTRIBUTE* pda)
{
    if (pda == nullptr)
        return E_INVALIDARG;

    *pda = _displayAttribute;
    return S_OK;
}

STDAPI CDisplayAttributeInfoInput::SetAttributeInfo(const TF_DISPLAYATTRIBUTE* pda)
{
    if (pda == nullptr)
        return E_INVALIDARG;

    _displayAttribute = *pda;
    return S_OK;
}

STDAPI CDisplayAttributeInfoInput::Reset()
{
    // Reset to default values
    ZeroMemory(&_displayAttribute, sizeof(_displayAttribute));
    _displayAttribute.crText.type = TF_CT_NONE;
    _displayAttribute.crBk.type = TF_CT_NONE;
    _displayAttribute.lsStyle = TF_LS_DOT;
    _displayAttribute.fBoldLine = FALSE;
    _displayAttribute.crLine.type = TF_CT_NONE;
    _displayAttribute.bAttr = TF_ATTR_INPUT;
    return S_OK;
}

// ============================================================================
// CDisplayAttributeProvider
// ============================================================================

CDisplayAttributeProvider::CDisplayAttributeProvider()
    : _refCount(1)
{
}

CDisplayAttributeProvider::~CDisplayAttributeProvider()
{
}

STDAPI CDisplayAttributeProvider::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfDisplayAttributeProvider))
    {
        *ppvObj = (ITfDisplayAttributeProvider*)this;
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CDisplayAttributeProvider::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CDisplayAttributeProvider::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);
    if (cr == 0)
    {
        delete this;
    }
    return cr;
}

STDAPI CDisplayAttributeProvider::EnumDisplayAttributeInfo(IEnumTfDisplayAttributeInfo** ppEnum)
{
    if (ppEnum == nullptr)
        return E_INVALIDARG;

    *ppEnum = new CEnumDisplayAttributeInfo();
    return (*ppEnum != nullptr) ? S_OK : E_OUTOFMEMORY;
}

STDAPI CDisplayAttributeProvider::GetDisplayAttributeInfo(REFGUID guid, ITfDisplayAttributeInfo** ppInfo)
{
    if (ppInfo == nullptr)
        return E_INVALIDARG;

    *ppInfo = nullptr;

    if (IsEqualGUID(guid, c_guidDisplayAttributeInput))
    {
        *ppInfo = new CDisplayAttributeInfoInput();
        return (*ppInfo != nullptr) ? S_OK : E_OUTOFMEMORY;
    }

    return E_INVALIDARG;
}

// ============================================================================
// CEnumDisplayAttributeInfo
// ============================================================================

CEnumDisplayAttributeInfo::CEnumDisplayAttributeInfo()
    : _refCount(1)
    , _index(0)
{
}

CEnumDisplayAttributeInfo::~CEnumDisplayAttributeInfo()
{
}

STDAPI CEnumDisplayAttributeInfo::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IEnumTfDisplayAttributeInfo))
    {
        *ppvObj = (IEnumTfDisplayAttributeInfo*)this;
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CEnumDisplayAttributeInfo::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CEnumDisplayAttributeInfo::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);
    if (cr == 0)
    {
        delete this;
    }
    return cr;
}

STDAPI CEnumDisplayAttributeInfo::Clone(IEnumTfDisplayAttributeInfo** ppEnum)
{
    if (ppEnum == nullptr)
        return E_INVALIDARG;

    CEnumDisplayAttributeInfo* pClone = new CEnumDisplayAttributeInfo();
    if (pClone == nullptr)
        return E_OUTOFMEMORY;

    pClone->_index = _index;
    *ppEnum = pClone;
    return S_OK;
}

STDAPI CEnumDisplayAttributeInfo::Next(ULONG ulCount, ITfDisplayAttributeInfo** rgInfo, ULONG* pcFetched)
{
    if (rgInfo == nullptr)
        return E_INVALIDARG;

    ULONG fetched = 0;

    // We only have one display attribute (input)
    while (fetched < ulCount && _index < 1)
    {
        rgInfo[fetched] = new CDisplayAttributeInfoInput();
        if (rgInfo[fetched] == nullptr)
        {
            // Clean up already allocated
            for (ULONG i = 0; i < fetched; i++)
            {
                rgInfo[i]->Release();
                rgInfo[i] = nullptr;
            }
            return E_OUTOFMEMORY;
        }
        fetched++;
        _index++;
    }

    if (pcFetched != nullptr)
        *pcFetched = fetched;

    return (fetched == ulCount) ? S_OK : S_FALSE;
}

STDAPI CEnumDisplayAttributeInfo::Reset()
{
    _index = 0;
    return S_OK;
}

STDAPI CEnumDisplayAttributeInfo::Skip(ULONG ulCount)
{
    // We only have 1 item
    if (_index + ulCount > 1)
    {
        _index = 1;
        return S_FALSE;
    }
    _index += ulCount;
    return S_OK;
}
