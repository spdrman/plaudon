import { useCallback, useEffect, useId, useRef, type ReactNode } from "react";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export interface DialogProps {
  open: boolean;
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  footer?: ReactNode;
  onClose: () => void;
  /** Destructive dialogs must spell out the consequence (§5.3). */
  destructive?: boolean;
}

/**
 * Modal dialog with focus trap and focus restoration (WCAG 2.4.3 / 2.1.2).
 */
export function Dialog({
  open,
  title,
  description,
  children,
  footer,
  onClose,
  destructive = false,
}: DialogProps) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const restoreRef = useRef<HTMLElement | null>(null);
  const titleId = useId();
  const descId = useId();

  useEffect(() => {
    if (!open) return;
    restoreRef.current = document.activeElement as HTMLElement | null;
    const node = dialogRef.current;
    const first = node?.querySelector<HTMLElement>(FOCUSABLE) ?? node;
    first?.focus();
    return () => {
      restoreRef.current?.focus?.();
    };
  }, [open]);

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;
      const node = dialogRef.current;
      if (!node) return;
      const items = Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
        (el) => el.offsetParent !== null || el === document.activeElement,
      );
      if (items.length === 0) return;
      const first = items[0]!;
      const last = items[items.length - 1]!;
      const active = document.activeElement;
      if (event.shiftKey && (active === first || !node.contains(active))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    },
    [onClose],
  );

  if (!open) return null;

  return (
    <div className="dialog__backdrop" data-testid="dialog-backdrop">
      <div
        className="dialog"
        role={destructive ? "alertdialog" : "dialog"}
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descId : undefined}
        ref={dialogRef}
        onKeyDown={onKeyDown}
        tabIndex={-1}
      >
        <h2 className="dialog__title" id={titleId}>
          {title}
        </h2>
        {description ? <div id={descId}>{description}</div> : null}
        {children}
        {footer ? <div className="dialog__actions">{footer}</div> : null}
      </div>
    </div>
  );
}
