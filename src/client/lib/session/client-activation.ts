// Launching the App client from a page: activation memory, the runtime control
// port and the launch presentation carried in the URL. Persistence here is
// per-tab convenience only; approval still comes from loopback authority.
import { isLang, type Lang } from "../../locales";
import { withBrowserDebug } from "../debug";
import { NATIVE_CLIENT_PORT_END, NATIVE_CLIENT_PORT_START } from "../../native/discovery-ports";
import { readSessionValue, writeSessionValue } from "./storage";

const CLIENT_LAUNCH_STORAGE_KEY = "piik:client-launch:v1";
const CLIENT_RUNTIME_STORAGE_KEY = "piik:client-runtime-port";
const CLIENT_ACTIVATION_EVENT = "piik-client-activation";

// Keep the selected App through this tab's reloads, without copying it into invites.
let clientPortInPage = parseClientRuntimePort(readSessionValue(CLIENT_RUNTIME_STORAGE_KEY));
let clientActivatedInPage = clientPortInPage !== null;

export function clientRuntimePort(): number | null { return clientPortInPage; }

function parseClientRuntimePort(value: string | null): number | null {
  const port = Number(value);
  return Number.isInteger(port) && port >= NATIVE_CLIENT_PORT_START && port <= NATIVE_CLIENT_PORT_END ? port : null;
}

export function isClientActivated(): boolean {
  try { return clientActivatedInPage || window.localStorage.getItem(CLIENT_LAUNCH_STORAGE_KEY) === "1"; }
  catch { return clientActivatedInPage; }
}

export function activateClient(): void {
  clientActivatedInPage = true;
  try { window.localStorage.setItem(CLIENT_LAUNCH_STORAGE_KEY, "1"); } catch { /* Optional persistence. */ }
  window.dispatchEvent(new Event(CLIENT_ACTIVATION_EVENT));
}

export function watchClientActivation(changed: () => void): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key === CLIENT_LAUNCH_STORAGE_KEY || event.key === null) changed();
  };
  window.addEventListener(CLIENT_ACTIVATION_EVENT, changed);
  window.addEventListener("storage", onStorage);
  return () => {
    window.removeEventListener(CLIENT_ACTIVATION_EVENT, changed);
    window.removeEventListener("storage", onStorage);
  };
}

interface ClientLaunchBootstrap {
  accessToken: string | null;
  launchedByClient: boolean;
  presentation: Partial<ClientLaunchPresentation> | null;
}

export interface ClientLaunchPresentation {
  lang: Lang;
  vis: boolean;
  theme: "light" | "dark" | null;
  /** Domains explicitly chosen in the launcher, rather than system defaults. */
  explicit?: readonly ("copy" | "theme")[];
}

export function clientLaunchURL(target: string, presentation: ClientLaunchPresentation, debug?: boolean, controlPort?: number): string {
  const url = new URL(target);
  const params = new URLSearchParams(url.hash.slice(1));
  if (controlPort !== undefined) url.searchParams.set("piik-client-port", String(controlPort));
  params.set("piik-lang", presentation.lang);
  params.set("piik-mode", presentation.vis ? "vis" : "text");
  params.set("piik-theme", presentation.theme ?? "system");
  // Keep the public tuple for older Sites. The non-secret provenance goes in
  // the query: an unknown fragment key would corrupt their exact #v= parser.
  url.searchParams.delete("piik-preferences");
  if (presentation.explicit?.length) {
    url.searchParams.set("piik-preferences", presentation.explicit.join(","));
  }
  url.hash = params.toString();
  return withBrowserDebug(url.toString(), debug);
}

export function takeClientLaunchBootstrap(): ClientLaunchBootstrap {
  const params = new URLSearchParams(window.location.hash.replace(/^#/, ""));
  const keys = [
    "client-access",
    "piik-client",
    "piik-lang",
    "piik-mode",
    "piik-theme",
  ] as const;
  const query = new URLSearchParams(window.location.search);
  const present = keys.some((key) => params.has(key)) || query.has("piik-preferences") || query.has("piik-client-port");
  const accessValue = params.get("client-access");
  const launchedFromFragment = params.get("piik-client") === "1";
  if (launchedFromFragment) {
    clientActivatedInPage = true;
    clientPortInPage = parseClientRuntimePort(query.get("piik-client-port"));
    if (clientPortInPage !== null) writeSessionValue(CLIENT_RUNTIME_STORAGE_KEY, String(clientPortInPage));
    else try { window.sessionStorage.removeItem(CLIENT_RUNTIME_STORAGE_KEY); } catch { /* Storage is optional. */ }
  }
  const lang = params.get("piik-lang");
  const mode = params.get("piik-mode");
  const theme = params.get("piik-theme");
  const preferenceFields = query.get("piik-preferences")?.split(",") ?? [];
  const presentation: Partial<ClientLaunchPresentation> = {
    ...(isLang(lang) ? { lang } : {}),
    ...(mode === "vis" || mode === "text" ? { vis: mode === "vis" } : {}),
    ...(theme === "light" || theme === "dark" || theme === "system"
      ? { theme: theme === "system" ? null : theme } : {}),
  };
  let launchedByClient = launchedFromFragment;
  try {
    if (launchedFromFragment) {
      window.localStorage.setItem(CLIENT_LAUNCH_STORAGE_KEY, "1");
    } else {
      launchedByClient =
        window.localStorage.getItem(CLIENT_LAUNCH_STORAGE_KEY) === "1";
    }
  } catch {
    // The launch fragment still enables the current load when storage is blocked.
  }
  const result: ClientLaunchBootstrap = {
    accessToken: accessValue || null,
    launchedByClient,
    presentation: launchedFromFragment && Object.keys(presentation).length > 0
      ? {
          ...presentation,
          explicit: (["copy", "theme"] as const).filter((key) => preferenceFields.includes(key)),
        }
      : null,
  };
  if (present) {
    for (const key of keys) params.delete(key);
    query.delete("piik-preferences");
    query.delete("piik-client-port");
    const search = query.toString();
    const remaining = params.toString();
    window.history.replaceState(
      window.history.state,
      "",
      `${window.location.pathname}${search ? `?${search}` : ""}${remaining ? `#${remaining}` : ""}`,
    );
  }
  return result;
}
