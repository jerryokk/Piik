// The Piik App window (Tauri WebView2) marks itself before any page script
// runs. The marker gates presentation only: Host UI is shown inside the App
// window and replaced by an App-required notice in ordinary browsers. Native
// authority still comes from loopback origin approval, never from this value.

interface AppShellMarker {
  readonly version: string;
}

declare global {
  interface Window {
    readonly __PIIK_APP__?: AppShellMarker;
  }
}

/**
 * The exact initialization script the App injects into its window, with the
 * version JSON-encoded. For version "3.0.0" it is:
 *
 *   Object.defineProperty(window,"__PIIK_APP__",{value:Object.freeze({version:"3.0.0"}),enumerable:false,configurable:false,writable:false});
 *
 * Rust (Tauri `WebviewWindowBuilder::initialization_script`) can produce it with
 *
 *   format!(r#"Object.defineProperty(window,"__PIIK_APP__",{{value:Object.freeze({{version:{}}}),enumerable:false,configurable:false,writable:false}});"#,
 *     serde_json::to_string(env!("CARGO_PKG_VERSION")).unwrap())
 *
 * Browser gates reuse this function to emulate the App window's Host page.
 */
export function appShellInitScript(version: string): string {
  return `Object.defineProperty(window,"__PIIK_APP__",{value:Object.freeze({version:${JSON.stringify(version)}}),enumerable:false,configurable:false,writable:false});`;
}

/** Whether this page runs inside the Piik App window. */
export function inAppShell(): boolean {
  const marker: unknown = (globalThis as { __PIIK_APP__?: unknown }).__PIIK_APP__;
  return typeof marker === "object" && marker !== null && Object.isFrozen(marker) &&
    typeof (marker as { version?: unknown }).version === "string";
}
