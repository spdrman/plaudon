import "./settings.css";
import { useState } from "react";
import type { LocalOnlyModeResponse } from "../../ipc/contract";
import { useAppState } from "../../app/AppState";
import { useCommandMutation } from "../../ipc/useCommand";
import { StatusBadge } from "../../components/StatusBadge";
import { ErrorNotice } from "./ErrorNotice";

/**
 * Privacy & network (§5.2 privacy settings, §16.2A). The toggle is the single
 * egress switch; the enforced policy we render comes back from the gate, never
 * from local guesswork.
 */
export function PrivacySection() {
  const { capabilities, refreshCapabilities } = useAppState();
  const setMode = useCommandMutation("set_local_only_mode");
  const [policy, setPolicy] = useState<LocalOnlyModeResponse | null>(null);
  const [attempted, setAttempted] = useState<boolean | null>(null);
  const enabled = policy?.enabled ?? capabilities?.preferences.local_only_mode ?? true;

  async function toggle(next: boolean) {
    setAttempted(next);
    try {
      // §15.2: the egress change is rejected unless it is explicitly confirmed.
      setPolicy(await setMode.run({ enabled: next, confirmed: true }));
      await refreshCapabilities();
    } catch {
      /* rendered from setMode.error */
    }
  }

  return (
    <>
      <p className="settings-section__prose">
        Recordings, transcripts and summaries stay on this computer. Cloud AI is opt-in: Plaudon
        sends nothing to a provider until you choose one and consent for that purpose.
      </p>
      <div className="settings-section__row">
        <label className="settings-choice">
          <input
            type="checkbox"
            checked={enabled}
            disabled={setMode.pending}
            onChange={(event) => void toggle(event.target.checked)}
          />
          <span>Local-only mode (refuse all outbound network access)</span>
        </label>
        <StatusBadge
          tone={enabled ? "success" : "warning"}
          glyph={enabled ? "✓" : "!"}
          label={enabled ? "Local only" : "Network allowed for providers you enable"}
        />
      </div>
      {policy ? (
        <div className="settings-section__policy" aria-live="polite">
          <p className="settings-section__note">Enforced policy: {policy.description}</p>
          {policy.allowed_hosts.length === 0 ? (
            <p className="settings-section__note">No hosts are reachable.</p>
          ) : (
            <ul className="settings-hosts" aria-label="Allowed hosts">
              {policy.allowed_hosts.map((host) => (
                <li key={host}>{host}</li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
      {setMode.error ? (
        <ErrorNotice
          envelope={setMode.error.envelope}
          onRetry={() => void toggle(attempted ?? !enabled)}
        />
      ) : null}
    </>
  );
}
