#define NOMINMAX
#include <windows.h>

#include <codecapi.h>
#include <d3d11_1.h>
#include <dxgi1_2.h>
#include <fcntl.h>
#include <io.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#ifdef PIIK_H264_FIXTURE
#include <pdh.h>
#include <pdhmsg.h>
#endif
#include <winternl.h>
#include <wrl/client.h>

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>
#include <winrt/base.h>

// The test/probe translation units include this file directly and inherit
// these product headers, so keep the original include surface here.
#include "capture_target.h"
#include "capture_border.h"
#include "capture_event.h"
#include "capture_geometry.h"
#include "capture_color.h"
#include "capture_sdr.h"
#include "process_audio.h"
#include "h264_encoder.h"
#ifndef PIIK_H264_FIXTURE
#include "adaptive_encoder.h"
#include "capture_control.h"
#include "capture_input.h"
#include "capture_output.h"
#include "h264_decoder.h"
#include "output_mailbox.h"
#include "vp8_decoder.h"
#include <vpx/vpx_codec.h>
#endif

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cwctype>
#include <exception>
#include <iomanip>
#include <iostream>
#include <limits>
#include <memory>
#include <mutex>
#include <numeric>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

using Microsoft::WRL::ComPtr;

// Product code is split by responsibility; this file keeps the fixture encoder
// probe/evidence path and both entry points.
#include "capture_arguments.h"
#include "capture_encoder.h"
#include "capture_io.h"
#ifndef PIIK_H264_FIXTURE
#include "capture_audio.h"
#include "capture_probe.h"
#include "capture_runs.h"
#endif
#include "capture_workers.h"

namespace {

// The test/probe translation units compile this file directly and use the
// product seams by unqualified name; the unnamed namespace keeps that contract.
using namespace piik::capture::windows;
using piik::capture::OutputKind;
using piik::capture::ProtocolWriter;

#ifdef PIIK_H264_FIXTURE
constexpr UINT32 kFrameRate = kDefaultVideoProfile.frame_rate;
constexpr UINT32 kBitRate = kDefaultVideoProfile.bit_rate;
constexpr UINT32 kGopFrames = kFrameRate * 2;
constexpr UINT32 kMaxEventsPerPump = 64;
constexpr UINT32 kFrameCount = 360;
constexpr UINT32 kRateProbeBitRate = kBitRate / 2;
constexpr UINT32 kRateProbeFrames = kFrameCount / 3;
constexpr UINT32 kMaxInFlight = 8;
constexpr DWORD kMaxPdhArrayBytes = 4 * 1024 * 1024;
constexpr DWORD kMaxPdhArrayItems = 16 * 1024;
constexpr LONGLONG kFrameDuration100ns = 10'000'000 / kFrameRate;
#endif
std::wstring Lower(std::wstring value) {
  std::transform(value.begin(), value.end(), value.begin(),
                 [](wchar_t character) { return std::towlower(character); });
  return value;
}

void PrintAdapters(const std::vector<Adapter>& adapters) {
  for (const Adapter& adapter : adapters) {
    std::cout << "adapter_index=" << adapter.index
              << " name=" << NarrowAscii(adapter.description.Description)
              << " vendor=0x" << std::hex << std::setw(4) << std::setfill('0')
              << adapter.description.VendorId << " device=0x" << std::setw(4)
              << adapter.description.DeviceId << std::dec
              << " luid=" << LuidString(adapter.description.AdapterLuid) << '\n';
  }
}

#ifdef PIIK_H264_FIXTURE
std::vector<UINT8> SyntheticNv12(
    UINT32 frame_index, const VideoProfile& profile = kDefaultVideoProfile) {
  const auto width = profile.width;
  const auto height = profile.height;
  std::vector<UINT8> pixels(width * height * 3 / 2);
  const UINT32 bar_start = (frame_index * 11) % width;
  for (UINT32 y = 0; y < height; ++y) {
    for (UINT32 x = 0; x < width; ++x) {
      UINT32 distance = (x + width - bar_start) % width;
      UINT8 base = static_cast<UINT8>(16 + ((x / 8 + y / 8 + frame_index) % 180));
      pixels[y * width + x] = distance < 96 ? static_cast<UINT8>(235) : base;
    }
  }
  UINT8* uv = pixels.data() + width * height;
  for (UINT32 y = 0; y < height / 2; ++y) {
    for (UINT32 x = 0; x < width; x += 2) {
      uv[y * width + x] = static_cast<UINT8>(96 + (frame_index % 64));
      uv[y * width + x + 1] = static_cast<UINT8>(160 - (frame_index % 64));
    }
  }
  return pixels;
}

ComPtr<ID3D11Texture2D> CreateSyntheticTexture(
    ID3D11Device* device, UINT32 frame_index,
    const VideoProfile& profile = kDefaultVideoProfile) {
  std::vector<UINT8> pixels = SyntheticNv12(frame_index, profile);
  D3D11_TEXTURE2D_DESC description = {};
  description.Width = profile.width;
  description.Height = profile.height;
  description.MipLevels = 1;
  description.ArraySize = 1;
  description.Format = DXGI_FORMAT_NV12;
  description.SampleDesc.Count = 1;
  description.Usage = D3D11_USAGE_DEFAULT;

  D3D11_SUBRESOURCE_DATA initial = {};
  initial.pSysMem = pixels.data();
  initial.SysMemPitch = profile.width;
  initial.SysMemSlicePitch = static_cast<UINT>(pixels.size());

  ComPtr<ID3D11Texture2D> texture;
  Check(device->CreateTexture2D(&description, &initial, &texture),
        "input-texture-create");
  return texture;
}

ComPtr<IMFSample> CreateInputSample(ID3D11Device* device, UINT32 frame_index) {
  auto texture = CreateSyntheticTexture(device, frame_index);
  return CreateSurfaceSample(texture.Get(),
      static_cast<LONGLONG>(frame_index) * kFrameDuration100ns, kFrameDuration100ns);
}
#endif

#ifdef PIIK_H264_FIXTURE
ULONGLONG ProcessCpu100ns() {
  FILETIME created = {};
  FILETIME exited = {};
  FILETIME kernel = {};
  FILETIME user = {};
  if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user)) {
    Check(HRESULT_FROM_WIN32(GetLastError()), "process-cpu-sample");
  }
  ULARGE_INTEGER kernel_value = {};
  kernel_value.LowPart = kernel.dwLowDateTime;
  kernel_value.HighPart = kernel.dwHighDateTime;
  ULARGE_INTEGER user_value = {};
  user_value.LowPart = user.dwLowDateTime;
  user_value.HighPart = user.dwHighDateTime;
  return kernel_value.QuadPart + user_value.QuadPart;
}

class VideoEncodeSampler final {
 public:
  explicit VideoEncodeSampler(LUID adapter_luid)
      : adapter_luid_(adapter_luid), process_id_(GetCurrentProcessId()) {}

  ~VideoEncodeSampler() { Stop(); }

  void Start() {
    worker_ = std::thread([this]() { Run(); });
  }

  void Stop() {
    stop_.store(true);
    if (worker_.joinable()) {
      worker_.join();
    }
  }

  std::vector<double> Samples() const {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!error_.empty()) {
      Fail("videoencode-counter", error_);
    }
    return samples_;
  }

 private:
  void Run() noexcept {
    PDH_HQUERY query = nullptr;
    PDH_HCOUNTER counter = nullptr;
    PDH_STATUS status = PdhOpenQueryW(nullptr, 0, &query);
    if (status == ERROR_SUCCESS) {
      status = PdhAddEnglishCounterW(
          query, L"\\GPU Engine(*)\\Utilization Percentage", 0, &counter);
    }
    if (status == ERROR_SUCCESS) {
      status = PdhCollectQueryData(query);
    }
    if (status != ERROR_SUCCESS) {
      SetError("could not initialize the Windows GPU Engine counter");
      if (query != nullptr) {
        PdhCloseQuery(query);
      }
      return;
    }

    std::wostringstream prefix;
    prefix << L"pid_" << process_id_ << L"_luid_0x" << std::hex
           << std::setfill(L'0') << std::setw(8)
           << static_cast<UINT32>(adapter_luid_.HighPart) << L"_0x"
           << std::setw(8) << adapter_luid_.LowPart << L"_";
    std::wstring expected_prefix = Lower(prefix.str());

    while (!stop_.load()) {
      std::this_thread::sleep_for(std::chrono::milliseconds(250));
      status = PdhCollectQueryData(query);
      if (status != ERROR_SUCCESS) {
        SetError("could not sample the Windows GPU Engine counter");
        break;
      }

      DWORD bytes = 0;
      DWORD count = 0;
      status = PdhGetFormattedCounterArrayW(counter,
                                            PDH_FMT_DOUBLE | PDH_FMT_NOCAP100,
                                            &bytes, &count, nullptr);
      if (status != PDH_MORE_DATA) {
        if (status == PDH_CSTATUS_NO_INSTANCE || status == PDH_NO_DATA) {
          StoreSample(0.0);
          continue;
        }
        SetError("could not size the Windows GPU Engine counter array");
        break;
      }
      if (bytes == 0 || bytes > kMaxPdhArrayBytes ||
          count > kMaxPdhArrayItems ||
          count > bytes / sizeof(PDH_FMT_COUNTERVALUE_ITEM_W)) {
        SetError("Windows GPU Engine counter array exceeded its bounds");
        break;
      }
      std::vector<UINT8> storage(bytes);
      auto* items = reinterpret_cast<PDH_FMT_COUNTERVALUE_ITEM_W*>(
          storage.data());
      status = PdhGetFormattedCounterArrayW(
          counter, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, &bytes, &count, items);
      if (status != ERROR_SUCCESS) {
        SetError("could not read the Windows GPU Engine counter array");
        break;
      }
      if (bytes > storage.size() || count > kMaxPdhArrayItems ||
          count > storage.size() / sizeof(PDH_FMT_COUNTERVALUE_ITEM_W)) {
        SetError("Windows GPU Engine counter array changed beyond its bounds");
        break;
      }

      double total = 0.0;
      for (DWORD index = 0; index < count; ++index) {
        std::wstring name = Lower(items[index].szName);
        if (name.find(expected_prefix) != std::wstring::npos &&
            name.find(L"engtype_videoencode") != std::wstring::npos &&
            items[index].FmtValue.CStatus == ERROR_SUCCESS) {
          total += std::max(0.0, items[index].FmtValue.doubleValue);
        }
      }
      StoreSample(total);
    }
    PdhCloseQuery(query);
  }

  void StoreSample(double value) {
    std::lock_guard<std::mutex> lock(mutex_);
    samples_.push_back(value);
  }

  void SetError(const std::string& error) {
    std::lock_guard<std::mutex> lock(mutex_);
    error_ = error;
  }

  LUID adapter_luid_;
  DWORD process_id_;
  std::atomic<bool> stop_ = false;
  std::thread worker_;
  mutable std::mutex mutex_;
  std::vector<double> samples_;
  std::string error_;
};

struct RunEvidence final {
  UINT32 inputs = 0;
  UINT32 outputs = 0;
  UINT32 max_in_flight = 0;
  UINT32 recovery_units = 0;
  UINT64 output_bytes = 0;
  std::array<UINT64, 3> rate_phase_bytes{};
  std::vector<double> latencies_ms;
  std::vector<double> video_encode;
  std::string profile_level_id;
  ULONGLONG cpu_100ns = 0;
  double wall_seconds = 0.0;
};
#endif

#ifdef PIIK_H264_FIXTURE
bool IsRecoveryFrame(UINT32 frame) {
  return frame % kGopFrames == 0;
}

double Percentile95(std::vector<double> values) {
  if (values.empty()) {
    return 0.0;
  }
  std::sort(values.begin(), values.end());
  size_t index = static_cast<size_t>(
      std::ceil(static_cast<double>(values.size()) * 0.95)) - 1;
  return values[std::min(index, values.size() - 1)];
}

RunEvidence RunEncoder(const Adapter& adapter, const DeviceContext& device,
                       SelectedTransform& selected) {
  ConfigureCodec(selected.codec.Get());
  ComPtr<IMFMediaType> output_type = CreateOutputType();
  Check(selected.transform->SetOutputType(0, output_type.Get(), 0),
        "mft-set-output-type");
  ComPtr<IMFMediaType> input_type = CreateInputType();
  Check(selected.transform->SetInputType(0, input_type.Get(), 0),
        "mft-set-input-type");
  ValidateMediaTypes(selected.transform.Get());
  ValidateCodecReadback(selected.codec.Get());

  MFT_OUTPUT_STREAM_INFO output_info = {};
  Check(selected.transform->GetOutputStreamInfo(0, &output_info),
        "mft-output-stream-info");

  Check(selected.transform->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
                                            0),
        "mft-begin-streaming");
  Check(selected.transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM,
                                            0),
        "mft-start-stream");

  RunEvidence evidence;
  std::vector<std::chrono::steady_clock::time_point> submitted(kFrameCount);
  std::vector<bool> received(kFrameCount, false);
  std::optional<std::string> observed_profile;
  UINT32 pending_input = 0;
  bool drain_sent = false;
  bool drain_complete = false;

  VideoEncodeSampler video_encode(adapter.description.AdapterLuid);
  ULONGLONG cpu_start = ProcessCpu100ns();
  auto wall_start = std::chrono::steady_clock::now();
  auto next_input = wall_start;
  auto deadline = wall_start + std::chrono::seconds(30);
  video_encode.Start();

  try {
    while ((!drain_complete || evidence.outputs != kFrameCount) &&
           std::chrono::steady_clock::now() < deadline) {
      for (UINT32 event_count = 0; event_count < kMaxEventsPerPump;
           ++event_count) {
        if (std::chrono::steady_clock::now() >= deadline) {
          break;
        }
        ComPtr<IMFMediaEvent> event;
        HRESULT event_result = selected.events->GetEvent(MF_EVENT_FLAG_NO_WAIT,
                                                          &event);
        if (event_result == MF_E_NO_EVENTS_AVAILABLE) {
          break;
        }
        Check(event_result, "mft-get-event");
        HRESULT event_status = S_OK;
        Check(event->GetStatus(&event_status), "mft-event-status-read");
        Check(event_status, "mft-event-status");
        MediaEventType type = MEUnknown;
        Check(event->GetType(&type), "mft-event-type");

        if (type == METransformNeedInput) {
          if (evidence.inputs < kFrameCount) {
            if (pending_input >= kMaxInFlight) {
              Fail("mft-input-request-bound",
                   "hardware MFT queued more than eight input requests");
            }
            ++pending_input;
          }
        } else if (type == METransformHaveOutput) {
          ComPtr<IMFSample> sample = PullOutput(selected.transform.Get(),
                                                output_info);
          if (!sample) continue;
          LONGLONG sample_time = -1;
          Check(sample->GetSampleTime(&sample_time), "output-sample-time");
          if (sample_time < 0 || sample_time % kFrameDuration100ns != 0) {
            Fail("output-timestamp", "output timestamp is off the input cadence");
          }
          UINT32 frame = static_cast<UINT32>(sample_time / kFrameDuration100ns);
          if (frame >= kFrameCount || received[frame]) {
            Fail("output-order", "output timestamp is duplicate or out of range");
          }
          if (frame != evidence.outputs) {
            Fail("output-reordering", "output frames were reordered");
          }

          std::vector<UINT8> bytes = ReadSample(sample.Get());
          NalSummary nal = InspectAnnexB(bytes);
          if (!nal.annex_b) {
            Fail("bitstream-annexb", "output is not Annex-B H264");
          }
          if (nal.profile_level_id) {
            if (observed_profile && *observed_profile != *nal.profile_level_id) {
              Fail("bitstream-profile-change",
                   "SPS profile-level-id changed during the run");
            }
            observed_profile = nal.profile_level_id;
          }
          if (IsRecoveryFrame(frame) && !(nal.sps && nal.pps && nal.idr)) {
            Fail("bitstream-recovery-unit",
                 "requested recovery access unit lacks SPS, PPS, or IDR");
          }
          if (IsRecoveryFrame(frame)) {
            ++evidence.recovery_units;
          }

          received[frame] = true;
          ++evidence.outputs;
          evidence.output_bytes += bytes.size();
          evidence.rate_phase_bytes[std::min<UINT32>(
              frame / kRateProbeFrames, 2)] += bytes.size();
          auto now = std::chrono::steady_clock::now();
          evidence.latencies_ms.push_back(
              std::chrono::duration<double, std::milli>(now - submitted[frame])
                  .count());
        } else if (type == METransformDrainComplete) {
          drain_complete = true;
        } else if (type == MEError) {
          Fail("mft-error-event", "hardware MFT emitted MEError");
        }
      }

      auto now = std::chrono::steady_clock::now();
      if (pending_input > 0 && evidence.inputs < kFrameCount &&
          evidence.inputs - evidence.outputs < kMaxInFlight &&
          now >= next_input) {
        UINT32 frame = evidence.inputs;
        if (frame == kRateProbeFrames) {
          SetU32(selected.codec.Get(), CODECAPI_AVEncCommonMeanBitRate,
                 kRateProbeBitRate, "codec-runtime-bitrate-low");
        } else if (frame == kRateProbeFrames * 2) {
          SetU32(selected.codec.Get(), CODECAPI_AVEncCommonMeanBitRate,
                 kBitRate, "codec-runtime-bitrate-high");
        }
        if (IsRecoveryFrame(frame)) {
          ForceKeyFrame(selected.codec.Get());
        }
        ComPtr<IMFSample> sample = CreateInputSample(device.device.Get(), frame);
        submitted[frame] = std::chrono::steady_clock::now();
        Check(selected.transform->ProcessInput(0, sample.Get(), 0),
              "mft-process-input");
        ++evidence.inputs;
        --pending_input;
        evidence.max_in_flight = std::max(
            evidence.max_in_flight, evidence.inputs - evidence.outputs);
        if (evidence.max_in_flight > kMaxInFlight) {
          Fail("mft-in-flight-bound", "hardware MFT exceeded eight in-flight frames");
        }
        next_input = wall_start + std::chrono::microseconds(
                                      static_cast<int64_t>(evidence.inputs) *
                                      1'000'000 / kFrameRate);
        if (evidence.inputs == kFrameCount) {
          Check(selected.transform->ProcessMessage(
                    MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0),
                "mft-end-stream");
          Check(selected.transform->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN,
                                                    0),
                "mft-drain");
          drain_sent = true;
        }
      } else {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
      }
    }
  } catch (...) {
    video_encode.Stop();
    selected.transform->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
    selected.transform->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    throw;
  }

  video_encode.Stop();
  auto wall_end = std::chrono::steady_clock::now();
  ULONGLONG cpu_end = ProcessCpu100ns();
  Check(selected.transform->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0),
        "mft-end-streaming");

  if (!drain_sent || !drain_complete) {
    Fail("mft-drain-timeout", "hardware MFT did not drain within 30 seconds");
  }
  if (evidence.inputs != kFrameCount || evidence.outputs != kFrameCount) {
    Fail("frame-count", "hardware MFT did not produce 360 outputs for 360 inputs");
  }
  if (!observed_profile) {
    Fail("bitstream-sps", "no SPS profile-level-id was observed");
  }
  evidence.profile_level_id = *observed_profile;
  evidence.video_encode = video_encode.Samples();
  evidence.cpu_100ns = cpu_end - cpu_start;
  evidence.wall_seconds =
      std::chrono::duration<double>(wall_end - wall_start).count();

  double max_video_encode = evidence.video_encode.empty()
                                ? 0.0
                                : *std::max_element(evidence.video_encode.begin(),
                                                    evidence.video_encode.end());
  double mean_video_encode = evidence.video_encode.empty()
                                 ? 0.0
                                 : std::accumulate(evidence.video_encode.begin(),
                                                   evidence.video_encode.end(),
                                                   0.0) /
                                       evidence.video_encode.size();
  std::cout << "evidence_inputs=" << evidence.inputs << '\n'
            << "evidence_outputs=" << evidence.outputs << '\n'
            << "evidence_recovery_units=" << evidence.recovery_units << '\n'
            << "evidence_max_in_flight=" << evidence.max_in_flight << '\n'
            << "evidence_output_bytes=" << evidence.output_bytes << '\n'
            << "evidence_rate_phase_high_1_bytes="
            << evidence.rate_phase_bytes[0] << '\n'
            << "evidence_rate_phase_low_bytes="
            << evidence.rate_phase_bytes[1] << '\n'
            << "evidence_rate_phase_high_2_bytes="
            << evidence.rate_phase_bytes[2] << '\n'
            << "evidence_profile_level_id=" << evidence.profile_level_id << '\n'
            << std::fixed << std::setprecision(3)
            << "evidence_latency_p95_ms="
            << Percentile95(evidence.latencies_ms) << '\n'
            << "evidence_wall_seconds=" << evidence.wall_seconds << '\n'
            << "evidence_process_cpu_seconds="
            << static_cast<double>(evidence.cpu_100ns) / 10'000'000.0 << '\n'
            << "evidence_videoencode_samples=" << evidence.video_encode.size()
            << '\n'
            << "evidence_videoencode_mean_percent=" << mean_video_encode << '\n'
            << "evidence_videoencode_max_percent=" << max_video_encode << '\n'
            << std::flush;

  if (evidence.video_encode.size() < 10 || max_video_encode <= 0.1) {
    Fail("videoencode-attribution",
         "no process-and-adapter-attributed VideoEncode activity was observed");
  }
  if (!(evidence.rate_phase_bytes[1] < evidence.rate_phase_bytes[0] &&
        evidence.rate_phase_bytes[2] > evidence.rate_phase_bytes[1])) {
    Fail("codec-runtime-bitrate-effect",
         "live bitrate update did not lower and restore encoded output");
  }
  if (!SupportsH264ProfileLevelId(*observed_profile)) {
    Fail("bitstream-profile",
         "SPS uses unsupported H.264 coding tools or level");
  }
  return evidence;
}
#endif
#ifdef PIIK_H264_FIXTURE
struct Arguments final {
  bool list = false;
  std::optional<UINT> adapter_index;
  UINT mft_index = 0;
};

Arguments ParseArguments(int count, wchar_t** values) {
  Arguments arguments;
  for (int index = 1; index < count; ++index) {
    std::wstring argument = values[index];
    if (argument == L"--list-adapters") {
      arguments.list = true;
    } else if (argument == L"--adapter-index" && index + 1 < count) {
      arguments.adapter_index = ParseIndex(values[++index], "argument-adapter");
    } else if (argument == L"--mft-index" && index + 1 < count) {
      arguments.mft_index = ParseIndex(values[++index], "argument-mft");
    } else {
      Fail("arguments", "unsupported or incomplete command-line argument");
    }
  }
  if (!arguments.list && !arguments.adapter_index) {
    Fail("arguments", "--adapter-index is required for an encode run");
  }
  return arguments;
}

void PrintEvidence(const Adapter& adapter, UINT mft_index,
                   const SelectedTransform& selected,
                   const RunEvidence& evidence) {
  double max_video_encode = *std::max_element(evidence.video_encode.begin(),
                                               evidence.video_encode.end());
  double mean_video_encode = std::accumulate(evidence.video_encode.begin(),
                                              evidence.video_encode.end(), 0.0) /
                             evidence.video_encode.size();
  std::cout << "result=go-native-h264-hardware-fixture\n"
            << "adapter_index=" << adapter.index << '\n'
            << "adapter_name=" << NarrowAscii(adapter.description.Description)
            << '\n'
            << "adapter_luid=" << LuidString(adapter.description.AdapterLuid)
            << '\n'
            << "mft_index=" << mft_index << '\n'
            << "mft_name=" << selected.name << '\n'
            << "mft_clsid=" << selected.clsid << '\n'
            << "inputs=" << evidence.inputs << '\n'
            << "outputs=" << evidence.outputs << '\n'
            << "recovery_units=" << evidence.recovery_units << '\n'
            << "max_in_flight=" << evidence.max_in_flight << '\n'
            << "output_bytes=" << evidence.output_bytes << '\n'
            << "profile_level_id=" << evidence.profile_level_id << '\n'
            << std::fixed << std::setprecision(3)
            << "latency_p95_ms=" << Percentile95(evidence.latencies_ms) << '\n'
            << "wall_seconds=" << evidence.wall_seconds << '\n'
            << "process_cpu_seconds="
            << static_cast<double>(evidence.cpu_100ns) / 10'000'000.0 << '\n'
            << "videoencode_samples=" << evidence.video_encode.size() << '\n'
            << "videoencode_mean_percent=" << mean_video_encode << '\n'
            << "videoencode_max_percent=" << max_video_encode << '\n';
}
#endif

}  // namespace

#ifdef PIIK_H264_FIXTURE
int wmain(int argc, wchar_t** argv) {
  try {
    Arguments arguments = ParseArguments(argc, argv);
    Runtime runtime;
    std::vector<Adapter> adapters = EnumerateAdapters();
    if (arguments.list) {
      PrintAdapters(adapters);
      return 0;
    }

    const Adapter& adapter = SelectAdapter(adapters, *arguments.adapter_index);
    DeviceContext device = CreateDevice(adapter);
    ActivationList activations = EnumerateHardwareEncoders(adapter);
    for (UINT32 index = 0; index < activations.count; ++index) {
      std::wstring name = GetOptionalString(activations.values[index],
                                            MFT_FRIENDLY_NAME_Attribute);
      std::cout << "mft_candidate_index=" << index
                << " name=" << NarrowAscii(name) << '\n';
    }
    SelectedTransform selected = ActivateTransform(
        activations, arguments.mft_index, device.manager.Get());
    std::cout << "selected_adapter_index=" << adapter.index << '\n'
              << "selected_adapter_name="
              << NarrowAscii(adapter.description.Description) << '\n'
              << "selected_adapter_luid="
              << LuidString(adapter.description.AdapterLuid) << '\n'
              << "selected_mft_index=" << arguments.mft_index << '\n'
              << "selected_mft_name=" << selected.name << '\n'
              << "selected_mft_clsid=" << selected.clsid << '\n'
              << std::flush;
    RunEvidence evidence = RunEncoder(adapter, device, selected);
    PrintEvidence(adapter, arguments.mft_index, selected, evidence);
    return 0;
  } catch (const GateFailure& error) {
    std::cerr << "result=no-go-native-h264-hardware-fixture\n"
              << "stage=" << error.stage() << '\n'
              << "detail=" << error.what() << '\n';
    if (FAILED(error.result())) {
      std::cerr << "hresult=0x" << std::hex << std::setfill('0')
                << std::setw(8) << static_cast<UINT32>(error.result()) << '\n';
    }
    return 2;
  } catch (const std::exception& error) {
    std::cerr << "result=no-go-native-h264-hardware-fixture\n"
              << "stage=unclassified\n"
              << "detail=" << error.what() << '\n';
    return 3;
  }
}
#endif

#ifndef PIIK_H264_FIXTURE
int wmain(int argc, wchar_t** argv) {
  try {
    ProductArguments arguments = ParseProductArguments(argc, argv);
    if (arguments.mode == ProductArguments::Mode::microphones) return piik::capture::WriteMicrophoneList();
    if (arguments.mode == ProductArguments::Mode::list) {
      return piik::capture::WriteSourceList();
    }
    if (arguments.mode == ProductArguments::Mode::probe) {
      Runtime runtime;
      WriteCapabilityProbe();
      return 0;
    }
    if (_setmode(_fileno(stdout), _O_BINARY) == -1) {
      Fail("stdout-binary", "could not switch stdout to binary mode");
    }
    if (arguments.mode == ProductArguments::Mode::preview) {
      Check(piik::capture::WriteSourcePreview(
                arguments.target_kind, arguments.source_id, arguments.pid,
                arguments.creation_time),
            "capture-preview");
      return 0;
    }
    if (arguments.mode == ProductArguments::Mode::audio || arguments.mode == ProductArguments::Mode::microphone) {
      Check(RunAudioCapture(arguments), "process-audio-capture");
      return 0;
    }
    Runtime runtime;
    if (arguments.mode == ProductArguments::Mode::encoded) RunEncodedVideo(arguments);
    else RunVideoCapture(arguments);
    return 0;
  } catch (const GateFailure& error) {
    std::cerr << "result=window-capture-failed\n"
              << "stage=" << error.stage() << '\n'
              << "detail=" << error.what() << '\n';
    if (FAILED(error.result())) {
      std::cerr << "hresult=0x" << std::hex << std::setfill('0')
                << std::setw(8) << static_cast<UINT32>(error.result()) << '\n';
    }
    return 2;
  } catch (const winrt::hresult_error& error) {
    std::cerr << "result=window-capture-failed\n"
              << "stage=windows-runtime\n"
              << "hresult=0x" << std::hex << std::setfill('0')
              << std::setw(8) << static_cast<UINT32>(error.code()) << '\n';
    return 2;
  } catch (const std::exception& error) {
    std::cerr << "result=window-capture-failed\n"
              << "stage=unclassified\n"
              << "detail=" << error.what() << '\n';
    return 3;
  }
}
#endif
