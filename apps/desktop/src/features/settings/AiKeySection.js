import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
import "./settings.css";
import { useState } from "react";
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
    async function onSubmit(event) {
        event.preventDefault();
        if (draft.trim().length === 0)
            return;
        try {
            await saveKey.run({ key: draft });
            setDraft("");
            await refreshCapabilities();
        }
        catch {
            /* rendered from saveKey.error */
        }
    }
    async function onRemove() {
        try {
            await removeKey.run({});
            setConfirmOpen(false);
            await refreshCapabilities();
        }
        catch {
            /* rendered from removeKey.error */
        }
    }
    return (_jsxs(_Fragment, { children: [_jsx("div", { className: "settings-section__row", children: _jsx(StatusBadge, { tone: present ? "success" : "neutral", glyph: present ? "✓" : "–", label: present ? "Key saved in the system keychain" : "No key saved" }) }), _jsx("p", { className: "settings-section__note", children: "Plaudon never displays a saved key and never returns it from the backend. Having a key does not grant cloud processing on its own." }), _jsxs("form", { className: "settings-form", onSubmit: (event) => void onSubmit(event), children: [_jsx(TextField, { label: present ? "Replace OpenRouter API key" : "Set OpenRouter API key", type: "password", autoComplete: "off", value: draft, onChange: (event) => setDraft(event.target.value) }), _jsx(Button, { type: "submit", variant: "primary", disabled: saveKey.pending, children: present ? "Replace key" : "Set key" }), present ? (_jsx(Button, { variant: "danger", onClick: () => setConfirmOpen(true), children: "Remove key" })) : null] }), saveKey.error ? _jsx(ErrorNotice, { envelope: saveKey.error.envelope }) : null, removeKey.error ? (_jsx(ErrorNotice, { envelope: removeKey.error.envelope, onRetry: () => void onRemove() })) : null, _jsx(Dialog, { open: confirmOpen, destructive: true, title: "Remove the OpenRouter key?", description: "The key is deleted from the system keychain. Cloud requests stop working until you add a key again. Local transcription is unaffected.", onClose: () => setConfirmOpen(false), footer: _jsxs(_Fragment, { children: [_jsx(Button, { onClick: () => setConfirmOpen(false), children: "Cancel" }), _jsx(Button, { variant: "danger", onClick: () => void onRemove(), disabled: removeKey.pending, children: "Remove key" })] }) })] }));
}
