#include "IPCClient.h"
#include <sstream>
#include <cstdarg>
#include <cstring>

#pragma comment(lib, "advapi32.lib")

// Static member initialization
IPCLogLevel CIPCClient::s_logLevel = IPCLogLevel::Info;

CIPCClient::CIPCClient()
    : _hPipe(INVALID_HANDLE_VALUE)
    , _hEvent(NULL)
    , _serviceStartAttempted(FALSE)
    , _circuitState(CircuitState::Closed)
    , _consecutiveFailures(0)
    , _lastFailureTime(0)
{
    // Create event for overlapped I/O
    _hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (_hEvent == NULL)
    {
        _LogError(L"Failed to create overlapped event: %d", GetLastError());
    }
}

CIPCClient::~CIPCClient()
{
    Disconnect();
    if (_hEvent != NULL)
    {
        CloseHandle(_hEvent);
        _hEvent = NULL;
    }
}

// ============================================================================
// Logging helpers
// ============================================================================

void CIPCClient::_Log(IPCLogLevel level, const wchar_t* format, ...)
{
    if (static_cast<int>(level) > static_cast<int>(s_logLevel))
        return;

    wchar_t buffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(buffer, _countof(buffer), _TRUNCATE, format, args);
    va_end(args);

    OutputDebugStringW(buffer);
}

void CIPCClient::_LogError(const wchar_t* format, ...)
{
    if (s_logLevel < IPCLogLevel::Error)
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    wchar_t buffer[1100];
    _snwprintf_s(buffer, _countof(buffer), _TRUNCATE, L"[WindInput][ERROR] %s\n", msgBuffer);
    OutputDebugStringW(buffer);
}

void CIPCClient::_LogInfo(const wchar_t* format, ...)
{
    if (s_logLevel < IPCLogLevel::Info)
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    wchar_t buffer[1100];
    _snwprintf_s(buffer, _countof(buffer), _TRUNCATE, L"[WindInput] %s\n", msgBuffer);
    OutputDebugStringW(buffer);
}

void CIPCClient::_LogDebug(const wchar_t* format, ...)
{
    if (s_logLevel < IPCLogLevel::Debug)
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    wchar_t buffer[1100];
    _snwprintf_s(buffer, _countof(buffer), _TRUNCATE, L"[WindInput][DBG] %s\n", msgBuffer);
    OutputDebugStringW(buffer);
}

// ============================================================================
// Encoding helpers
// ============================================================================

std::wstring CIPCClient::_Utf8ToWide(const char* utf8, size_t length)
{
    if (length == 0) return L"";

    int wideSize = MultiByteToWideChar(CP_UTF8, 0, utf8, (int)length, nullptr, 0);
    if (wideSize <= 0) return L"";

    std::wstring result(wideSize, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8, (int)length, &result[0], wideSize);
    return result;
}

std::string CIPCClient::_WideToUtf8(const std::wstring& wide)
{
    if (wide.empty()) return "";

    int utf8Size = WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), (int)wide.length(), nullptr, 0, nullptr, nullptr);
    if (utf8Size <= 0) return "";

    std::string result(utf8Size, '\0');
    WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), (int)wide.length(), &result[0], utf8Size, nullptr, nullptr);
    return result;
}

// ============================================================================
// Circuit Breaker
// ============================================================================

void CIPCClient::_RecordSuccess()
{
    _consecutiveFailures = 0;
    if (_circuitState != CircuitState::Closed)
    {
        _LogInfo(L"Circuit breaker closed (service recovered)");
        _circuitState = CircuitState::Closed;
    }
}

void CIPCClient::_RecordFailure()
{
    _consecutiveFailures++;
    _lastFailureTime = GetTickCount();

    if (_consecutiveFailures >= IPCConfig::MAX_CONSECUTIVE_FAILURES)
    {
        if (_circuitState != CircuitState::Open)
        {
            _LogError(L"Circuit breaker OPEN after %d consecutive failures", _consecutiveFailures);
            _circuitState = CircuitState::Open;
        }
    }
}

BOOL CIPCClient::_ShouldAttemptOperation()
{
    if (_circuitState == CircuitState::Closed)
    {
        return TRUE;
    }

    if (_circuitState == CircuitState::Open)
    {
        DWORD elapsed = GetTickCount() - _lastFailureTime;
        if (elapsed >= IPCConfig::CIRCUIT_RESET_INTERVAL_MS)
        {
            _LogInfo(L"Circuit breaker half-open, attempting reconnection...");
            _circuitState = CircuitState::HalfOpen;
            return TRUE;
        }
        return FALSE;
    }

    return TRUE;
}

void CIPCClient::ResetCircuitBreaker()
{
    _consecutiveFailures = 0;
    _circuitState = CircuitState::Closed;
    _LogInfo(L"Circuit breaker manually reset");
}

BOOL CIPCClient::IsServiceAvailable()
{
    return _ShouldAttemptOperation() && (IsConnected() || Connect());
}

// ============================================================================
// Overlapped I/O helpers
// ============================================================================

BOOL CIPCClient::_WriteWithTimeout(const void* data, DWORD size, DWORD timeoutMs)
{
    if (_hPipe == INVALID_HANDLE_VALUE || _hEvent == NULL)
        return FALSE;

    OVERLAPPED overlapped = {};
    overlapped.hEvent = _hEvent;
    ResetEvent(_hEvent);

    DWORD bytesWritten = 0;
    BOOL result = WriteFile(_hPipe, data, size, &bytesWritten, &overlapped);

    if (result)
    {
        return bytesWritten == size;
    }

    DWORD error = GetLastError();
    if (error != ERROR_IO_PENDING)
    {
        _LogError(L"WriteFile failed immediately: %d", error);
        return FALSE;
    }

    DWORD waitResult = WaitForSingleObject(_hEvent, timeoutMs);

    if (waitResult == WAIT_TIMEOUT)
    {
        _LogError(L"Write operation timed out after %dms", timeoutMs);
        CancelIo(_hPipe);
        return FALSE;
    }

    if (waitResult != WAIT_OBJECT_0)
    {
        _LogError(L"WaitForSingleObject failed: %d", GetLastError());
        CancelIo(_hPipe);
        return FALSE;
    }

    if (!GetOverlappedResult(_hPipe, &overlapped, &bytesWritten, FALSE))
    {
        _LogError(L"GetOverlappedResult failed: %d", GetLastError());
        return FALSE;
    }

    return bytesWritten == size;
}

BOOL CIPCClient::_ReadWithTimeout(void* buffer, DWORD size, DWORD* bytesRead, DWORD timeoutMs)
{
    if (_hPipe == INVALID_HANDLE_VALUE || _hEvent == NULL)
        return FALSE;

    OVERLAPPED overlapped = {};
    overlapped.hEvent = _hEvent;
    ResetEvent(_hEvent);

    *bytesRead = 0;
    BOOL result = ReadFile(_hPipe, buffer, size, bytesRead, &overlapped);

    if (result)
    {
        return TRUE;
    }

    DWORD error = GetLastError();
    if (error != ERROR_IO_PENDING)
    {
        _LogDebug(L"ReadFile failed immediately: %d", error);
        return FALSE;
    }

    DWORD waitResult = WaitForSingleObject(_hEvent, timeoutMs);

    if (waitResult == WAIT_TIMEOUT)
    {
        _LogError(L"Read operation timed out after %dms", timeoutMs);
        CancelIo(_hPipe);
        return FALSE;
    }

    if (waitResult != WAIT_OBJECT_0)
    {
        _LogError(L"WaitForSingleObject failed: %d", GetLastError());
        CancelIo(_hPipe);
        return FALSE;
    }

    if (!GetOverlappedResult(_hPipe, &overlapped, bytesRead, FALSE))
    {
        _LogError(L"GetOverlappedResult failed: %d", GetLastError());
        return FALSE;
    }

    return TRUE;
}

// ============================================================================
// Service management
// ============================================================================

BOOL CIPCClient::_StartService()
{
    _LogInfo(L"Attempting to start Go service...");

    WCHAR dllPath[MAX_PATH];

    if (GetModuleFileNameW(g_hInstance, dllPath, MAX_PATH) == 0)
    {
        _LogError(L"Failed to get module path");
        return FALSE;
    }

    WCHAR* lastSlash = wcsrchr(dllPath, L'\\');
    if (lastSlash)
    {
        wcscpy_s(lastSlash + 1, MAX_PATH - (lastSlash - dllPath + 1), L"wind_input.exe");
    }

    _LogDebug(L"Starting service: %s", dllPath);

    STARTUPINFOW si = { sizeof(STARTUPINFOW) };
    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;

    PROCESS_INFORMATION pi = {};

    if (!CreateProcessW(
        dllPath,
        nullptr,
        nullptr,
        nullptr,
        FALSE,
        CREATE_NEW_CONSOLE,
        nullptr,
        nullptr,
        &si,
        &pi))
    {
        _LogError(L"Failed to start service: error=%d", GetLastError());
        return FALSE;
    }

    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);

    _LogInfo(L"Service started successfully");
    return TRUE;
}

// ============================================================================
// Connection management
// ============================================================================

BOOL CIPCClient::Connect()
{
    if (_hPipe != INVALID_HANDLE_VALUE)
    {
        _LogDebug(L"Already connected to pipe");
        return TRUE;
    }

    if (!_ShouldAttemptOperation())
    {
        _LogDebug(L"Circuit breaker open, skipping connection attempt");
        return FALSE;
    }

    _LogDebug(L"Connecting to Go Service (binary protocol)...");

    for (int attempt = 0; attempt < 3; attempt++)
    {
        if (!WaitNamedPipeW(PIPE_NAME, IPCConfig::CONNECT_TIMEOUT_MS))
        {
            DWORD error = GetLastError();
            if (error == ERROR_FILE_NOT_FOUND)
            {
                if (!_serviceStartAttempted)
                {
                    _serviceStartAttempted = TRUE;
                    if (_StartService())
                    {
                        Sleep(500);
                        continue;
                    }
                }
                _LogDebug(L"Pipe not found, service not available");
                break;
            }
            else if (error == ERROR_SEM_TIMEOUT)
            {
                _LogDebug(L"WaitNamedPipe timed out, attempt %d", attempt + 1);
                continue;
            }
        }

        _hPipe = CreateFileW(
            PIPE_NAME,
            GENERIC_READ | GENERIC_WRITE,
            0,
            nullptr,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            nullptr);

        if (_hPipe != INVALID_HANDLE_VALUE)
        {
            DWORD mode = PIPE_READMODE_BYTE;
            SetNamedPipeHandleState(_hPipe, &mode, nullptr, nullptr);

            _LogInfo(L"Connected to Go Service (binary protocol v%d.%d)",
                     PROTOCOL_VERSION >> 12, PROTOCOL_VERSION & 0xFFF);
            _RecordSuccess();
            return TRUE;
        }

        DWORD error = GetLastError();
        _LogDebug(L"Connection attempt %d failed: error=%d", attempt + 1, error);

        if (error == ERROR_PIPE_BUSY)
        {
            Sleep(50);
            continue;
        }

        break;
    }

    _LogError(L"Failed to connect to Go Service");
    _RecordFailure();
    return FALSE;
}

void CIPCClient::Disconnect()
{
    if (_hPipe != INVALID_HANDLE_VALUE)
    {
        CancelIo(_hPipe);
        CloseHandle(_hPipe);
        _hPipe = INVALID_HANDLE_VALUE;
        _LogDebug(L"Disconnected from Go Service");
    }
}

// ============================================================================
// Binary message sending
// ============================================================================

BOOL CIPCClient::_SendBinaryMessage(uint16_t command, const void* payload, uint32_t payloadSize, bool async)
{
    // Build header
    IpcHeader header;
    header.version = async ? (PROTOCOL_VERSION | ASYNC_FLAG) : PROTOCOL_VERSION;
    header.command = command;
    header.length = payloadSize;

    // Write header
    if (!_WriteWithTimeout(&header, sizeof(header), IPCConfig::WRITE_TIMEOUT_MS))
    {
        _LogError(L"Failed to write message header");
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    // Write payload if any
    if (payloadSize > 0 && payload != nullptr)
    {
        if (!_WriteWithTimeout(payload, payloadSize, IPCConfig::WRITE_TIMEOUT_MS))
        {
            _LogError(L"Failed to write message payload");
            _RecordFailure();
            Disconnect();
            return FALSE;
        }
    }

    _RecordSuccess();
    return TRUE;
}

// ============================================================================
// Message sending
// ============================================================================

BOOL CIPCClient::SendKeyEvent(uint32_t keyCode, uint32_t scanCode, uint32_t modifiers, uint8_t eventType)
{
    if (!_ShouldAttemptOperation())
    {
        _LogDebug(L"Circuit open, skipping key event");
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    KeyPayload payload;
    payload.keyCode = keyCode;
    payload.scanCode = scanCode;
    payload.modifiers = modifiers;
    payload.eventType = eventType;
    memset(payload.reserved, 0, sizeof(payload.reserved));

    _LogDebug(L"Sending key event: keyCode=0x%X, mods=0x%X, type=%d", keyCode, modifiers, eventType);

    return _SendBinaryMessage(CMD_KEY_EVENT, &payload, sizeof(payload));
}

BOOL CIPCClient::SendCaretUpdate(int x, int y, int height)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    CaretPayload payload;
    payload.x = x;
    payload.y = y;
    payload.height = height;

    _LogDebug(L"Sending caret update (async): x=%d, y=%d, h=%d", x, y, height);

    // Send async - no response needed for caret updates
    return _SendBinaryMessage(CMD_CARET_UPDATE, &payload, sizeof(payload), true /* async */);
}

BOOL CIPCClient::SendFocusLost()
{
    if (!IsConnected())
    {
        return FALSE;
    }

    _LogDebug(L"Sending focus_lost (async)");
    // Send async - no response needed for focus lost
    return _SendBinaryMessage(CMD_FOCUS_LOST, nullptr, 0, true /* async */);
}

BOOL CIPCClient::SendFocusGained(int caretX, int caretY, int caretHeight)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogDebug(L"Sending focus_gained with caret: x=%d, y=%d, h=%d", caretX, caretY, caretHeight);

    CaretPayload payload;
    payload.x = caretX;
    payload.y = caretY;
    payload.height = caretHeight;

    return _SendBinaryMessage(CMD_FOCUS_GAINED, &payload, sizeof(payload));
}

BOOL CIPCClient::SendIMEDeactivated()
{
    if (!IsConnected())
    {
        return FALSE;
    }

    _LogInfo(L"Sending ime_deactivated (async)");
    // Send async - no response needed for IME deactivated
    return _SendBinaryMessage(CMD_IME_DEACTIVATED, nullptr, 0, true /* async */);
}

BOOL CIPCClient::SendIMEActivated()
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogInfo(L"Sending ime_activated");
    return _SendBinaryMessage(CMD_IME_ACTIVATED, nullptr, 0);
}

// ============================================================================
// Message receiving
// ============================================================================

BOOL CIPCClient::_ReceiveBinaryMessage(IpcHeader& header, std::vector<uint8_t>& payload)
{
    // Read header
    DWORD bytesRead;
    if (!_ReadWithTimeout(&header, sizeof(header), &bytesRead, IPCConfig::READ_TIMEOUT_MS))
    {
        _LogError(L"Failed to read message header");
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    if (bytesRead != sizeof(header))
    {
        _LogError(L"Incomplete header read: got %d bytes", bytesRead);
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    // Validate header
    if ((header.version >> 12) != (PROTOCOL_VERSION >> 12))
    {
        _LogError(L"Protocol version mismatch: got 0x%04X, expected 0x%04X", header.version, PROTOCOL_VERSION);
        _RecordFailure();
        return FALSE;
    }

    if (header.length > IPCConfig::MAX_MESSAGE_SIZE)
    {
        _LogError(L"Invalid payload length: %d", header.length);
        _RecordFailure();
        return FALSE;
    }

    _LogDebug(L"Received header: cmd=0x%04X, len=%d", header.command, header.length);

    // Read payload if any
    if (header.length > 0)
    {
        payload.resize(header.length);
        DWORD totalRead = 0;

        while (totalRead < header.length)
        {
            DWORD chunkRead;
            if (!_ReadWithTimeout(payload.data() + totalRead, header.length - totalRead, &chunkRead, IPCConfig::READ_TIMEOUT_MS))
            {
                _LogError(L"Failed to read payload");
                _RecordFailure();
                Disconnect();
                return FALSE;
            }

            if (chunkRead == 0)
            {
                _LogError(L"Incomplete payload read: expected %d, got %d", header.length, totalRead);
                _RecordFailure();
                Disconnect();
                return FALSE;
            }

            totalRead += chunkRead;
        }
    }
    else
    {
        payload.clear();
    }

    _RecordSuccess();
    return TRUE;
}

BOOL CIPCClient::ReceiveResponse(ServiceResponse& response)
{
    IpcHeader header;
    std::vector<uint8_t> payload;

    if (!_ReceiveBinaryMessage(header, payload))
    {
        return FALSE;
    }

    return _ParseResponse(header, payload, response);
}

BOOL CIPCClient::_ParseResponse(const IpcHeader& header, const std::vector<uint8_t>& payload, ServiceResponse& response)
{
    // Reset response
    response = ServiceResponse();

    switch (header.command)
    {
    case CMD_ACK:
        response.type = ResponseType::Ack;
        _LogDebug(L"Response: Ack");
        break;

    case CMD_PASS_THROUGH:
        response.type = ResponseType::PassThrough;
        _LogDebug(L"Response: PassThrough (key not handled)");
        break;

    case CMD_CONSUMED:
        response.type = ResponseType::Consumed;
        _LogDebug(L"Response: Consumed");
        break;

    case CMD_CLEAR_COMPOSITION:
        response.type = ResponseType::ClearComposition;
        _LogDebug(L"Response: ClearComposition");
        break;

    case CMD_COMMIT_TEXT:
        {
            response.type = ResponseType::CommitText;

            if (payload.size() < sizeof(CommitTextHeader))
            {
                _LogError(L"CommitText payload too short");
                return FALSE;
            }

            const CommitTextHeader* commitHeader = reinterpret_cast<const CommitTextHeader*>(payload.data());
            response.modeChanged = (commitHeader->flags & COMMIT_FLAG_MODE_CHANGED) != 0;
            response.chineseMode = (commitHeader->flags & COMMIT_FLAG_CHINESE_MODE) != 0;

            // Extract text
            if (commitHeader->textLength > 0)
            {
                size_t textOffset = sizeof(CommitTextHeader);
                if (textOffset + commitHeader->textLength <= payload.size())
                {
                    response.text = _Utf8ToWide(
                        reinterpret_cast<const char*>(payload.data() + textOffset),
                        commitHeader->textLength);
                }
            }

            // Extract new composition
            if ((commitHeader->flags & COMMIT_FLAG_HAS_NEW_COMPOSITION) && commitHeader->compositionLength > 0)
            {
                size_t compOffset = sizeof(CommitTextHeader) + commitHeader->textLength;
                if (compOffset + commitHeader->compositionLength <= payload.size())
                {
                    response.newComposition = _Utf8ToWide(
                        reinterpret_cast<const char*>(payload.data() + compOffset),
                        commitHeader->compositionLength);
                }
            }

            _LogDebug(L"Response: CommitText text='%s', modeChanged=%d, newComp='%s'",
                      response.text.c_str(), response.modeChanged, response.newComposition.c_str());
        }
        break;

    case CMD_UPDATE_COMPOSITION:
        {
            response.type = ResponseType::UpdateComposition;

            if (payload.size() < sizeof(CompositionHeader))
            {
                _LogError(L"UpdateComposition payload too short");
                return FALSE;
            }

            const CompositionHeader* compHeader = reinterpret_cast<const CompositionHeader*>(payload.data());
            response.caretPos = compHeader->caretPos;

            // Extract composition text
            size_t textLength = payload.size() - sizeof(CompositionHeader);
            if (textLength > 0)
            {
                response.composition = _Utf8ToWide(
                    reinterpret_cast<const char*>(payload.data() + sizeof(CompositionHeader)),
                    textLength);
            }

            _LogDebug(L"Response: UpdateComposition text='%s', caret=%d",
                      response.composition.c_str(), response.caretPos);
        }
        break;

    case CMD_MODE_CHANGED:
        {
            response.type = ResponseType::ModeChanged;

            if (payload.size() >= 4)
            {
                response.statusFlags = *reinterpret_cast<const uint32_t*>(payload.data());
                response.chineseMode = (response.statusFlags & STATUS_CHINESE_MODE) != 0;
            }

            _LogDebug(L"Response: ModeChanged chineseMode=%d", response.chineseMode);
        }
        break;

    case CMD_STATUS_UPDATE:
        {
            response.type = ResponseType::StatusUpdate;

            if (payload.size() < sizeof(StatusHeader))
            {
                _LogError(L"StatusUpdate payload too short");
                return FALSE;
            }

            const StatusHeader* statusHeader = reinterpret_cast<const StatusHeader*>(payload.data());
            response.statusFlags = statusHeader->flags;
            response.chineseMode = (statusHeader->flags & STATUS_CHINESE_MODE) != 0;

            // Extract hotkeys
            size_t hotkeysOffset = sizeof(StatusHeader);
            uint32_t totalHotkeys = statusHeader->keyDownCount + statusHeader->keyUpCount;

            if (payload.size() >= hotkeysOffset + totalHotkeys * sizeof(uint32_t))
            {
                const uint32_t* hotkeys = reinterpret_cast<const uint32_t*>(payload.data() + hotkeysOffset);

                for (uint32_t i = 0; i < statusHeader->keyDownCount; i++)
                {
                    response.keyDownHotkeys.push_back(hotkeys[i]);
                }

                for (uint32_t i = 0; i < statusHeader->keyUpCount; i++)
                {
                    response.keyUpHotkeys.push_back(hotkeys[statusHeader->keyDownCount + i]);
                }
            }

            _LogDebug(L"Response: StatusUpdate mode=%d, width=%d, punct=%d, toolbar=%d, keyDown=%d, keyUp=%d",
                      response.IsChineseMode(), response.IsFullWidth(), response.IsChinesePunct(),
                      response.IsToolbarVisible(), (int)response.keyDownHotkeys.size(), (int)response.keyUpHotkeys.size());
        }
        break;

    case CMD_SYNC_HOTKEYS:
        {
            response.type = ResponseType::StatusUpdate; // Treat as status update

            if (payload.size() < sizeof(StatusHeader))
            {
                _LogError(L"SyncHotkeys payload too short");
                return FALSE;
            }

            const StatusHeader* syncHeader = reinterpret_cast<const StatusHeader*>(payload.data());

            // Extract hotkeys
            size_t hotkeysOffset = sizeof(StatusHeader);
            uint32_t totalHotkeys = syncHeader->keyDownCount + syncHeader->keyUpCount;

            if (payload.size() >= hotkeysOffset + totalHotkeys * sizeof(uint32_t))
            {
                const uint32_t* hotkeys = reinterpret_cast<const uint32_t*>(payload.data() + hotkeysOffset);

                for (uint32_t i = 0; i < syncHeader->keyDownCount; i++)
                {
                    response.keyDownHotkeys.push_back(hotkeys[i]);
                }

                for (uint32_t i = 0; i < syncHeader->keyUpCount; i++)
                {
                    response.keyUpHotkeys.push_back(hotkeys[syncHeader->keyDownCount + i]);
                }
            }

            _LogInfo(L"Response: SyncHotkeys keyDown=%d, keyUp=%d",
                     (int)response.keyDownHotkeys.size(), (int)response.keyUpHotkeys.size());
        }
        break;

    default:
        _LogError(L"Unknown response command: 0x%04X", header.command);
        response.type = ResponseType::Error;
        return FALSE;
    }

    return TRUE;
}

// ============================================================================
// Async and Batch support
// ============================================================================

BOOL CIPCClient::SendAsync(uint16_t command, const void* payload, uint32_t size)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    return _SendBinaryMessage(command, payload, size, true /* async */);
}

BOOL CIPCClient::SendSync(uint16_t command, const void* payload, uint32_t size, ServiceResponse& response)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    if (!_SendBinaryMessage(command, payload, size, false /* sync */))
    {
        return FALSE;
    }

    return ReceiveResponse(response);
}

void CIPCClient::BeginBatch()
{
    _batchBuffer.clear();
    _batchNeedResponse.clear();
    _batchCount = 0;

    // Reserve space for BatchHeader (will be filled in SendBatch)
    _batchBuffer.resize(sizeof(BatchHeader));
}

void CIPCClient::AddBatchEvent(uint16_t command, const void* payload, uint32_t size, bool needResponse)
{
    // Build event header
    IpcHeader eventHeader;
    eventHeader.version = needResponse ? PROTOCOL_VERSION : (PROTOCOL_VERSION | ASYNC_FLAG);
    eventHeader.command = command;
    eventHeader.length = size;

    // Append header to buffer
    size_t offset = _batchBuffer.size();
    _batchBuffer.resize(offset + sizeof(IpcHeader) + size);

    memcpy(_batchBuffer.data() + offset, &eventHeader, sizeof(IpcHeader));

    // Append payload if any
    if (size > 0 && payload != nullptr)
    {
        memcpy(_batchBuffer.data() + offset + sizeof(IpcHeader), payload, size);
    }

    _batchNeedResponse.push_back(needResponse);
    _batchCount++;
}

BOOL CIPCClient::SendBatch(std::vector<ServiceResponse>& responses)
{
    if (_batchCount == 0)
    {
        return TRUE;
    }

    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    // Fill in BatchHeader at the beginning of buffer
    BatchHeader* batchHeader = reinterpret_cast<BatchHeader*>(_batchBuffer.data());
    batchHeader->eventCount = _batchCount;
    batchHeader->reserved = 0;

    _LogDebug(L"Sending batch with %d events, size=%zu", _batchCount, _batchBuffer.size());

    // Send the batch message
    if (!_SendBinaryMessage(CMD_BATCH_EVENTS, _batchBuffer.data(), static_cast<uint32_t>(_batchBuffer.size()), false))
    {
        return FALSE;
    }

    // Count how many sync responses we expect
    int syncCount = 0;
    for (bool needResp : _batchNeedResponse)
    {
        if (needResp)
        {
            syncCount++;
        }
    }

    // Receive batch response if there are sync events
    if (syncCount > 0)
    {
        return ReceiveBatchResponse(responses, syncCount);
    }

    return TRUE;
}

BOOL CIPCClient::ReceiveBatchResponse(std::vector<ServiceResponse>& responses, int expectedCount)
{
    // Read batch response header
    IpcHeader header;
    std::vector<uint8_t> payload;

    if (!_ReceiveBinaryMessage(header, payload))
    {
        return FALSE;
    }

    if (header.command != CMD_BATCH_RESPONSE)
    {
        _LogError(L"Expected batch response, got command 0x%04X", header.command);
        return FALSE;
    }

    if (payload.size() < sizeof(BatchHeader))
    {
        _LogError(L"Batch response payload too short: %zu bytes", payload.size());
        return FALSE;
    }

    // Parse batch response header
    const BatchHeader* batchHeader = reinterpret_cast<const BatchHeader*>(payload.data());
    uint16_t responseCount = batchHeader->eventCount; // eventCount holds response count in batch response

    if (responseCount != expectedCount)
    {
        _LogError(L"Batch response count mismatch: expected %d, got %d", expectedCount, responseCount);
        // Continue processing what we have
    }

    _LogDebug(L"Received batch response with %d responses", responseCount);

    // Parse individual responses
    size_t offset = sizeof(BatchHeader);
    responses.clear();
    responses.reserve(responseCount);

    for (uint16_t i = 0; i < responseCount && offset < payload.size(); i++)
    {
        // Check if we have enough data for a header
        if (offset + sizeof(IpcHeader) > payload.size())
        {
            _LogError(L"Batch response %d: incomplete header at offset %zu", i, offset);
            break;
        }

        // Parse response header
        const IpcHeader* respHeader = reinterpret_cast<const IpcHeader*>(payload.data() + offset);
        offset += sizeof(IpcHeader);

        // Check if we have enough data for the payload
        if (offset + respHeader->length > payload.size())
        {
            _LogError(L"Batch response %d: incomplete payload at offset %zu", i, offset);
            break;
        }

        // Extract payload
        std::vector<uint8_t> respPayload;
        if (respHeader->length > 0)
        {
            respPayload.assign(payload.begin() + offset, payload.begin() + offset + respHeader->length);
            offset += respHeader->length;
        }

        // Parse response
        ServiceResponse response;
        if (_ParseResponse(*respHeader, respPayload, response))
        {
            responses.push_back(std::move(response));
        }
        else
        {
            _LogError(L"Failed to parse batch response %d", i);
            ServiceResponse errorResp;
            errorResp.type = ResponseType::Error;
            responses.push_back(std::move(errorResp));
        }
    }

    return TRUE;
}
