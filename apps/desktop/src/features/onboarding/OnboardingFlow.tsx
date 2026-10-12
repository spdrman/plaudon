import { useEffect, useReducer, useRef, useState, type FormEvent } from "react";
import "./onboarding.css";
import type { Capabilities, EngineInventoryEntry, Preferences } from "../../ipc/contract";
import { useCommandMutation } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { TextField } from "../../components/Field";
import { StatusBadge, type StatusTone } from "../../components/StatusBadge";
import { ErrorNotice } from "../settings/ErrorNotice";
import { formatBytes } from "../settings/bytes";
import {
  currentStep,
  initialOnboardingState,
  onboardingReducer,
  progressLabel,
  SKIPPABLE_STEPS,
  STEP_TITLES,
  LAST_STEP_INDEX,
} from "./onboardingMachine";

export interface OnboardingFlowProps {
  capabilities: Capabilities;
  onCompleted: (preferences: Preferences) => void;
}

const STATUS_TONES: Record<string, StatusTone> = {
  supported: "success",
  experimental: "warning",
  unsupported: "danger",
  unknown: "neutral",
};

const STATUS_GLYPHS: Record<string, string> = {
  supported: "✓",
  experimental: "~",
  unsupported: "✕",
  unknown: "?",
};

const STATUS_LABELS: Record<string, string> = {
  supported: "Supported",
  experimental: "Experimental",
  unsupported: "Unsupported",
  unknown: "Unknown",
};

/**
 * First-run onboarding (§4.1, §5.2). Nothing here touches the network, asks
 * for an OS permission or downloads a model; every command is issued only in
 * direct response to a user action.
 */
export function OnboardingFlow({ capabilities, onCompleted }: OnboardingFlowProps) {
  const [state, dispatch] = useReducer(onboardingReducer, initialOnboardingState);
  const step = currentStep(state);
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  const firstRender = useRef(true);

  const localOnly = useCommandMutation("set_local_only_mode");
  const saveKey = useCommandMutation("set_openrouter_key");
  const complete = useCommandMutation("complete_onboarding");

  const [keyDraft, setKeyDraft] = useState("");

  useEffect(() => {
    if (firstRender.current) {
      firstRender.current = false;
      return;
    }
    headingRef.current?.focus();
  }, [state.stepIndex]);

  const sttEngines = capabilities.engines.filter((engine) => engine.stage === "stt");

  async function chooseMode(nextLocalOnly: boolean) {
    dispatch({ type: "choose-mode", localOnly: nextLocalOnly });
    await localOnly.run({ enabled: nextLocalOnly, confirmed: true }).catch(() => undefined);
  }

  async function onSaveKey(event: FormEvent) {
    event.preventDefault();
    const key = keyDraft;
    if (key.trim().length === 0) return;
    try {
      await saveKey.run({ key });
      // The secret never survives the call: no state, no DOM, no logging (§16.4).
      setKeyDraft("");
      dispatch({ type: "key-saved" });
    } catch {
      /* rendered from saveKey.error */
    }
  }

  async function onFinish() {
    try {
      const preferences = await complete.run({
        default_stt_engine_id: state.defaultSttEngineId,
        default_text_engine_id: null,
        local_only_mode: state.localOnly,
        notifications: capabilities.preferences.notifications,
      });
      onCompleted(preferences);
    } catch {
      /* rendered from complete.error */
    }
  }

  return (
    <main
      className="onboarding"
      aria-labelledby="onboarding-title"
      onKeyDown={(event) => {
        // Escape must never discard setup or close the window (§5.3).
        if (event.key === "Escape") event.stopPropagation();
      }}
    >
      <h1 className="onboarding__title" id="onboarding-title">
        Set up Plaudon
      </h1>
      <p className="onboarding__progress">{progressLabel(state)}</p>

      <section className="onboarding__step" aria-labelledby="onboarding-step-title">
        <h2 className="onboarding__step-title" id="onboarding-step-title" tabIndex={-1} ref={headingRef}>
          {STEP_TITLES[step]}
        </h2>

        {step === "privacy" ? <PrivacyStep /> : null}

        {step === "mode" ? (
          <ModeStep
            localOnly={state.localOnly}
            onChoose={(value) => void chooseMode(value)}
            pending={localOnly.pending}
          />
        ) : null}

        {step === "engines" ? (
          <EnginesStep
            engines={capabilities.engines}
            sttEngines={sttEngines}
            freeBytes={capabilities.storage.free_bytes}
            localOnly={state.localOnly}
            selected={state.defaultSttEngineId}
            onSelect={(engineId) => dispatch({ type: "choose-engine", engineId })}
          />
        ) : null}

        {step === "key" ? (
          <form className="onboarding__form" onSubmit={(event) => void onSaveKey(event)}>
            <p>
              An OpenRouter key is optional. Transcription runs locally without one, and adding a
              key does not send anything anywhere by itself.
            </p>
            <TextField
              label="OpenRouter API key"
              type="password"
              autoComplete="off"
              value={keyDraft}
              onChange={(event) => setKeyDraft(event.target.value)}
              hint="Stored in the system keychain by the backend. It is never shown again."
            />
            <p aria-live="polite" className="onboarding__note">
              {state.keySaved ? "Key saved to the system keychain." : "No key saved."}
            </p>
            <Button type="submit" variant="primary" disabled={saveKey.pending}>
              Save key
            </Button>
          </form>
        ) : null}

        {step === "sources" ? (
          <SourcesStep onContinueToImport={() => dispatch({ type: "next" })} />
        ) : null}

        {step === "finish" ? (
          <FinishStep
            localOnly={state.localOnly}
            engineId={state.defaultSttEngineId}
            engines={sttEngines}
          />
        ) : null}

        {localOnly.error ? <ErrorNotice envelope={localOnly.error.envelope} /> : null}
        {saveKey.error ? <ErrorNotice envelope={saveKey.error.envelope} /> : null}
        {complete.error ? (
          <ErrorNotice envelope={complete.error.envelope} onRetry={() => void onFinish()} />
        ) : null}
      </section>

      <nav className="onboarding__actions" aria-label="Onboarding steps">
        <Button onClick={() => dispatch({ type: "back" })} disabled={state.stepIndex === 0}>
          Back
        </Button>
        {SKIPPABLE_STEPS[step] ? (
          <Button variant="quiet" onClick={() => dispatch({ type: "skip" })}>
            Skip this step
          </Button>
        ) : null}
        {state.stepIndex === LAST_STEP_INDEX ? (
          <Button variant="primary" onClick={() => void onFinish()} disabled={complete.pending}>
            Finish setup
          </Button>
        ) : (
          <Button variant="primary" onClick={() => dispatch({ type: "next" })}>
            Next
          </Button>
        )}
      </nav>
    </main>
  );
}

function PrivacyStep() {
  return (
    <div className="onboarding__prose">
      <p>
        Your recordings, transcripts and summaries stay on this computer unless you explicitly
        choose a cloud AI provider. Cloud processing is opt-in, per purpose, and never turned on
        for you.
      </p>
      <p>
        Plaudon asks for no network access and no operating-system permission during setup. Audio
        is read from files you pick; nothing is uploaded while you answer these questions.
      </p>
      <details className="onboarding__disclosure">
        <summary>What would leave this machine if you opt in later</summary>
        <ul>
          <li>Transcript text you send to a cloud text model, for that request only.</li>
          <li>The model and provider you chose, so the request can be routed.</li>
          <li>Never: audio files, file paths, your API key, or anything you did not select.</li>
        </ul>
      </details>
    </div>
  );
}

function ModeStep({
  localOnly,
  onChoose,
  pending,
}: {
  localOnly: boolean;
  onChoose: (localOnly: boolean) => void;
  pending: boolean;
}) {
  return (
    <fieldset className="onboarding__fieldset">
      <legend>Network policy</legend>
      <label className="onboarding__radio">
        <input
          type="radio"
          name="local-only-mode"
          value="local"
          checked={localOnly}
          disabled={pending}
          onChange={() => onChoose(true)}
        />
        <span>
          <strong>Local only</strong> (recommended) — all outbound network access is refused by the
          backend.
        </span>
      </label>
      <label className="onboarding__radio">
        <input
          type="radio"
          name="local-only-mode"
          value="cloud"
          checked={!localOnly}
          disabled={pending}
          onChange={() => onChoose(false)}
        />
        <span>
          <strong>Allow cloud AI later</strong> — choosing this now grants nothing by itself. No
          request is ever sent until you pick a provider and consent for that purpose.
        </span>
      </label>
    </fieldset>
  );
}

function EnginesStep({
  engines,
  sttEngines,
  freeBytes,
  localOnly,
  selected,
  onSelect,
}: {
  engines: EngineInventoryEntry[];
  sttEngines: EngineInventoryEntry[];
  freeBytes: number;
  localOnly: boolean;
  selected: string | null;
  onSelect: (engineId: string | null) => void;
}) {
  const selectable = sttEngines.filter((engine) => engine.status !== "unsupported");
  return (
    <div className="onboarding__engines">
      <ul className="onboarding__engine-list">
        {engines.map((engine) => (
          <li className="onboarding__engine" key={engine.engine_id}>
            <div className="onboarding__engine-head">
              <span className="onboarding__engine-name">{engine.display_name}</span>
              <span className="onboarding__engine-stage">
                {engine.stage === "stt" ? "Speech to text" : "Text"}
              </span>
              <StatusBadge
                tone={STATUS_TONES[engine.status] ?? "neutral"}
                glyph={STATUS_GLYPHS[engine.status] ?? "?"}
                label={STATUS_LABELS[engine.status] ?? "Unknown"}
              />
            </div>
            {engine.unavailable_reason ? (
              <p className="onboarding__engine-reason">{engine.unavailable_reason}</p>
            ) : null}
            {engine.installed_models.length > 0 ? (
              <ul className="onboarding__model-list">
                {engine.installed_models.map((model) => (
                  <li key={model.model_id}>
                    {model.display_name} — {formatBytes(model.size_bytes)} ·{" "}
                    {model.installed ? "installed" : "not installed"}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="onboarding__engine-reason">No models installed for this engine.</p>
            )}
            {engine.download_bytes_required !== null ? (
              <p className="onboarding__engine-disk">
                Needs {formatBytes(engine.download_bytes_required)}, {formatBytes(freeBytes)} free
                on this disk.
              </p>
            ) : (
              <p className="onboarding__engine-disk">Download size unknown.</p>
            )}
          </li>
        ))}
      </ul>

      <p className="onboarding__note">
        {localOnly
          ? "Local-only mode is on, so Plaudon downloads nothing — not even a model. "
          : "This build downloads nothing during setup. "}
        Installing models happens in Settings once a model source is configured, or from the
        offline model pack shipped with the installer.
      </p>

      <fieldset className="onboarding__fieldset">
        <legend>Default speech-to-text engine</legend>
        {selectable.length === 0 ? (
          <p className="onboarding__note">No usable speech-to-text engine was detected yet.</p>
        ) : (
          selectable.map((engine) => (
            <label className="onboarding__radio" key={engine.engine_id}>
              <input
                type="radio"
                name="default-stt-engine"
                value={engine.engine_id}
                checked={selected === engine.engine_id}
                onChange={() => onSelect(engine.engine_id)}
              />
              <span>{engine.display_name}</span>
            </label>
          ))
        )}
      </fieldset>
    </div>
  );
}

function SourcesStep({ onContinueToImport }: { onContinueToImport: () => void }) {
  return (
    <div className="onboarding__prose">
      <p>Every source below is optional. You can add audio at any time from the Library.</p>
      <ul className="onboarding__sources">
        <li>
          <strong>Import audio</strong> — pick files from this computer. This is the path this
          build supports end to end.
          <div className="onboarding__source-action">
            <Button variant="primary" onClick={onContinueToImport}>
              Continue to import audio
            </Button>
          </div>
        </li>
        <li>
          <strong>Connect PLAUD device (direct)</strong>{" "}
          <StatusBadge tone="neutral" glyph="–" label="Not in this build" /> — direct BLE/USB
          transfer is not shipped yet, so there is nothing to connect to here.
        </li>
        <li>
          <strong>Connect PLAUD account (cloud)</strong>{" "}
          <StatusBadge tone="neutral" glyph="–" label="Not in this build" /> — the official cloud
          CLI integration is not shipped yet, and nothing signs you in behind your back.
        </li>
      </ul>
    </div>
  );
}

function FinishStep({
  localOnly,
  engineId,
  engines,
}: {
  localOnly: boolean;
  engineId: string | null;
  engines: EngineInventoryEntry[];
}) {
  const engine = engines.find((candidate) => candidate.engine_id === engineId);
  return (
    <div className="onboarding__prose">
      <p>Plaudon will save these choices and open your library, where you can import audio.</p>
      <dl className="onboarding__summary">
        <dt>Network policy</dt>
        <dd>{localOnly ? "Local only" : "Cloud AI allowed later, nothing granted yet"}</dd>
        <dt>Default speech-to-text engine</dt>
        <dd>{engine ? engine.display_name : "Not chosen"}</dd>
      </dl>
    </div>
  );
}
