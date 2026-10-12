import { useId, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes } from "react";

interface FieldShellProps {
  label: string;
  hint?: ReactNode;
  error?: string;
  children: (ids: { controlId: string; describedBy: string | undefined }) => ReactNode;
}

function FieldShell({ label, hint, error, children }: FieldShellProps) {
  const controlId = useId();
  const hintId = `${controlId}-hint`;
  const errorId = `${controlId}-error`;
  const describedBy = [hint ? hintId : null, error ? errorId : null].filter(Boolean).join(" ");
  return (
    <div className="field">
      <label className="field__label" htmlFor={controlId}>
        {label}
      </label>
      {children({ controlId, describedBy: describedBy || undefined })}
      {hint ? (
        <span className="field__hint" id={hintId}>
          {hint}
        </span>
      ) : null}
      {error ? (
        <span className="field__error" id={errorId}>
          <span aria-hidden="true">! </span>
          {error}
        </span>
      ) : null}
    </div>
  );
}

export interface TextFieldProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, "id" | "aria-describedby"> {
  label: string;
  hint?: ReactNode;
  error?: string;
}

export function TextField({ label, hint, error, ...rest }: TextFieldProps) {
  return (
    <FieldShell label={label} hint={hint} error={error}>
      {({ controlId, describedBy }) => (
        <input
          {...rest}
          id={controlId}
          className="field__control"
          aria-describedby={describedBy}
          aria-invalid={error ? true : undefined}
        />
      )}
    </FieldShell>
  );
}

export interface SelectFieldProps
  extends Omit<SelectHTMLAttributes<HTMLSelectElement>, "id" | "aria-describedby"> {
  label: string;
  hint?: ReactNode;
  error?: string;
  children: ReactNode;
}

export function SelectField({ label, hint, error, children, ...rest }: SelectFieldProps) {
  return (
    <FieldShell label={label} hint={hint} error={error}>
      {({ controlId, describedBy }) => (
        <select {...rest} id={controlId} className="field__control" aria-describedby={describedBy}>
          {children}
        </select>
      )}
    </FieldShell>
  );
}
