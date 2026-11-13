#define NOMINMAX
#include <windows.h>

#include <mfapi.h>
#include <wrl/client.h>

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>
#include <winrt/base.h>

#include "capture_border.h"
#include "capture_color.h"
#include "capture_devices.h"
#include "capture_encoder.h"
#include "capture_error.h"
#include "capture_event.h"
#include "capture_io.h"
#include "capture_runs.h"
#include "capture_sdr.h"
#include "capture_target.h"
#include "capture_workers.h"
#include "h264_decoder.h"
#include "vp8_decoder.h"
#include <vpx/vpx_codec.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <exception>
#include <functional>
#include <iomanip>
#include <iostream>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>
#include <syncstream>
#include <thread>
#include <utility>
#include <vector>

// Live WGC capture and encoded-input relay run loops.
namespace piik::capture::windows {

using piik::capture::OutputKind;
using piik::capture::ProtocolWriter;

namespace {

void WriteVideoStarting(ProtocolWriter& writer, const ProductArguments& arguments,
                        const Adapter& adapter, const VideoEncoderSelection& encoder) {
  const bool hardware = encoder.kind == OutputKind::h264;
  std::ostringstream status;
  status << "{\"state\":\"starting\",\"hardwareOnly\":" << (hardware ? "true" : "false")
         << ",\"codec\":" << JSONString(hardware ? "h264" : "vp8")
         << ",\"adapterIndex\":" << adapter.index
         << ",\"adapterName\":" << JSONString(NarrowAscii(adapter.description.Description))
         << ",\"adapterIdentity\":" << JSONString(LuidString(adapter.description.AdapterLuid))
         << ",\"encoderName\":" << JSONString(hardware ? encoder.initial->name : "libvpx VP8")
         << ",\"encoderIdentity\":" << JSONString(hardware ? encoder.initial->identity :
              std::string("libvpx/") + vpx_codec_version_str());
  if (hardware) status << ",\"encoderIndex\":" << arguments.mft_index;
  AppendOutputProfiles(status, arguments.outputs);
  status << '}';
  Check(writer.WriteStatus(status.str()), "capture-status-starting");
}

std::vector<std::unique_ptr<OutputWorker>> CreateOutputWorkers(
    const ProductArguments& arguments, const Adapter& adapter, const DeviceContext& device,
    ProtocolWriter& writer, VideoEncoderSelection encoder,
    const std::function<void(const AdaptiveAccessUnit&)>& on_active,
    const std::function<void(size_t, std::exception_ptr)>& on_failure,
    const std::function<void()>& on_refresh = {}) {
  const bool hardware = encoder.kind == OutputKind::h264;
  const UINT encoder_index = arguments.mft_index;
  const bool capture = arguments.mode == ProductArguments::Mode::video;
  const size_t original_layer = capture && arguments.outputs.size() > 1 ? 1 : 0;
  std::vector<std::unique_ptr<OutputWorker>> workers;
  for (size_t layer = 0; layer < arguments.outputs.size(); ++layer) {
    const auto profile = arguments.outputs[layer];
    const bool original = layer == original_layer;
    AdaptiveEncoder::Factory create;
    if (hardware) {
      create = [&adapter, &device, encoder_index](const VideoProfile& selected_profile) -> std::unique_ptr<VideoEncoder> {
        auto activations = EnumerateHardwareEncoders(adapter);
        auto selected = ActivateTransform(activations, encoder_index, device.manager.Get());
        return std::make_unique<LiveEncoder>(std::move(selected), selected_profile);
      };
    }
    workers.push_back(std::make_unique<OutputWorker>(
        static_cast<UINT8>(layer), profile, encoder.kind, device.device.Get(), writer,
        std::move(create), original ? std::move(encoder.initial) : nullptr, capture && layer < 2,
        [original, on_active](const AdaptiveAccessUnit& output) { if (original) on_active(output); },
        [layer, on_failure](std::exception_ptr error) { on_failure(layer, error); }, on_refresh));
  }
  return workers;
}

bool ApplyOutputControl(const piik::capture::CaptureControl& control,
                        const std::vector<std::unique_ptr<OutputWorker>>& workers) {
  if (control.layer >= static_cast<int>(workers.size())) Fail("control-layer", "output layer is unavailable");
  if (control.kind == 'A') {
    return workers[control.layer]->SetActive(control.value != 0);
  } else if (control.kind == 'K') {
    for (size_t layer = 0; layer < workers.size(); ++layer) {
      if (control.layer == -1 || static_cast<size_t>(control.layer) == layer) workers[layer]->RequestKeyFrame();
    }
  } else if (control.kind == 'B') {
    workers[control.layer]->SetBitrate(control.value);
  }
  return false;
}

}  // namespace

void RunEncodedVideo(ProductArguments arguments) {
  auto adapters = EnumerateAdapters();
  DeviceContext device;
  auto encoder = SelectVideoEncoder(arguments, adapters, device);
  const auto& adapter = SelectAdapter(adapters, arguments.adapter_index);
  const bool hardware = arguments.codec == "h264";
  ProtocolWriter writer;
  WriteVideoStarting(writer, arguments, adapter, encoder);
  HANDLE input_thread_handle = nullptr;
  Check(DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(),
                         &input_thread_handle, 0, FALSE, DUPLICATE_SAME_ACCESS), "encoded-input-thread");
  UniqueHandle input_thread(input_thread_handle);
  std::atomic<bool> active{false};
  std::atomic<size_t> failed{0};
  auto workers = CreateOutputWorkers(arguments, adapter, device, writer, std::move(encoder),
      [&](const AdaptiveAccessUnit& output) {
        if (!active.exchange(true)) WriteVideoActive(writer, arguments, hardware, output);
      },
      [&](size_t layer, std::exception_ptr error) {
        (void)writer.WriteUnavailable(static_cast<UINT8>(layer), OutputFailureDetail(layer, error));
        if (failed.fetch_add(1) + 1 == arguments.outputs.size()) CancelSynchronousIo(input_thread.get());
      });
  auto cleanup = [&]() {
    writer.Stop();
    for (auto& worker : workers) worker->Stop();
    for (auto& worker : workers) worker->Join();
  };
  try {
    std::unique_ptr<piik::capture::H264Decoder> h264;
    std::unique_ptr<piik::capture::Vp8Decoder> vp8;
    if (hardware) h264 = std::make_unique<piik::capture::H264Decoder>(device.manager.Get());
    else vp8 = std::make_unique<piik::capture::Vp8Decoder>();
    InputReader reader;
    bool began = false;
    UINT64 last_timestamp = 0;
    auto submit = [&](ComPtr<ID3D11Texture2D> texture, UINT32 width, UINT32 height,
                      UINT64 timestamp, UINT64 duration, DXGI_COLOR_SPACE_TYPE color) {
      if ((began && timestamp <= last_timestamp) || width < arguments.profile.width || height < arguments.profile.height) {
        Fail("encoded-input-output", "decoded frame changed the source timeline or output bounds");
      }
      auto input = std::make_shared<CaptureInput>();
      input->texture = std::move(texture);
      input->color_space = color;
      input->width = width;
      input->height = height;
      input->presentation = {static_cast<LONG>(width), static_cast<LONG>(height)};
      input->timestamp = timestamp;
      input->capture_timestamp = timestamp;
      Check(writer.WriteBegin(timestamp, duration), "decoded-frame-begin");
      for (auto& worker : workers) worker->Submit(input);
      began = true;
      last_timestamp = timestamp;
    };
    for (;;) {
      if (failed.load() == workers.size()) Fail("encoded-outputs", "all derived outputs are unavailable");
      for (const auto& input : reader.Read(true)) {
        if (input.kind == 7) {
          const auto control = input.Control();
          if (control.kind == 'Q') { cleanup(); return; }
          ApplyOutputControl(control, workers);
          continue;
        }
        if (input.kind != (hardware ? 2 : 4)) Fail("encoded-input-codec", "encoded source codec changed");
        if (h264) {
          h264->Decode(input, [&](IMFSample* sample, UINT32 width, UINT32 height, LONG stride) {
            LONGLONG timestamp = -1, duration = 0;
            Check(sample->GetSampleTime(&timestamp), "decoded-timestamp");
            Check(sample->GetSampleDuration(&duration), "decoded-duration");
            if (timestamp < 0 || duration <= 0) Fail("decoded-timing", "decoder did not preserve source timing");
            const auto area = h264->DisplayArea();
            submit(OwnDecodedTexture(device, sample, width, height, stride, area),
                   area.right - area.left, area.bottom - area.top,
                   static_cast<UINT64>(timestamp), static_cast<UINT64>(duration), h264->ColorSpace());
          });
        } else {
          const auto decoded = vp8->Decode(input.data);
          if (!decoded.nv12.empty()) submit(UploadDecodedNV12(device, decoded.nv12.data(), decoded.width, decoded.height),
              decoded.width, decoded.height, input.timestamp, input.duration, kSdrVideoColor);
        }
      }
    }
  } catch (...) {
    cleanup();
    throw;
  }
}

void RunVideoCapture(ProductArguments arguments) {
  const bool window_target =
      arguments.target_kind == piik::capture::TargetKind::window;
  HRESULT identity = window_target
                         ? piik::capture::ValidateWindowTarget(
                               arguments.source_id, arguments.pid,
                               arguments.creation_time)
                         : piik::capture::ValidateDisplayTarget(
                               arguments.source_id);
  Check(identity, "target-identity");

  std::vector<Adapter> adapters = EnumerateAdapters();
  DeviceContext device;
  auto encoder = SelectVideoEncoder(arguments, adapters, device);
  const Adapter& adapter = SelectAdapter(adapters, arguments.adapter_index);
  const bool hardware = encoder.kind == OutputKind::h264;
  piik::capture::CapturePresentation presentation(arguments.target_kind,
                                                      arguments.source_id);
  ProtocolWriter writer;

  using namespace winrt::Windows::Graphics::Capture;
  using winrt::Windows::Graphics::DirectX::DirectXPixelFormat;
  if (!GraphicsCaptureSession::IsSupported()) {
    Fail("capture-support", "Windows Graphics Capture is unavailable");
  }
  UniqueHandle process(window_target
                           ? OpenProcess(SYNCHRONIZE, FALSE, arguments.pid)
                           : nullptr);
  if (window_target && process.get() == nullptr) {
    Check(HRESULT_FROM_WIN32(GetLastError()), "target-process-handle");
  }
  Check(window_target
            ? piik::capture::ValidateWindowTarget(
                  arguments.source_id, arguments.pid, arguments.creation_time)
            : piik::capture::ValidateDisplayTarget(arguments.source_id),
        "target-identity-before-capture");
  if (window_target) {
    DWORD affinity = 0;
    const bool available = GetWindowDisplayAffinity(
        reinterpret_cast<HWND>(static_cast<UINT_PTR>(arguments.source_id)), &affinity);
    const DWORD error = available ? ERROR_SUCCESS : GetLastError();
    std::osyncstream diagnostic(std::cerr);
    diagnostic << "result=capture-window-policy";
    if (available) diagnostic << " display-affinity=0x" << std::hex << affinity;
    else diagnostic << " display-affinity=unknown error=" << error;
    diagnostic << '\n';
  }
  auto capture_device = CreateCaptureDevice(device.device.Get());
  GraphicsCaptureItem item = window_target
                                 ? CreateCaptureItem(reinterpret_cast<HWND>(
                                       static_cast<UINT_PTR>(arguments.source_id)))
                                 : CreateCaptureItem(reinterpret_cast<HMONITOR>(
                                       static_cast<UINT_PTR>(arguments.source_id)));
  Check(window_target
            ? piik::capture::ValidateWindowTarget(
                  arguments.source_id, arguments.pid, arguments.creation_time)
            : piik::capture::ValidateDisplayTarget(arguments.source_id),
        "target-identity-after-item");
  auto initial_size = item.Size();
  if (initial_size.Width <= 0 || initial_size.Height <= 0) {
    Fail("capture-size", "selected window has no capturable content");
  }
  CaptureDisplayColor display_color(arguments.target_kind, arguments.source_id);
  CaptureSdrConverter sdr(device.device.Get());
  auto pool_format = display_color.Resolve().Format();
  Direct3D11CaptureFramePool pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
      capture_device, static_cast<DirectXPixelFormat>(pool_format), 2,
      initial_size);
  GraphicsCaptureSession capture_session = pool.CreateCaptureSession(item);
  EnableFastCaptureUpdates(capture_session);
  piik::capture::CaptureBorder border;

  auto shutdown = std::make_shared<winrt::handle>(
      CreateEventW(nullptr, TRUE, FALSE, nullptr));
  auto frame_ready = std::make_shared<winrt::handle>(
      CreateEventW(nullptr, FALSE, FALSE, nullptr));
  if (!*shutdown || !*frame_ready) {
    Check(HRESULT_FROM_WIN32(GetLastError()), "capture-events");
  }

  auto frame_token = pool.FrameArrived(
      piik::capture::CaptureEventHandler(frame_ready));
  auto closed_token = item.Closed(piik::capture::CaptureEventHandler(shutdown));
  std::vector<std::unique_ptr<OutputWorker>> workers;
  std::atomic<bool> refresh_requested{false};
  std::atomic<bool> active_status_written{false};
  std::mutex failure_mutex;
  std::exception_ptr failure;
  auto fail_capture = [&](std::exception_ptr error) {
    {
      std::lock_guard<std::mutex> lock(failure_mutex);
      if (!failure) failure = error;
    }
    SetEvent(shutdown->get());
  };
  auto cleanup = [&]() noexcept {
    SetEvent(shutdown->get());
    writer.Stop();
    for (auto& worker : workers) worker->Stop();
    try {
      pool.FrameArrived(frame_token);
    } catch (...) {
    }
    try {
      item.Closed(closed_token);
    } catch (...) {
    }
    border.Close();
    try {
      capture_session.Close();
    } catch (...) {
    }
    try {
      pool.Close();
    } catch (...) {
    }
    for (auto& worker : workers) worker->Join();
  };

  try {
    const UINT64 frame_duration = static_cast<UINT64>(
        arguments.profile.frame_duration_100ns());
    const DWORD control_wait_ms = static_cast<DWORD>((frame_duration + 9'999) / 10'000);
    WriteVideoStarting(writer, arguments, adapter, encoder);
    workers = CreateOutputWorkers(arguments, adapter, device, writer, std::move(encoder),
      [&](const AdaptiveAccessUnit& output) {
        if (!active_status_written.exchange(true)) WriteVideoActive(writer, arguments, hardware, output);
      },
      [&](size_t layer, std::exception_ptr error) {
        if (layer == (arguments.outputs.size() > 1 ? 1u : 0u)) {
          fail_capture(error);
          return;
        }
        if (FAILED(writer.WriteUnavailable(static_cast<UINT8>(layer), OutputFailureDetail(layer, error)))) fail_capture(error);
      }, [&] {
        refresh_requested.store(true);
        SetEvent(frame_ready->get());
      });
    if (!arguments.show_capture_border) border.Start(capture_session);
    capture_session.StartCapture();

    UINT64 previous_timestamp = 0;
    UINT64 previous_capture_timestamp = 0;
    UINT64 minimum_capture_timestamp = 0;
    bool window_minimized = false;
    UINT64 next_output_timestamp = 0;
    UINT64 source_frames = 0;
    auto pool_size = initial_size;
    std::shared_ptr<const CaptureInput> latest_input;
    bool refresh_input = false;
    ControlReader controls;
    auto submit = [&](std::shared_ptr<const CaptureInput> input) {
      Check(writer.WriteBegin(input->timestamp, frame_duration), "capture-frame-begin");
      for (auto& worker : workers) worker->Submit(input);
      previous_timestamp = input->timestamp;
      ++source_frames;
      refresh_input = false;
    };
    const HANDLE window_waits[] = {
        process.get(), shutdown->get(), frame_ready->get()};
    const HANDLE display_waits[] = {shutdown->get(), frame_ready->get()};
    for (;;) {
      refresh_input = refresh_requested.exchange(false) || refresh_input;
      for (const auto& control : controls.Read()) {
        if (control.kind == 'Q') {
          cleanup();
          return;
        }
        if (control.kind == 'A') {
          refresh_input = ApplyOutputControl(control, workers) || refresh_input;
          continue;
        }
        ApplyOutputControl(control, workers);
        refresh_input = true;
      }
      border.Apply(capture_session);
      if (window_target && !IsWindow(reinterpret_cast<HWND>(
              static_cast<UINT_PTR>(arguments.source_id)))) {
        Fail("capture-closed", "selected source stopped capture");
      }
      // WGC can deliver a minimized title-bar frame after the window is restored.
      // Fence that suspended content by capture time, not current window geometry.
      const bool minimized = window_target && IsIconic(reinterpret_cast<HWND>(
          static_cast<UINT_PTR>(arguments.source_id)));
      if (window_minimized && !minimized) {
        minimum_capture_timestamp = static_cast<UINT64>(MFGetSystemTime());
      }
      window_minimized = minimized;
      DWORD wait = window_target
                       ? WaitForMultipleObjects(3, window_waits, FALSE, control_wait_ms)
                       : WaitForMultipleObjects(2, display_waits, FALSE, control_wait_ms);
      if (window_target && wait == WAIT_OBJECT_0) {
        Fail("target-exited", "selected target process exited");
      }
      const DWORD shutdown_index =
          window_target ? WAIT_OBJECT_0 + 1 : WAIT_OBJECT_0;
      const DWORD frame_index =
          window_target ? WAIT_OBJECT_0 + 2 : WAIT_OBJECT_0 + 1;
      if (wait == shutdown_index) {
        {
          std::lock_guard<std::mutex> lock(failure_mutex);
          if (failure) std::rethrow_exception(failure);
        }
        // A worker failure was handled above; the other live wake is item.Closed.
        Fail("capture-closed", "selected source stopped capture");
      }
      if (wait == WAIT_TIMEOUT) {
        // An explicit output/control request may reuse a quiet source; silence is not EOF.
        if (refresh_input && latest_input) {
          submit(RefreshCaptureInput(*latest_input, previous_timestamp));
        }
        continue;
      }
      if (wait != frame_index) {
        Check(HRESULT_FROM_WIN32(GetLastError()), "capture-wait");
      }

      Direct3D11CaptureFrame latest{nullptr};
      for (;;) {
        Direct3D11CaptureFrame next = pool.TryGetNextFrame();
        if (!next) break;
        latest = std::move(next);
      }
      if (!latest) continue;
      // A minimized window has no capturable picture.
      if (window_target && IsIconic(reinterpret_cast<HWND>(
              static_cast<UINT_PTR>(arguments.source_id)))) {
        latest.Close();
        continue;
      }
      INT64 signed_timestamp = latest.SystemRelativeTime().count();
      if (signed_timestamp <= 0) {
        Fail("capture-timestamp", "captured frame has no QPC timestamp");
      }
      UINT64 timestamp = static_cast<UINT64>(signed_timestamp);
      const auto presentation_timestamp = CapturePresentationTimestamp(
          timestamp, previous_capture_timestamp, previous_timestamp,
          minimum_capture_timestamp);
      if (!presentation_timestamp) continue;
      auto content_size = latest.ContentSize();
      if (content_size.Width <= 0 || content_size.Height <= 0) continue;
      // ContentSize owns this rendered frame's geometry. Item/window bounds can
      // differ on Windows 10; comparing them would discard every valid frame.
      const auto color = display_color.Resolve();
      if (content_size.Width != pool_size.Width ||
          content_size.Height != pool_size.Height || color.Format() != pool_format) {
        latest.Close();
        latest = nullptr;
        latest_input.reset();
        pool.Recreate(capture_device,
                      static_cast<DirectXPixelFormat>(color.Format()), 2,
                      content_size);
        pool_size = content_size;
        pool_format = color.Format();
        continue;
      }
      ComPtr<ID3D11Texture2D> source = CaptureTexture(latest);
      D3D11_TEXTURE2D_DESC source_description = {};
      source->GetDesc(&source_description);
      UINT32 content_width = std::min<UINT32>(
          source_description.Width, static_cast<UINT32>(content_size.Width));
      UINT32 content_height = std::min<UINT32>(
          source_description.Height, static_cast<UINT32>(content_size.Height));
      auto input = std::make_shared<CaptureInput>();
      input->texture = sdr.Convert(source.Get(), color);
      input->width = content_width;
      input->height = content_height;
      input->timestamp = *presentation_timestamp;
      input->capture_timestamp = timestamp;
      input->presentation = presentation.Resolve(content_width, content_height,
          source_frames % arguments.profile.gop_frames() == 0);
      latest.Close();
      latest = nullptr;
      previous_capture_timestamp = timestamp;
      latest_input = input;
      const UINT64 timing_tolerance = frame_duration / 20;
      if (next_output_timestamp != 0 && timestamp < next_output_timestamp &&
          next_output_timestamp - timestamp > timing_tolerance) {
        refresh_input = true;
        continue;
      }
      if (next_output_timestamp == 0) {
        next_output_timestamp = timestamp + frame_duration;
      } else {
        const UINT64 elapsed = timestamp > next_output_timestamp
                                   ? timestamp - next_output_timestamp
                                   : 0;
        next_output_timestamp +=
            (elapsed / frame_duration + 1) * frame_duration;
      }
      submit(input);
    }
  } catch (...) {
    const HRESULT removed = device.device->GetDeviceRemovedReason();
    cleanup();
    Check(removed, "capture-device-lost");
    throw;
  }
}

}  // namespace piik::capture::windows
