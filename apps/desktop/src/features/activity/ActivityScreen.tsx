import { useCallback, useState } from "react";
import "./activity.css";
import type { ActivityEntry, NotificationCategory } from "../../ipc/contract";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { StatusBadge, type StatusTone } from "../../components/StatusBadge";

const ACTIVITY_EVENTS = [
  "activity.added",
  "job.updated",
  "recording.imported",
  "transcript.completed",
] as const;

const PAGE_SIZE = 25;

export const CATEGORY_PRESENTATION: Record<
  NotificationCategory,
  { label: string; glyph: string; tone: StatusTone }
> = {
  import_finished: { label: "Import finished", glyph: "IN", tone: "success" },
  transcription_finished: { label: "Transcription finished", glyph: "TR", tone: "success" },
  requires_attention: { label: "Requires attention", glyph: "!", tone: "warning" },
  cloud_budget_exceeded: { label: "Cloud budget exceeded", glyph: "$", tone: "danger" },
};

export interface ActivityScreenProps {
  onOpenTarget: (target: { kind: "recording" | "job" | "settings"; id: string }) => void;
}

/**
 * §5.5 / §22(13): the in-app activity history records every completion and
 * attention event, whether or not the OS delivered a notification.
 */
export function ActivityScreen({ onOpenTarget }: ActivityScreenProps) {
  const [extra, setExtra] = useState<ActivityEntry[]>([]);
  const [tailCursor, setTailCursor] = useState<string | null>(null);

  const { data, error, loading, refetch } = useCommandQuery("list_activity", {
    cursor: null,
    limit: PAGE_SIZE,
  });
  const more = useCommandMutation("list_activity");

  const reload = useCallback(() => {
    setExtra([]);
    setTailCursor(null);
    void refetch();
  }, [refetch]);
  useRefetchOnEvents(ACTIVITY_EVENTS, reload);

  const items = [...(data?.items ?? []), ...extra];
  const nextCursor = tailCursor ?? data?.next_cursor ?? null;

  const loadMore = useCallback(async () => {
    if (!nextCursor) return;
    try {
      const page = await more.run({ cursor: nextCursor, limit: PAGE_SIZE });
      setExtra((current) => [...current, ...page.items]);
      setTailCursor(page.next_cursor);
    } catch {
      // Rendered from `more.error` below.
    }
  }, [more, nextCursor]);

  return (
    <section className="activity" aria-labelledby="activity-heading">
      <h2 id="activity-heading" className="activity__heading">
        Activity history
      </h2>
      <p className="activity__intro">
        Every completion and attention event is recorded here, even when desktop notifications are
        turned off or denied by the operating system.
      </p>

      <p className="activity__status" role="status">
        {loading
          ? "Loading activity…"
          : `${items.length} entr${items.length === 1 ? "y" : "ies"}, ${data?.unread_count ?? 0} unread`}
      </p>

      {error ? (
        <div className="activity__error" role="alert">
          <p>{error.envelope.message}</p>
          <Button onClick={reload}>Retry</Button>
        </div>
      ) : null}

      {more.error ? (
        <p className="activity__error" role="alert">
          {more.error.envelope.message}
        </p>
      ) : null}

      {!loading && !error && items.length === 0 ? (
        <EmptyState
          title="No activity yet"
          body="Imports, finished transcriptions and anything needing your attention are listed here."
          actions={<Button onClick={reload}>Refresh</Button>}
        />
      ) : null}

      {items.length > 0 ? (
        <ul className="activity__list">
          {items.map((entry) => (
            <ActivityRow key={entry.activity_id} entry={entry} onOpenTarget={onOpenTarget} />
          ))}
        </ul>
      ) : null}

      {nextCursor ? (
        <Button onClick={() => void loadMore()} disabled={more.pending}>
          Load older activity
        </Button>
      ) : null}
    </section>
  );
}

function ActivityRow({
  entry,
  onOpenTarget,
}: {
  entry: ActivityEntry;
  onOpenTarget: ActivityScreenProps["onOpenTarget"];
}) {
  const category = CATEGORY_PRESENTATION[entry.category];
  const target = entry.target;
  return (
    <li className="activity-row">
      <div className="activity-row__head">
        <StatusBadge tone={category.tone} glyph={category.glyph} label={category.label} />
        {/* Unread is marked with words, never with color alone (§5.3). */}
        <span className="activity-row__read">{entry.read ? "Read" : "Unread"}</span>
      </div>
      <p className="activity-row__title">{entry.title}</p>
      {entry.body ? <p className="activity-row__body">{entry.body}</p> : null}
      <p className="activity-row__meta">
        <time dateTime={entry.occurred_at}>{formatTimestamp(entry.occurred_at)}</time>
        {" · "}
        {entry.notification_delivered
          ? "Desktop notification delivered"
          : `Desktop notification not delivered${
              entry.notification_suppressed_reason
                ? `: ${entry.notification_suppressed_reason}`
                : ""
            }`}
      </p>
      {target ? (
        <Button variant="quiet" onClick={() => onOpenTarget(target)}>
          {target.kind === "recording"
            ? "Open recording"
            : target.kind === "job"
              ? "Open job"
              : "Open settings"}
        </Button>
      ) : null}
    </li>
  );
}

export function formatTimestamp(iso: string): string {
  const parsed = Date.parse(iso);
  if (Number.isNaN(parsed)) return iso;
  return new Date(parsed).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}
