#include "capture_probe.h"

#include <windows.h>

#include <mfidl.h>
#include <windows.graphics.capture.interop.h>
#include <winternl.h>

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>

#include "capture_border.h"
#include "capture_io.h"
#include "h264_encoder.h"
#include "process_audio.h"

#include <chrono>
#include <condition_variable>
#include <functional>
#include <iostream>
#include <memory>
#include <mutex>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

// Capability probe: Windows build, WGC support, adapters/encoders, audio APIs.
namespace piik::capture::windows {

struct AudioCapabilityProbe::State final {
  std::mutex mutex;
  std::condition_variable changed;
  Capabilities capabilities;
  unsigned pending = 3;
};

UINT32 WindowsBuild() {
  HMODULE module = GetModuleHandleW(L"ntdll.dll");
  if (module == nullptr) {
    Fail("windows-version", "ntdll.dll is unavailable");
  }
  using RtlGetVersionFunction = LONG(WINAPI*)(PRTL_OSVERSIONINFOW);
  auto get_version = reinterpret_cast<RtlGetVersionFunction>(
      GetProcAddress(module, "RtlGetVersion"));
  if (get_version == nullptr) {
    Fail("windows-version", "RtlGetVersion is unavailable");
  }
  RTL_OSVERSIONINFOW version = {};
  version.dwOSVersionInfoSize = sizeof(version);
  if (get_version(&version) < 0) {
    Fail("windows-version", "could not read Windows version");
  }
  return version.dwBuildNumber;
}

AudioCapabilityProbe::AudioCapabilityProbe(
    Clock::time_point deadline, std::function<bool(bool)> process,
    std::function<bool()> system)
    : state_(std::make_shared<State>()), deadline_(deadline) {
  Start([process](const std::shared_ptr<State>& state) {
    const bool available = process(false);
    std::lock_guard lock(state->mutex);
    state->capabilities.process = available;
  });
  Start([process = std::move(process)](const std::shared_ptr<State>& state) {
    const bool available = process(true);
    std::lock_guard lock(state->mutex);
    state->capabilities.exclusion = available;
  });
  Start([system = std::move(system)](const std::shared_ptr<State>& state) {
    const bool available = system();
    std::lock_guard lock(state->mutex);
    state->capabilities.system = available;
  });
}

AudioCapabilityProbe::AudioCapabilityProbe()
    : AudioCapabilityProbe(Clock::now() + std::chrono::seconds(1),
                           piik::capture::ProcessAudioAvailable,
                           piik::capture::SystemAudioAvailable) {}

AudioCapabilityProbe::Capabilities AudioCapabilityProbe::Snapshot() const {
  std::unique_lock lock(state_->mutex);
  state_->changed.wait_until(lock, deadline_, [state = state_]() { return state->pending == 0; });
  return state_->capabilities;
}

void AudioCapabilityProbe::Start(
    std::function<void(const std::shared_ptr<State>&)> query) {
  std::thread([state = state_, query = std::move(query)]() {
    const HRESULT apartment = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (SUCCEEDED(apartment)) {
      try { query(state); } catch (...) { /* Optional capabilities remain unavailable. */ }
      CoUninitialize();
    }
    { std::lock_guard lock(state->mutex); --state->pending; }
    state->changed.notify_one();
  }).detach();
}

void WriteCapabilityProbe(AudioCapabilityProbe audio) {
  const UINT32 build = WindowsBuild();
  bool window_capture = false;
  try {
    using namespace winrt::Windows::Graphics::Capture;
    // Probe the entry points used by capture and previews, not their OS versions.
    window_capture = GraphicsCaptureSession::IsSupported() &&
        winrt::Windows::Foundation::Metadata::ApiInformation::IsMethodPresent(
            L"Windows.Graphics.Capture.Direct3D11CaptureFramePool", L"CreateFreeThreaded") &&
        !!winrt::get_activation_factory<GraphicsCaptureItem, IGraphicsCaptureItemInterop>();
  } catch (const winrt::hresult_error&) {
    window_capture = false;
  }

  std::vector<Adapter> adapters = EnumerateAdapters();
  std::ostringstream output;
  output << "{\"protocol\":7,\"platform\":\"windows\",\"platformBuild\":"
         << JSONString(std::to_string(build))
         << ",\"videoCapture\":" << (window_capture ? "true" : "false")
         << ",\"captureBorderControl\":"
         << (window_capture && piik::capture::CaptureBorder::Supported() ? "true" : "false")
         << ",\"microphone\":true,\"softwareVP8\":true"
         << ",\"adapters\":[";
  for (size_t adapter_index = 0; adapter_index < adapters.size();
       ++adapter_index) {
    if (adapter_index != 0) output << ',';
    const Adapter& adapter = adapters[adapter_index];
    ActivationList encoders = EnumerateHardwareEncoders(adapter, false);
    output << "{\"index\":" << adapter.index << ",\"name\":"
           << JSONString(NarrowAscii(adapter.description.Description))
           << ",\"identity\":"
           << JSONString(LuidString(adapter.description.AdapterLuid))
           << ",\"hardwareH264\":[";
    for (UINT32 encoder_index = 0; encoder_index < encoders.count;
         ++encoder_index) {
      if (encoder_index != 0) output << ',';
      IMFActivate* encoder = encoders.values[encoder_index];
      std::wstring name =
          GetOptionalString(encoder, MFT_FRIENDLY_NAME_Attribute);
      GUID clsid = {};
      Check(encoder->GetGUID(MFT_TRANSFORM_CLSID_Attribute, &clsid),
            "mft-probe-clsid");
      output << "{\"index\":" << encoder_index << ",\"name\":"
             << JSONString(NarrowAscii(name)) << ",\"identity\":"
             << JSONString(GuidString(clsid)) << '}';
    }
    output << "]}";
  }
  const auto capabilities = audio.Snapshot();
  output << "],\"processAudio\":" << (capabilities.process ? "true" : "false")
         << ",\"processAudioExclusion\":" << (capabilities.exclusion ? "true" : "false")
         << ",\"systemAudio\":" << (capabilities.system ? "true" : "false") << '}';
  std::cout << output.str();
  if (!std::cout.good()) {
    Fail("capability-output", "could not write the capability result");
  }
}

}  // namespace piik::capture::windows
