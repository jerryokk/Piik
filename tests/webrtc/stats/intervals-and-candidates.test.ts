import { afterEach, describe, expect, it } from "vitest";
import { collectConnectionMetrics, collectConnectionMetricsFromReport, createStatsAccumulator } from "../../../src/client/webrtc/stats";
import { restoreClientState } from "../../support/client-state.ts";

afterEach(restoreClientState);
describe("WebRTC stats parsing", () => {
  it("rebases sender retransmission deltas after counter rollback", async () => {
    const outbound = (
      timestamp: number,
      retransmittedPacketsSent: number,
      retransmittedBytesSent: number,
      packetsSent: number,
      packetsLost: number,
    ) =>
      new Map<string, unknown>([
        [
          "outbound",
          {
            id: "outbound",
            type: "outbound-rtp",
            timestamp,
            kind: "video",
            ssrc: 101,
            remoteId: "remote-inbound",
            bytesSent: timestamp * 10,
            framesEncoded: timestamp / 10,
            packetsSent,
            retransmittedPacketsSent,
            retransmittedBytesSent,
          },
        ],
        [
          "remote-inbound",
          {
            id: "remote-inbound",
            type: "remote-inbound-rtp",
            timestamp,
            kind: "video",
            packetsLost,
          },
        ],
      ]) as unknown as RTCStatsReport;
    const reports = [
      outbound(1_000, 2, 200, 100, 1),
      outbound(2_000, 5, 800, 400, 3),
      outbound(3_000, 1, 100, 50, 1),
      outbound(4_000, 4, 700, 350, 4),
    ];
    const connection = {
      getStats: async () => reports.shift()!,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();
    const sample = () =>
      collectConnectionMetrics(connection, "send", accumulator);
    const first = await sample();
    const stable = await sample();
    const reset = await sample();
    const afterReset = await sample();

    expect(first.intervalRetransmittedPackets).toBeNull();
    expect(stable).toMatchObject({
      intervalPacketsSent: 300,
      intervalPacketsLost: 2,
      intervalRetransmittedPackets: 3,
      intervalRetransmittedBytes: 600,
    });
    expect(reset).toMatchObject({
      intervalPacketsSent: null,
      intervalPacketsLost: null,
      intervalRetransmittedPackets: null,
      intervalRetransmittedBytes: null,
    });
    expect(afterReset).toMatchObject({
      intervalPacketsSent: 300,
      intervalPacketsLost: 3,
      intervalRetransmittedPackets: 3,
      intervalRetransmittedBytes: 600,
    });
  });

  it("keeps an out-of-contract relay candidate unclassified", async () => {
    const report = new Map<string, Record<string, unknown>>([
      [
        "transport",
        {
          id: "transport",
          type: "transport",
          timestamp: 2_000,
          selectedCandidatePairId: "pair",
        },
      ],
      [
        "pair",
        {
          id: "pair",
          type: "candidate-pair",
          timestamp: 2_000,
          transportId: "transport",
          state: "succeeded",
          nominated: true,
          localCandidateId: "local",
          remoteCandidateId: "remote",
        },
      ],
      [
        "local",
        {
          id: "local",
          type: "local-candidate",
          timestamp: 2_000,
          candidateType: "relay",
          protocol: "udp",
        },
      ],
      [
        "remote",
        {
          id: "remote",
          type: "remote-candidate",
          timestamp: 2_000,
          candidateType: "host",
          protocol: "udp",
        },
      ],
      [
        "inbound",
        {
          id: "inbound",
          type: "inbound-rtp",
          timestamp: 2_000,
          kind: "video",
          transportId: "transport",
          bytesReceived: 2_000,
          framesDecoded: 100,
          totalDecodeTime: 0.5,
        },
      ],
    ]);
    const secondReport = new Map(report);
    secondReport.set("inbound", {
      id: "inbound",
      type: "inbound-rtp",
      timestamp: 4_000,
      kind: "video",
      transportId: "transport",
      bytesReceived: 4_000,
      framesDecoded: 150,
      totalDecodeTime: 1.5,
    });
    const reports = [report, secondReport];
    const connection = {
      getStats: async () =>
        reports.shift() as unknown as RTCStatsReport,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();

    const firstMetrics = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const metrics = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );

    expect(firstMetrics.intervalDecodeMs).toBeNull();
    expect(metrics).toMatchObject({
      path: "unknown",
      iceProtocol: "udp",
      localCandidateType: "relay",
      remoteCandidateType: "host",
      intervalDecodeMs: 20,
    });
  });

  it("rebases interval metrics when the RTP stream or its counters reset", async () => {
    const inbound = (
      id: string,
      timestamp: number,
      bytesReceived: number,
      framesDecoded: number,
      totalDecodeTime: number,
    ) =>
      new Map<string, unknown>([
        [
          id,
          {
            id,
            type: "inbound-rtp",
            timestamp,
            kind: "video",
            bytesReceived,
            framesDecoded,
            totalDecodeTime,
          },
        ],
      ]) as unknown as RTCStatsReport;
    const reports = [
      inbound("inbound-a", 1_000, 10_000, 100, 0.5),
      inbound("inbound-b", 2_000, 20_000, 150, 1.5),
      inbound("inbound-b", 3_000, 22_000, 160, 1.7),
      inbound("inbound-b", 4_000, 1_000, 10, 0.1),
      inbound("inbound-b", 5_000, 3_000, 20, 0.3),
    ];
    const connection = {
      getStats: async () => reports.shift()!,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();

    await collectConnectionMetrics(connection, "receive", accumulator);
    const streamChanged = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const afterStreamChange = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const countersReset = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const afterCounterReset = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );

    expect(streamChanged).toMatchObject({
      bitrateKbps: null,
      framesPerSecond: null,
      intervalDecodeMs: null,
    });
    expect(afterStreamChange).toMatchObject({
      bitrateKbps: 16,
      framesPerSecond: 10,
    });
    expect(afterStreamChange.intervalDecodeMs).toBeCloseTo(20);
    expect(countersReset).toMatchObject({
      bitrateKbps: null,
      framesPerSecond: null,
      intervalDecodeMs: null,
    });
    expect(afterCounterReset).toMatchObject({
      bitrateKbps: 16,
      framesPerSecond: 10,
    });
    expect(afterCounterReset.intervalDecodeMs).toBeCloseTo(20);
  });

  it("rebases receiver event deltas on SSRC changes and counter resets", async () => {
    const inbound = (
      timestamp: number,
      ssrc: number,
      framesDropped: number,
      freezeCount: number,
      totalFreezesDuration: number,
      pauseCount: number,
      totalPausesDuration: number,
      retransmittedPacketsReceived: number,
      retransmittedBytesReceived: number,
    ) =>
      new Map<string, unknown>([
        [
          "inbound",
          {
            id: "inbound",
            type: "inbound-rtp",
            timestamp,
            kind: "video",
            ssrc,
            bytesReceived: timestamp * 10,
            framesDecoded: timestamp / 10,
            framesDropped,
            freezeCount,
            totalFreezesDuration,
            pauseCount,
            totalPausesDuration,
            retransmittedPacketsReceived,
            retransmittedBytesReceived,
          },
        ],
      ]) as unknown as RTCStatsReport;
    const reports = [
      inbound(1_000, 101, 2, 1, 0.25, 0, 0, 3, 300),
      inbound(2_000, 101, 5, 2, 2.75, 1, 5.5, 7, 900),
      inbound(3_000, 202, 20, 8, 4, 3, 16, 30, 4_000),
      inbound(4_000, 202, 22, 9, 6.25, 4, 21.5, 33, 4_600),
      inbound(5_000, 202, 1, 0, 0.25, 0, 0.25, 2, 200),
      inbound(6_000, 202, 4, 1, 2.5, 1, 6.25, 6, 1_000),
    ];
    const connection = {
      getStats: async () => reports.shift()!,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();
    const sample = () =>
      collectConnectionMetrics(connection, "receive", accumulator);
    const first = await sample();
    const stable = await sample();
    const changedSsrc = await sample();
    const afterSsrcChange = await sample();
    const reset = await sample();
    const afterReset = await sample();

    expect(first).toMatchObject({
      sampleWindowMs: null,
      intervalFramesDropped: null,
      intervalFreezeCount: null,
      intervalFreezeDurationMs: null,
      intervalPauseCount: null,
      intervalPauseDurationMs: null,
      intervalRetransmittedPackets: null,
      intervalRetransmittedBytes: null,
    });
    expect(stable).toMatchObject({
      sampleWindowMs: 1_000,
      intervalFramesDropped: 3,
      intervalFreezeCount: 1,
      intervalFreezeDurationMs: 2_500,
      intervalPauseCount: 1,
      intervalPauseDurationMs: 5_500,
      intervalRetransmittedPackets: 4,
      intervalRetransmittedBytes: 600,
    });
    expect(changedSsrc).toMatchObject({
      rtpSsrc: 202,
      sampleWindowMs: null,
      intervalFramesDropped: null,
      intervalFreezeCount: null,
      intervalFreezeDurationMs: null,
      intervalPauseCount: null,
      intervalPauseDurationMs: null,
      intervalRetransmittedPackets: null,
      intervalRetransmittedBytes: null,
    });
    expect(afterSsrcChange).toMatchObject({
      sampleWindowMs: 1_000,
      intervalFramesDropped: 2,
      intervalFreezeCount: 1,
      intervalFreezeDurationMs: 2_250,
      intervalPauseCount: 1,
      intervalPauseDurationMs: 5_500,
      intervalRetransmittedPackets: 3,
      intervalRetransmittedBytes: 600,
    });
    expect(reset).toMatchObject({
      sampleWindowMs: 1_000,
      intervalFramesDropped: null,
      intervalFreezeCount: null,
      intervalFreezeDurationMs: null,
      intervalPauseCount: null,
      intervalPauseDurationMs: null,
      intervalRetransmittedPackets: null,
      intervalRetransmittedBytes: null,
    });
    expect(afterReset).toMatchObject({
      sampleWindowMs: 1_000,
      intervalFramesDropped: 3,
      intervalFreezeCount: 1,
      intervalFreezeDurationMs: 2_250,
      intervalPauseCount: 1,
      intervalPauseDurationMs: 6_000,
      intervalRetransmittedPackets: 4,
      intervalRetransmittedBytes: 800,
    });
  });

  it("keeps unavailable pause counters unknown and receive-only", async () => {
    const report = (
      type: "inbound-rtp" | "outbound-rtp",
      timestamp: number,
    ) =>
      new Map<string, unknown>([
        [
          "video",
          {
            id: "video",
            type,
            timestamp,
            kind: "video",
            ssrc: 101,
            ...(type === "inbound-rtp"
              ? { bytesReceived: timestamp, framesDecoded: timestamp / 100 }
              : {
                  bytesSent: timestamp,
                  framesEncoded: timestamp / 100,
                  pauseCount: timestamp / 1_000,
                  totalPausesDuration: timestamp / 1_000,
                }),
          },
        ],
      ]) as unknown as RTCStatsReport;

    const receiveAccumulator = createStatsAccumulator();
    collectConnectionMetricsFromReport(
      report("inbound-rtp", 1_000),
      "receive",
      receiveAccumulator,
    );
    const receive = collectConnectionMetricsFromReport(
      report("inbound-rtp", 2_000),
      "receive",
      receiveAccumulator,
    );
    expect(receive).toMatchObject({
      intervalPauseCount: null,
      intervalPauseDurationMs: null,
    });

    const sendAccumulator = createStatsAccumulator();
    collectConnectionMetricsFromReport(
      report("outbound-rtp", 1_000),
      "send",
      sendAccumulator,
    );
    const send = collectConnectionMetricsFromReport(
      report("outbound-rtp", 2_000),
      "send",
      sendAccumulator,
    );
    expect(send).toMatchObject({
      intervalPauseCount: null,
      intervalPauseDurationMs: null,
    });
  });

  it("derives a bounded inbound C window from adjacent RTP counters", async () => {
    const inbound = (
      timestamp: number,
      packetsReceived: number,
      packetsLost: number,
      framesDecoded: number,
      totalDecodeTime: number,
    ) =>
      new Map<string, unknown>([
        [
          "transport",
          { id: "transport", type: "transport", timestamp },
        ],
        [
          "codec",
          {
            id: "codec",
            type: "codec",
            timestamp,
            transportId: "transport",
            mimeType: "video/H264",
            sdpFmtpLine:
              "profile-level-id=42e01f; packetization-mode=1; " +
              "sprop-parameter-sets=must-not-leave-stats",
          },
        ],
        [
          "inbound",
          {
            id: "inbound",
            type: "inbound-rtp",
            timestamp,
            kind: "video",
            ssrc: 101,
            transportId: "transport",
            codecId: "codec",
            bytesReceived: timestamp * 10,
            packetsReceived,
            packetsLost,
            framesDecoded,
            framesDropped: 3,
            freezeCount: 1,
            totalFreezesDuration: 0.25,
            totalDecodeTime,
            frameWidth: 1_920,
            frameHeight: 1_080,
            scalabilityMode: "L3T3_KEY",
          },
        ],
      ]) as unknown as RTCStatsReport;
    const reports = [
      inbound(1_000, 1_000, 10, 60, 0.12),
      inbound(3_000, 2_500, 12, 180, 0.42),
      inbound(5_000, 4_000, 11, 300, 0.72),
    ];
    const connection = {
      getStats: async () => reports.shift()!,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();

    await collectConnectionMetrics(connection, "receive", accumulator);
    const stable = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const correctedLoss = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );

    expect(stable).toMatchObject({
      sampleWindowMs: 2_000,
      frameWidth: 1_920,
      frameHeight: 1_080,
      intervalPacketsReceived: 1_500,
      intervalPacketsLost: 2,
      packetLossPercent: (2 / 1_502) * 100,
      intervalFramesDecoded: 120,
      intervalDecodeMs: 2.5,
      codec: "video/H264",
      codecProfile: "profile-level-id=42e01f",
      codecParameters: "packetization-mode=1",
      scalabilityMode: null,
    });
    expect(JSON.stringify(stable)).not.toContain("sprop-parameter-sets");
    expect(correctedLoss).toMatchObject({
      intervalPacketsLost: null,
      packetLossPercent: null,
    });
  });

  it("reports verified inbound audio codec and interval transport facts", async () => {
    const report = (
      timestamp: number,
      videoPacketsReceived: number,
      videoPacketsLost: number,
      audioBytesReceived: number,
      audioPacketsReceived: number,
      audioPacketsLost: number,
      audioCodecTransportId = "transport",
    ) =>
      new Map<string, unknown>([
        [
          "transport",
          { id: "transport", type: "transport", timestamp },
        ],
        [
          "video-codec",
          {
            id: "video-codec",
            type: "codec",
            timestamp,
            transportId: "transport",
            mimeType: "video/VP8",
          },
        ],
        [
          "audio-codec",
          {
            id: "audio-codec",
            type: "codec",
            timestamp,
            transportId: audioCodecTransportId,
            mimeType: "audio/opus",
            clockRate: 48_000,
            channels: 2,
            sdpFmtpLine: "minptime=10;useinbandfec=1",
          },
        ],
        [
          "video-inbound",
          {
            id: "video-inbound",
            type: "inbound-rtp",
            timestamp,
            kind: "video",
            ssrc: 101,
            transportId: "transport",
            codecId: "video-codec",
            bytesReceived: timestamp * 10,
            packetsReceived: videoPacketsReceived,
            packetsLost: videoPacketsLost,
            framesDecoded: timestamp / 20,
          },
        ],
        [
          "audio-inbound",
          {
            id: "audio-inbound",
            type: "inbound-rtp",
            timestamp,
            kind: "audio",
            ssrc: 202,
            trackIdentifier: "shared-audio",
            transportId: "transport",
            codecId: "audio-codec",
            bytesReceived: audioBytesReceived,
            packetsReceived: audioPacketsReceived,
            packetsLost: audioPacketsLost,
            jitter: 0.003,
          },
        ],
      ]) as unknown as RTCStatsReport;
    const reports = [
      report(1_000, 100, 5, 10_000, 1_000, 5),
      report(3_000, 190, 15, 50_000, 1_180, 10),
      report(5_000, 190, 15, 50_000, 1_180, 10, "other-transport"),
    ];
    const connection = {
      getStats: async () => reports.shift()!,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();

    const first = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const stable = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const zeroDenominator = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );

    expect(first).toMatchObject({
      packetLossPercent: null,
      audioBitrateKbps: null,
      audioPacketLossPercent: null,
      audioJitterMs: 3,
      audioCodec: "audio/opus",
      audioCodecClockRate: 48_000,
      audioCodecChannels: 2,
      audioCodecParameters: "minptime=10;useinbandfec=1",
    });
    expect(stable.packetLossPercent).toBeCloseTo(10);
    expect(stable.audioBitrateKbps).toBeCloseTo(160);
    expect(stable.audioPacketLossPercent).toBeCloseTo((5 / 185) * 100);
    expect(zeroDenominator).toMatchObject({
      packetLossPercent: null,
      audioBitrateKbps: 0,
      audioPacketLossPercent: null,
      audioCodec: null,
      audioCodecClockRate: null,
      audioCodecChannels: null,
      audioCodecParameters: null,
    });
  });

  it("derives local inbound playout, jitter-buffer, and concealment evidence safely", async () => {
    const report = (
      timestamp: number,
      video: Record<string, unknown> = {},
      audio: Record<string, unknown> = {},
    ) =>
      new Map<string, unknown>([
        [
          "video-inbound",
          {
            id: "video-inbound",
            type: "inbound-rtp",
            timestamp,
            kind: "video",
            ssrc: 101,
            trackIdentifier: "shared-video",
            bytesReceived: timestamp * 10,
            framesDecoded: timestamp / 20,
            ...video,
          },
        ],
        [
          "audio-inbound",
          {
            id: "audio-inbound",
            type: "inbound-rtp",
            timestamp,
            kind: "audio",
            ssrc: 202,
            trackIdentifier: "shared-audio",
            bytesReceived: timestamp * 2,
            ...audio,
          },
        ],
      ]) as unknown as RTCStatsReport;
    const reports = [
      report(
        1_000,
        {
          estimatedPlayoutTimestamp: 10_000,
          jitterBufferDelay: 1.2,
          jitterBufferEmittedCount: 60,
        },
        {
          estimatedPlayoutTimestamp: 10_012.5,
          jitterBufferDelay: 960,
          jitterBufferEmittedCount: 48_000,
          totalSamplesReceived: 48_000,
          concealedSamples: 480,
          concealmentEvents: 2,
        },
      ),
      report(
        3_000,
        {
          estimatedPlayoutTimestamp: 12_000,
          jitterBufferDelay: 4.2,
          jitterBufferEmittedCount: 180,
        },
        {
          estimatedPlayoutTimestamp: 12_008.5,
          jitterBufferDelay: 3_120,
          jitterBufferEmittedCount: 144_000,
          totalSamplesReceived: 144_000,
          concealedSamples: 1_440,
          concealmentEvents: 5,
        },
      ),
      report(
        5_000,
        {
          estimatedPlayoutTimestamp: 14_000,
          jitterBufferDelay: 7.8,
          jitterBufferEmittedCount: 300,
        },
        {
          ssrc: 303,
          trackIdentifier: "replacement-audio",
          estimatedPlayoutTimestamp: 13_997,
          jitterBufferDelay: 2_000,
          jitterBufferEmittedCount: 96_000,
          totalSamplesReceived: 96_000,
          concealedSamples: 5_000,
          concealmentEvents: 30,
        },
      ),
      report(
        7_000,
        {
          estimatedPlayoutTimestamp: 16_000,
          jitterBufferDelay: 1,
          jitterBufferEmittedCount: 20,
        },
        {
          ssrc: 303,
          trackIdentifier: "replacement-audio",
          estimatedPlayoutTimestamp: 15_996,
          jitterBufferDelay: 480,
          jitterBufferEmittedCount: 24_000,
          totalSamplesReceived: 24_000,
          concealedSamples: 100,
          concealmentEvents: 1,
        },
      ),
      report(9_000, {}, { ssrc: 303, trackIdentifier: "replacement-audio" }),
    ];
    const connection = {
      getStats: async () => reports.shift()!,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();

    const first = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const stable = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const changedAudio = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const reset = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );
    const unsupported = await collectConnectionMetrics(
      connection,
      "receive",
      accumulator,
    );

    expect(first).toMatchObject({
      audioVideoPlayoutDeltaMs: 12.5,
      videoJitterBufferDelayMs: null,
      audioJitterBufferDelayMs: null,
      audioConcealedSamplesPercent: null,
      intervalAudioConcealmentEvents: null,
    });
    expect(stable.audioVideoPlayoutDeltaMs).toBeCloseTo(8.5);
    expect(stable.videoJitterBufferDelayMs).toBeCloseTo(25);
    expect(stable.audioJitterBufferDelayMs).toBeCloseTo(22.5);
    expect(stable.audioConcealedSamplesPercent).toBeCloseTo(1);
    expect(stable.intervalAudioConcealmentEvents).toBe(3);
    expect(changedAudio).toMatchObject({
      audioVideoPlayoutDeltaMs: -3,
      audioJitterBufferDelayMs: null,
      audioConcealedSamplesPercent: null,
      intervalAudioConcealmentEvents: null,
    });
    expect(changedAudio.videoJitterBufferDelayMs).toBeCloseTo(30);
    expect(reset).toMatchObject({
      audioVideoPlayoutDeltaMs: -4,
      videoJitterBufferDelayMs: null,
      audioJitterBufferDelayMs: null,
      audioConcealedSamplesPercent: null,
      intervalAudioConcealmentEvents: null,
    });
    expect(unsupported).toMatchObject({
      audioVideoPlayoutDeltaMs: null,
      videoJitterBufferDelayMs: null,
      audioJitterBufferDelayMs: null,
      audioConcealedSamplesPercent: null,
      intervalAudioConcealmentEvents: null,
    });
  });

  it("uses linked remote inbound counters for outbound audio loss", async () => {
    const report = (
      timestamp: number,
      audioBytesSent: number,
      packetsReceived: number | undefined,
      packetsLost: number | undefined,
    ) =>
      new Map<string, unknown>([
        [
          "transport",
          { id: "transport", type: "transport", timestamp },
        ],
        [
          "video-outbound",
          {
            id: "video-outbound",
            type: "outbound-rtp",
            timestamp,
            kind: "video",
            ssrc: 101,
            bytesSent: timestamp * 10,
            packetsSent: timestamp / 10,
            framesEncoded: timestamp / 20,
          },
        ],
        [
          "audio-codec",
          {
            id: "audio-codec",
            type: "codec",
            timestamp,
            transportId: "transport",
            mimeType: "audio/opus",
            clockRate: 48_000,
            channels: 2,
          },
        ],
        [
          "audio-outbound",
          {
            id: "audio-outbound",
            type: "outbound-rtp",
            timestamp,
            kind: "audio",
            ssrc: 202,
            transportId: "transport",
            codecId: "audio-codec",
            remoteId: "audio-remote-inbound",
            bytesSent: audioBytesSent,
          },
        ],
        [
          "audio-remote-inbound",
          {
            id: "audio-remote-inbound",
            type: "remote-inbound-rtp",
            timestamp,
            kind: "audio",
            packetsReceived,
            packetsLost,
            jitter: 0.004,
          },
        ],
      ]) as unknown as RTCStatsReport;
    const firstReport = report(1_000, 20_000, 500, 1);
    (firstReport as unknown as Map<string, unknown>).delete(
      "audio-remote-inbound",
    );
    const reports = [
      firstReport,
      report(3_000, 50_000, 500, 1),
      report(5_000, 80_000, 650, 4),
      report(7_000, 110_000, 50, 1),
      report(9_000, 140_000, 200, undefined),
    ];
    const connection = {
      getStats: async () => reports.shift()!,
    } as unknown as RTCPeerConnection;
    const accumulator = createStatsAccumulator();

    await collectConnectionMetrics(connection, "send", accumulator);
    const remoteAppeared = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
    );
    const stable = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
    );
    const reset = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
    );
    const missing = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
    );

    expect(remoteAppeared).toMatchObject({
      audioBitrateKbps: 120,
      audioPacketLossPercent: null,
    });
    expect(stable).toMatchObject({
      audioBitrateKbps: 120,
      audioJitterMs: 4,
      audioCodec: "audio/opus",
      audioCodecClockRate: 48_000,
      audioCodecChannels: 2,
    });
    expect(stable.audioPacketLossPercent).toBeCloseTo((3 / 153) * 100);
    expect(reset.audioPacketLossPercent).toBeNull();
    expect(missing.audioPacketLossPercent).toBeNull();
  });
});
