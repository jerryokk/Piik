import { afterEach, describe, expect, it } from "vitest";
import { qualityEvidenceWindowFromMetrics } from "../../../src/client/media/viewer-quality-evidence.ts";
import { collectConnectionMetrics, collectNativeSenderQualityFromReport, createNativeSenderQualityAccumulator, createStatsAccumulator } from "../../../src/client/webrtc/stats";
import { restoreClientState } from "../../support/client-state.ts";

afterEach(restoreClientState);
describe("WebRTC stats parsing", () => {
  it("sums sender layers and rebases incomplete bitrate windows", () => {
    const accumulator = createNativeSenderQualityAccumulator();
    const sample = (
      timestamp: number,
      bytesByLayer: Readonly<Record<string, number>>,
    ): number | null => {
      const records: Array<[string, Record<string, unknown>]> = [
        [
          "source",
          {
            id: "source",
            type: "media-source",
            timestamp,
            trackIdentifier: "track",
          },
        ],
      ];
      for (const [id, bytesSent] of Object.entries(bytesByLayer)) {
        records.push([
          id,
          {
            id,
            type: "outbound-rtp",
            kind: "video",
            timestamp,
            mediaSourceId: "source",
            bytesSent,
            framesEncoded: timestamp / 20,
            qualityLimitationReason: "none",
            qualityLimitationDurations: {
              none: timestamp / 1_000,
              bandwidth: 0,
              cpu: 0,
              other: 0,
            },
          },
        ]);
      }
      return collectNativeSenderQualityFromReport(
        new Map(records) as unknown as RTCStatsReport,
        "track",
        accumulator,
      ).bitrateKbps;
    };

    expect(sample(1_000, { low: 10_000, high: 20_000 })).toBeNull();
    expect(sample(3_000, { low: 110_000, high: 220_000 })).toBe(1_200);
    expect(
      sample(5_000, { low: 210_000, high: 420_000, mid: 50_000 }),
    ).toBeNull();
    expect(
      sample(7_000, { low: 310_000, high: 620_000, mid: 150_000 }),
    ).toBe(1_600);
    expect(
      sample(9_000, { low: 10_000, high: 820_000, mid: 250_000 }),
    ).toBeNull();
    expect(
      sample(11_000, {
        low: 110_000,
        high: 1_020_000,
        mid: 350_000,
      }),
    ).toBe(1_600);
  });

  it("binds media, remote evidence, transport, and pair without guessing", async () => {
    const entry = (
      id: string,
      type: string,
      values: Record<string, unknown>,
    ): [string, Record<string, unknown>] => [
      id,
      { id, type, timestamp: 2_000, ...values },
    ];
    const report = new Map<string, Record<string, unknown>>([
      entry("transport-a", "transport", {
        selectedCandidatePairId: "pair-a",
      }),
      entry("transport-z", "transport", {
        selectedCandidatePairId: "pair-z",
      }),
      entry("pair-a", "candidate-pair", {
        transportId: "transport-a",
        localCandidateId: "local-a",
        remoteCandidateId: "remote-candidate-a",
        currentRoundTripTime: 0.02,
      }),
      entry("pair-z", "candidate-pair", {
        transportId: "transport-z",
        localCandidateId: "local-z",
        remoteCandidateId: "remote-candidate-z",
      }),
      entry("local-a", "local-candidate", {
        candidateType: "host",
        protocol: "udp",
        address: "192.0.2.10",
        port: 50_000,
      }),
      entry("remote-candidate-a", "remote-candidate", {
        candidateType: "srflx",
        foundation: "sp1",
        protocol: "udp",
        address: "2001:db8::10",
        port: 50_001,
      }),
      entry("local-z", "local-candidate", {
        candidateType: "relay",
        protocol: "tcp",
      }),
      entry("remote-candidate-z", "remote-candidate", {
        candidateType: "relay",
        protocol: "tcp",
      }),
      entry("outbound-a", "outbound-rtp", {
        kind: "video",
        ssrc: 111,
        mid: "0",
        transportId: "transport-a",
        mediaSourceId: "source-a",
        remoteId: "remote-inbound-a",
        codecId: "codec-a",
        rid: "main",
        scalabilityMode: "L2T3_KEY",
        bytesSent: 2_000,
        framesEncoded: 60,
        totalEncodeTime: 0.3,
        encoderImplementation: "ExternalEncoder",
        powerEfficientEncoder: true,
      }),
      entry("outbound-z", "outbound-rtp", {
        kind: "video",
        ssrc: 999,
        transportId: "transport-z",
        mediaSourceId: "source-z",
        remoteId: "remote-inbound-z",
        codecId: "codec-z",
        bytesSent: 99_000,
        framesEncoded: 99,
      }),
      entry("source-a", "media-source", {
        kind: "video",
        trackIdentifier: "capture-track-a",
        framesPerSecond: 58.5,
      }),
      entry("source-z", "media-source", {
        kind: "video",
        trackIdentifier: "capture-track-z",
      }),
      entry("remote-inbound-a", "remote-inbound-rtp", {
        kind: "video",
        packetsLost: 2,
        jitter: 0.004,
      }),
      entry("remote-inbound-z", "remote-inbound-rtp", {
        kind: "video",
        packetsLost: 999,
        jitter: 0.9,
      }),
      entry("codec-a", "codec", {
        transportId: "transport-a",
        mimeType: "video/H264",
        sdpFmtpLine:
          "profile-level-id=42E01F; packetization-mode=1; " +
          "level-asymmetry-allowed=1; sprop-parameter-sets=do-not-expose; " +
          "x-google-start-bitrate=99999",
      }),
      entry("codec-z", "codec", {
        transportId: "transport-z",
        mimeType: "video/VP9",
        sdpFmtpLine: "profile-id=3; max-fr=15; max-fs=1200",
      }),
    ]);
    const connection = {
      getStats: async () => report as unknown as RTCStatsReport,
    } as unknown as RTCPeerConnection;

    const accumulator = createStatsAccumulator();
    const metrics = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
      { trackIdentifier: "capture-track-a" },
    );

    expect(metrics).toMatchObject({
      sampleTimestampMs: 2_000,
      sampleWindowMs: null,
      rtpStatsId: "outbound-a",
      rtpSsrc: 111,
      rtpMid: "0",
      rtpRid: "main",
      trackIdentifier: "capture-track-a",
      mediaSourceFramesPerSecond: 58.5,
      selectedCandidatePairId: "pair-a",
      path: "direct",
      natTraversalPath: "predicted",
      iceProtocol: "udp",
      localCandidateAddress: "192.0.2.10",
      localCandidatePort: 50_000,
      remoteCandidateAddress: "2001:db8::10",
      remoteCandidatePort: 50_001,
      packetsLost: 2,
      jitterMs: 4,
      rttMs: 20,
      codec: "video/H264",
      codecProfile: "profile-level-id=42e01f",
      codecParameters:
        "packetization-mode=1; level-asymmetry-allowed=1",
      scalabilityMode: "L2T3_KEY",
      encoderImplementation: "ExternalEncoder",
      powerEfficientEncoder: true,
      intervalFramesEncoded: null,
      intervalEncodeTimeMs: null,
    });
    expect(JSON.stringify(metrics)).not.toContain("sprop-parameter-sets");
    expect(JSON.stringify(metrics)).not.toContain("x-google-start-bitrate");

    report.get("remote-candidate-a")!.address = null;
    delete report.get("remote-candidate-a")!.port;
    expect(
      await collectConnectionMetrics(connection, "send", accumulator, {
        trackIdentifier: "capture-track-a",
      }),
    ).toMatchObject({
      selectedCandidatePairId: "pair-a",
      localCandidateAddress: "192.0.2.10",
      localCandidatePort: 50_000,
      remoteCandidateAddress: null,
      remoteCandidatePort: null,
    });
    report.get("remote-candidate-a")!.address = "2001:db8::10";
    report.get("remote-candidate-a")!.port = 50_001;
    report.get("remote-candidate-a")!.foundation = "ordinary";
    expect(
      await collectConnectionMetrics(connection, "send", accumulator, {
        trackIdentifier: "capture-track-a",
      }),
    ).toMatchObject({ natTraversalPath: "ordinary" });

    report.get("transport-a")!.selectedCandidatePairId = "pair-z";
    const wrongTransportPair = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
      { trackIdentifier: "capture-track-a" },
    );
    expect(wrongTransportPair).toMatchObject({
      rtpStatsId: "outbound-a",
      selectedCandidatePairId: null,
      path: "unknown",
    });

    report.get("transport-a")!.selectedCandidatePairId = "pair-a";
    report.get("codec-a")!.transportId = "transport-z";
    const wrongTransportCodec = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
      { trackIdentifier: "capture-track-a" },
    );
    expect(wrongTransportCodec).toMatchObject({
      rtpStatsId: "outbound-a",
      codec: null,
      codecProfile: null,
      codecParameters: null,
    });
    report.get("codec-a")!.transportId = "transport-a";

    report.get("local-a")!.type = "remote-candidate";
    report.get("remote-candidate-a")!.type = "local-candidate";
    const malformedCandidateReferences = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
      { trackIdentifier: "capture-track-a" },
    );
    expect(malformedCandidateReferences).toMatchObject({
      selectedCandidatePairId: null,
      path: "unknown",
      localCandidateType: null,
      remoteCandidateType: null,
    });
    report.get("local-a")!.type = "local-candidate";
    report.get("remote-candidate-a")!.type = "remote-candidate";

    delete report.get("outbound-a")!.transportId;
    const unboundTransport = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
      { trackIdentifier: "capture-track-a" },
    );
    expect(unboundTransport).toMatchObject({
      rtpStatsId: "outbound-a",
      selectedCandidatePairId: null,
      path: "unknown",
      codec: null,
      codecProfile: null,
      codecParameters: null,
    });
    report.get("outbound-a")!.transportId = "transport-a";

    report.set("outbound-a-layer-2", {
      id: "outbound-a-layer-2",
      type: "outbound-rtp",
      timestamp: 3_000,
      kind: "video",
      ssrc: 112,
      transportId: "transport-a",
      mediaSourceId: "source-a",
      bytesSent: 3_000,
      framesEncoded: 90,
    });
    const ambiguous = await collectConnectionMetrics(
      connection,
      "send",
      accumulator,
      { trackIdentifier: "capture-track-a" },
    );
    expect(ambiguous).toMatchObject({
      rtpStatsId: null,
      selectedCandidatePairId: null,
      packetsLost: null,
      codec: null,
      codecProfile: null,
      codecParameters: null,
      scalabilityMode: null,
    });
  });

  it.each([
    {
      mimeType: "video/VP8",
      fmtp: "max-fr=60; max-fs=3600",
      profile: null,
      parameters: "max-fr=60; max-fs=3600",
    },
    {
      mimeType: "video/VP9",
      fmtp: "profile-id=2; max-fr=60; max-fs=3600",
      profile: "profile-id=2",
      parameters: "max-fr=60; max-fs=3600",
    },
    {
      mimeType: "video/AV1",
      fmtp: "profile=1; level-idx=8; tier=0",
      profile: "profile=1",
      parameters: "level-idx=8; tier=0",
    },
    {
      mimeType: "video/H264",
      fmtp:
        "profile-level-id=42e01f; profile-level-id=invalid; " +
        "packetization-mode=1",
      profile: null,
      parameters: "packetization-mode=1",
    },
    {
      mimeType: "video/H264",
      fmtp: `profile-level-id=42e01f; ignored=${"x".repeat(2_048)}`,
      profile: null,
      parameters: null,
    },
    {
      mimeType: "video/H264",
      fmtp: [
        "profile-level-id=42e01f",
        ...Array.from({ length: 32 }, (_, index) => `ignored-${index}=1`),
      ].join(";"),
      profile: null,
      parameters: null,
    },
    {
      mimeType: "video/H265",
      fmtp: "profile-id=1; tier-flag=0; level-id=93",
      profile: null,
      parameters: null,
    },
  ])(
    "derives only allowlisted $mimeType format parameters",
    async ({ mimeType, fmtp, profile, parameters }) => {
      const report = new Map<string, unknown>([
        [
          "transport",
          { id: "transport", type: "transport", timestamp: 1_000 },
        ],
        [
          "outbound",
          {
            id: "outbound",
            type: "outbound-rtp",
            timestamp: 1_000,
            kind: "video",
            ssrc: 101,
            transportId: "transport",
            codecId: "codec",
            bytesSent: 1_000,
            framesEncoded: 30,
          },
        ],
        [
          "codec",
          {
            id: "codec",
            type: "codec",
            timestamp: 1_000,
            transportId: "transport",
            mimeType,
            sdpFmtpLine: fmtp,
          },
        ],
      ]) as unknown as RTCStatsReport;
      const connection = {
        getStats: async () => report,
      } as unknown as RTCPeerConnection;

      const metrics = await collectConnectionMetrics(
        connection,
        "send",
        createStatsAccumulator(),
      );
      expect(metrics).toMatchObject({
        codec: mimeType,
        codecProfile: profile,
        codecParameters: parameters,
        scalabilityMode: null,
      });
      expect(
        qualityEvidenceWindowFromMetrics({
          ...metrics,
          sampleWindowMs: 2_000,
        })?.metrics,
      ).toMatchObject({
        codec: mimeType,
        codecProfile: profile,
        codecParameters: parameters,
      });
    },
  );
});
