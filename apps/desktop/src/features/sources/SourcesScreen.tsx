import { useCallback, useState } from "react";
import "./sources.css";
import type {
  PlaudConnectorSnapshot,
  PlaudImportOutcome,
  PlaudItemSummary,
  PlaudListResponse,
} from "../../ipc/contract";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { useAppState } from "../../app/AppState";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { Button } from "../../components/Button";
import { ConnectorPanel } from "./ConnectorPanel";
import { RecordingsList } from "./RecordingsList";
import { canList, type ControlRun } from "./plaudPresentation";

const SOURCE_EVENTS = [
  "source.auth_required",
  "source.sync_finished",
  "recording.source_updated",
] as const;

const NO_ARGS = {} as const;

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

  const [override, setOverride] = useState<PlaudConnectorSnapshot | null>(null);
  const [items, setItems] = useState<PlaudItemSummary[]>([]);
  const [listing, setListing] = useState<PlaudListResponse | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [outcomes, setOutcomes] = useState<ReadonlyMap<string, PlaudImportOutcome>>(new Map());

  const snapshot = override ?? probe.data;
  const busy = connect.pending || disconnect.pending || list.pending || importer.pending;

  const refreshSnapshot = useCallback(async () => {
    setOverride(null);
    await probe.refetch();
  }, [probe]);
  useRefetchOnEvents(SOURCE_EVENTS, () => void refreshSnapshot());

  const loadPage = useCallback(
    async (cursor: string | null, keepOutcomes = false) => {
      try {
        const page = await list.run({ cursor });
        setListing(page);
        setItems((current) => (cursor === null ? page.items : [...current, ...page.items]));
        if (cursor === null) {
          setSelected(new Set());
          if (!keepOutcomes) setOutcomes(new Map());
        }
      } catch {
        // Rendered from `list.error` below.
      }
      // The connector's state and warnings changed with the attempt.
      await refreshSnapshot();
    },
    [list, refreshSnapshot],
  );

  const run = useCallback(
    async (control: ControlRun) => {
      if (control === "review_local_only_mode") {
        navigate({ name: "settings" });
        return;
      }
      try {
        const next =
          control === "connect"
            ? await connect.run({ user_intent: "connect_plaud_account" })
            : await probe.refetch().then(() => null);
        if (next) setOverride(next);
      } catch {
        await refreshSnapshot();
      }
    },
    [connect, navigate, probe, refreshSnapshot],
  );

  const onDisconnect = useCallback(async () => {
    try {
      const response = await disconnect.run(NO_ARGS);
      setOverride(response.snapshot);
      setItems([]);
      setListing(null);
      setSelected(new Set());
      setOutcomes(new Map());
    } catch {
      await refreshSnapshot();
    }
  }, [disconnect, refreshSnapshot]);

  const onImport = useCallback(async () => {
    try {
      const response = await importer.run({ external_ids: [...selected] });
      setOutcomes(new Map(response.results.map((result) => [result.external_id, result])));
      await loadPage(null, true);
    } catch {
      // Rendered from `importer.error` below.
    }
  }, [importer, loadPage, selected]);

  const toggle = (externalId: string) =>
    setSelected((current) => {
      const next = new Set(current);
      if (!next.delete(externalId)) next.add(externalId);
      return next;
    });
  const toggleAll = () =>
    setSelected((current) => {
      const selectable = items.filter((i) => i.imported_recording_id === null);
      return selectable.every((i) => current.has(i.external_id))
        ? new Set()
        : new Set(selectable.map((i) => i.external_id));
    });

  const failure = connect.error ?? disconnect.error ?? list.error ?? importer.error ?? null;

  return (
    <div className="sources">
      <p className="sources__status" role="status">
        {probe.loading && !snapshot ? "Checking the PLAUD connector…" : ""}
      </p>

      {probe.error && !snapshot ? (
        <div className="sources__error" role="alert">
          <p>{probe.error.envelope.message}</p>
          <Button onClick={() => void probe.refetch()}>Try again</Button>
        </div>
      ) : null}

      {snapshot ? (
        <ConnectorPanel
          snapshot={snapshot}
          busy={busy}
          onRun={(control) => void run(control)}
          onDisconnect={() => void onDisconnect()}
        />
      ) : null}

      {failure ? (
        <div className="sources__error" role="alert">
          <p>{failure.envelope.message}</p>
          {failure.envelope.diagnostic_id ? (
            <p className="sources__diagnostic">Diagnostic id: {failure.envelope.diagnostic_id}</p>
          ) : null}
        </div>
      ) : null}

      {snapshot && canList(snapshot.status.state) ? (
        <div className="sources__list-controls">
          <Button disabled={busy} onClick={() => void loadPage(null)}>
            {listing ? "Refresh recordings" : "List my PLAUD recordings"}
          </Button>
        </div>
      ) : null}

      {listing ? (
        <RecordingsList
          items={items}
          listing={listing}
          selected={selected}
          outcomes={outcomes}
          busy={busy}
          onToggle={toggle}
          onToggleAll={toggleAll}
          onImport={() => void onImport()}
          onLoadMore={() => void loadPage(listing.next_cursor)}
        />
      ) : null}
    </div>
  );
}
