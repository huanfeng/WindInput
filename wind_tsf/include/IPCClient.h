#pragma once

#include "Globals.h"
#include <string>
#include <functional>

// Response types from Go Service
enum class ResponseType
{
    Ack,
    InsertText,
    UpdateComposition,
    ClearComposition,
    ModeChanged,
    Unknown
};

// Response from Go Service
struct ServiceResponse
{
    ResponseType type;
    std::wstring text;      // For InsertText
    std::wstring composition; // For UpdateComposition
    int caretPos;           // For UpdateComposition
    BOOL chineseMode;       // For ModeChanged
    std::wstring error;
};

// Callback for receiving responses
using ResponseCallback = std::function<void(const ServiceResponse&)>;

class CIPCClient
{
public:
    CIPCClient();
    ~CIPCClient();

    // Connect to named pipe server
    BOOL Connect();

    // Disconnect
    void Disconnect();

    // Send key event to Go Service
    BOOL SendKeyEvent(const std::wstring& key, int keyCode, int modifiers = 0);

    // Send caret position update to Go Service
    BOOL SendCaretUpdate(int x, int y, int height);

    // Send focus lost notification
    BOOL SendFocusLost();

    // Send toggle mode request
    BOOL SendToggleMode();

    // Check if connected
    BOOL IsConnected() const { return _hPipe != INVALID_HANDLE_VALUE; }

    // Receive response from service (call this after sending)
    BOOL ReceiveResponse(ServiceResponse& response);

private:
    HANDLE _hPipe;
    BOOL _serviceStartAttempted;

    // Start the Go service if not running
    BOOL _StartService();

    // Send message (length-prefixed JSON)
    BOOL _SendMessage(const std::wstring& message);

    // Receive message (length-prefixed JSON)
    BOOL _ReceiveMessage(std::wstring& message);

    // Parse response JSON
    BOOL _ParseResponse(const std::wstring& json, ServiceResponse& response);
};
