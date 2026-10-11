/**
 * Persistent Import control (§5.2). Always visible, even when a source
 * connector is unavailable, because manual import is the fallback path (§5.4).
 */
import { Button } from "../../components/Button";
import { useImport } from "./useImport";
import "./import.css";

export function ImportButton() {
  const { pickFiles, pending } = useImport();
  return (
    <Button
      variant="primary"
      glyph="⤒"
      aria-busy={pending}
      onClick={() => void pickFiles()}
      className="import-button"
    >
      Import audio
    </Button>
  );
}
