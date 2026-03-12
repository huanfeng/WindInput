#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <dwrite.h>
#include <wrl/client.h>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <cwchar>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

#pragma comment(lib, "dwrite.lib")

using Microsoft::WRL::ComPtr;

namespace {

constexpr wchar_t kDefaultFontName[] = L"Microsoft YaHei";
constexpr wchar_t kSymbolFontName[] = L"Segoe UI Symbol";

struct FormatKey {
    std::wstring family;
    int weight = 0;
    int size = 0;
    bool symbol = false;

    bool operator==(const FormatKey& other) const {
        return weight == other.weight &&
               size == other.size &&
               symbol == other.symbol &&
               family == other.family;
    }
};

struct FormatKeyHash {
    size_t operator()(const FormatKey& key) const {
        size_t h = std::hash<std::wstring>{}(key.family);
        h ^= static_cast<size_t>(key.weight) + 0x9e3779b9 + (h << 6) + (h >> 2);
        h ^= static_cast<size_t>(key.size) + 0x9e3779b9 + (h << 6) + (h >> 2);
        h ^= static_cast<size_t>(key.symbol) + 0x9e3779b9 + (h << 6) + (h >> 2);
        return h;
    }
};

constexpr size_t kMaxTextFormats = 32;

// ---------------------------------------------------------------------------
// GdiTextRenderer: bridges IDWriteTextLayout::Draw() to
// IDWriteBitmapRenderTarget::DrawGlyphRun(), eliminating D2D entirely.
// ---------------------------------------------------------------------------
class GdiTextRenderer : public IDWriteTextRenderer {
public:
    GdiTextRenderer(IDWriteBitmapRenderTarget* bitmapRT,
                    IDWriteRenderingParams* params,
                    COLORREF textColor)
        : refCount_(1), bitmapRT_(bitmapRT), params_(params), textColor_(textColor) {}

    // IUnknown
    ULONG STDMETHODCALLTYPE AddRef() override {
        return InterlockedIncrement(&refCount_);
    }
    ULONG STDMETHODCALLTYPE Release() override {
        ULONG count = InterlockedDecrement(&refCount_);
        if (count == 0) delete this;
        return count;
    }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppvObject) override {
        if (!ppvObject) return E_POINTER;
        if (riid == __uuidof(IUnknown) ||
            riid == __uuidof(IDWritePixelSnapping) ||
            riid == __uuidof(IDWriteTextRenderer)) {
            *ppvObject = this;
            AddRef();
            return S_OK;
        }
        *ppvObject = nullptr;
        return E_NOINTERFACE;
    }

    // IDWritePixelSnapping
    HRESULT STDMETHODCALLTYPE IsPixelSnappingDisabled(void*, BOOL* isDisabled) override {
        *isDisabled = FALSE;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetCurrentTransform(void*, DWRITE_MATRIX* transform) override {
        return bitmapRT_->GetCurrentTransform(transform);
    }
    HRESULT STDMETHODCALLTYPE GetPixelsPerDip(void*, FLOAT* pixelsPerDip) override {
        *pixelsPerDip = bitmapRT_->GetPixelsPerDip();
        return S_OK;
    }

    // IDWriteTextRenderer
    HRESULT STDMETHODCALLTYPE DrawGlyphRun(
        void*,
        FLOAT baselineOriginX,
        FLOAT baselineOriginY,
        DWRITE_MEASURING_MODE measuringMode,
        const DWRITE_GLYPH_RUN* glyphRun,
        const DWRITE_GLYPH_RUN_DESCRIPTION*,
        IUnknown*) override
    {
        return bitmapRT_->DrawGlyphRun(
            baselineOriginX, baselineOriginY,
            measuringMode, glyphRun,
            params_, textColor_);
    }

    HRESULT STDMETHODCALLTYPE DrawUnderline(void*, FLOAT, FLOAT, const DWRITE_UNDERLINE*, IUnknown*) override {
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE DrawStrikethrough(void*, FLOAT, FLOAT, const DWRITE_STRIKETHROUGH*, IUnknown*) override {
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE DrawInlineObject(void*, FLOAT, FLOAT, IDWriteInlineObject*, BOOL, BOOL, IUnknown*) override {
        return S_OK;
    }

    void SetTextColor(COLORREF color) { textColor_ = color; }

private:
    ULONG refCount_;
    IDWriteBitmapRenderTarget* bitmapRT_;
    IDWriteRenderingParams* params_;
    COLORREF textColor_;
};

// ---------------------------------------------------------------------------
// SharedResources: global singleton holding DWrite factory, GDI interop,
// shared bitmap render target, and text format cache with LRU eviction.
// ---------------------------------------------------------------------------
class SharedResources {
public:
    bool IsValid() const {
        return valid_;
    }

    ComPtr<IDWriteTextFormat> GetTextFormat(
        const std::wstring& family,
        int weight,
        float scale,
        int fontSize,
        bool useSymbol
    ) {
        if (!valid_ || fontSize <= 0) {
            return nullptr;
        }

        const int scaledSize = (std::max)(1, static_cast<int>(std::lround(fontSize * scale)));
        FormatKey key{
            useSymbol ? std::wstring(kSymbolFontName) : family,
            weight > 0 ? weight : static_cast<int>(DWRITE_FONT_WEIGHT_NORMAL),
            scaledSize,
            useSymbol,
        };

        std::lock_guard<std::mutex> lock(mu_);
        auto it = formats_.find(key);
        if (it != formats_.end()) {
            TouchFormatLRU(key);
            return it->second;
        }

        ComPtr<IDWriteTextFormat> format;
        HRESULT hr = dwriteFactory_->CreateTextFormat(
            key.family.c_str(),
            nullptr,
            static_cast<DWRITE_FONT_WEIGHT>(key.weight),
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            static_cast<FLOAT>(key.size),
            L"zh-CN",
            &format
        );
        if (FAILED(hr) || !format) {
            return nullptr;
        }

        format->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        format->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);

        EvictFormatLRU();
        formats_.emplace(key, format);
        formatLRU_.push_back(key);
        return format;
    }

    IDWriteFactory* DWriteFactory() const {
        return dwriteFactory_.Get();
    }

    // Lock the shared bitmap render target for a draw session.
    // Caller must call UnlockDraw() when done.
    // Returns the BitmapRenderTarget resized to (width x height) and the
    // rendering params for DrawGlyphRun.
    bool LockDraw(int width, int height,
                  IDWriteBitmapRenderTarget** ppBitmapRT,
                  IDWriteRenderingParams** ppParams) {
        drawMu_.lock();

        HRESULT hr = S_OK;
        if (!bitmapRT_) {
            HDC screenDC = GetDC(nullptr);
            hr = gdiInterop_->CreateBitmapRenderTarget(screenDC, width, height, &bitmapRT_);
            ReleaseDC(nullptr, screenDC);
            if (FAILED(hr) || !bitmapRT_) {
                drawMu_.unlock();
                return false;
            }
            // Font sizes from Go are already DPI-scaled physical pixels.
            // Disable BitmapRT's automatic DPI scaling to avoid double-scaling.
            bitmapRT_->SetPixelsPerDip(1.0f);
        } else {
            SIZE curSize{};
            bitmapRT_->GetSize(&curSize);
            if (curSize.cx < width || curSize.cy < height) {
                int newW = (std::max)(static_cast<int>(curSize.cx), width);
                int newH = (std::max)(static_cast<int>(curSize.cy), height);
                hr = bitmapRT_->Resize(newW, newH);
                if (FAILED(hr)) {
                    drawMu_.unlock();
                    return false;
                }
            }
        }

        *ppBitmapRT = bitmapRT_.Get();
        *ppParams = renderingParams_.Get();
        return true;
    }

    void UnlockDraw() {
        drawMu_.unlock();
    }

    void ReleaseDrawResources() {
        std::lock_guard<std::mutex> lock(drawMu_);
        bitmapRT_.Reset();
    }

private:
    friend SharedResources* AcquireSharedResources();

    SharedResources() {
        valid_ = Initialize();
    }

    bool Initialize() {
        HRESULT hr = DWriteCreateFactory(
            DWRITE_FACTORY_TYPE_SHARED,
            __uuidof(IDWriteFactory),
            reinterpret_cast<IUnknown**>(dwriteFactory_.GetAddressOf())
        );
        if (FAILED(hr)) {
            return false;
        }

        hr = dwriteFactory_->GetGdiInterop(&gdiInterop_);
        if (FAILED(hr) || !gdiInterop_) {
            return false;
        }

        // Use monitor-aware rendering params for optimal ClearType quality.
        HMONITOR monitor = MonitorFromWindow(nullptr, MONITOR_DEFAULTTOPRIMARY);
        hr = dwriteFactory_->CreateMonitorRenderingParams(monitor, &renderingParams_);
        if (FAILED(hr) || !renderingParams_) {
            // Fallback to default rendering params.
            hr = dwriteFactory_->CreateRenderingParams(&renderingParams_);
            if (FAILED(hr)) {
                return false;
            }
        }

        return true;
    }

    void TouchFormatLRU(const FormatKey& key) {
        for (auto it = formatLRU_.begin(); it != formatLRU_.end(); ++it) {
            if (*it == key) {
                formatLRU_.erase(it);
                formatLRU_.push_back(key);
                return;
            }
        }
    }

    void EvictFormatLRU() {
        while (formats_.size() >= kMaxTextFormats && !formatLRU_.empty()) {
            formats_.erase(formatLRU_.front());
            formatLRU_.erase(formatLRU_.begin());
        }
    }

    bool valid_ = false;
    mutable std::mutex mu_;
    ComPtr<IDWriteFactory> dwriteFactory_;
    ComPtr<IDWriteGdiInterop> gdiInterop_;
    ComPtr<IDWriteRenderingParams> renderingParams_;
    std::unordered_map<FormatKey, ComPtr<IDWriteTextFormat>, FormatKeyHash> formats_;
    std::vector<FormatKey> formatLRU_;

    std::mutex drawMu_;
    ComPtr<IDWriteBitmapRenderTarget> bitmapRT_;
};

std::mutex gSharedResourcesMu;
std::unique_ptr<SharedResources> gSharedResources;

SharedResources* AcquireSharedResources() {
    std::lock_guard<std::mutex> lock(gSharedResourcesMu);
    if (!gSharedResources) {
        auto resources = std::unique_ptr<SharedResources>(new SharedResources());
        if (!resources->IsValid()) {
            return nullptr;
        }
        gSharedResources = std::move(resources);
    }
    return gSharedResources.get();
}

bool ShutdownSharedResources() {
    std::lock_guard<std::mutex> lock(gSharedResourcesMu);
    if (gSharedResources) {
        gSharedResources->ReleaseDrawResources();
    }
    gSharedResources.reset();
    return true;
}

class Renderer {
public:
    Renderer() = default;

    bool IsValid() const {
        return AcquireSharedResources() != nullptr;
    }

    void SetFont(const wchar_t* fontName) {
        std::lock_guard<std::mutex> lock(mu_);
        fontName_ = (fontName && fontName[0] != L'\0') ? fontName : kDefaultFontName;
    }

    void SetFontParams(int weight, float scale) {
        std::lock_guard<std::mutex> lock(mu_);
        fontWeight_ = weight > 0 ? weight : static_cast<int>(DWRITE_FONT_WEIGHT_NORMAL);
        fontScale_ = scale > 0.0f ? scale : 1.0f;
    }

    bool MeasureString(const wchar_t* text, int fontSize, bool useSymbol, int* width) {
        if (!text || !width || fontSize <= 0) {
            return false;
        }

        std::lock_guard<std::mutex> lock(mu_);
        auto* shared = AcquireSharedResources();
        if (!shared) {
            return false;
        }
        auto format = shared->GetTextFormat(fontName_, fontWeight_, fontScale_, fontSize, useSymbol);
        if (!format) {
            return false;
        }

        ComPtr<IDWriteTextLayout> layout;
        HRESULT hr = shared->DWriteFactory()->CreateTextLayout(
            text,
            static_cast<UINT32>(wcslen(text)),
            format.Get(),
            10000.0f,
            1000.0f,
            &layout
        );
        if (FAILED(hr) || !layout) {
            return false;
        }

        DWRITE_TEXT_METRICS metrics{};
        hr = layout->GetMetrics(&metrics);
        if (FAILED(hr)) {
            return false;
        }

        *width = static_cast<int>(std::lround(metrics.widthIncludingTrailingWhitespace));
        return true;
    }

    bool BeginDraw(uint8_t* rgba, int width, int height, int stride) {
        std::lock_guard<std::mutex> lock(mu_);
        if (!rgba || width <= 0 || height <= 0 || stride <= 0) {
            return false;
        }

        EndDrawLocked();

        auto* shared = AcquireSharedResources();
        if (!shared) {
            return false;
        }

        IDWriteBitmapRenderTarget* bitmapRT = nullptr;
        IDWriteRenderingParams* params = nullptr;
        if (!shared->LockDraw(width, height, &bitmapRT, &params)) {
            return false;
        }

        // Create a memory DC + DIB section for RGBA <-> BGRA conversion.
        HDC screenDC = GetDC(nullptr);
        if (!screenDC) {
            shared->UnlockDraw();
            return false;
        }

        HDC memDC = CreateCompatibleDC(screenDC);
        ReleaseDC(nullptr, screenDC);
        if (!memDC) {
            shared->UnlockDraw();
            return false;
        }

        BITMAPINFO bi{};
        bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
        bi.bmiHeader.biWidth = width;
        bi.bmiHeader.biHeight = -height;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;

        void* bits = nullptr;
        HBITMAP bitmap = CreateDIBSection(memDC, &bi, DIB_RGB_COLORS, &bits, nullptr, 0);
        if (!bitmap || !bits) {
            DeleteDC(memDC);
            shared->UnlockDraw();
            return false;
        }

        HGDIOBJ oldBmp = SelectObject(memDC, bitmap);
        if (!oldBmp) {
            DeleteObject(bitmap);
            DeleteDC(memDC);
            shared->UnlockDraw();
            return false;
        }

        // Copy RGBA -> BGRA into our DIB.
        auto* dst = static_cast<uint8_t*>(bits);
        for (int y = 0; y < height; ++y) {
            uint8_t* srcRow = rgba + y * stride;
            uint8_t* dstRow = dst + y * width * 4;
            for (int x = 0; x < width; ++x) {
                const int si = x * 4;
                dstRow[si + 0] = srcRow[si + 2];
                dstRow[si + 1] = srcRow[si + 1];
                dstRow[si + 2] = srcRow[si + 0];
                dstRow[si + 3] = 255;
            }
        }

        // Blit the background into the shared BitmapRenderTarget.
        HDC bitmapDC = bitmapRT->GetMemoryDC();
        BitBlt(bitmapDC, 0, 0, width, height, memDC, 0, 0, SRCCOPY);

        bitmapRT_ = bitmapRT;
        renderingParams_ = params;
        drawRGBA_ = rgba;
        drawStride_ = stride;
        drawWidth_ = width;
        drawHeight_ = height;
        drawDC_ = memDC;
        drawBitmap_ = bitmap;
        drawOldBitmap_ = oldBmp;
        drawBits_ = dst;
        inDraw_ = true;
        return true;
    }

    bool DrawString(const wchar_t* text, int x, int y, int fontSize, uint32_t rgba, bool useSymbol) {
        std::lock_guard<std::mutex> lock(mu_);
        if (!inDraw_ || !text || fontSize <= 0 || !bitmapRT_) {
            return false;
        }

        auto* shared = AcquireSharedResources();
        if (!shared) {
            return false;
        }
        auto format = shared->GetTextFormat(fontName_, fontWeight_, fontScale_, fontSize, useSymbol);
        if (!format) {
            return false;
        }

        ComPtr<IDWriteTextLayout> layout;
        HRESULT hr = shared->DWriteFactory()->CreateTextLayout(
            text,
            static_cast<UINT32>(wcslen(text)),
            format.Get(),
            10000.0f,
            1000.0f,
            &layout
        );
        if (FAILED(hr) || !layout) {
            return false;
        }

        // Get baseline from line metrics.
        DWRITE_TEXT_METRICS metrics{};
        layout->GetMetrics(&metrics);

        UINT32 lineCount = 0;
        float baseline = metrics.height * 0.8f;
        hr = layout->GetLineMetrics(nullptr, 0, &lineCount);
        if ((hr == HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER) || hr == E_NOT_SUFFICIENT_BUFFER) && lineCount > 0) {
            std::vector<DWRITE_LINE_METRICS> lines(lineCount);
            if (SUCCEEDED(layout->GetLineMetrics(lines.data(), lineCount, &lineCount)) && !lines.empty()) {
                baseline = lines[0].baseline;
            }
        }

        // Convert RGBA to COLORREF (0x00BBGGRR).
        COLORREF textColor = RGB(rgba & 0xFF, (rgba >> 8) & 0xFF, (rgba >> 16) & 0xFF);

        // Draw using our GDI text renderer bridge.
        auto* renderer = new GdiTextRenderer(bitmapRT_, renderingParams_, textColor);
        float originX = static_cast<float>(x);
        float originY = static_cast<float>(y) - baseline;
        layout->Draw(nullptr, renderer, originX, originY);
        renderer->Release();

        return true;
    }

    bool EndDraw() {
        std::lock_guard<std::mutex> lock(mu_);
        return EndDrawLocked();
    }

    ~Renderer() {
        std::lock_guard<std::mutex> lock(mu_);
        EndDrawLocked();
    }

private:
    bool EndDrawLocked() {
        if (!inDraw_) {
            return true;
        }

        // Blit rendered result from BitmapRenderTarget back to our DIB.
        if (bitmapRT_ && drawDC_) {
            HDC bitmapDC = bitmapRT_->GetMemoryDC();
            BitBlt(drawDC_, 0, 0, drawWidth_, drawHeight_, bitmapDC, 0, 0, SRCCOPY);
        }

        // Convert BGRA -> RGBA and detect text pixels for alpha.
        if (drawRGBA_ && drawBits_) {
            for (int y = 0; y < drawHeight_; ++y) {
                uint8_t* dstRow = drawRGBA_ + y * drawStride_;
                uint8_t* srcRow = drawBits_ + y * drawWidth_ * 4;
                for (int x = 0; x < drawWidth_; ++x) {
                    const int i = x * 4;
                    const uint8_t newR = srcRow[i + 2];
                    const uint8_t newG = srcRow[i + 1];
                    const uint8_t newB = srcRow[i + 0];
                    const uint8_t oldR = dstRow[i + 0];
                    const uint8_t oldG = dstRow[i + 1];
                    const uint8_t oldB = dstRow[i + 2];

                    dstRow[i + 0] = newR;
                    dstRow[i + 1] = newG;
                    dstRow[i + 2] = newB;

                    if (newR != oldR || newG != oldG || newB != oldB) {
                        dstRow[i + 3] = 255;
                    }
                }
            }
        }

        if (drawDC_) {
            SelectObject(drawDC_, drawOldBitmap_);
            DeleteObject(drawBitmap_);
            DeleteDC(drawDC_);
        }

        bitmapRT_ = nullptr;
        renderingParams_ = nullptr;
        drawRGBA_ = nullptr;
        drawStride_ = 0;
        drawWidth_ = 0;
        drawHeight_ = 0;
        drawDC_ = nullptr;
        drawBitmap_ = nullptr;
        drawOldBitmap_ = nullptr;
        drawBits_ = nullptr;
        inDraw_ = false;

        auto* shared = AcquireSharedResources();
        if (shared) {
            shared->UnlockDraw();
        }
        return true;
    }

    std::mutex mu_;
    std::wstring fontName_ = kDefaultFontName;
    int fontWeight_ = static_cast<int>(DWRITE_FONT_WEIGHT_NORMAL);
    float fontScale_ = 1.0f;

    // Borrowed from SharedResources during draw session (not owned)
    IDWriteBitmapRenderTarget* bitmapRT_ = nullptr;
    IDWriteRenderingParams* renderingParams_ = nullptr;

    bool inDraw_ = false;
    uint8_t* drawRGBA_ = nullptr;
    int drawStride_ = 0;
    int drawWidth_ = 0;
    int drawHeight_ = 0;
    HDC drawDC_ = nullptr;
    HBITMAP drawBitmap_ = nullptr;
    HGDIOBJ drawOldBitmap_ = nullptr;
    uint8_t* drawBits_ = nullptr;
};

float ScaleFromBits(uint32_t bits) {
    float scale = 1.0f;
    static_assert(sizeof(scale) == sizeof(bits), "float size mismatch");
    memcpy(&scale, &bits, sizeof(scale));
    if (!(scale > 0.0f)) {
        scale = 1.0f;
    }
    return scale;
}

} // namespace

extern "C" {

__declspec(dllexport) void* WindDWriteCreateRenderer() {
    try {
        auto* renderer = new Renderer();
        if (!renderer->IsValid()) {
            delete renderer;
            return nullptr;
        }
        return renderer;
    } catch (...) {
        return nullptr;
    }
}

__declspec(dllexport) void WindDWriteDestroyRenderer(void* handle) {
    auto* renderer = static_cast<Renderer*>(handle);
    delete renderer;
}

__declspec(dllexport) BOOL WindDWriteSetFont(void* handle, const wchar_t* fontName) {
    auto* renderer = static_cast<Renderer*>(handle);
    if (!renderer) {
        return FALSE;
    }
    renderer->SetFont(fontName);
    return TRUE;
}

__declspec(dllexport) BOOL WindDWriteSetFontParams(void* handle, int weight, uint32_t scaleBits) {
    auto* renderer = static_cast<Renderer*>(handle);
    if (!renderer) {
        return FALSE;
    }
    renderer->SetFontParams(weight, ScaleFromBits(scaleBits));
    return TRUE;
}

__declspec(dllexport) BOOL WindDWriteMeasureString(
    void* handle,
    const wchar_t* text,
    int fontSize,
    BOOL useSymbol,
    int* width
) {
    auto* renderer = static_cast<Renderer*>(handle);
    if (!renderer) {
        return FALSE;
    }
    return renderer->MeasureString(text, fontSize, useSymbol != FALSE, width) ? TRUE : FALSE;
}

__declspec(dllexport) BOOL WindDWriteBeginDraw(
    void* handle,
    uint8_t* rgba,
    int width,
    int height,
    int stride
) {
    auto* renderer = static_cast<Renderer*>(handle);
    if (!renderer) {
        return FALSE;
    }
    return renderer->BeginDraw(rgba, width, height, stride) ? TRUE : FALSE;
}

__declspec(dllexport) BOOL WindDWriteDrawString(
    void* handle,
    const wchar_t* text,
    int x,
    int y,
    int fontSize,
    uint32_t rgba,
    BOOL useSymbol
) {
    auto* renderer = static_cast<Renderer*>(handle);
    if (!renderer) {
        return FALSE;
    }
    return renderer->DrawString(text, x, y, fontSize, rgba, useSymbol != FALSE) ? TRUE : FALSE;
}

__declspec(dllexport) BOOL WindDWriteEndDraw(void* handle) {
    auto* renderer = static_cast<Renderer*>(handle);
    if (!renderer) {
        return FALSE;
    }
    return renderer->EndDraw() ? TRUE : FALSE;
}

__declspec(dllexport) BOOL WindDWriteShutdown() {
    return ShutdownSharedResources() ? TRUE : FALSE;
}

} // extern "C"
