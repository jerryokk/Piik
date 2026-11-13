// Per-tab storage helpers. Every browser storage call is optional: a blocked
// store must not prevent the current session from working.

export function readSessionValue(key: string): string | null {
  try {
    return window.sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeSessionValue(key: string, value: string): void {
  try {
    window.sessionStorage.setItem(key, value);
  } catch {
    // The current page can still use the in-memory value in restricted browsers.
  }
}
