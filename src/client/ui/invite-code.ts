// Invitation links as a scannable code. The QR encoder is a data-only
// dependency; this module owns the one place Piik turns an invite URL into
// modules, so the panel only positions and styles the result.
import qrcode from "qrcode-generator";

// Error correction stays at the format's medium level: invitation URLs carry a
// viewer credential and are often scanned off a laptop screen or print.
const ERROR_CORRECTION = "M";
// Four modules of quiet zone is the specification's minimum for reliable
// detection; it is part of the rendered image, not the panel's padding.
const QUIET_ZONE = 4;

interface InviteCode {
  /** Square module grid; `true` is a dark module. Rows are top to bottom. */
  modules: readonly (readonly boolean[])[];
  /** Total modules including the quiet zone, per side. */
  size: number;
}

/** Encodes an invitation link, or `null` when it cannot fit a QR code. */
export function inviteCode(link: string): InviteCode | null {
  if (!link) return null;
  let code: ReturnType<typeof qrcode>;
  try {
    // Type 0 lets the encoder pick the smallest version that fits.
    code = qrcode(0, ERROR_CORRECTION);
    code.addData(link);
    code.make();
  } catch {
    // An over-long URL is a real possibility for a deep-link deployment; the
    // panel then shows only the link instead of a broken grid.
    return null;
  }
  const count = code.getModuleCount();
  const size = count + QUIET_ZONE * 2;
  const modules = Array.from({ length: size }, (_, row) =>
    Array.from({ length: size }, (_, column) => {
      const inner = row - QUIET_ZONE;
      const innerColumn = column - QUIET_ZONE;
      return inner >= 0 && inner < count && innerColumn >= 0 && innerColumn < count
        ? code.isDark(inner, innerColumn)
        : false;
    }),
  );
  return { modules, size };
}

/** SVG path data for one module grid, in module units. */
export function inviteCodePath({ modules }: InviteCode): string {
  const parts: string[] = [];
  modules.forEach((row, y) => {
    let start = -1;
    row.forEach((dark, x) => {
      if (dark && start < 0) start = x;
      if (!dark && start >= 0) {
        parts.push(`M${start} ${y}h${x - start}v1h-${x - start}z`);
        start = -1;
      }
    });
    if (start >= 0) {
      parts.push(`M${start} ${y}h${row.length - start}v1h-${row.length - start}z`);
    }
  });
  return parts.join("");
}
