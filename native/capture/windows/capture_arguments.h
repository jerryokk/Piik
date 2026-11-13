#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include "capture_error.h"
#include "capture_target.h"
#include "h264_encoder.h"

#include <limits>
#include <string>
#include <vector>

namespace piik::capture::windows {

// Kept inline: the fixture build compiles main.cpp alone and shares this parser.
inline UINT ParseIndex(const wchar_t* value, const std::string& stage) {
  try {
    size_t consumed = 0;
    unsigned long parsed = std::stoul(value, &consumed, 10);
    if (value[consumed] != L'\0' || parsed > std::numeric_limits<UINT>::max()) {
      Fail(stage, "index is not a bounded unsigned integer");
    }
    return static_cast<UINT>(parsed);
  } catch (const std::exception&) {
    Fail(stage, "index is not a bounded unsigned integer");
  }
}

struct ProductArguments final {
  enum class Mode { list, microphones, probe, preview, audio, microphone, video, encoded } mode = Mode::list;
  piik::capture::TargetKind target_kind =
      piik::capture::TargetKind::window;
  std::wstring microphone_device;
  DWORD pid = 0;
  UINT64 creation_time = 0;
  UINT64 source_id = 0;
  UINT adapter_index = 0;
  UINT mft_index = 0;
  std::string codec = "auto";
  VideoProfile profile;
  std::vector<VideoProfile> outputs;
  bool show_capture_border = false;
  bool exclude_audio = false;
};

UINT64 ParseUint64(const wchar_t* value, const std::string& stage);
UINT64 ParseNonNegativeUint64(const wchar_t* value, const std::string& stage);
DegradationPreference ParseDegradationPreference(const wchar_t* value);
void ValidateVideoProfile(const VideoProfile& profile);
ProductArguments ParseProductArguments(int count, wchar_t** values);

}  // namespace piik::capture::windows
