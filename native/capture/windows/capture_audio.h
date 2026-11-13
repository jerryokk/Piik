#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include "capture_arguments.h"

namespace piik::capture::windows {

HRESULT RunAudioCapture(const ProductArguments& arguments);

}  // namespace piik::capture::windows
