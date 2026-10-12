/**
 * Backup & Restore panel (J2): create encrypted/plaintext portable archives,
 * inspect-before-restore with dry-run conflicts, and restore with mode/policy.
 */
import { useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { TextField } from "../../components/Field";
import { useCommandMutation } from "../../ipc/useCommand";
import type {
  CreateBackupResponse,
  InspectArchiveResponse,
  RestoreArchiveResponse,
  RestoreMode,
  ConflictPolicy,
} from "../../ipc/contract";
import { CommandErrorNotice } from "../recording/errorNotice";

export function BackupRestoreSection() {
  const [backupDialogOpen, setBackupDialogOpen] = useState(false);
  const [restoreDialogOpen, setRestoreDialogOpen] = useState(false);
  const [password, setPassword] = useState("");
  const [sensitiveProfile, setSensitiveProfile] = useState(false);
  const [backupResult, setBackupResult] = useState<CreateBackupResponse | null>(null);
  
  const [archivePath, setArchivePath] = useState("");
  const [restorePassword, setRestorePassword] = useState("");
  const [inspection, setInspection] = useState<InspectArchiveResponse | null>(null);
  const [restoreMode, setRestoreMode] = useState<RestoreMode>("merge");
  const [conflictPolicy, setConflictPolicy] = useState<ConflictPolicy>("skip");
  const [restoreTombstoned, setRestoreTombstoned] = useState(false);
  const [restoreResult, setRestoreResult] = useState<RestoreArchiveResponse | null>(null);

  const createBackup = useCommandMutation("create_portable_backup");
  const inspectArchive = useCommandMutation("inspect_restore_archive");
  const restoreArchive = useCommandMutation("restore_portable_archive");

  const handleCreateBackup = async () => {
    const directory = await openDialog({
      title: "Choose backup destination",
      directory: true,
    });

    if (!directory) return;

    try {
      const response = await createBackup.run({
        destination_directory: directory,
        password: password.trim() || null,
        sensitive_profile: sensitiveProfile,
      });
      setBackupResult(response);
    } catch (err) {
      // Error shown by CommandErrorNotice
    }
  };

  const handleChooseArchive = async () => {
    const path = await openDialog({
      title: "Choose archive to restore",
      filters: [{ name: "Plaudon Archive", extensions: ["plaudon"] }],
    });

    if (!path) return;

    setArchivePath(path);
    setInspection(null);
    setRestoreResult(null);
  };

  const handleInspectArchive = async () => {
    if (!archivePath) return;

    try {
      const response = await inspectArchive.run({
        archive_path: archivePath,
        password: restorePassword.trim() || null,
      });
      setInspection(response);
    } catch (err) {
      // Error shown by CommandErrorNotice
    }
  };

  const handleRestore = async () => {
    if (!archivePath || !inspection) return;

    try {
      const response = await restoreArchive.run({
        archive_path: archivePath,
        password: restorePassword.trim() || null,
        mode: restoreMode,
        on_conflict: conflictPolicy,
        restore_tombstoned: restoreTombstoned,
      });
      setRestoreResult(response);
    } catch (err) {
      // Error shown by CommandErrorNotice
    }
  };

  return (
    <section className="settings-section">
      <h2>Backup & Restore</h2>
      <p>Create portable backups of your library or restore from an archive.</p>

      <div style={{ display: "flex", gap: "1rem", marginTop: "1rem" }}>
        <Button variant="default" onClick={() => setBackupDialogOpen(true)}>
          Create backup
        </Button>
        <Button variant="default" onClick={() => setRestoreDialogOpen(true)}>
          Restore from archive
        </Button>
      </div>

      <Dialog
        open={backupDialogOpen}
        title="Create backup"
        onClose={() => {
          setBackupDialogOpen(false);
          setBackupResult(null);
          setPassword("");
          setSensitiveProfile(false);
        }}
        footer={
          backupResult ? (
            <Button
              variant="primary"
              onClick={() => {
                setBackupDialogOpen(false);
                setBackupResult(null);
                setPassword("");
                setSensitiveProfile(false);
              }}
            >
              Done
            </Button>
          ) : (
            <>
              <Button variant="quiet" onClick={() => setBackupDialogOpen(false)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                onClick={handleCreateBackup}
                disabled={createBackup.pending}
              >
                {createBackup.pending ? "Creating…" : "Create backup now"}
              </Button>
            </>
          )
        }
      >
        {backupResult ? (
          <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
            <p style={{ color: "var(--color-success)" }}>
              ✓ Backup created successfully
            </p>

            <dl>
              <dt>File</dt>
              <dd>{backupResult.file_name}</dd>
              <dt>Entries</dt>
              <dd>{backupResult.entry_count}</dd>
              <dt>Content size</dt>
              <dd>{(backupResult.total_byte_length / 1024 / 1024).toFixed(2)} MB</dd>
              <dt>File size</dt>
              <dd>{(backupResult.file_byte_length / 1024 / 1024).toFixed(2)} MB</dd>
              <dt>Protection</dt>
              <dd>{backupResult.protection_disclosure}</dd>
              <dt>Integrity digest</dt>
              <dd style={{ fontFamily: "monospace", fontSize: "0.9em", wordBreak: "break-all" }}>
                {backupResult.integrity_digest}
              </dd>
              <dt>Note</dt>
              <dd>{backupResult.integrity_note}</dd>
            </dl>

            {backupResult.nonportable_settings.length > 0 && (
              <section>
                <h3>Settings not included</h3>
                <ul>
                  {backupResult.nonportable_settings.map((setting, i) => (
                    <li key={i}>{setting}</li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
            <p>Create a portable backup of your entire library.</p>

            <TextField
              label="Password (optional)"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder="Leave empty for unencrypted backup"
            />

            {!password.trim() && (
              <p style={{ 
                color: "var(--color-warning)", 
                padding: "0.75rem",
                backgroundColor: "var(--color-warning-bg, rgba(255, 180, 0, 0.1))",
                borderRadius: "4px"
              }}>
                ⚠ <strong>This backup will NOT be encrypted.</strong> Anyone with the file can read
                your recordings.
              </p>
            )}

            {password.trim() && (
              <>
                <p style={{ 
                  padding: "0.75rem",
                  backgroundColor: "var(--color-success-bg, rgba(0, 180, 0, 0.1))",
                  borderRadius: "4px"
                }}>
                  This backup will be encrypted with your password.
                </p>

                <label style={{ display: "flex", gap: "0.5rem", alignItems: "flex-start" }}>
                  <input
                    type="checkbox"
                    checked={sensitiveProfile}
                    onChange={(e) => setSensitiveProfile(e.target.checked)}
                  />
                  <span>
                    Use sensitive profile (slower but more secure; recommended for highly
                    sensitive data)
                  </span>
                </label>

                <p style={{ 
                  padding: "0.75rem",
                  backgroundColor: "var(--color-danger-bg, rgba(255, 0, 0, 0.1))",
                  borderRadius: "4px",
                  color: "var(--color-danger)"
                }}>
                  <strong>We cannot recover a lost password.</strong> Store it securely.
                </p>
              </>
            )}

            {createBackup.error ? <CommandErrorNotice error={createBackup.error} /> : null}
          </div>
        )}
      </Dialog>

      <Dialog
        open={restoreDialogOpen}
        title="Restore from archive"
        onClose={() => {
          setRestoreDialogOpen(false);
          setArchivePath("");
          setRestorePassword("");
          setInspection(null);
          setRestoreResult(null);
        }}
        footer={
          restoreResult ? (
            <Button
              variant="primary"
              onClick={() => {
                setRestoreDialogOpen(false);
                setArchivePath("");
                setRestorePassword("");
                setInspection(null);
                setRestoreResult(null);
              }}
            >
              Done
            </Button>
          ) : inspection ? (
            <>
              <Button
                variant="quiet"
                onClick={() => {
                  setInspection(null);
                }}
              >
                Back
              </Button>
              <Button
                variant="primary"
                onClick={handleRestore}
                disabled={restoreArchive.pending}
              >
                {restoreArchive.pending ? "Restoring..." : "Restore"}
              </Button>
            </>
          ) : (
            <>
              <Button variant="quiet" onClick={() => setRestoreDialogOpen(false)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                onClick={handleInspectArchive}
                disabled={!archivePath || inspectArchive.pending}
              >
                {inspectArchive.pending ? "Inspecting..." : "Inspect archive"}
              </Button>
            </>
          )
        }
      >
        {restoreResult ? (
          <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
            <p style={{ color: "var(--color-success)" }}>
              ✓ Archive restored successfully
            </p>

            <dl>
              <dt>Recordings restored</dt>
              <dd>{restoreResult.recordings_restored}</dd>
              <dt>Transcripts restored</dt>
              <dd>{restoreResult.transcripts_restored}</dd>
              <dt>Outputs restored</dt>
              <dd>{restoreResult.outputs_restored}</dd>
              <dt>Sources restored</dt>
              <dd>{restoreResult.sources_restored}</dd>
              <dt>Media committed</dt>
              <dd>{restoreResult.media_committed}</dd>
              <dt>Media reused</dt>
              <dd>{restoreResult.media_reused}</dd>
            </dl>

            {restoreResult.conflicts.length > 0 && (
              <section>
                <h3>Conflicts</h3>
                <ul>
                  {restoreResult.conflicts.map((conflict, i) => (
                    <li key={i}>
                      {conflict.entry_path}: {conflict.reason}
                    </li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        ) : inspection ? (
          <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
            <section>
              <h3>Archive information</h3>
              <dl>
                <dt>Created</dt>
                <dd>{new Date(inspection.created_at).toLocaleString()}</dd>
                <dt>Source OS</dt>
                <dd>{inspection.source_os}</dd>
                <dt>Encrypted</dt>
                <dd>{inspection.encrypted ? "Yes" : "No"}</dd>
                <dt>Format version</dt>
                <dd>{inspection.format_version}</dd>
                <dt>Schema version</dt>
                <dd>{inspection.manifest_schema_version}</dd>
                <dt>Content size</dt>
                <dd>{(inspection.total_byte_length / 1024 / 1024).toFixed(2)} MB</dd>
                <dt>Integrity verified</dt>
                <dd>{inspection.payloads_verified ? "✓ Yes" : "✕ No"}</dd>
              </dl>
            </section>

            <section>
              <h3>Contents</h3>
              <ul>
                {inspection.entries_by_namespace.map((ns, i) => (
                  <li key={i}>
                    {ns.label}: {ns.count}
                  </li>
                ))}
              </ul>
            </section>

            {inspection.nonportable_settings.length > 0 && (
              <section>
                <h3>Settings not in archive</h3>
                <ul>
                  {inspection.nonportable_settings.map((setting, i) => (
                    <li key={i}>{setting}</li>
                  ))}
                </ul>
              </section>
            )}

            {inspection.conflicts.length > 0 && (
              <section>
                <h3>Conflicts</h3>
                <p style={{ color: "var(--color-warning)" }}>
                  The following records already exist in this library:
                </p>
                <ul>
                  {inspection.conflicts.map((conflict, i) => (
                    <li key={i}>
                      {conflict.entry_path}: {conflict.reason}
                    </li>
                  ))}
                </ul>
              </section>
            )}

            <section>
              <h3>Restore options</h3>
              
              <label style={{ display: "block", marginTop: "0.5rem" }}>
                <span style={{ display: "block", marginBottom: "0.25rem", fontWeight: "bold" }}>
                  Mode
                </span>
                <select
                  value={restoreMode}
                  onChange={(e) => setRestoreMode(e.target.value as RestoreMode)}
                  style={{ width: "100%" }}
                  disabled={!inspection.target_is_empty && restoreMode === "new_library"}
                >
                  <option value="new_library" disabled={!inspection.target_is_empty}>
                    New library (requires empty target)
                    {!inspection.target_is_empty ? " — not available" : ""}
                  </option>
                  <option value="merge">Merge into existing library</option>
                </select>
              </label>

              <label style={{ display: "block", marginTop: "0.5rem" }}>
                <span style={{ display: "block", marginBottom: "0.25rem", fontWeight: "bold" }}>
                  On conflict
                </span>
                <select
                  value={conflictPolicy}
                  onChange={(e) => setConflictPolicy(e.target.value as ConflictPolicy)}
                  style={{ width: "100%" }}
                >
                  <option value="skip">Skip conflicting records</option>
                  <option value="fail">Abort on any conflict</option>
                </select>
              </label>

              <label style={{ display: "flex", gap: "0.5rem", alignItems: "flex-start", marginTop: "0.5rem" }}>
                <input
                  type="checkbox"
                  checked={restoreTombstoned}
                  onChange={(e) => setRestoreTombstoned(e.target.checked)}
                />
                <span>
                  Restore previously deleted recordings (overrides deletion)
                </span>
              </label>
            </section>

            {restoreArchive.error ? <CommandErrorNotice error={restoreArchive.error} /> : null}
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
            <div>
              <Button variant="default" onClick={handleChooseArchive}>
                {archivePath ? "Choose different archive" : "Choose archive"}
              </Button>
              {archivePath && (
                <p style={{ marginTop: "0.5rem", fontSize: "0.9em" }}>
                  Selected: {archivePath}
                </p>
              )}
            </div>

            {archivePath && (
              <TextField
                label="Password (if encrypted)"
                type="password"
                value={restorePassword}
                onChange={(e) => setRestorePassword(e.target.value)}
                placeholder="Leave empty if not encrypted"
              />
            )}

            {inspectArchive.error ? <CommandErrorNotice error={inspectArchive.error} /> : null}
          </div>
        )}
      </Dialog>
    </section>
  );
}
