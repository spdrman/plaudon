import type { PlaudConnectorSnapshot } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { StatusBadge, type StatusTone } from "../../components/StatusBadge";
import { ImportButton } from "../import/ImportButton";
import {
  COMPATIBILITY_LABELS,
  STATE_LABELS,
  nextActionControl,
  type ControlRun,
} from "./plaudPresentation";

const STATE_TONE: Record<PlaudConnectorSnapshot["status"]["state"], StatusTone> = {
  unconfigured: "neutral",
  cli_detected: "waiting",
  auth_required: "warning",
  authenticated: "success",
  enumerating: "running",
  ready: "success",
  expired_auth: "warning",
  incompatible_version: "warning",
  error: "danger",
};

export interface ConnectorPanelProps {
  snapshot: PlaudConnectorSnapshot;
  busy: boolean;
  onRun: (run: ControlRun) => void;
  onDisconnect: () => void;
}

/**
 * Connector diagnostics (K1.5, §4.2, §7.6), rendered from one `plaud_probe`
 * snapshot. The headline, detail, warnings and disconnect wording are Rust's
 * strings and are rendered verbatim: this component composes no sentence and
 * there is no vendor text anywhere in the payload to render.
 */
export function ConnectorPanel({ snapshot, busy, onRun, onDisconnect }: ConnectorPanelProps) {
  const { diagnostics, status, disconnect } = snapshot;
  const control = nextActionControl(diagnostics.next_action);
  const usable =
    diagnostics.next_action.kind === "install_tested_version"
      ? diagnostics.next_action.usable_versions
      : [];

  return (
    <section className="connector" aria-labelledby="connector-heading">
      <h2 id="connector-heading" className="connector__headline">
        {diagnostics.headline}
      </h2>
      <p className="connector__detail">{diagnostics.detail}</p>

      <dl className="connector__facts">
        <dt>State</dt>
        <dd>
          <StatusBadge
            tone={STATE_TONE[diagnostics.state]}
            glyph={diagnostics.state === "ready" || diagnostics.state === "authenticated" ? "✓" : "•"}
            label={STATE_LABELS[diagnostics.state]}
          />
        </dd>
        <dt>Tool version</dt>
        <dd>{diagnostics.detected_version ?? "Not detected"}</dd>
        <dt>Compatibility</dt>
        <dd>{COMPATIBILITY_LABELS[diagnostics.compatibility]}</dd>
        <dt>Found in</dt>
        <dd>{diagnostics.program_origin ?? "Not found"}</dd>
        {status.last_error ? (
          <>
            <dt>Last error code</dt>
            <dd>
              <code>{status.last_error}</code>
            </dd>
          </>
        ) : null}
      </dl>

      {diagnostics.warnings.length > 0 ? (
        <ul className="connector__warnings" aria-label="Warnings">
          {diagnostics.warnings.map((warning) => (
            <li key={warning} className="connector__warning">
              <span aria-hidden="true" className="status-badge__glyph">
                !
              </span>
              {warning}
            </li>
          ))}
        </ul>
      ) : null}

      {usable.length > 0 ? (
        <p className="connector__versions">
          Tested versions: <span>{usable.join(", ")}</span>
        </p>
      ) : null}

      <div className="connector__actions">
        {control?.kind === "link" ? (
          <a className="btn btn--primary" href={control.href} target="_blank" rel="noreferrer noopener">
            {control.label}
          </a>
        ) : null}
        {control?.kind === "action" ? (
          <Button variant="primary" disabled={busy} onClick={() => onRun(control.run)}>
            {control.label}
          </Button>
        ) : null}
        {control?.kind === "import_manually" ? <ImportButton /> : null}
        {diagnostics.state === "unconfigured" ||
        diagnostics.state === "incompatible_version" ? (
          <Button disabled={busy} onClick={() => onRun("retry")}>
            Check again
          </Button>
        ) : null}
      </div>

      {diagnostics.state !== "unconfigured" ? (
        <div className="connector__disconnect">
          <p className="connector__disconnect-body">{disconnect.body}</p>
          <Button variant="danger" disabled={busy} onClick={onDisconnect}>
            {disconnect.label}
          </Button>
        </div>
      ) : null}
    </section>
  );
}
