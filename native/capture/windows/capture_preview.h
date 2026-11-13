#pragma once

#include "capture_target.h"

#include <windows.h>

#include <vector>

namespace piik::capture {

// One BMP preview of the current source frame; WGC, DWM and GDI fallbacks
// share the entry point.
HRESULT CapturePreview(TargetKind kind, UINT64 source_id,
                       std::vector<BYTE>* output);

}  // namespace piik::capture
