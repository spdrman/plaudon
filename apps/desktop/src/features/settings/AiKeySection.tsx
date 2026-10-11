import "./settings.css";
import { useState, type FormEvent } from "react";
import { useAppState } from "../../app/AppState";
import { useCommandMutation } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { TextField } from "../../components/Field";
import { StatusBadge } from "../../components/StatusBadge";
import { Dialog } from "../../components/Dialog";
import { ErrorNotice } from "./ErrorNotice";

/**
 * OpenRouter key (§16.4): the UI only ever learns whether a key is present.
 * The value is handed to the backend once and dropped from React state.
 */
export function AiKeySection() {
  const { capabilities, refreshCapabilities } = useAppState();
  const saveKey = useCommandMutation("set_openrouter_key");
  const removeKey = useCommandMutation("remove_openrouter_key");
  const [draft, setDraft] = useState("");
  const [confirmOpen, setConfirmOpen] = useState(false);
  const present = capabilities?.openrouter_key_present ?? false;

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (draft.trim().length === 0) return;
    try {
      await saveKey.run({ key: draft });
      setDraft("");
      await refreshCapabilities();
    } catch {
      /* rendered from saveKey.error */
    }
  }

  async function onRemove() {
    try {
      await removeKey.run({});
      setConfirmOpen(false);
      await refreshCapabilities();
    } catch {
      /* rendered from removeKey.error */
    }
  }

  return (
    <>
      <div className="settings-section__row">
        <StatusBadge
          tone={present ? "success" : "neutral"}
          glyph={present ? "✓" : "–"}
          label={present ? "Key saved in the system keychain" : "No key saved"}
        />
      </div>
      <p className="settings-section__note">
        Plaudon never displays a saved key and never returns it from the backend. Having a key does
        not grant cloud processing on its own.
      </p>
      <form className="settings-form" onSubmit={(event) => void onSubmit(event)}>
        <TextField
          label={present ? "Replace OpenRouter API key" : "Set OpenRouter API key"}
          type="password"
          autoComplete="off"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
        <Button type="submit" variant="primary" disabled={saveKey.pending}>
          {present ? "Replace key" : "Set key"}
        </Button>
        {present ? (
          <Button variant="danger" onClick={() => setConfirmOpen(true)}>
            Remove key
          </Button>
        ) : null}
      </form>
      {saveKey.error ? <ErrorNotice envelope={saveKey.error.envelope} /> : null}
      {removeKey.error ? (
        <ErrorNotice envelope={removeKey.error.envelope} onRetry={() => void onRemove()} />
      ) : null}
      <Dialog
        open={confirmOpen}
        destructive
        title="Remove the OpenRouter key?"
        description="The key is deleted from the system keychain. Cloud requests stop working until you add a key again. Local transcription is unaffected."
        onClose={() => setConfirmOpen(false)}
        footer={
          <>
            <Button onClick={() => setConfirmOpen(false)}>Cancel</Button>
            <Button variant="danger" onClick={() => void onRemove()} disabled={removeKey.pending}>
              Remove key
            </Button>
          </>
        }
      />
    </>
  );
}
