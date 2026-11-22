import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { ArrowUpRight, Check, CircleAlert, MessageSquare, Star, X } from "lucide-react";
import type { NativeIdentity } from "@agent-wechat/shared";
import { desktop, desktopInvoke } from "./desktop.ts";
import { t } from "./i18n.ts";
import {
  buildIssueUrl,
  GITHUB_REPO_URL,
  SupportPromptController,
  type SupportPromptStep,
} from "./support-prompt.ts";
import "./support-prompt.css";

let cachedDesktopVersion: string | null | undefined;
let desktopVersionRequest: Promise<string | null> | null = null;

function loadDesktopVersion(): Promise<string | null> {
  if (!desktop) return Promise.resolve(null);
  if (cachedDesktopVersion !== undefined) return Promise.resolve(cachedDesktopVersion);
  if (!desktopVersionRequest) {
    desktopVersionRequest = desktopInvoke<{ currentVersion?: string }>("updates_status")
      .then((value) => {
        cachedDesktopVersion = typeof value?.currentVersion === "string" ? value.currentVersion : null;
        return cachedDesktopVersion;
      })
      .catch(() => {
        desktopVersionRequest = null;
        return null;
      });
  }
  return desktopVersionRequest;
}

export function useSupportIssueUrl(hubVersion?: Pick<NativeIdentity, "version" | "target"> | null): string {
  const [desktopVersion, setDesktopVersion] = useState<string | null>(
    desktop && cachedDesktopVersion !== undefined ? cachedDesktopVersion : null,
  );
  useEffect(() => {
    if (!desktop || cachedDesktopVersion !== undefined) return;
    let cancelled = false;
    void loadDesktopVersion().then((value) => {
      if (!cancelled) setDesktopVersion(value);
    });
    return () => {
      cancelled = true;
    };
  }, []);
  return useMemo(
    () => buildIssueUrl({ desktop, desktopVersion, hubVersion }),
    [desktopVersion, hubVersion],
  );
}

/**
 * Tracks message sends and shows the two-step GitHub Star / Issue prompt at most
 * once per session after 3 distinct active days and 15 sent messages, once the
 * window is idle and no call, draft, modal, or update notice is active.
 */
export function useSupportPrompt({
  simulator,
  suppressed,
  hardInterrupt,
}: {
  simulator: boolean;
  /** Suppresses *opening* the prompt when idle finishes (composer draft, modals, etc.). */
  suppressed: boolean;
  /** High-priority interruption (call, disconnect, sign-out) that closes an open check-in without snapping back. */
  hardInterrupt: boolean;
}) {
  const controller = useMemo(() => new SupportPromptController({ simulator }), [simulator]);
  const step = useSyncExternalStore(controller.subscribe, controller.snapshot);

  useEffect(() => controller.start(), [controller]);
  useEffect(() => {
    controller.updateGuards({ suppressed, hardInterrupt });
  }, [controller, suppressed, hardInterrupt]);

  return {
    step,
    visible: step !== null,
    onMessageSent: controller.onMessageSent,
    choosePositive: controller.choosePositive,
    chooseNegative: controller.chooseNegative,
    completeStar: controller.completeStar,
    completeAlreadyStarred: controller.completeAlreadyStarred,
    completeIssue: controller.completeIssue,
    dismiss: controller.dismiss,
  };
}

export function SupportPrompt({
  step,
  issueUrl,
  onPositive,
  onNegative,
  onStar,
  onAlreadyStarred,
  onIssue,
  onDismiss,
}: {
  step: SupportPromptStep;
  issueUrl: string;
  onPositive: () => void;
  onNegative: () => void;
  onStar: () => void;
  onAlreadyStarred: () => void;
  onIssue: () => void;
  onDismiss: () => void;
}) {
  const isThanks =
    step === "thanks-star" || step === "thanks-already" || step === "thanks-issue";

  const primaryActionRef = useRef<HTMLAnchorElement | null>(null);
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);
  const moveFocusAfterTransitionRef = useRef(false);

  // Preserve keyboard focus inside the card when a step transition unmounts the clicked button.
  useEffect(() => {
    if (!moveFocusAfterTransitionRef.current) return;
    moveFocusAfterTransitionRef.current = false;
    if (step === "star" || step === "issue") {
      primaryActionRef.current?.focus();
    } else if (isThanks) {
      closeButtonRef.current?.focus();
    }
  }, [step, isThanks]);

  const iconClass =
    step === "star"
      ? "support-prompt-icon star"
      : isThanks
        ? "support-prompt-icon done"
        : "support-prompt-icon";

  const icon =
    step === "star" ? (
      <Star className="icon" aria-hidden="true" strokeWidth={1.7} />
    ) : step === "issue" ? (
      <CircleAlert className="icon" aria-hidden="true" strokeWidth={1.7} />
    ) : isThanks ? (
      <Check className="icon" aria-hidden="true" strokeWidth={1.9} />
    ) : (
      <MessageSquare className="icon" aria-hidden="true" strokeWidth={1.7} />
    );

  const title =
    step === "ask"
      ? t("supportPrompt.askTitle")
      : step === "star"
        ? t("supportPrompt.starTitle")
        : step === "issue"
          ? t("supportPrompt.issueTitle")
          : step === "thanks-already"
            ? t("supportPrompt.thanksAlreadyTitle")
            : step === "thanks-issue"
              ? t("supportPrompt.thanksIssueTitle")
              : t("supportPrompt.thanksStarTitle");

  const description =
    step === "ask"
      ? t("supportPrompt.askDescription")
      : step === "star"
        ? t("supportPrompt.starDescription")
        : step === "issue"
          ? t("supportPrompt.issueDescription")
          : t("supportPrompt.thanksStarDescription");

  return (
    <aside
      className="support-prompt"
      role="region"
      aria-label={t("supportPrompt.regionLabel")}
      onKeyDown={(event) => {
        if (event.key === "Escape") onDismiss();
      }}
    >
      <div className="support-prompt-head">
        <span className={iconClass}>{icon}</span>
        <div className="support-prompt-copy" aria-live="polite">
          <p className="support-prompt-title">{title}</p>
          <p className="support-prompt-description">{description}</p>
        </div>
        <button
          ref={closeButtonRef}
          type="button"
          className="support-prompt-close"
          aria-label={t("supportPrompt.dismiss")}
          onClick={onDismiss}
        >
          <X className="icon" aria-hidden="true" strokeWidth={1.8} />
        </button>
      </div>
      {!isThanks && (
        <div className="support-prompt-actions">
          {step === "ask" && (
            <>
              <button
                type="button"
                className="support-prompt-btn primary"
                onClick={() => {
                  moveFocusAfterTransitionRef.current = true;
                  onPositive();
                }}
              >
                {t("supportPrompt.askPositive")}
              </button>
              <button
                type="button"
                className="support-prompt-btn secondary"
                onClick={() => {
                  moveFocusAfterTransitionRef.current = true;
                  onNegative();
                }}
              >
                {t("supportPrompt.askNegative")}
              </button>
            </>
          )}
          {step === "star" && (
            <>
              <a
                ref={primaryActionRef}
                className="support-prompt-btn primary"
                href={GITHUB_REPO_URL}
                target="_blank"
                rel="noopener noreferrer"
                onClick={() => {
                  moveFocusAfterTransitionRef.current = true;
                  onStar();
                }}
              >
                <Star className="icon" aria-hidden="true" strokeWidth={1.7} />
                {t("supportPrompt.starAction")}
                <ArrowUpRight className="icon" aria-hidden="true" strokeWidth={1.8} />
              </a>
              <button
                type="button"
                className="support-prompt-btn ghost"
                onClick={() => {
                  moveFocusAfterTransitionRef.current = true;
                  onAlreadyStarred();
                }}
              >
                {t("supportPrompt.alreadyStarred")}
              </button>
            </>
          )}
          {step === "issue" && (
            <>
              <a
                ref={primaryActionRef}
                className="support-prompt-btn primary"
                href={issueUrl}
                target="_blank"
                rel="noopener noreferrer"
                onClick={() => {
                  moveFocusAfterTransitionRef.current = true;
                  onIssue();
                }}
              >
                {t("supportPrompt.issueAction")}
                <ArrowUpRight className="icon" aria-hidden="true" strokeWidth={1.8} />
              </a>
              <button
                type="button"
                className="support-prompt-btn ghost"
                onClick={onDismiss}
              >
                {t("supportPrompt.dismiss")}
              </button>
            </>
          )}
        </div>
      )}
    </aside>
  );
}

export function SupportSettingsLinks({ issueUrl }: { issueUrl: string }) {
  return (
    <div className="setting support-settings">
      <span>
        <strong>{t("supportPrompt.settingsTitle")}</strong>
        <small>{t("supportPrompt.settingsDescription")}</small>
      </span>
      <div className="support-settings-links">
        <a
          className="support-prompt-btn secondary"
          href={GITHUB_REPO_URL}
          target="_blank"
          rel="noopener noreferrer"
        >
          <Star className="icon" aria-hidden="true" strokeWidth={1.7} />
          {t("supportPrompt.starAction")}
          <ArrowUpRight className="icon" aria-hidden="true" strokeWidth={1.8} />
        </a>
        <a
          className="support-prompt-btn secondary"
          href={issueUrl}
          target="_blank"
          rel="noopener noreferrer"
        >
          {t("supportPrompt.reportIssue")}
          <ArrowUpRight className="icon" aria-hidden="true" strokeWidth={1.8} />
        </a>
      </div>
    </div>
  );
}
