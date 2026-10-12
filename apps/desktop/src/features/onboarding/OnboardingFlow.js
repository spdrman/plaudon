import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useEffect, useReducer, useRef, useState } from "react";
import "./onboarding.css";
import { useCommandMutation } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { TextField } from "../../components/Field";
import { StatusBadge } from "../../components/StatusBadge";
import { ErrorNotice } from "../settings/ErrorNotice";
import { formatBytes } from "../settings/bytes";
import { currentStep, initialOnboardingState, onboardingReducer, progressLabel, SKIPPABLE_STEPS, STEP_TITLES, LAST_STEP_INDEX, } from "./onboardingMachine";
const STATUS_TONES = {
    supported: "success",
    experimental: "warning",
    unsupported: "danger",
    unknown: "neutral",
};
const STATUS_GLYPHS = {
    supported: "✓",
    experimental: "~",
    unsupported: "✕",
    unknown: "?",
};
/** Label text is derived from the backend-provided status; no claim is hardcoded here. */
function statusLabel(status) {
    return status ? status.charAt(0).toUpperCase() + status.slice(1) : "unknown";
}
/**
 * First-run onboarding (§4.1, §5.2). Nothing here touches the network, asks
 * for an OS permission or downloads a model; every command is issued only in
 * direct response to a user action.
 */
export function OnboardingFlow({ capabilities, onCompleted }) {
    const [state, dispatch] = useReducer(onboardingReducer, initialOnboardingState);
    const step = currentStep(state);
    const headingRef = useRef(null);
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
    async function chooseMode(nextLocalOnly) {
        dispatch({ type: "choose-mode", localOnly: nextLocalOnly });
        await localOnly.run({ enabled: nextLocalOnly, confirmed: true }).catch(() => undefined);
    }
    async function onSaveKey(event) {
        event.preventDefault();
        const key = keyDraft;
        if (key.trim().length === 0)
            return;
        try {
            await saveKey.run({ key });
            // The secret never survives the call: no state, no DOM, no logging (§16.4).
            setKeyDraft("");
            dispatch({ type: "key-saved" });
        }
        catch {
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
        }
        catch {
            /* rendered from complete.error */
        }
    }
    return (_jsxs("main", { className: "onboarding", "aria-labelledby": "onboarding-title", onKeyDown: (event) => {
            // Escape must never discard setup or close the window (§5.3).
            if (event.key === "Escape")
                event.stopPropagation();
        }, children: [_jsx("h1", { className: "onboarding__title", id: "onboarding-title", children: "Set up Plaudon" }), _jsx("p", { className: "onboarding__progress", children: progressLabel(state) }), _jsxs("section", { className: "onboarding__step", "aria-labelledby": "onboarding-step-title", children: [_jsx("h2", { className: "onboarding__step-title", id: "onboarding-step-title", tabIndex: -1, ref: headingRef, children: STEP_TITLES[step] }), step === "privacy" ? _jsx(PrivacyStep, {}) : null, step === "mode" ? (_jsx(ModeStep, { localOnly: state.localOnly, onChoose: (value) => void chooseMode(value), pending: localOnly.pending })) : null, step === "engines" ? (_jsx(EnginesStep, { engines: capabilities.engines, sttEngines: sttEngines, freeBytes: capabilities.storage.free_bytes, localOnly: state.localOnly, selected: state.defaultSttEngineId, onSelect: (engineId) => dispatch({ type: "choose-engine", engineId }) })) : null, step === "key" ? (_jsxs("form", { className: "onboarding__form", onSubmit: (event) => void onSaveKey(event), children: [_jsx("p", { children: "An OpenRouter key is optional. Transcription runs locally without one, and adding a key does not send anything anywhere by itself." }), _jsx(TextField, { label: "OpenRouter API key", type: "password", autoComplete: "off", value: keyDraft, onChange: (event) => setKeyDraft(event.target.value), hint: "Stored in the system keychain by the backend. It is never shown again." }), _jsx("p", { "aria-live": "polite", className: "onboarding__note", children: state.keySaved ? "Key saved to the system keychain." : "No key saved." }), _jsx(Button, { type: "submit", variant: "primary", disabled: saveKey.pending, children: "Save key" })] })) : null, step === "sources" ? (_jsx(SourcesStep, { onContinueToImport: () => dispatch({ type: "next" }) })) : null, step === "finish" ? (_jsx(FinishStep, { localOnly: state.localOnly, engineId: state.defaultSttEngineId, engines: sttEngines })) : null, localOnly.error ? _jsx(ErrorNotice, { envelope: localOnly.error.envelope }) : null, saveKey.error ? _jsx(ErrorNotice, { envelope: saveKey.error.envelope }) : null, complete.error ? (_jsx(ErrorNotice, { envelope: complete.error.envelope, onRetry: () => void onFinish() })) : null] }), _jsxs("nav", { className: "onboarding__actions", "aria-label": "Onboarding steps", children: [_jsx(Button, { onClick: () => dispatch({ type: "back" }), disabled: state.stepIndex === 0, children: "Back" }), SKIPPABLE_STEPS[step] ? (_jsx(Button, { variant: "quiet", onClick: () => dispatch({ type: "skip" }), children: "Skip this step" })) : null, state.stepIndex === LAST_STEP_INDEX ? (_jsx(Button, { variant: "primary", onClick: () => void onFinish(), disabled: complete.pending, children: "Finish setup" })) : (_jsx(Button, { variant: "primary", onClick: () => dispatch({ type: "next" }), children: "Next" }))] })] }));
}
function PrivacyStep() {
    return (_jsxs("div", { className: "onboarding__prose", children: [_jsx("p", { children: "Your recordings, transcripts and summaries stay on this computer unless you explicitly choose a cloud AI provider. Cloud processing is opt-in, per purpose, and never turned on for you." }), _jsx("p", { children: "Plaudon asks for no network access and no operating-system permission during setup. Audio is read from files you pick; nothing is uploaded while you answer these questions." }), _jsxs("details", { className: "onboarding__disclosure", children: [_jsx("summary", { children: "What would leave this machine if you opt in later" }), _jsxs("ul", { children: [_jsx("li", { children: "Transcript text you send to a cloud text model, for that request only." }), _jsx("li", { children: "The model and provider you chose, so the request can be routed." }), _jsx("li", { children: "Never: audio files, file paths, your API key, or anything you did not select." })] })] })] }));
}
function ModeStep({ localOnly, onChoose, pending, }) {
    return (_jsxs("fieldset", { className: "onboarding__fieldset", children: [_jsx("legend", { children: "Network policy" }), _jsxs("label", { className: "onboarding__radio", children: [_jsx("input", { type: "radio", name: "local-only-mode", value: "local", checked: localOnly, disabled: pending, onChange: () => onChoose(true) }), _jsxs("span", { children: [_jsx("strong", { children: "Local only" }), " (recommended) \u2014 all outbound network access is refused by the backend."] })] }), _jsxs("label", { className: "onboarding__radio", children: [_jsx("input", { type: "radio", name: "local-only-mode", value: "cloud", checked: !localOnly, disabled: pending, onChange: () => onChoose(false) }), _jsxs("span", { children: [_jsx("strong", { children: "Allow cloud AI later" }), " \u2014 choosing this now grants nothing by itself. No request is ever sent until you pick a provider and consent for that purpose."] })] })] }));
}
function EnginesStep({ engines, sttEngines, freeBytes, localOnly, selected, onSelect, }) {
    const selectable = sttEngines.filter((engine) => engine.status !== "unsupported");
    return (_jsxs("div", { className: "onboarding__engines", children: [_jsx("ul", { className: "onboarding__engine-list", children: engines.map((engine) => (_jsxs("li", { className: "onboarding__engine", children: [_jsxs("div", { className: "onboarding__engine-head", children: [_jsx("span", { className: "onboarding__engine-name", children: engine.display_name }), _jsx("span", { className: "onboarding__engine-stage", children: engine.stage === "stt" ? "Speech to text" : "Text" }), _jsx(StatusBadge, { tone: STATUS_TONES[engine.status] ?? "neutral", glyph: STATUS_GLYPHS[engine.status] ?? "?", label: statusLabel(engine.status) })] }), engine.unavailable_reason ? (_jsx("p", { className: "onboarding__engine-reason", children: engine.unavailable_reason })) : null, engine.installed_models.length > 0 ? (_jsx("ul", { className: "onboarding__model-list", children: engine.installed_models.map((model) => (_jsxs("li", { children: [model.display_name, " \u2014 ", formatBytes(model.size_bytes), " \u00B7", " ", model.installed ? "installed" : "not installed"] }, model.model_id))) })) : (_jsx("p", { className: "onboarding__engine-reason", children: "No models installed for this engine." })), engine.download_bytes_required !== null ? (_jsxs("p", { className: "onboarding__engine-disk", children: ["Needs ", formatBytes(engine.download_bytes_required), ", ", formatBytes(freeBytes), " free on this disk."] })) : (_jsx("p", { className: "onboarding__engine-disk", children: "Download size unknown." }))] }, engine.engine_id))) }), _jsxs("p", { className: "onboarding__note", children: [localOnly
                        ? "Local-only mode is on, so Plaudon downloads nothing — not even a model. "
                        : "This build downloads nothing during setup. ", "Installing models happens in Settings once a model source is configured, or from the offline model pack shipped with the installer."] }), _jsxs("fieldset", { className: "onboarding__fieldset", children: [_jsx("legend", { children: "Default speech-to-text engine" }), selectable.length === 0 ? (_jsx("p", { className: "onboarding__note", children: "No usable speech-to-text engine was detected yet." })) : (selectable.map((engine) => (_jsxs("label", { className: "onboarding__radio", children: [_jsx("input", { type: "radio", name: "default-stt-engine", value: engine.engine_id, checked: selected === engine.engine_id, onChange: () => onSelect(engine.engine_id) }), _jsx("span", { children: engine.display_name })] }, engine.engine_id))))] })] }));
}
function SourcesStep({ onContinueToImport }) {
    return (_jsxs("div", { className: "onboarding__prose", children: [_jsx("p", { children: "Every source below is optional. You can add audio at any time from the Library." }), _jsxs("ul", { className: "onboarding__sources", children: [_jsxs("li", { children: [_jsx("strong", { children: "Import audio" }), " \u2014 pick files from this computer. This is the path this build supports end to end.", _jsx("div", { className: "onboarding__source-action", children: _jsx(Button, { variant: "primary", onClick: onContinueToImport, children: "Continue to import audio" }) })] }), _jsxs("li", { children: [_jsx("strong", { children: "Connect PLAUD device (direct)" }), " ", _jsx(StatusBadge, { tone: "neutral", glyph: "\u2013", label: "Not in this build" }), " \u2014 direct BLE/USB transfer is not shipped yet, so there is nothing to connect to here."] }), _jsxs("li", { children: [_jsx("strong", { children: "Connect PLAUD account (cloud)" }), " ", _jsx(StatusBadge, { tone: "neutral", glyph: "\u2013", label: "Not in this build" }), " \u2014 the official cloud CLI integration is not shipped yet, and nothing signs you in behind your back."] })] })] }));
}
function FinishStep({ localOnly, engineId, engines, }) {
    const engine = engines.find((candidate) => candidate.engine_id === engineId);
    return (_jsxs("div", { className: "onboarding__prose", children: [_jsx("p", { children: "Plaudon will save these choices and open your library, where you can import audio." }), _jsxs("dl", { className: "onboarding__summary", children: [_jsx("dt", { children: "Network policy" }), _jsx("dd", { children: localOnly ? "Local only" : "Cloud AI allowed later, nothing granted yet" }), _jsx("dt", { children: "Default speech-to-text engine" }), _jsx("dd", { children: engine ? engine.display_name : "Not chosen" })] })] }));
}
