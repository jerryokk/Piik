import { useId, useRef, useState } from "react";
import type { StatusDescriptor } from "../../media-status";
import { useCopy, type CopyKey } from "../../copy";
import { Glyph } from "../../icons";
import { Comic } from "../comic/Comic";
import { HintComic, isHintKind } from "../hints";
import { BrowserDiagnosticsButton } from "../shell/Header";
import { StatusIndicator } from "../people/StatusIndicator";
import "./share-failure-help.css";

/** The current status owns this disclosure; closing/replacing it retains no error history. */
export function ShareFailureHelp({ status, label, code, checks }: {
  status: StatusDescriptor; label: string; code: string; checks: CopyKey[];
}) {
  const { vis, t } = useCopy();
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState<"idle" | "done" | "failed">("idle");
  const copyRequest = useRef<object | null>(null);
  const hint = (status.tooltip ?? status.comic)!;
  return <>
    <StatusIndicator status={status} label={label} onActivate={() => {
      copyRequest.current = null;
      setCopied("idle");
      setOpen(true);
      dialog.current?.showModal();
    }} />
    <dialog ref={dialog} className="lr-share-help" aria-labelledby={titleId} onClose={() => {
      copyRequest.current = null;
      setOpen(false);
    }}>
      {open && <>
        <header><h2 id={titleId}>{vis && <Glyph name="alert" size={20} />}<span className={vis ? "visually-hidden" : undefined}>{t("shareHelp.title")}</span></h2>
          <button type="button" className="lr-btn is-dense is-quiet" aria-label={t("common.close")} autoFocus onClick={() => dialog.current?.close()}>
            <Glyph name="x" size={18} />
          </button>
        </header>
        <div className="lr-share-help-scene" aria-hidden="true">
          {isHintKind(hint) ? <HintComic kind={hint} tone={status.tone} /> : <Comic kind={hint} theme="paper" tone={status.tone} />}
        </div>
        <p className={vis ? "visually-hidden" : "lr-share-help-reason"}>{label}</p>
        <code>{code}</code>
        <ol className={vis ? "visually-hidden" : undefined}>
          {checks.map(key => <li key={key}>{t(key)}</li>)}
          <li>{t("shareHelp.report")}</li>
        </ol>
        {!vis && code.startsWith("app/") && <p className="lr-share-help-note">{t("shareHelp.appDebug")}</p>}
        <nav aria-label={t("shareHelp.title")}>
          <BrowserDiagnosticsButton />
          <button type="button" className="lr-btn" aria-label={t(copied === "done" ? "shareHelp.copied" : "shareHelp.copy")} onClick={() => {
            // Deliberately excludes URLs, invitation credentials and raw exceptions.
            const version = import.meta.env.VITE_PIIK_VERSION ?? "development";
            const request = copyRequest.current = {};
            void Promise.resolve().then(() => navigator.clipboard.writeText(`Piik Web ${version}\n${label}\n${code}`)).then(
              () => { if (copyRequest.current === request) setCopied("done"); },
              () => { if (copyRequest.current === request) setCopied("failed"); });
          }}><Glyph name={copied === "done" ? "check" : "copy"} size={16} />{!vis && t(copied === "done" ? "shareHelp.copied" : "shareHelp.copy")}</button>
        </nav>
        <span role="status">{copied === "failed" ? t("shareHelp.copyFailed") : ""}</span>
      </>}
    </dialog>
  </>;
}
