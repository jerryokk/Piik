// Evidence shapes shared by the App media gate, its drivers and assertions.

export interface Probe {
  protocol: number;
  processAudio: boolean;
  systemAudio: boolean;
  adapters: Array<{
    index: number;
    hardwareH264: Array<{ index: number }>;
  }>;
}

export interface WindowTarget {
  kind: "window" | "display";
  sourceId: string;
  pid?: number;
  creationTime?: string;
  title: string;
}

export interface CaptureEvidence {
  starting: boolean;
  active: boolean;
  frames: number;
  keyFrameRequested: boolean;
  recoveryFrame: boolean;
  audioReady: boolean;
  audioFrames: number;
  audioNonZeroSamples: number;
}

export interface MediaEvidence {
  captureActive: boolean;
  connected: boolean;
  connectedEdges: number;
  frames: number;
  edgeFrames: number[];
  audioPackets: number;
  edgeAudioPackets: number[];
  audioEnergy: number;
  edgeAudioEnergy: number[];
  audioSamples: number;
  edgeAudioSamples: number[];
  audioBytes: number;
  edgeAudioBytes: number[];
  audioAvailable: boolean;
  width: number;
  height: number;
  localType: string | null;
  remoteType: string | null;
  nativeCandidateTypes: string[];
  liveProfileUpdated: boolean;
  pausedProfileUpdated: boolean;
  error: string | null;
}