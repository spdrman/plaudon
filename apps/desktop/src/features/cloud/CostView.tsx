import "./cloud.css";
import type { CostValue } from "../../ipc/contract";
import { costPresentation } from "./cloudPresentation";

export interface CostViewProps {
  cost: CostValue;
  /** Adds the longer sentence under the label instead of only to AT. */
  showDetail?: boolean;
}

/**
 * Single rendering of a §10.4 cost. Estimates are italic, carry a `≈` glyph and
 * say "Estimated"; finalized amounts say "Final"; an unknown cost says "Cost
 * unknown" and never shows a zero amount.
 */
export function CostView({ cost, showDetail = false }: CostViewProps) {
  const presentation = costPresentation(cost);
  return (
    <span className={`cloud-cost cloud-cost--${cost.confidence}`} data-confidence={cost.confidence}>
      <span aria-hidden="true">{presentation.glyph}</span>
      <span>{presentation.label}</span>
      {showDetail ? (
        <span className="cloud-budget__note">{presentation.detail}</span>
      ) : (
        <span className="sr-only">{presentation.detail}</span>
      )}
    </span>
  );
}
