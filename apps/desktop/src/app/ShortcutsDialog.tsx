import { Dialog } from "../components/Dialog";
import { Button } from "../components/Button";
import { SHORTCUTS, formatShortcut, isMacPlatform } from "./shortcuts";

export interface ShortcutsDialogProps {
  open: boolean;
  onClose: () => void;
}

export function ShortcutsDialog({ open, onClose }: ShortcutsDialogProps) {
  const mac = isMacPlatform(typeof navigator === "undefined" ? undefined : navigator);
  const groups = [...new Set(SHORTCUTS.map((s) => s.group))];
  return (
    <Dialog
      open={open}
      title="Keyboard shortcuts"
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          Close
        </Button>
      }
    >
      {groups.map((group) => (
        <section key={group} aria-label={group}>
          <h3>{group}</h3>
          <dl className="shortcut-list">
            {SHORTCUTS.filter((s) => s.group === group).map((shortcut) => (
              <div className="shortcut-list__row" key={`${group}-${shortcut.keys}-${shortcut.description}`}>
                <dt>
                  <kbd>{formatShortcut(shortcut.keys, mac)}</kbd>
                </dt>
                <dd>{shortcut.description}</dd>
              </div>
            ))}
          </dl>
        </section>
      ))}
    </Dialog>
  );
}
