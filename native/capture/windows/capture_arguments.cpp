#include "capture_arguments.h"

#include "capture_control.h"
#include "video_bounds.h"

#include <algorithm>
#include <limits>
#include <string>
#include <vector>

// Product command-line grammar and its bounded validation.
namespace piik::capture::windows {
namespace {

using piik::capture::TargetKind;

TargetKind ParseTargetKind(const wchar_t* value) {
  const std::wstring kind(value);
  if (kind == L"window") return TargetKind::window;
  if (kind == L"display") return TargetKind::display;
  Fail("argument-target-kind", "capture target kind is unsupported");
}

void ParseOutputProfiles(ProductArguments& arguments, int first, int count, wchar_t** values) {
  for (int index = first; index < count; index += 5) {
    if (std::wstring(values[index]) != L"--output") Fail("argument-output", "output profile is missing");
    VideoProfile output = arguments.profile;
    output.width = ParseIndex(values[index + 1], "argument-output-width");
    output.height = ParseIndex(values[index + 2], "argument-output-height");
    output.frame_rate = ParseIndex(values[index + 3], "argument-output-fps");
    output.bit_rate = ParseIndex(values[index + 4], "argument-output-bitrate");
    if (!piik::capture::ValidVideoFormat(output.width, output.height, output.frame_rate) ||
        output.bit_rate < 1'000 || output.bit_rate > piik::capture::kMaxVideoBitrate) {
      Fail("argument-output", "output profile exceeds codec bounds");
    }
    if (arguments.mode == ProductArguments::Mode::video &&
        (output.width > arguments.profile.width || output.height > arguments.profile.height ||
         output.frame_rate > arguments.profile.frame_rate || output.bit_rate > arguments.profile.bit_rate)) {
      Fail("argument-output", "output profile exceeds source bounds");
    }
    arguments.outputs.push_back(output);
  }
}

}  // namespace

UINT64 ParseUint64(const wchar_t* value, const std::string& stage) {
  try {
    size_t consumed = 0;
    unsigned long long parsed = std::stoull(value, &consumed, 10);
    if (value[consumed] != L'\0' || parsed == 0) {
      Fail(stage, "value is not a positive bounded integer");
    }
    return static_cast<UINT64>(parsed);
  } catch (const std::exception&) {
    Fail(stage, "value is not a positive bounded integer");
  }
}

UINT64 ParseNonNegativeUint64(const wchar_t* value, const std::string& stage) {
  try {
    size_t consumed = 0;
    unsigned long long parsed = std::stoull(value, &consumed, 10);
    if (value[consumed] != L'\0') {
      Fail(stage, "value is not a bounded integer");
    }
    return static_cast<UINT64>(parsed);
  } catch (const std::exception&) {
    Fail(stage, "value is not a bounded integer");
  }
}

DegradationPreference ParseDegradationPreference(const wchar_t* value) {
  const std::wstring preference(value);
  if (preference == L"maintain-resolution") {
    return DegradationPreference::resolution;
  }
  if (preference == L"balanced") return DegradationPreference::balanced;
  if (preference == L"maintain-framerate") {
    return DegradationPreference::framerate;
  }
  Fail("argument-preference", "degradation preference is invalid");
}

void ValidateVideoProfile(const VideoProfile& profile) {
  if (!piik::capture::ValidVideoFormat(profile.width, profile.height, profile.frame_rate) ||
      profile.width < 4 || profile.height < 4 ||
      profile.bit_rate < 100'000 || profile.bit_rate > piik::capture::kMaxVideoBitrate) {
    Fail("argument-profile", "video profile is outside the product bounds");
  }
}

ProductArguments ParseProductArguments(int count, wchar_t** values) {
  ProductArguments arguments;
  if (count > 2 && std::wstring(values[1]) == L"--capture-video" &&
      std::wstring(values[count - 1]) == L"--show-capture-border") {
    arguments.show_capture_border = true;
    --count;
  }
  if (count == 2 && std::wstring(values[1]) == L"--list") return arguments;
  if (count == 2 && std::wstring(values[1]) == L"--probe") {
    arguments.mode = ProductArguments::Mode::probe;
    return arguments;
  }
  if (count == 6 && std::wstring(values[1]) == L"--preview") {
    arguments.mode = ProductArguments::Mode::preview;
    arguments.target_kind = ParseTargetKind(values[2]);
    arguments.source_id = ParseUint64(values[3], "argument-source");
    UINT64 pid = ParseNonNegativeUint64(values[4], "argument-pid");
    if (pid > std::numeric_limits<DWORD>::max()) {
      Fail("argument-pid", "PID is outside the Windows process range");
    }
    arguments.pid = static_cast<DWORD>(pid);
    arguments.creation_time =
        ParseNonNegativeUint64(values[5], "argument-creation-time");
    return arguments;
  }
  if (count == 2 && std::wstring(values[1]) == L"--list-microphones") {
    arguments.mode = ProductArguments::Mode::microphones;
    return arguments;
  }
  if ((count == 2 || (count == 4 && std::wstring(values[2]) == L"--device")) && std::wstring(values[1]) == L"--capture-microphone") {
    if (count == 4) {
      arguments.microphone_device = values[3];
      if (arguments.microphone_device.empty() || arguments.microphone_device.size() > 512) Fail("argument-device", "microphone device is invalid");
    }
    arguments.mode = ProductArguments::Mode::microphone;
    return arguments;
  }
  if (count == 5 && std::wstring(values[1]) == L"--capture-audio") {
    arguments.mode = ProductArguments::Mode::audio;
    arguments.exclude_audio = std::wstring(values[2]) == L"exclude";
    arguments.target_kind = arguments.exclude_audio ? piik::capture::TargetKind::window : ParseTargetKind(values[2]);
  } else if (count >= 16 && count <= 11 + 5 * piik::capture::kMaxOutputs && (count - 11) % 5 == 0 &&
             std::wstring(values[1]) == L"--encoded-video" &&
             std::wstring(values[2]) == L"--codec" &&
             std::wstring(values[4]) == L"--adapter-index" &&
             std::wstring(values[6]) == L"--mft-index" &&
             std::wstring(values[8]) == L"--preference" &&
             std::wstring(values[10]) == L"--protocol-v7") {
    arguments.mode = ProductArguments::Mode::encoded;
    arguments.codec = NarrowAscii(values[3]);
    if (arguments.codec != "h264" && arguments.codec != "vp8") Fail("argument-codec", "encoded input codec is unsupported");
    arguments.adapter_index = ParseIndex(values[5], "argument-adapter");
    arguments.mft_index = ParseIndex(values[7], "argument-mft");
    arguments.profile.preference = ParseDegradationPreference(values[9]);
    ParseOutputProfiles(arguments, 11, count, values);
    arguments.profile = arguments.outputs.front();
    for (const auto& output : arguments.outputs) {
      arguments.profile.width = std::max(arguments.profile.width, output.width);
      arguments.profile.height = std::max(arguments.profile.height, output.height);
      arguments.profile.frame_rate = std::max(arguments.profile.frame_rate, output.frame_rate);
      arguments.profile.bit_rate = std::max(arguments.profile.bit_rate, output.bit_rate);
    }
    return arguments;
  } else if (count >= 28 && count <= 23 + 5 * piik::capture::kMaxOutputs && (count - 23) % 5 == 0 &&
             std::wstring(values[1]) == L"--capture-video" &&
             std::wstring(values[6]) == L"--adapter-index" &&
             std::wstring(values[8]) == L"--mft-index" &&
             std::wstring(values[10]) == L"--width" &&
             std::wstring(values[12]) == L"--height" &&
             std::wstring(values[14]) == L"--fps" &&
             std::wstring(values[16]) == L"--bitrate" &&
             std::wstring(values[18]) == L"--preference" &&
             std::wstring(values[20]) == L"--codec" &&
             std::wstring(values[22]) == L"--protocol-v7") {
    arguments.mode = ProductArguments::Mode::video;
    arguments.target_kind = ParseTargetKind(values[2]);
    arguments.source_id = ParseUint64(values[3], "argument-source");
    arguments.adapter_index = ParseIndex(values[7], "argument-adapter");
    arguments.mft_index = ParseIndex(values[9], "argument-mft");
    arguments.profile.width = ParseIndex(values[11], "argument-width");
    arguments.profile.height = ParseIndex(values[13], "argument-height");
    arguments.profile.frame_rate = ParseIndex(values[15], "argument-fps");
    arguments.profile.bit_rate = ParseIndex(values[17], "argument-bitrate");
    arguments.profile.preference = ParseDegradationPreference(values[19]);
    const std::wstring codec(values[21]);
    if (codec != L"auto" && codec != L"h264" && codec != L"vp8") {
      Fail("argument-codec", "video codec is unsupported");
    }
    arguments.codec = NarrowAscii(codec);
    ValidateVideoProfile(arguments.profile);
    ParseOutputProfiles(arguments, 23, count, values);
    const auto& original = arguments.outputs[arguments.outputs.size() > 1 ? 1 : 0];
    if (original.width != arguments.profile.width || original.height != arguments.profile.height ||
        original.frame_rate != arguments.profile.frame_rate || original.bit_rate != arguments.profile.bit_rate) {
      Fail("argument-output-source", "original output must match the source profile");
    }
  } else {
    Fail("arguments", "unsupported or incomplete command-line argument");
  }
  const int pid_index = arguments.mode == ProductArguments::Mode::audio ? 3 : 4;
  const int creation_index = pid_index + 1;
  UINT64 pid = ParseNonNegativeUint64(values[pid_index], "argument-pid");
  if (pid > std::numeric_limits<DWORD>::max()) {
    Fail("argument-pid", "PID is outside the Windows process range");
  }
  arguments.pid = static_cast<DWORD>(pid);
  arguments.creation_time =
      ParseNonNegativeUint64(values[creation_index], "argument-creation-time");
  if (arguments.target_kind == piik::capture::TargetKind::window &&
      (arguments.pid == 0 || arguments.creation_time == 0)) {
    Fail("argument-window-identity", "window identity is incomplete");
  }
  if (arguments.target_kind == piik::capture::TargetKind::display &&
      (arguments.pid != 0 || arguments.creation_time != 0)) {
    Fail("argument-display-identity", "display identity is invalid");
  }
  return arguments;
}

}  // namespace piik::capture::windows
