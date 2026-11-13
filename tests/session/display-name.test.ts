import { afterEach, describe, expect, it, vi } from "vitest";
import { defaultHostDisplayName, defaultViewerDisplayName, readDisplayName, saveDisplayName } from "../../src/client/lib/room/display-name.ts";
import { setCopy } from "../../src/client/ui/copy.ts";
import { deriveParticipantTopology } from "../../src/client/lib/room/participant-topology.ts";
import { labelParticipantNames, labelParticipantSnapshot } from "../../src/client/lib/room/viewer-presence.ts";
import { restoreClientState } from "../support/client-state.ts";

afterEach(restoreClientState);
describe("browser-local display name", () => {
  it("disambiguates distinct chat senders while retaining repeated and historical names", () => {
    const first = { peerId: "viewer_AAAAAAsuffix", displayName: "观众" };
    const other = { peerId: "viewer_BBBBBBsuffix", displayName: "观众" };
    expect(labelParticipantNames([first, first]).map(sender => sender.label)).toEqual(["观众", "观众"]);
    const senders = labelParticipantNames([first, first, other, { ...first, displayName: "新昵称" }]);
    expect(senders[0].label).toBe(senders[1].label);
    expect(senders[0].label).not.toBe(senders[2].label);
    expect(senders[0].peerIdSuffix.length).toBeGreaterThan(6);
    expect(senders.map(sender => sender.displayName)).toEqual(["观众", "观众", "观众", "新昵称"]);
    expect(senders[3].label).toBe("新昵称");
  });

  it("localizes text defaults and uses visual emoji identities", () => {
    setCopy({ lang: "zh", vis: false });
    expect(defaultViewerDisplayName(false)).toBe("观众");
    expect(defaultHostDisplayName(false)).toBe("房主");

    setCopy({ lang: "en", vis: false });
    expect(defaultViewerDisplayName(false)).toBe("Viewer");
    expect(defaultHostDisplayName(false)).toBe("Host");

    expect(defaultViewerDisplayName(true)).toBe("👤");
    expect(defaultHostDisplayName(true)).toBe("📺");
  });

  it("stores only the canonical preference and falls back when cleared", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });

    expect(saveDisplayName("  Cafe\u0301\u00a0玩家 ")).toBe("Café 玩家");
    expect(readDisplayName()).toBe("Café 玩家");
    expect(saveDisplayName("\u202ehidden")).toBeNull();
    expect(saveDisplayName("\n")).toBeNull();
    expect(readDisplayName()).toBe("Café 玩家");
    expect(saveDisplayName("   ")).toBe("观众");
    expect(readDisplayName()).toBe("观众");
  });

  it("extends only colliding room-scoped peer ID suffixes", () => {
    const { viewers: labeled } = labelParticipantSnapshot([
      {
        role: "viewer",
        peerId: "viewer_AAAAAAsuffix",
        displayName: "同名",
        upstream: { kind: "peer", peerId: "host_12345678" },
      },
      {
        role: "viewer",
        peerId: "viewer_BBBBBBsuffix",
        displayName: "同名",
        upstream: { kind: "peer", peerId: "viewer_AAAAAAsuffix" },
      },
      {
        role: "viewer",
        peerId: "viewer_independent",
        displayName: "朋友",
        upstream: { kind: "sfu" },
      },
    ]);

    expect(labeled[0].peerIdSuffix.length).toBeGreaterThan(6);
    expect(labeled[1].peerIdSuffix.length).toBeGreaterThan(6);
    expect(labeled[0].peerIdSuffix).not.toBe(labeled[1].peerIdSuffix);
    expect(labeled[2].peerIdSuffix).toHaveLength(6);
    expect(labeled[2].label).toBe("朋友");
    expect(labeled[0].label).toContain("同名 (");
  });

  it("keeps the shortest hidden suffix for distinct display names", () => {
    const { viewers: labeled } = labelParticipantSnapshot([
      {
        role: "viewer",
        peerId: "viewer_AAAAAAsuffix",
        displayName: "甲",
        upstream: { kind: "sfu" },
      },
      {
        role: "viewer",
        peerId: "viewer_BBBBBBsuffix",
        displayName: "乙",
        upstream: { kind: "sfu" },
      },
    ]);

    expect(labeled.map((viewer) => viewer.label)).toEqual(["甲", "乙"]);
    expect(labeled.map((viewer) => viewer.peerIdSuffix.length)).toEqual([6, 6]);
  });

  it("uses the minimum suffix when duplicate names are already distinct", () => {
    const { viewers: labeled } = labelParticipantSnapshot([
      {
        role: "viewer",
        peerId: "viewer_AAAAAA111111",
        displayName: "同名",
        upstream: { kind: "none" },
      },
      {
        role: "viewer",
        peerId: "viewer_BBBBBB222222",
        displayName: "同名",
        upstream: { kind: "none" },
      },
    ]);

    expect(labeled.map((viewer) => viewer.peerIdSuffix)).toEqual([
      "111111",
      "222222",
    ]);
    expect(labeled.map((viewer) => viewer.label)).toEqual([
      "同名 (111111)",
      "同名 (222222)",
    ]);
  });

  it("labels every Viewer in a participant snapshot without including the Host", () => {
    const { viewers: labeled } = labelParticipantSnapshot([
      {
        role: "host",
        peerId: "host_12345678",
        displayName: "分享者",
        upstream: { kind: "none" },
      },
      {
        role: "viewer",
        peerId: "viewer_111111",
        displayName: "朋友甲",
        upstream: { kind: "peer", peerId: "host_12345678" },
      },
      {
        role: "viewer",
        peerId: "viewer_222222",
        displayName: "朋友乙",
        upstream: { kind: "sfu" },
      },
    ]);

    expect(labeled.map((viewer) => viewer.label)).toEqual([
      "朋友甲",
      "朋友乙",
    ]);
  });

  it("labels Host and Viewer name collisions before splitting roles", () => {
    const labeled = labelParticipantSnapshot([
      {
        role: "host",
        peerId: "host_123456",
        displayName: "同名",
        upstream: { kind: "none" },
      },
      {
        role: "viewer",
        peerId: "viewer_654321",
        displayName: "同名",
        upstream: { kind: "peer", peerId: "host_123456" },
      },
      {
        role: "viewer",
        peerId: "viewer_789012",
        displayName: "唯一名称",
        upstream: { kind: "sfu" },
      },
    ]);

    expect(labeled.host?.label).toBe("同名 (123456)");
    expect(labeled.viewers.map((viewer) => viewer.label)).toEqual([
      "同名 (654321)",
      "唯一名称",
    ]);
  });

  it("derives exact peer and SFU branches without guessing orphaned routes", () => {
    const viewer = (
      peerId: string,
      upstream:
        | { kind: "none" }
        | { kind: "sfu" }
        | { kind: "peer"; peerId: string },
    ) => ({ role: "viewer" as const, peerId, displayName: peerId, upstream });
    const topology = deriveParticipantTopology(
      "host_12345678",
      labelParticipantSnapshot([
        viewer("viewer_root_1", { kind: "peer", peerId: "host_12345678" }),
        viewer("viewer_child_1", { kind: "peer", peerId: "viewer_root_1" }),
        viewer("viewer_sfu_1", { kind: "sfu" }),
        viewer("viewer_sfu_child", {
          kind: "peer",
          peerId: "viewer_sfu_1",
        }),
        viewer("viewer_pending", { kind: "none" }),
        viewer("viewer_orphan", { kind: "peer", peerId: "viewer_offline" }),
      ]).viewers,
    );

    expect(topology.peerRoots[0].viewer.peerId).toBe("viewer_root_1");
    expect(topology.peerRoots[0].children[0].viewer.peerId).toBe(
      "viewer_child_1",
    );
    expect(topology.sfuRoots[0].children[0].viewer.peerId).toBe(
      "viewer_sfu_child",
    );
    expect(topology.pending.map((entry) => entry.peerId)).toEqual([
      "viewer_orphan",
      "viewer_pending",
    ]);
  });
});
