#include "ClassFactory.h"
#include "TextService.h"

CClassFactory::CClassFactory() : _refCount(1)
{
    DllAddRef();
}

CClassFactory::~CClassFactory()
{
    DllRelease();
}

STDAPI CClassFactory::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IClassFactory))
    {
        *ppvObj = (IClassFactory*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CClassFactory::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CClassFactory::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);

    if (cr == 0)
    {
        delete this;
    }

    return cr;
}

STDAPI CClassFactory::CreateInstance(IUnknown* pUnkOuter, REFIID riid, void** ppvObj)
{
    // 诊断：类厂实例化入口。拒绝发生在 DllGetClassObject 之后、这里之前，
    // 说明是 msctf 对类厂本身 QueryInterface/激活策略层面的拒绝。
    {
        WCHAR szIid[64] = {};
        StringFromGUID2(riid, szIid, ARRAYSIZE(szIid));
        WIND_LOG_DEBUG_FMT(
            L"ClassFactory::CreateInstance riid=%ls pUnkOuter=%ls",
            szIid, pUnkOuter == nullptr ? L"null" : L"non-null");
    }

    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (pUnkOuter != nullptr)
        return CLASS_E_NOAGGREGATION;

    CTextService* pTextService = new CTextService();
    if (pTextService == nullptr)
        return E_OUTOFMEMORY;

    HRESULT hr = pTextService->QueryInterface(riid, ppvObj);
    pTextService->Release();

    return hr;
}

STDAPI CClassFactory::LockServer(BOOL fLock)
{
    if (fLock)
        DllAddRef();
    else
        DllRelease();

    return S_OK;
}
