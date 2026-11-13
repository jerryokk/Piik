#define NOMINMAX
#include <windows.h>
#include <dwmapi.h>
#include <mmdeviceapi.h>
#include <functiondiscoverykeys_devpkey.h>
#include <wrl/client.h>

#include "capture_geometry.h"
#include "capture_preview.h"
#include "capture_target.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <iostream>
#include <limits>
#include <string>
#include <utility>
#include <vector>

namespace piik::capture {
namespace {

using Microsoft::WRL::ComPtr;

constexpr size_t kMaxSources = 100;

SIZE ReadDisplayPresentation(const wchar_t* device_name, SIZE captured) {
  UINT32 path_count = 0;
  UINT32 mode_count = 0;
  constexpr UINT32 flags = QDC_ONLY_ACTIVE_PATHS;
  if (GetDisplayConfigBufferSizes(flags, &path_count, &mode_count) !=
      ERROR_SUCCESS) return captured;
  std::vector<DISPLAYCONFIG_PATH_INFO> paths(path_count);
  std::vector<DISPLAYCONFIG_MODE_INFO> modes(mode_count);
  if (QueryDisplayConfig(flags, &path_count, paths.data(), &mode_count,
                         modes.data(), nullptr) != ERROR_SUCCESS) return captured;

  const DISPLAYCONFIG_PATH_INFO* matched = nullptr;
  for (UINT32 index = 0; index < path_count; ++index) {
    const auto& path = paths[index];
    DISPLAYCONFIG_SOURCE_DEVICE_NAME name{};
    name.header = {DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, sizeof(name),
                   path.sourceInfo.adapterId, path.sourceInfo.id};
    if (DisplayConfigGetDeviceInfo(&name.header) != ERROR_SUCCESS) return captured;
    if (wcscmp(name.viewGdiDeviceName, device_name) != 0) continue;
    // A cloned desktop has no single physical presentation to reproduce.
    if (matched != nullptr) return captured;
    matched = &path;
  }
  if (matched == nullptr ||
      !matched->targetInfo.targetAvailable ||
      (matched->flags & DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE) != 0 ||
      matched->sourceInfo.modeInfoIdx >= mode_count ||
      matched->targetInfo.modeInfoIdx >= mode_count) return captured;
  const auto& source = modes[matched->sourceInfo.modeInfoIdx];
  const auto& target = modes[matched->targetInfo.modeInfoIdx];
  const auto same_adapter = [](LUID first, LUID second) {
    return first.HighPart == second.HighPart && first.LowPart == second.LowPart;
  };
  if (source.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE ||
      target.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_TARGET ||
      source.id != matched->sourceInfo.id || target.id != matched->targetInfo.id ||
      !same_adapter(source.adapterId, matched->sourceInfo.adapterId) ||
      !same_adapter(target.adapterId, matched->targetInfo.adapterId)) return captured;
  const auto active = target.targetMode.targetVideoSignalInfo.activeSize;
  return DisplayedFrameSize(
      captured,
      {static_cast<LONG>(source.sourceMode.width),
       static_cast<LONG>(source.sourceMode.height)},
      {static_cast<LONG>(active.cx), static_cast<LONG>(active.cy)},
      matched->targetInfo.scaling, matched->targetInfo.rotation);
}

struct SourceTarget {
  TargetKind kind;
  UINT64 source_id;
  DWORD pid;
  UINT64 creation_time;
  std::string title;
  bool primary;
};

HRESULT ReadProcessCreationTime(HANDLE process, UINT64* creation_time) {
  FILETIME created{}, exited{}, kernel{}, user{};
  if (!GetProcessTimes(process, &created, &exited, &kernel, &user)) {
    return HRESULT_FROM_WIN32(GetLastError());
  }
  ULARGE_INTEGER value{};
  value.LowPart = created.dwLowDateTime;
  value.HighPart = created.dwHighDateTime;
  if (value.QuadPart == 0) return HRESULT_FROM_WIN32(ERROR_INVALID_DATA);
  *creation_time = value.QuadPart;
  return S_OK;
}

std::string Utf8(const std::wstring& value) {
  if (value.empty()) return {};
  const int size = WideCharToMultiByte(
      CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
      static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
  if (size <= 0) return {};
  std::string result(static_cast<size_t>(size), '\0');
  if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                          static_cast<int>(value.size()), result.data(), size,
                          nullptr, nullptr) != size) {
    return {};
  }
  return result;
}

std::string JsonString(const std::string& value) {
  static constexpr char hex[] = "0123456789abcdef";
  std::string result = "\"";
  for (const unsigned char character : value) {
    switch (character) {
      case '\\': result += "\\\\"; break;
      case '"': result += "\\\""; break;
      case '\b': result += "\\b"; break;
      case '\f': result += "\\f"; break;
      case '\n': result += "\\n"; break;
      case '\r': result += "\\r"; break;
      case '\t': result += "\\t"; break;
      default:
        if (character < 0x20) {
          result += "\\u00";
          result += hex[character >> 4];
          result += hex[character & 0xf];
        } else {
          result.push_back(static_cast<char>(character));
        }
    }
  }
  result += '"';
  return result;
}

BOOL CALLBACK CollectWindow(HWND window, LPARAM parameter) {
  auto* targets = reinterpret_cast<std::vector<SourceTarget>*>(parameter);
  // FALSE makes EnumWindows report failure; a full list is still usable.
  if (targets->size() >= kMaxSources) return TRUE;
  if (!IsWindowVisible(window)) return TRUE;
  const LONG_PTR extended_style = GetWindowLongPtrW(window, GWL_EXSTYLE);
  // An owned window can still be an independent taskbar application window.
  if (GetWindow(window, GW_OWNER) != nullptr &&
      (extended_style & WS_EX_APPWINDOW) == 0) {
    return TRUE;
  }
  if ((extended_style & WS_EX_TOOLWINDOW) != 0 ||
      (extended_style & WS_EX_NOACTIVATE) != 0) {
    return TRUE;
  }
  BOOL cloaked = FALSE;
  if (SUCCEEDED(DwmGetWindowAttribute(window, DWMWA_CLOAKED, &cloaked,
                                      sizeof(cloaked))) &&
      cloaked != FALSE) {
    return TRUE;
  }
  const int length = GetWindowTextLengthW(window);
  if (length <= 0 || length > 512) return TRUE;
  DWORD pid = 0;
  GetWindowThreadProcessId(window, &pid);
  if (pid == 0 || pid == GetCurrentProcessId()) return TRUE;
  std::wstring title(static_cast<size_t>(length) + 1, L'\0');
  const int copied = GetWindowTextW(window, title.data(), length + 1);
  if (copied <= 0) return TRUE;
  title.resize(static_cast<size_t>(copied));
  std::string utf8 = Utf8(title);
  if (utf8.empty()) return TRUE;
  HANDLE process =
      OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
  if (process == nullptr) return TRUE;
  UINT64 creation_time = 0;
  const HRESULT result = ReadProcessCreationTime(process, &creation_time);
  CloseHandle(process);
  if (SUCCEEDED(result)) {
    targets->push_back({
        TargetKind::window,
        static_cast<UINT64>(reinterpret_cast<UINT_PTR>(window)), pid,
        creation_time, std::move(utf8), false});
  }
  return TRUE;
}

BOOL CALLBACK CollectDisplay(HMONITOR monitor, HDC, LPRECT, LPARAM parameter) {
  auto* targets = reinterpret_cast<std::vector<SourceTarget>*>(parameter);
  if (targets->size() >= kMaxSources) return TRUE;
  MONITORINFOEXW info{};
  info.cbSize = sizeof(info);
  if (!GetMonitorInfoW(monitor, &info)) return TRUE;
  std::string title = Utf8(info.szDevice);
  if (title.empty()) return TRUE;
  targets->push_back({
      TargetKind::display,
      static_cast<UINT64>(reinterpret_cast<UINT_PTR>(monitor)), 0, 0,
      std::move(title), (info.dwFlags & MONITORINFOF_PRIMARY) != 0});
  return TRUE;
}

}  // namespace

int WriteMicrophoneList() {
  if (FAILED(CoInitializeEx(nullptr, COINIT_MULTITHREADED))) return 2;
  const int result = []() {
    ComPtr<IMMDeviceEnumerator> enumerator;
    ComPtr<IMMDeviceCollection> devices;
    if (FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&enumerator))) ||
        FAILED(enumerator->EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE, &devices))) return 2;
    UINT count = 0;
    if (FAILED(devices->GetCount(&count))) return 2;
    std::cout << '[';
    bool first = true;
    for (UINT index = 0; index < count && index < 64; ++index) {
      ComPtr<IMMDevice> device;
      ComPtr<IPropertyStore> properties;
      if (FAILED(devices->Item(index, &device)) || FAILED(device->OpenPropertyStore(STGM_READ, &properties))) continue;
      LPWSTR id = nullptr;
      PROPVARIANT name;
      PropVariantInit(&name);
      if (SUCCEEDED(device->GetId(&id)) && SUCCEEDED(properties->GetValue(PKEY_Device_FriendlyName, &name)) &&
          name.vt == VT_LPWSTR && name.pwszVal != nullptr) {
        if (!first) std::cout << ',';
        first = false;
        std::cout << "{\"id\":" << JsonString(Utf8(id))
                  << ",\"label\":" << JsonString(Utf8(name.pwszVal)) << '}';
      }
      CoTaskMemFree(id);
      PropVariantClear(&name);
    }
    std::cout << ']';
    return std::cout.good() ? 0 : 2;
  }();
  CoUninitialize();
  return result;
}

int WriteSourceList() {
  std::vector<SourceTarget> targets;
  if (!EnumDisplayMonitors(nullptr, nullptr, CollectDisplay,
                           reinterpret_cast<LPARAM>(&targets)) ||
      !EnumWindows(CollectWindow, reinterpret_cast<LPARAM>(&targets))) {
    return 2;
  }
  std::stable_sort(targets.begin(), targets.end(),
                   [](const SourceTarget& left, const SourceTarget& right) {
                     if (left.kind != right.kind) {
                       return left.kind == TargetKind::display;
                     }
                     if (left.primary != right.primary) return left.primary;
                     return left.title < right.title;
                   });
  std::cout << '[';
  for (size_t index = 0; index < targets.size(); ++index) {
    if (index != 0) std::cout << ',';
    const SourceTarget& target = targets[index];
    std::cout << "{\"kind\":\""
              << (target.kind == TargetKind::display ? "display" : "window")
              << "\",\"sourceId\":\"" << target.source_id << '\"';
    if (target.kind == TargetKind::window) {
      std::cout << ",\"pid\":" << target.pid << ",\"creationTime\":\""
                << target.creation_time << '\"';
    }
    std::cout << ",\"title\":" << JsonString(target.title) << '}';
  }
  std::cout << ']';
  return std::cout.good() ? 0 : 2;
}

HRESULT WriteSourcePreview(TargetKind kind, UINT64 source_id, DWORD pid,
                           UINT64 expected_creation_time) {
  HRESULT result = kind == TargetKind::window
                       ? ValidateWindowTarget(source_id, pid,
                                              expected_creation_time)
                       : ValidateDisplayTarget(source_id);
  if (FAILED(result)) return result;
  std::vector<BYTE> bitmap;
  result = CapturePreview(kind, source_id, &bitmap);
  if (FAILED(result)) return result;
  DWORD written = 0;
  HANDLE output = GetStdHandle(STD_OUTPUT_HANDLE);
  if (!WriteFile(output, bitmap.data(), static_cast<DWORD>(bitmap.size()),
                 &written, nullptr) || written != bitmap.size()) {
    return HRESULT_FROM_WIN32(GetLastError());
  }
  return S_OK;
}

HRESULT ValidateProcessTarget(DWORD pid, UINT64 expected_creation_time) {
  if (pid == 0 || expected_creation_time == 0) {
    return HRESULT_FROM_WIN32(ERROR_INVALID_PARAMETER);
  }
  HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
  if (process == nullptr) return HRESULT_FROM_WIN32(GetLastError());
  const HRESULT result = ValidateProcessTarget(process, expected_creation_time);
  CloseHandle(process);
  return result;
}

HRESULT ValidateProcessTarget(HANDLE process, UINT64 expected_creation_time) {
  UINT64 actual_creation_time = 0;
  HRESULT result = ReadProcessCreationTime(process, &actual_creation_time);
  if (SUCCEEDED(result) && actual_creation_time != expected_creation_time) {
    result = HRESULT_FROM_WIN32(ERROR_INVALID_DATA);
  }
  return result;
}

HRESULT ValidateWindowTarget(UINT64 source_id, DWORD pid,
                             UINT64 expected_creation_time) {
  if (source_id == 0 ||
      source_id >
          static_cast<UINT64>(std::numeric_limits<UINT_PTR>::max())) {
    return HRESULT_FROM_WIN32(ERROR_INVALID_PARAMETER);
  }
  HWND window = reinterpret_cast<HWND>(static_cast<UINT_PTR>(source_id));
  DWORD window_pid = 0;
  if (!IsWindow(window) || GetWindowThreadProcessId(window, &window_pid) == 0 ||
      window_pid != pid) {
    return HRESULT_FROM_WIN32(ERROR_INVALID_WINDOW_HANDLE);
  }
  return ValidateProcessTarget(pid, expected_creation_time);
}

SIZE CapturePresentation::Resolve(UINT32 width, UINT32 height, bool key_frame) {
  const SIZE captured{static_cast<LONG>(width), static_cast<LONG>(height)};
  const HWND window = kind_ == TargetKind::window
                          ? reinterpret_cast<HWND>(static_cast<UINT_PTR>(source_id_))
                          : nullptr;
  const HMONITOR monitor = window != nullptr
                               ? MonitorFromWindow(window, MONITOR_DEFAULTTONULL)
                               : reinterpret_cast<HMONITOR>(
                                     static_cast<UINT_PTR>(source_id_));
  // WGC/CCD use physical pixels; fullscreen eligibility must use them too.
  const auto previous_dpi =
      SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
  MONITORINFOEXW info{};
  info.cbSize = sizeof(info);
  bool eligible = previous_dpi != nullptr && GetMonitorInfoW(monitor, &info);
  if (eligible && window != nullptr) {
    RECT client{};
    POINT origin{};
    eligible = !IsIconic(window) && GetClientRect(window, &client) &&
               ClientToScreen(window, &origin);
    OffsetRect(&client, origin.x, origin.y);
    eligible = eligible && EqualRect(&client, &info.rcMonitor);
  }
  if (previous_dpi != nullptr) SetThreadDpiAwarenessContext(previous_dpi);
  if (!eligible) {
    monitor_ = nullptr;
    return captured;
  }
  if (key_frame || monitor != monitor_ || !EqualRect(&info.rcMonitor, &bounds_) ||
      captured.cx != captured_.cx || captured.cy != captured_.cy) {
    presentation_ = ReadDisplayPresentation(info.szDevice, captured);
    monitor_ = monitor;
    bounds_ = info.rcMonitor;
    captured_ = captured;
  }
  return presentation_;
}

HRESULT ValidateDisplayTarget(UINT64 source_id) {
  if (source_id == 0 ||
      source_id >
          static_cast<UINT64>(std::numeric_limits<UINT_PTR>::max())) {
    return HRESULT_FROM_WIN32(ERROR_INVALID_PARAMETER);
  }
  HMONITOR monitor =
      reinterpret_cast<HMONITOR>(static_cast<UINT_PTR>(source_id));
  MONITORINFO info{};
  info.cbSize = sizeof(info);
  return GetMonitorInfoW(monitor, &info)
             ? S_OK
             : HRESULT_FROM_WIN32(ERROR_INVALID_MONITOR_HANDLE);
}

}  // namespace piik::capture
