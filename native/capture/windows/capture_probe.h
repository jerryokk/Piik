#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <chrono>
#include <condition_variable>
#include <functional>
#include <memory>
#include <mutex>
#include <thread>

namespace piik::capture::windows {

UINT32 WindowsBuild();

// Optional WASAPI activation must not consume the helper's entire discovery
// budget. These three workers belong to this one-shot probe process. Each owns
// its COM apartment and shared results even if a driver returns after Snapshot.
class AudioCapabilityProbe final {
 public:
  using Clock = std::chrono::steady_clock;
  struct Capabilities final {
    bool process = false;
    bool system = false;
    bool exclusion = false;
  };
  explicit AudioCapabilityProbe(Clock::time_point deadline, std::function<bool(bool)> process,
                                std::function<bool()> system);
  AudioCapabilityProbe();

  Capabilities Snapshot() const;

 private:
  struct State;
  void Start(std::function<void(const std::shared_ptr<State>&)> query);

  const std::shared_ptr<State> state_;
  const Clock::time_point deadline_;
};

// Capability JSON is owned here; live capture and the build check share it.
void WriteCapabilityProbe(AudioCapabilityProbe audio = AudioCapabilityProbe());

}  // namespace piik::capture::windows
