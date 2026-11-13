#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include "capture_arguments.h"
#include "capture_encoder.h"

#include <vector>

namespace piik::capture::windows {

// Product video entry points: live WGC capture and encoded-input relay runs.
void RunEncodedVideo(ProductArguments arguments);
void RunVideoCapture(ProductArguments arguments);

}  // namespace piik::capture::windows
