#include "capture_audio.h"

#include "capture_io.h"
#include "capture_output.h"
#include "process_audio.h"

// Product audio modes: microphone, process-tree, display loopback.
namespace piik::capture::windows {

using piik::capture::OutputKind;
using piik::capture::ProtocolWriter;

HRESULT RunAudioCapture(const ProductArguments& arguments) {
  UniqueHandle stop(CreateEventW(nullptr, TRUE, FALSE, nullptr));
  if (stop.get() == nullptr) return HRESULT_FROM_WIN32(GetLastError());
  ProtocolWriter writer;
  ControlReader controls;
  auto should_stop = [&]() {
    const auto commands = controls.Read();
    if (commands.empty()) return false;
    if (commands.front().kind != 'Q') Fail("audio-control", "audio process only accepts stop");
    return true;
  };
  auto ready = [&writer]() {
    return writer.WriteStatus("{\"state\":\"active\",\"audio\":true}");
  };
  auto pcm = [&writer](UINT64 timestamp100ns, const BYTE* data, DWORD size) {
    return writer.Write(OutputKind::pcm, 0, timestamp100ns,
                        piik::capture::kAudioChunkDuration100ns, data,
                        size);
  };
  if (arguments.mode == ProductArguments::Mode::microphone) {
    return piik::capture::CaptureMicrophone(arguments.microphone_device, stop.get(), should_stop, ready, pcm);
  }
  if (arguments.target_kind == piik::capture::TargetKind::display) {
    return piik::capture::CaptureSystemAudio(
        stop.get(), should_stop, ready, pcm);
  }
  return piik::capture::CaptureProcessAudio(
      arguments.pid, arguments.creation_time, arguments.exclude_audio, stop.get(),
      should_stop,
      ready, pcm);
}

}  // namespace piik::capture::windows
