import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { forwardRef } from "react";
export const Button = forwardRef(function Button({ variant = "default", glyph, className, children, type = "button", ...rest }, ref) {
    const classes = ["btn", variant === "default" ? null : `btn--${variant}`, className]
        .filter(Boolean)
        .join(" ");
    return (_jsxs("button", { ref: ref, type: type, className: classes, ...rest, children: [glyph ? (_jsx("span", { "aria-hidden": "true", className: "status-badge__glyph", children: glyph })) : null, children] }));
});
