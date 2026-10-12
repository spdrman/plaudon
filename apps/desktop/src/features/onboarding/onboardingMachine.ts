/**
 * Pure step machine for first-run onboarding (§4.1, §5.2).
 *
 * The reducer owns ordering, skippability and the choices later sent to
 * `complete_onboarding`. It performs no IO: every command is issued by the
 * React layer in direct response to a user action.
 */

export const ONBOARDING_STEPS = [
  "privacy",
  "mode",
  "engines",
  "key",
  "sources",
  "finish",
] as const;

export type OnboardingStepId = (typeof ONBOARDING_STEPS)[number];

export const STEP_TITLES: Record<OnboardingStepId, string> = {
  privacy: "Your recordings stay on this computer",
  mode: "Local only, or allow cloud AI later",
  engines: "Transcription engines on this machine",
  key: "Optional: add an OpenRouter key",
  sources: "Where your audio comes from",
  finish: "Finish setup",
};

/** Steps the user may leave without making any choice (§5.2). */
export const SKIPPABLE_STEPS: Record<OnboardingStepId, boolean> = {
  privacy: false,
  mode: false,
  engines: false,
  key: true,
  sources: true,
  finish: false,
};

export const LAST_STEP_INDEX = ONBOARDING_STEPS.length - 1;

export interface OnboardingState {
  stepIndex: number;
  /** Local-only mode is the default posture (§16.2A). */
  localOnly: boolean;
  /** True once the user explicitly answered the local/cloud question. */
  modeChosen: boolean;
  defaultSttEngineId: string | null;
  keySaved: boolean;
  skipped: OnboardingStepId[];
}

export type OnboardingAction =
  | { type: "next" }
  | { type: "back" }
  | { type: "skip" }
  | { type: "choose-mode"; localOnly: boolean }
  | { type: "choose-engine"; engineId: string | null }
  | { type: "key-saved" };

export const initialOnboardingState: OnboardingState = {
  stepIndex: 0,
  localOnly: true,
  modeChosen: false,
  defaultSttEngineId: null,
  keySaved: false,
  skipped: [],
};

export function onboardingReducer(
  state: OnboardingState,
  action: OnboardingAction,
): OnboardingState {
  switch (action.type) {
    case "next":
      return state.stepIndex >= LAST_STEP_INDEX
        ? state
        : { ...state, stepIndex: state.stepIndex + 1 };
    case "back":
      return state.stepIndex === 0 ? state : { ...state, stepIndex: state.stepIndex - 1 };
    case "skip": {
      const step = currentStep(state);
      if (!SKIPPABLE_STEPS[step]) return state;
      return {
        ...state,
        stepIndex: Math.min(state.stepIndex + 1, LAST_STEP_INDEX),
        skipped: state.skipped.includes(step) ? state.skipped : [...state.skipped, step],
      };
    }
    case "choose-mode":
      return { ...state, localOnly: action.localOnly, modeChosen: true };
    case "choose-engine":
      return { ...state, defaultSttEngineId: action.engineId };
    case "key-saved":
      return { ...state, keySaved: true };
    default:
      return state;
  }
}

/* ------------------------------------------------------------- selectors */

export function currentStep(state: OnboardingState): OnboardingStepId {
  return ONBOARDING_STEPS[state.stepIndex] ?? "privacy";
}

/** Text progress indicator; status is never a bar or color alone (§5.3). */
export function progressLabel(state: OnboardingState): string {
  return `Step ${state.stepIndex + 1} of ${ONBOARDING_STEPS.length}`;
}
