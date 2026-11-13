import { parse, parsePayloads } from "sdp-transform";

export function offerHasNativeVideoCodec(sdp: string): boolean {
  try {
    const session = parse(sdp);
    const video = session.media.filter((media) => {
      const direction = media.direction ?? session.direction ?? "sendrecv";
      const bundled = media.bundleOnly && (session.groups ?? []).some(group =>
        group.type === "BUNDLE" && String(group.mids).split(" ").includes(String(media.mid)));
      return media.type === "video" && (media.port > 0 || bundled) &&
        (direction === "sendonly" || direction === "sendrecv");
    });
    if (video.length !== 1) return false;
    const media = video[0]!;
    const payloads = new Set(parsePayloads(media.payloads ?? ""));
    return media.rtp.some((codec) => {
      if (!payloads.has(codec.payload) || codec.rate !== 90_000) return false;
      if (codec.codec.toLowerCase() === "vp8") return true;
      if (codec.codec.toLowerCase() !== "h264") return false;
      return nativeH264Format(media.fmtp?.find(fmtp => fmtp.payload === codec.payload)?.config ?? "");
    });
  } catch {
    return false;
  }
}

function nativeH264Format(fmtp: string): boolean {
  // Preflight the same encoded-forwarding subset as mediaedge.forwardableH264;
  // a MIME-only match must not turn unsupported media into an App failure.
  const parameters = new Map(fmtp.split(";").map(part => {
    const [key, value] = part.split(/=(.*)/, 2);
    return [key?.trim().toLowerCase(), value?.trim().toLowerCase()] as const;
  }));
  const profile = parameters.get("profile-level-id") ?? "";
  if (parameters.get("packetization-mode") !== "1" || !/^[0-9a-f]{6}$/.test(profile)) return false;
  const idc = parseInt(profile.slice(0, 2), 16);
  const constraints = parseInt(profile.slice(2, 4), 16);
  const level = parseInt(profile.slice(4), 16);
  // RFC 6184 Table 5: equivalent Constrained Baseline coding-tool subsets.
  return ((idc === 0x42 && (constraints & 0x4f) === 0x40) ||
    (idc === 0x4d && (constraints & 0x8f) === 0x80) ||
    (idc === 0x58 && (constraints & 0xcf) === 0xc0)) &&
    [10, 11, 12, 13, 20, 21, 22, 30, 31, 32, 40, 41, 42, 50, 51, 52].includes(level);
}
