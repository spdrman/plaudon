import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
import "./settings.css";
import { useState } from "react";
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
    const [policy, setPolicy] = useState(null);
    const [attempted, setAttempted] = useState(null);
    const enabled = policy?.enabled ?? capabilities?.preferences.local_only_mode ?? true;
    async function toggle(next) {
        setAttempted(next);
        try {
            // §15.2: the egress change is rejected unless it is explicitly confirmed.
            setPolicy(await setMode.run({ enabled: next, confirmed: true }));
            await refreshCapabilities();
        }
        catch {
            /* rendered from setMode.error */
        }
    }
    return (_jsxs(_Fragment, { children: [_jsx("p", { className: "settings-section__prose", children: "Recordings, transcripts and summaries stay on this computer. Cloud AI is opt-in: Plaudon sends nothing to a provider until you choose one and consent for that purpose." }), _jsxs("div", { className: "settings-section__row", children: [_jsxs("label", { className: "settings-choice", children: [_jsx("input", { type: "checkbox", checked: enabled, disabled: setMode.pending, onChange: (event) => void toggle(event.target.checked) }), _jsx("span", { children: "Local-only mode (refuse all outbound network access)" })] }), _jsx(StatusBadge, { tone: enabled ? "success" : "warning", glyph: enabled ? "✓" : "!", label: enabled ? "Local only" : "Network allowed for providers you enable" })] }), policy ? (_jsxs("div", { className: "settings-section__policy", "aria-live": "polite", children: [_jsxs("p", { className: "settings-section__note", children: ["Enforced policy: ", policy.description] }), policy.allowed_hosts.length === 0 ? (_jsx("p", { className: "settings-section__note", children: "No hosts are reachable." })) : (_jsx("ul", { className: "settings-hosts", "aria-label": "Allowed hosts", children: policy.allowed_hosts.map((host) => (_jsx("li", { children: host }, host))) }))] })) : null, setMode.error ? (_jsx(ErrorNotice, { envelope: setMode.error.envelope, onRetry: () => void toggle(attempted ?? !enabled) })) : null] }));
}
