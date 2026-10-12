import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useCallback, useState } from "react";
import "./sources.css";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { useAppState } from "../../app/AppState";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { Button } from "../../components/Button";
import { ConnectorPanel } from "./ConnectorPanel";
import { RecordingsList } from "./RecordingsList";
import { canList } from "./plaudPresentation";
const SOURCE_EVENTS = [
    "source.auth_required",
    "source.sync_finished",
    "recording.source_updated",
];
const NO_ARGS = {};
/**
 * PLAUD and sources (K1.5). Everything the user reads about the connector
 * comes from one `plaud_probe` snapshot (§5.4); failures render the §15.4
 * envelope's own message. The screen only decides which command a control runs.
 */
export function SourcesScreen() {
    const { navigate } = useAppState();
    const probe = useCommandQuery("plaud_probe", NO_ARGS);
    const connect = useCommandMutation("plaud_connect");
    const disconnect = useCommandMutation("plaud_disconnect");
    const list = useCommandMutation("plaud_list");
    const importer = useCommandMutation("plaud_import");
    const [override, setOverride] = useState(null);
    const [items, setItems] = useState([]);
    const [listing, setListing] = useState(null);
    const [selected, setSelected] = useState(new Set());
    const [outcomes, setOutcomes] = useState(new Map());
    const snapshot = override ?? probe.data;
    const busy = connect.pending || disconnect.pending || list.pending || importer.pending;
    const refreshSnapshot = useCallback(async () => {
        setOverride(null);
        await probe.refetch();
    }, [probe]);
    useRefetchOnEvents(SOURCE_EVENTS, () => void refreshSnapshot());
    const loadPage = useCallback(async (cursor, keepOutcomes = false) => {
        try {
            const page = await list.run({ cursor });
            setListing(page);
            setItems((current) => (cursor === null ? page.items : [...current, ...page.items]));
            if (cursor === null) {
                setSelected(new Set());
                if (!keepOutcomes)
                    setOutcomes(new Map());
            }
        }
        catch {
            // Rendered from `list.error` below.
        }
        // The connector's state and warnings changed with the attempt.
        await refreshSnapshot();
    }, [list, refreshSnapshot]);
    const run = useCallback(async (control) => {
        if (control === "review_local_only_mode") {
            navigate({ name: "settings" });
            return;
        }
        try {
            const next = control === "connect"
                ? await connect.run({ user_intent: "connect_plaud_account" })
                : await probe.refetch().then(() => null);
            if (next)
                setOverride(next);
        }
        catch {
            await refreshSnapshot();
        }
    }, [connect, navigate, probe, refreshSnapshot]);
    const onDisconnect = useCallback(async () => {
        try {
            const response = await disconnect.run(NO_ARGS);
            setOverride(response.snapshot);
            setItems([]);
            setListing(null);
            setSelected(new Set());
            setOutcomes(new Map());
        }
        catch {
            await refreshSnapshot();
        }
    }, [disconnect, refreshSnapshot]);
    const onImport = useCallback(async () => {
        try {
            const response = await importer.run({ external_ids: [...selected] });
            setOutcomes(new Map(response.results.map((result) => [result.external_id, result])));
            await loadPage(null, true);
        }
        catch {
            // Rendered from `importer.error` below.
        }
    }, [importer, loadPage, selected]);
    const toggle = (externalId) => setSelected((current) => {
        const next = new Set(current);
        if (!next.delete(externalId))
            next.add(externalId);
        return next;
    });
    const toggleAll = () => setSelected((current) => {
        const selectable = items.filter((i) => i.imported_recording_id === null);
        return selectable.every((i) => current.has(i.external_id))
            ? new Set()
            : new Set(selectable.map((i) => i.external_id));
    });
    const failure = connect.error ?? disconnect.error ?? list.error ?? importer.error ?? null;
    return (_jsxs("div", { className: "sources", children: [_jsx("p", { className: "sources__status", role: "status", children: probe.loading && !snapshot ? "Checking the PLAUD connector…" : "" }), probe.error && !snapshot ? (_jsxs("div", { className: "sources__error", role: "alert", children: [_jsx("p", { children: probe.error.envelope.message }), _jsx(Button, { onClick: () => void probe.refetch(), children: "Try again" })] })) : null, snapshot ? (_jsx(ConnectorPanel, { snapshot: snapshot, busy: busy, onRun: (control) => void run(control), onDisconnect: () => void onDisconnect() })) : null, failure ? (_jsxs("div", { className: "sources__error", role: "alert", children: [_jsx("p", { children: failure.envelope.message }), failure.envelope.diagnostic_id ? (_jsxs("p", { className: "sources__diagnostic", children: ["Diagnostic id: ", failure.envelope.diagnostic_id] })) : null] })) : null, snapshot && canList(snapshot.status.state) ? (_jsx("div", { className: "sources__list-controls", children: _jsx(Button, { disabled: busy, onClick: () => void loadPage(null), children: listing ? "Refresh recordings" : "List my PLAUD recordings" }) })) : null, listing ? (_jsx(RecordingsList, { items: items, listing: listing, selected: selected, outcomes: outcomes, busy: busy, onToggle: toggle, onToggleAll: toggleAll, onImport: () => void onImport(), onLoadMore: () => void loadPage(listing.next_cursor) })) : null] }));
}
