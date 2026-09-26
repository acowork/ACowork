/**
 * First-boot restricted-mode gate (ADR-076 §决策 12 v2).
 *
 * Rendered by App.tsx whenever `authStatus === "setup_required"` —
 * the Gateway is up but the passwordless `admin` account exists, so
 * `/api/auth/login` and every other `/api/*` route will 403 until
 * setup completes.
 *
 * This view does NOT offer an interactive setup form on purpose.
 * The first password is intentionally not enterable from a remote
 * Desktop: it is set on the Gateway host (TTY prompt at `acowork-gateway
 * --daemon` startup, the `admin-setup` subcommand, or a manual
 * `gateway.toml` edit). The view's job is to (a) tell the user what
 * to do and (b) detect the moment setup is done and continue.
 */

import { useEffect } from "react";
import { useAuthStore } from "../../stores/authStore";
import { useTranslation } from "../../i18n/useTranslation";

export function SetupRequiredView() {
  const { t } = useTranslation();
  const pollUntilSetupComplete = useAuthStore((s) => s.pollUntilSetupComplete);
  const stopSetupPoll = useAuthStore((s) => s.stopSetupPoll);

  // Make sure the poller is running for the entire time this view is
  // mounted. `pollUntilSetupComplete` is idempotent.
  useEffect(() => {
    pollUntilSetupComplete();
    return () => stopSetupPoll();
  }, [pollUntilSetupComplete, stopSetupPoll]);

  return (
    <div className="flex h-screen w-screen items-center justify-center bg-page-bg">
      <div className="w-[28rem] max-w-[90vw] rounded-xl border border-border bg-surface px-8 py-7 text-text shadow-lg">
        <h1 className="text-lg font-semibold">
          {t("account.setupRequiredTitle", { defaultValue: "Gateway is not ready" })}
        </h1>
        <p className="mt-3 text-sm leading-relaxed text-text-secondary">
          {t("account.setupRequiredBody", {
            defaultValue:
              "The Gateway is in first-boot setup. A passwordless admin account has been created on the Gateway host. Until it is given a password, the Gateway only answers /health and /api/status — every other endpoint, including login, is blocked.",
          })}
        </p>

        <div className="mt-5 rounded-md border border-border/70 bg-surface-muted p-4 text-sm">
          <p className="font-medium">
            {t("account.setupRequiredStepsHeader", {
              defaultValue: "On the Gateway host, run ONE of:",
            })}
          </p>
          <ol className="mt-2 list-decimal pl-5 text-text-secondary">
            <li>
              <code className="font-mono text-xs">
                ssh &lt;gateway-host&gt; &amp;&amp; acowork-gateway --daemon
              </code>
              <span className="ml-2 text-xs">
                {t("account.setupRequiredInteractive", {
                  defaultValue: "(you will be prompted for the new password)",
                })}
              </span>
            </li>
            <li className="mt-2">
              <code className="font-mono text-xs">
                acowork-gateway admin-setup --password-file &lt;path&gt;
              </code>
              <span className="ml-2 text-xs">
                {t("account.setupRequiredFile", {
                  defaultValue: "(password from a file)",
                })}
              </span>
            </li>
            <li className="mt-2">
              {t("account.setupRequiredToml", {
                defaultValue:
                  'Edit gateway.toml: set [multi_user].bootstrap_admin = { username = "admin", password = "..." } and restart.',
              })}
            </li>
          </ol>
        </div>

        <p className="mt-5 text-xs text-text-muted">
          {t("account.setupRequiredPolling", {
            defaultValue:
              "This page checks the Gateway every 5 seconds and will continue automatically once setup is finished.",
          })}
        </p>
      </div>
    </div>
  );
}