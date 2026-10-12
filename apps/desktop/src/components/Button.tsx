import { forwardRef, type ButtonHTMLAttributes } from "react";

export type ButtonVariant = "default" | "primary" | "danger" | "quiet";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  /** Short text glyph rendered next to the label; never the only status signal. */
  glyph?: string;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "default", glyph, className, children, type = "button", ...rest },
  ref,
) {
  const classes = ["btn", variant === "default" ? null : `btn--${variant}`, className]
    .filter(Boolean)
    .join(" ");
  return (
    <button ref={ref} type={type} className={classes} {...rest}>
      {glyph ? (
        <span aria-hidden="true" className="status-badge__glyph">
          {glyph}
        </span>
      ) : null}
      {children}
    </button>
  );
});
