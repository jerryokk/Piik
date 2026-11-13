#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include "capture_control.h"
#include "capture_error.h"
#include "capture_input.h"

#include <algorithm>
#include <array>
#include <iomanip>
#include <sstream>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

// Small shared I/O and formatting helpers used by several capture seams.
namespace piik::capture::windows {

inline std::string LuidString(const LUID& luid) {
  std::ostringstream output;
  output << "0x" << std::hex << std::setfill('0') << std::setw(8)
         << static_cast<UINT32>(luid.HighPart) << ":0x" << std::setw(8)
         << luid.LowPart;
  return output.str();
}

inline std::string JSONString(const std::string& value) {
  std::string output = "\"";
  for (unsigned char character : value) {
    if (character == '\\' || character == '"') {
      output.push_back('\\');
      output.push_back(static_cast<char>(character));
    } else if (character >= 0x20 && character <= 0x7e) {
      output.push_back(static_cast<char>(character));
    } else {
      output.push_back('?');
    }
  }
  output.push_back('"');
  return output;
}

class UniqueHandle final {
 public:
  UniqueHandle() = default;
  explicit UniqueHandle(HANDLE value) : value_(value) {}
  UniqueHandle(const UniqueHandle&) = delete;
  UniqueHandle& operator=(const UniqueHandle&) = delete;
  UniqueHandle(UniqueHandle&& other) noexcept
      : value_(std::exchange(other.value_, nullptr)) {}
  ~UniqueHandle() {
    if (value_ != nullptr && value_ != INVALID_HANDLE_VALUE) CloseHandle(value_);
  }
  HANDLE get() const { return value_; }

 private:
  HANDLE value_ = nullptr;
};

class InputReader final {
 public:
  std::vector<piik::capture::InputEnvelope> Read(bool blocking = false) {
    HANDLE input = GetStdHandle(STD_INPUT_HANDLE);
    DWORD available = 0;
    if (input == INVALID_HANDLE_VALUE || input == nullptr) return {};
    if (!blocking && !PeekNamedPipe(input, nullptr, 0, nullptr, &available, nullptr)) {
      if (GetLastError() == ERROR_BROKEN_PIPE) return End();
      Check(HRESULT_FROM_WIN32(GetLastError()), "capture-control-peek");
    }
    if (!blocking && available == 0) return {};
    std::array<char, 32 * 1024> bytes{};
    DWORD read = 0;
    if (!ReadFile(input, bytes.data(),
                  blocking ? static_cast<DWORD>(bytes.size()) : std::min<DWORD>(available, static_cast<DWORD>(bytes.size())),
                  &read, nullptr)) {
      if (GetLastError() == ERROR_BROKEN_PIPE) return End();
      Check(HRESULT_FROM_WIN32(GetLastError()), "capture-control-read");
    }
    return envelopes_.Feed(std::string_view(bytes.data(), read));
  }

 private:
  std::vector<piik::capture::InputEnvelope> End() {
    if (!envelopes_.Empty()) Fail("capture-input-eof", "native input ended within an envelope");
    piik::capture::InputEnvelope stop;
    stop.kind = 7;
    stop.data = {'Q'};
    return {std::move(stop)};
  }
  piik::capture::InputEnvelopes envelopes_;
};

class ControlReader final {
 public:
  std::vector<piik::capture::CaptureControl> Read() {
    std::vector<piik::capture::CaptureControl> controls;
    for (const auto& envelope : input_.Read()) controls.push_back(envelope.Control());
    return controls;
  }

 private:
  InputReader input_;
};

}  // namespace piik::capture::windows
