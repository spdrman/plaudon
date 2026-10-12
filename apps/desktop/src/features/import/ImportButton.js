import { jsx as _jsx } from "react/jsx-runtime";
/**
 * Persistent Import control (§5.2). Always visible, even when a source
 * connector is unavailable, because manual import is the fallback path (§5.4).
 */
import { Button } from "../../components/Button";
import { useImport } from "./useImport";
import "./import.css";
export function ImportButton() {
    const { pickFiles, pending } = useImport();
    return (_jsx(Button, { variant: "primary", glyph: "\u2912", "aria-busy": pending, onClick: () => void pickFiles(), className: "import-button", children: "Import audio" }));
}
