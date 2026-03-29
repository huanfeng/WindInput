#pragma once

#include "Globals.h"

// Display Attribute Info for input/composition text (with underline)
class CDisplayAttributeInfoInput : public ITfDisplayAttributeInfo
{
public:
    CDisplayAttributeInfoInput();
    ~CDisplayAttributeInfoInput();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfDisplayAttributeInfo
    STDMETHODIMP GetGUID(GUID* pguid);
    STDMETHODIMP GetDescription(BSTR* pbstrDesc);
    STDMETHODIMP GetAttributeInfo(TF_DISPLAYATTRIBUTE* pda);
    STDMETHODIMP SetAttributeInfo(const TF_DISPLAYATTRIBUTE* pda);
    STDMETHODIMP Reset();

private:
    LONG _refCount;
    TF_DISPLAYATTRIBUTE _displayAttribute;
};

// Display Attribute Provider - enumerates available display attributes
class CDisplayAttributeProvider : public ITfDisplayAttributeProvider
{
public:
    CDisplayAttributeProvider();
    ~CDisplayAttributeProvider();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfDisplayAttributeProvider
    STDMETHODIMP EnumDisplayAttributeInfo(IEnumTfDisplayAttributeInfo** ppEnum);
    STDMETHODIMP GetDisplayAttributeInfo(REFGUID guid, ITfDisplayAttributeInfo** ppInfo);

private:
    LONG _refCount;
};

// Enumerator for display attributes
class CEnumDisplayAttributeInfo : public IEnumTfDisplayAttributeInfo
{
public:
    CEnumDisplayAttributeInfo();
    ~CEnumDisplayAttributeInfo();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // IEnumTfDisplayAttributeInfo
    STDMETHODIMP Clone(IEnumTfDisplayAttributeInfo** ppEnum);
    STDMETHODIMP Next(ULONG ulCount, ITfDisplayAttributeInfo** rgInfo, ULONG* pcFetched);
    STDMETHODIMP Reset();
    STDMETHODIMP Skip(ULONG ulCount);

private:
    LONG _refCount;
    ULONG _index;
};
