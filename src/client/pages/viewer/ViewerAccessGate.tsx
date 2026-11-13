//! Access gate for the Viewer page: checking, denied and password-retry states
//! render before the room stage. It receives every fact as props.

import type { FormEvent } from "react";
import { MAX_VIEWER_PASSWORD_LENGTH } from "../../../shared/protocol";
import type { ViewerFailureCode } from "../../media/viewer-presentation";
import type { StatusDescriptor } from "../../ui/media-status";
import type { CopyKey } from "../../ui/copy";
import { Glyph } from "../../ui/icons";
import { AppHeader, LedStrip } from "../../ui/components/shell/Header";
import { Comic, type ComicKind } from "../../ui/components/comic/Comic";
import { LoadingStatus } from "../../ui/components/people/WaitingStatus";
import { Btn, Pill } from "../../ui/components/shell/primitives";

interface ViewerAccessGateProps {
  accessState: "checking" | "ready" | "denied";
  failureCode: ViewerFailureCode | null;
  messageKey: CopyKey;
  activity: StatusDescriptor;
  vis: boolean;
  t: (key: CopyKey, vars?: Record<string, string>) => string;
  viewerGrant: string | undefined;
  invalidGrant: boolean;
  viewerPasswordDraft: string;
  viewerPasswordError: CopyKey | null;
  viewerPasswordExpanded: boolean;
  onViewerPasswordDraftChange(value: string): void;
  onViewerPasswordErrorCleared(): void;
  onViewerPasswordExpandedChange(expanded: boolean): void;
  onViewerPasswordSubmit(event: FormEvent<HTMLFormElement>): void;
}

export function ViewerAccessGate({
  accessState,
  failureCode,
  messageKey,
  activity,
  vis,
  t,
  viewerGrant,
  invalidGrant,
  viewerPasswordDraft,
  viewerPasswordError,
  viewerPasswordExpanded,
  onViewerPasswordDraftChange,
  onViewerPasswordErrorCleared,
  onViewerPasswordExpandedChange,
  onViewerPasswordSubmit,
}: ViewerAccessGateProps) {
  const missingCredential = failureCode === "INVALID_TOKEN" && !viewerGrant && !invalidGrant;
  const malformedInvite = invalidGrant && (failureCode === "INVALID_TOKEN" || failureCode === "ROOM_ACCESS_DENIED");
  const deniedMessageKey = malformedInvite ? "viewer.msg.invalidInvite"
    : missingCredential ? "viewer.msg.accessFailed" : messageKey;
  const codeOnlyDenied =
    !viewerGrant && failureCode === "ROOM_ACCESS_DENIED";
  const canRefresh =
    failureCode === null || missingCredential ||
    // ROOM_FULL is transient by nature: a seat frees when a Viewer leaves,
    // so the hint that says "try again" needs something to try.
    [
      "ROOM_FULL",
      "STALE_CLIENT",
      "SERVER_ERROR",
      "SESSION_REPLACED",
      "SIGNAL_TERMINATED",
    ].includes(failureCode);
  const deniedComic: ComicKind = malformedInvite ? "invalid-invite"
    : missingCredential ? "access-denied" : activity.comic ?? "signal-failed";
  const deniedHintKey: CopyKey = malformedInvite ? "viewer.hint.invite"
    : missingCredential ? "viewer.hint.accessFailed"
    : failureCode === "ROOM_FULL" ? "viewer.hint.full"
    : codeOnlyDenied
    ? "viewer.hint.denied"
    : failureCode === "ROOM_CLOSED" ? "viewer.hint.closed"
    : failureCode === "SESSION_REPLACED" ? "viewer.hint.sessionReplaced"
    : failureCode === "ROOM_NOT_FOUND"
      ? "viewer.hint.notFound"
      : failureCode === "INVALID_TOKEN"
        // A grant that the room no longer accepts is not a retry case:
        // the recovery is a new invite, not another attempt.
        ? "viewer.hint.invite"
        : "viewer.hint.generic";
  return (
    <div className="lr-app">
      <AppHeader
        led={
          accessState === "checking" ? (
            <LedStrip state="busy" label={t(messageKey)} comic="signal-connecting" />
          ) : undefined
        }
      />
      <main className="lr-join">
        {accessState === "checking" ? (
          <div className="lr-join-panel">
            <LoadingStatus label={messageKey} />
          </div>
        ) : (
          <div className="lr-join-panel">
            <Comic kind={deniedComic} theme="paper" tone={activity.tone} />
            <span className="visually-hidden" role="alert">
              {t(deniedMessageKey)}{"\n"}{t(deniedHintKey, { site: window.location.host })}
            </span>
            {vis ? null : (
              <div className="lr-access-text">
                <h1>{t(deniedMessageKey)}</h1>
                <p className="lr-join-site">{t(deniedHintKey, { site: window.location.host })}</p>
              </div>
            )}
            {codeOnlyDenied && (
              <>
                <Btn
                  icon="key"
                  cap="join.passwordAction"
                  title="join.passwordAction"
                  hint={viewerPasswordExpanded ? "hint-collapse" : "hint-password"}
                  expanded={viewerPasswordExpanded}
                  controls="viewer-password-retry"
                  onClick={() => onViewerPasswordExpandedChange(!viewerPasswordExpanded)}
                />
                {viewerPasswordExpanded ? (
                  <div id="viewer-password-retry">
                    <form
                    style={{ display: "grid", justifyItems: "center", gap: 14 }}
                    onSubmit={onViewerPasswordSubmit}
                  >
                <span className="lr-input is-password">
                  <Glyph name="key" size={17} />
                  <input
                    type="password"
                    value={viewerPasswordDraft}
                    maxLength={MAX_VIEWER_PASSWORD_LENGTH}
                    autoComplete="current-password"
                    autoFocus
                    aria-label={t("join.password")}
                    onChange={(event) => {
                      onViewerPasswordDraftChange(event.target.value);
                      onViewerPasswordErrorCleared();
                    }}
                  />
                </span>
                {viewerPasswordError && (
                  <Pill icon="alert" tone="bad" label={t(viewerPasswordError, { max: String(MAX_VIEWER_PASSWORD_LENGTH) })} alert comic="access-denied" />
                )}
                <Btn
                  icon="arrowRight"
                  tone="primary"
                  cap="join.submit"
                  title="join.submit"
                  type="submit"
                  hint="hint-password"
                />
                    </form>
                  </div>
                ) : null}
              </>
            )}
            {canRefresh && (
              <Btn
                icon="refresh"
                cap="common.refresh"
                title="common.refresh"
                hint="page-refresh"
                onClick={() => window.location.reload()}
              />
            )}
          </div>
        )}
      </main>
    </div>
  );
}
