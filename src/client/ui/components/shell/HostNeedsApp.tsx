import { withBrowserDebug } from "../../../lib/debug";
import { useCopy } from "../../copy";
import { Comic } from "../comic/Comic";
import { AppHeader } from "./Header";
import { Btn } from "./primitives";

/** Browser visit to the Host entry: explain where hosting lives, keep joining. */
export function HostNeedsApp() {
  return (
    <div className="lr-app">
      <AppHeader atHome />
      <main className="lr-join">
        <HostNeedsAppPanel />
      </main>
    </div>
  );
}

export function HostNeedsAppPanel() {
  const { t, vis } = useCopy();
  return (
    <section className="lr-join-panel" aria-labelledby="host-needs-app-title">
      <Comic kind="host-needs-app" theme="paper" />
      <div className={vis ? "visually-hidden" : "lr-access-text"}>
        <h1 id="host-needs-app-title">{t("hostApp.title")}</h1>
        <p>{t("hostApp.hint")}</p>
        <p>{t("hostApp.viewer")}</p>
      </div>
      <Btn
        icon="door"
        title="join.title"
        cap="join.title"
        tone="primary"
        hint="hint-join-go"
        onClick={() => window.location.assign(withBrowserDebug("/join"))}
      />
    </section>
  );
}
