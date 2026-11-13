import { describe, expect, it } from "vitest";
import { runtimeCapabilitiesSchema } from "../../src/shared/protocol.js";

describe("runtime capabilities", () => {
  it("reports only deployment facts", () => {
    expect(runtimeCapabilitiesSchema.parse({ sfu: true, natPrediction: true }))
      .toEqual({ sfu: true, natPrediction: true });
    expect(runtimeCapabilitiesSchema.parse({ sfu: true, sfuOnly: true, natPrediction: false }))
      .toEqual({ sfu: true, sfuOnly: true, natPrediction: false });
    expect(runtimeCapabilitiesSchema.parse({ sfu: false, natPrediction: false }))
      .toEqual({ sfu: false, natPrediction: false });
  });

  it("rejects malformed, missing and unknown capability documents", () => {
    for (const value of [null, [], true, {}, { sfu: true }, { natPrediction: false },
      { sfu: null, natPrediction: false }, { sfu: "true", natPrediction: false },
      { sfu: false, natPrediction: null }, { sfu: false, natPrediction: 1 },
      { sfu: false, natPrediction: false, sfuOnly: null },
      { sfu: false, natPrediction: false, sfuOnly: "true" },
      { sfu: false, natPrediction: false, roomInteractions: true },
      { sfu: false, natPrediction: false, roomReactionIds: ["heart"] },
      { sfu: false, natPrediction: false, roomVoiceListeningPresence: true },
    ]) {
      expect(runtimeCapabilitiesSchema.safeParse(value).success).toBe(false);
    }
  });
});
