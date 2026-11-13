import type { QualityProfile } from "../../media/quality";
import type { BrowserEncodingPool, BrowserPooledSender } from "../../media/browser-encoding-pool";
import { BrowserEncodingOutput, encodedStreams } from "../../media/browser-encoding-output";
import { debugError, debugEvent } from "../../lib/debug";
import {
  applyVideoCaptureProfile,
  configureAudioSender,
  configureVideoSender,
  needsStartupVideoProfile,
  screenAudioBitrate,
  audioSenderCeiling,
  audioQualitySettingsEqual,
  startupVideoProfile,
  videoQualitySettingsEqual,
  type AudioSenderParameterReadback,
} from "../../media/quality";
import { stopSenderVideoTrack } from "../../media/sender-video-track";
import { EMPTY_METRICS } from "../../types";
import { createStatsAccumulator } from "../stats";
import { applyVideoCodecPreference } from "../video-codec";
import { HostPeerConnection } from "./connection";
import type { HostPeerEvents, PeerIceConfig } from "./types";
import type { BrowserVideoCodecPreference } from "../video-codec";

/**
 * Host edge sender plumbing: capture clones, transceivers, encoded streams,
 * the encoding pool and sender quality parameters.
 */
export abstract class HostPeerSender extends HostPeerConnection {
  protected replacementVideoTrack: MediaStreamTrack | null = null;
  protected replacementAudioTrack: MediaStreamTrack | null = null;
  protected videoSender: RTCRtpSender | null = null;
  protected pooledVideo: BrowserPooledSender | null = null;
  private encodedOutput: BrowserEncodingOutput | null = null;
  private audioTransceiver: RTCRtpTransceiver | null = null;
  protected audioSender: RTCRtpSender | null = null;
  private appliedVideoProfile: QualityProfile | null = null;
  private appliedAudioBitrate: number | null = null;
  private appliedAudioSenderParameters:
    | AudioSenderParameterReadback
    | null = null;
  private senderMutationTail: Promise<void> = Promise.resolve();

  constructor(
    peerId: string,
    iceConfig: PeerIceConfig,
    stream: MediaStream,
    desiredProfile: QualityProfile,
    events: HostPeerEvents,
    videoCodec?: BrowserVideoCodecPreference,
    connectionId?: string,
    natPredictionEnabled?: boolean,
    videoPool?: BrowserEncodingPool | null,
  ) {
    super(peerId, iceConfig, stream, desiredProfile, events, videoCodec, connectionId, natPredictionEnabled, videoPool);
  }

  async start(): Promise<boolean> {
    const videoTrack = this.senderVideoTrack;
    if (!videoTrack) {
      this.setError(null, "host.capture.noSource");
      return false;
    }
    const audioTrack = this.stream.getAudioTracks()[0] ?? null;
    const videoTransceiver = this.connection.addTransceiver(videoTrack, {
      direction: "sendonly",
      streams: [this.stream],
    });
    if (!applyVideoCodecPreference(videoTransceiver, this.videoCodec)) {
      this.setError(null, "host.err.codecUnsupported");
      return false;
    }
    this.videoSender = videoTransceiver.sender;
    if (this.encodedStreamsEnabled) {
      this.encodedOutput = new BrowserEncodingOutput(this.videoSender, () => {
        this.fail();
      }, { connectionId: this.connectionId, peerId: this.peerId });
      this.encodedOutput.setPaused(this.paused);
    }
    this.attachVideoPool(this.stream.getVideoTracks()[0]!);
    this.audioTransceiver = this.connection.addTransceiver(audioTrack ?? "audio", {
      direction: audioTrack ? "sendonly" : "inactive",
      streams: [this.stream],
    });
    this.audioSender = this.audioTransceiver.sender;
    if (this.encodedStreamsEnabled) {
      // The connection flag also owns audio, including a later source with audio.
      const audio = encodedStreams(this.audioSender);
      void audio.readable.pipeTo(audio.writable).catch((error) => this.fail(error));
    }
    await this.enqueueSenderMutation(async () => {
      if (this.disposed || !this.videoSender || !this.audioSender) {
        return false;
      }
      return this.configureSender(this.videoSender, this.audioSender, {
        profile: startupVideoProfile(this.desiredProfile),
        profileRevision: this.profileRevision,
        video: true,
        audio: true,
      });
    });
    if (!(await this.createOffer()) || this.disposed) {
      return false;
    }
    this.startStatsSampling();
    this.emit();
    return true;
  }

  async replaceStream(nextStream: MediaStream): Promise<boolean> {
    const nextSourceVideoTrack = nextStream.getVideoTracks()[0];
    if (this.disposed || !nextSourceVideoTrack) {
      return false;
    }

    return this.enqueueSenderMutation(async () => {
      const videoSender = this.videoSender;
      const audioSender = this.audioSender;
      const audioTransceiver = this.audioTransceiver;
      const previousVideoTrack = this.senderVideoTrack;
      if (
        this.disposed ||
        !videoSender ||
        !audioSender ||
        !audioTransceiver ||
        !previousVideoTrack
      ) {
        return false;
      }

      const videoChanged = this.stream.getVideoTracks()[0] !== nextSourceVideoTrack;
      const nextVideoTrack = videoChanged ? this.cloneVideoTrack(nextSourceVideoTrack) : previousVideoTrack;
      let retainedNextVideoTrack = false;
      const nextAudioTrack = nextStream.getAudioTracks()[0] ?? null;
      const nextAudioDirection = nextAudioTrack ? "sendonly" : "inactive";
      const audioDirectionChanged =
        audioTransceiver.direction !== nextAudioDirection;
      const previousAudioDirection = audioTransceiver.direction;
      this.replacementVideoTrack = nextVideoTrack;
      this.replacementAudioTrack = nextAudioTrack;
      this.applyPausedState(nextVideoTrack, nextAudioTrack);
      const previousAudioTrack = audioSender.track;
      if (videoChanged) {
        this.statsSamplingBlocked = true;
        this.statsAccumulator = createStatsAccumulator();
      }

      try {
        try {
          if (videoChanged) await this.waitForOperation(() => videoSender.replaceTrack(nextVideoTrack));
          await this.waitForOperation(() => audioSender.replaceTrack(nextAudioTrack));
          if (nextVideoTrack.readyState === "ended") throw new Error("Replacement video track ended");
          if (audioDirectionChanged) {
            audioTransceiver.direction = nextAudioDirection;
          }
        } catch (error) {
          if (this.disposed) return false;
          const [videoRollback] = await Promise.allSettled([
            videoChanged ? this.waitForOperation(() => videoSender.replaceTrack(previousVideoTrack)) : Promise.resolve(),
            this.waitForOperation(() => audioSender.replaceTrack(previousAudioTrack)),
          ]);
          if (this.disposed) return false;
          if (audioDirectionChanged) {
            audioTransceiver.direction = previousAudioDirection;
          }
          if (videoRollback.status === "rejected") {
            this.fail(videoRollback.reason);
          }
          this.setError(error, "host.fail.source");
          return false;
        }

        if (this.disposed) {
          return false;
        }
        this.applyPausedState(nextVideoTrack, nextAudioTrack);
        this.senderVideoTrack = nextVideoTrack;
        this.stream = nextStream;
        retainedNextVideoTrack = true;
        if (videoChanged) {
          this.pooledVideo?.dispose();
          this.pooledVideo = null;
          this.attachVideoPool(nextSourceVideoTrack);
          if (previousVideoTrack !== this.encodedOutput?.track) stopSenderVideoTrack(previousVideoTrack);
          this.startupVideoProfilePending = needsStartupVideoProfile(this.desiredProfile);
          this.startupFramesBaseline = null;
          this.snapshot = { ...this.snapshot, metrics: { ...EMPTY_METRICS } };
        }
        await this.configureSender(videoSender, audioSender, {
          profile: startupVideoProfile(this.desiredProfile),
          profileRevision: this.profileRevision,
          video: videoChanged && this.connection.connectionState === "connected",
          audio: true,
        });
        if (this.disposed) return false;
        this.snapshot = { ...this.snapshot, error: null };
        this.emit();
        return !audioDirectionChanged || (await this.createOffer());
      } finally {
        if (videoChanged && !retainedNextVideoTrack) {
          stopSenderVideoTrack(nextVideoTrack);
        }
        if (this.replacementVideoTrack === nextVideoTrack) {
          this.replacementVideoTrack = null;
          this.replacementAudioTrack = null;
        }
        if (videoChanged) {
          this.statsAccumulator = createStatsAccumulator();
          this.statsSamplingBlocked = false;
        }
      }
    });
  }

  updateProfile(profile: QualityProfile): Promise<boolean> {
    return this.updateOwnedProfile(profile, false);
  }

  updateCaptureProfile(profile: QualityProfile): Promise<boolean> {
    return this.updateOwnedProfile(profile, true);
  }

  setPaused(paused: boolean): void {
    this.paused = paused;
    this.encodedOutput?.setPaused(paused);
    this.pooledVideo?.setPaused(paused);
    this.applyPausedState(this.senderVideoTrack, this.audioSender?.track ?? null);
    this.applyPausedState(this.replacementVideoTrack, this.replacementAudioTrack);
  }

  /** Begin periodic metric sampling; the stats layer owns the timer. */
  protected abstract startStatsSampling(): void;

  private applyPausedState(
    videoTrack: MediaStreamTrack | null,
    audioTrack: MediaStreamTrack | null,
  ): void {
    if (videoTrack) videoTrack.enabled = !this.paused;
    if (audioTrack) audioTrack.enabled = !this.paused;
  }

  private updateOwnedProfile(
    profile: QualityProfile,
    updateCaptureConstraints: boolean,
  ): Promise<boolean> {
    if (this.disposed) {
      return Promise.resolve(false);
    }
    if (!needsStartupVideoProfile(profile)) {
      this.startupVideoProfilePending = false;
    }
    const effectiveVideoProfile = this.startupVideoProfilePending
      ? startupVideoProfile(profile)
      : profile;
    const changed =
      !videoQualitySettingsEqual(this.desiredProfile, profile) ||
      !audioQualitySettingsEqual(this.desiredProfile, profile);
    this.desiredProfile = profile;
    // Connection recovery replays the same intent, not a new user mutation.
    const requestedRevision = changed ? ++this.profileRevision : this.profileRevision;
    return this.enqueueSenderMutation(async () => {
      const videoSender = this.videoSender;
      const audioSender = this.audioSender;
      const videoTrack = this.senderVideoTrack;
      if (
        this.disposed ||
        !videoSender ||
        !audioSender ||
        !videoTrack ||
        requestedRevision !== this.profileRevision
      ) {
        return false;
      }
      if (updateCaptureConstraints && videoTrack !== this.encodedOutput?.track) {
        try {
          await this.waitForOperation(() => applyVideoCaptureProfile(videoTrack, profile));
        } catch (error) {
          if (this.disposed) return false;
          throw error;
        }
        if (
          this.disposed ||
          this.senderVideoTrack !== videoTrack ||
          requestedRevision !== this.profileRevision
        ) {
          return false;
        }
      }
      const updateVideo =
        this.connection.connectionState === "connected" &&
        (this.appliedVideoProfile === null ||
          !videoQualitySettingsEqual(
            this.appliedVideoProfile,
            effectiveVideoProfile,
          ));
      const updateAudio =
        this.appliedAudioBitrate !==
          screenAudioBitrate(profile.maxAudioBitrate);
      if (!updateVideo && !updateAudio) {
        return true;
      }
      if (
        !(await this.configureSender(videoSender, audioSender, {
          profile: effectiveVideoProfile,
          profileRevision: requestedRevision,
          video: updateVideo,
          audio: updateAudio,
        }))
      ) {
        return false;
      }
      if (this.disposed || requestedRevision !== this.profileRevision) {
        return false;
      }
      if (updateVideo) {
        this.statsAccumulator = createStatsAccumulator();
      }
      this.snapshot = { ...this.snapshot, error: null };
      this.emit();
      return true;
    });
  }

  private attachVideoPool(source: MediaStreamTrack): void {
    const sender = this.videoSender;
    if (!sender || !this.videoPool || !this.encodedOutput) return;
    let binding: BrowserPooledSender | null = null;
    const owns = () => !this.disposed && this.pooledVideo === binding &&
      this.videoSender === sender && this.stream.getVideoTracks()[0] === source;
    const requestKey = () => this.enqueueSenderMutation(async () => {
      if (!owns()) return;
      const request = sender.setParameters as (parameters: RTCRtpSendParameters,
        options: { encodingOptions: Array<{ keyFrame: boolean }> }) => Promise<void>;
      await this.waitForOperation(() => request.call(sender, sender.getParameters(), { encodingOptions: [{ keyFrame: true }] }));
    });
    binding = this.videoPool.create(source, sender, this.connection, this.desiredProfile, this.encodedOutput,
      () => this.enqueueSenderMutation(async () => {
        if (!owns()) return false;
        const previous = this.senderVideoTrack!;
        const next = binding?.carrierScale() === undefined ? this.cloneVideoTrack(source) : this.encodedOutput!.track;
        try {
          this.applyPausedState(next, null);
          // The producer already limits real frames. Filtering its requested
          // canvas clock frames again leaves encoded frames waiting for a tick.
          if (next !== this.encodedOutput?.track) {
            await this.waitForOperation(() => applyVideoCaptureProfile(next, this.desiredProfile));
          }
          if (!owns()) { if (next !== this.encodedOutput?.track) stopSenderVideoTrack(next); return false; }
          if (next !== previous) await this.waitForOperation(() => sender.replaceTrack(next));
          await this.waitForOperation(() => configureVideoSender(sender,
            this.startupVideoProfilePending ? startupVideoProfile(this.desiredProfile) : this.desiredProfile,
            binding?.carrierScale()));
          if (!owns()) throw new DOMException("Retired Browser pool binding", "AbortError");
          if (next.readyState === "ended") throw new Error("Replacement video track ended");
          this.senderVideoTrack = next;
          if (previous !== next && previous !== this.encodedOutput?.track) stopSenderVideoTrack(previous);
          this.statsAccumulator = createStatsAccumulator();
          return true;
        } catch (error) {
          if (!this.disposed && sender.track !== previous) {
            try { await this.waitForOperation(() => sender.replaceTrack(previous)); } catch (rollbackError) { this.fail(rollbackError); }
          }
          if (next !== previous && next !== this.encodedOutput?.track) stopSenderVideoTrack(next);
          if (this.disposed) return false;
          debugError("encoding-pool", "carrier-attachment-failed", error, { connectionId: this.connectionId });
          return false;
        }
      }),
      requestKey,
      () => { if (owns()) this.fail(); });
    this.pooledVideo = binding;
    if (!binding) this.encodedOutput.passthrough(requestKey);
    binding?.setPaused(this.paused);
  }

  protected enqueueSenderMutation<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.senderMutationTail.then(operation);
    this.senderMutationTail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  protected retireSenderResources(): void {
    this.pooledVideo?.dispose();
    this.pooledVideo = null;
    this.encodedOutput?.dispose();
    this.encodedOutput = null;
    stopSenderVideoTrack(this.senderVideoTrack);
    this.senderVideoTrack = null;
    this.audioTransceiver = null;
  }

  protected replayProfileAfterReconnect(replay: boolean): void {
    if (replay) {
      this.appliedVideoProfile = null;
      this.appliedAudioBitrate = null;
      this.appliedAudioSenderParameters = null;
    }
    if (replay || !this.startupVideoProfilePending) {
      void this.updateProfile(this.desiredProfile);
    }
  }

  protected reconcileNegotiatedAudio(): void {
    const audio = this.audioSender;
    if (audio?.track && audioSenderCeiling(audio.getParameters(),
      screenAudioBitrate(this.desiredProfile.maxAudioBitrate)) !== audio.getParameters().encodings[0]?.maxBitrate) {
      // Never await sender work inside negotiation: source replacement can
      // already be waiting for this negotiation queue to finish.
      void this.enqueueSenderMutation(async () => {
        if (this.disposed || !this.videoSender || this.audioSender !== audio) return;
        const revision = this.profileRevision;
        const applied = await this.configureSender(this.videoSender, audio, {
          profile: this.desiredProfile, profileRevision: revision, video: false, audio: true,
        });
        // The negotiated ceiling changed independently of the requested
        // bitrate. Keep the old readback, but let an explicit update retry.
        if (!applied && !this.disposed && revision === this.profileRevision) this.appliedAudioBitrate = null;
      }).catch(error => debugError("webrtc", "audio-parameters-failed", error, { connectionId: this.connectionId }));
    }
  }

  private async configureSender(
    sender: RTCRtpSender,
    audioSender?: RTCRtpSender,
    mutation: {
      profile: QualityProfile;
      profileRevision: number;
      video: boolean;
      audio: boolean;
    } = {
      profile: this.desiredProfile,
      profileRevision: this.profileRevision,
      video: true,
      audio: true,
    },
  ): Promise<boolean> {
    const { profile, profileRevision } = mutation;
    let senderParameters = this.snapshot.senderParameters ?? null;
    let audioSenderParameters = this.appliedAudioSenderParameters;
    let videoSucceeded = true;
    let audioSucceeded = true;

    if (mutation.video) {
      try {
        senderParameters = await this.waitForOperation(() => configureVideoSender(sender, profile, this.pooledVideo?.carrierScale()));
      } catch (error) {
        if (this.disposed) return false;
        videoSucceeded = false;
        debugError("webrtc", "sender-parameters-failed", error, { connectionId: this.connectionId, profileRevision, requested: profile });
      }
    }

    if (
      this.disposed ||
      profileRevision !== this.profileRevision ||
      (mutation.video && this.videoSender !== sender)
    ) {
      return false;
    }

    const pooledVideo = this.pooledVideo;
    if (mutation.video && videoSucceeded && pooledVideo) {
      try { await this.waitForOperation(() => pooledVideo.updateProfile(this.desiredProfile)); }
      catch { videoSucceeded = false; }
    }

    if (mutation.audio && audioSender) {
      if (audioSender.track) {
        try {
          audioSenderParameters = await this.waitForOperation(() => configureAudioSender(
            audioSender,
            screenAudioBitrate(profile.maxAudioBitrate),
          ));
        } catch (error) {
          if (this.disposed) return false;
          audioSucceeded = false;
          debugError("webrtc", "audio-parameters-failed", error, { connectionId: this.connectionId, profileRevision });
        }
      } else {
        audioSenderParameters = null;
      }
    }

    if (
      this.disposed ||
      profileRevision !== this.profileRevision ||
      (mutation.video && this.videoSender !== sender) ||
      (mutation.audio && this.audioSender !== audioSender)
    ) {
      return false;
    }
    debugEvent("webrtc", "sender-parameters", { connectionId: this.connectionId, profileRevision,
      requested: profile, videoSucceeded, audioSucceeded, senderParameters, audioSenderParameters });
    if (mutation.video && videoSucceeded) {
      this.appliedVideoProfile = profile;
    }
    if (mutation.audio && audioSucceeded) {
      this.appliedAudioBitrate = screenAudioBitrate(
        profile.maxAudioBitrate,
      );
      this.appliedAudioSenderParameters = audioSenderParameters;
    }
    this.snapshot = {
      ...this.snapshot,
      senderParameters,
      audioSenderParameters: this.appliedAudioSenderParameters,
    };
    this.emit();
    return videoSucceeded && audioSucceeded;
  }
}
