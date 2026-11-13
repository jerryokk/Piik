import { debugTrack } from "../../lib/debug/webrtc";
import { STARTUP_VIDEO_ENCODED_FRAMES } from "../../media/quality";
import {
  captureMetrics,
  collectConnectionMetrics,
} from "../stats";
import { HostPeerSender } from "./sender";

/**
 * Host edge quality reporting: periodic metric sampling, startup-profile
 * completion and the resulting snapshot updates.
 */
export abstract class HostPeerStats extends HostPeerSender {
  private statsTimer: number | null = null;
  private statsInFlight = false;

  protected startStatsSampling(): void {
    this.statsTimer = window.setInterval(() => {
      void this.updateStats();
    }, 2_000);
  }

  protected stopStatsSampling(): void {
    if (this.statsTimer !== null) {
      window.clearInterval(this.statsTimer);
      this.statsTimer = null;
    }
  }

  private async updateStats(): Promise<void> {
    if (
      this.disposed ||
      this.connection.connectionState === "closed" ||
      this.statsInFlight ||
      this.statsSamplingBlocked
    ) {
      return;
    }
    const captureTrack = this.senderVideoTrack;
    const captureAudioTrack = this.audioSender?.track ?? null;
    if (
      !captureTrack ||
      this.videoSender?.track !== captureTrack ||
      (this.stream.getAudioTracks()[0] ?? null) !== captureAudioTrack
    ) {
      return;
    }
    this.statsInFlight = true;
    const statsAccumulator = this.statsAccumulator;
    try {
      const metricsPromise = this.waitForOperation(() => collectConnectionMetrics(
        this.connection,
        "send",
        statsAccumulator,
        {
          trackIdentifier: captureTrack.id,
          audioTrackIdentifier: captureAudioTrack?.id ?? null,
        },
      ));
      const capture = captureMetrics(captureTrack);
      debugTrack(captureTrack, { connectionId: this.connectionId, event: "sample" });
      const metrics = { ...(await metricsPromise), ...capture };
      if (
        this.disposed ||
        this.statsSamplingBlocked ||
        this.statsAccumulator !== statsAccumulator ||
        this.senderVideoTrack !== captureTrack ||
        this.videoSender?.track !== captureTrack ||
        (this.audioSender?.track ?? null) !== captureAudioTrack ||
        (this.stream.getAudioTracks()[0] ?? null) !== captureAudioTrack ||
        (metrics.trackIdentifier !== null &&
          metrics.trackIdentifier !== captureTrack.id)
      ) {
        return;
      }
      const encodedFrames = statsAccumulator.frames;
      if (this.startupVideoProfilePending && encodedFrames !== null) {
        if (this.startupFramesBaseline === null) {
          this.startupFramesBaseline = encodedFrames;
        } else if (
          encodedFrames - this.startupFramesBaseline >=
          STARTUP_VIDEO_ENCODED_FRAMES
        ) {
          this.startupVideoProfilePending = false;
          void this.updateProfile(this.desiredProfile);
        }
      }
      const deliveredMetrics = this.pooledVideo?.metrics(metrics) ?? metrics;
      this.snapshot = { ...this.snapshot, metrics: deliveredMetrics };
      this.emit();
    } catch {
      // Stats are observational and must never disrupt a healthy media path.
    } finally {
      this.statsInFlight = false;
    }
  }
}
