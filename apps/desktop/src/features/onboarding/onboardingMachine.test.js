import { describe, expect, it } from "vitest";
import { currentStep, initialOnboardingState, LAST_STEP_INDEX, onboardingReducer, ONBOARDING_STEPS, progressLabel, SKIPPABLE_STEPS, } from "./onboardingMachine";
function at(step) {
    return { ...initialOnboardingState, stepIndex: ONBOARDING_STEPS.indexOf(step) };
}
describe("onboardingMachine", () => {
    it("starts on the privacy step in local-only mode", () => {
        expect(currentStep(initialOnboardingState)).toBe("privacy");
        expect(initialOnboardingState.localOnly).toBe(true);
        expect(initialOnboardingState.modeChosen).toBe(false);
        expect(progressLabel(initialOnboardingState)).toBe("Step 1 of 6");
    });
    it("walks forward and back without leaving the range", () => {
        let state = onboardingReducer(initialOnboardingState, { type: "back" });
        expect(state.stepIndex).toBe(0);
        state = onboardingReducer(state, { type: "next" });
        expect(currentStep(state)).toBe("mode");
        expect(progressLabel(state)).toBe("Step 2 of 6");
        state = onboardingReducer(state, { type: "back" });
        expect(currentStep(state)).toBe("privacy");
        let last = at("finish");
        last = onboardingReducer(last, { type: "next" });
        expect(last.stepIndex).toBe(LAST_STEP_INDEX);
    });
    it("only skips the account-related steps and records them", () => {
        const key = onboardingReducer(at("key"), { type: "skip" });
        expect(currentStep(key)).toBe("sources");
        expect(key.skipped).toEqual(["key"]);
        const sources = onboardingReducer(key, { type: "skip" });
        expect(currentStep(sources)).toBe("finish");
        expect(sources.skipped).toEqual(["key", "sources"]);
        expect(onboardingReducer(at("privacy"), { type: "skip" })).toEqual(at("privacy"));
        expect(SKIPPABLE_STEPS).toEqual({
            privacy: false,
            mode: false,
            engines: false,
            key: true,
            sources: true,
            finish: false,
        });
    });
    it("records the mode, engine and key choices", () => {
        let state = onboardingReducer(initialOnboardingState, {
            type: "choose-mode",
            localOnly: false,
        });
        expect(state).toMatchObject({ localOnly: false, modeChosen: true });
        state = onboardingReducer(state, { type: "choose-engine", engineId: "whisper.cpp" });
        expect(state.defaultSttEngineId).toBe("whisper.cpp");
        state = onboardingReducer(state, { type: "key-saved" });
        expect(state.keySaved).toBe(true);
        // No step moved as a side effect of a choice.
        expect(state.stepIndex).toBe(0);
    });
});
