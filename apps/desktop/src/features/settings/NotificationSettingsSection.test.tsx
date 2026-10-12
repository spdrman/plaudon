import { describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { FakeBackend } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { NotificationSettingsSection } from "./NotificationSettingsSection";

const CATEGORY_LABELS = [
  "Import finished",
  "Transcription finished",
  "Requires attention",
  "Cloud budget exceeded",
];

describe("NotificationSettingsSection", () => {
  it("starts from the stored preferences and saves a category change", async () => {
    const backend = new FakeBackend();
    const { user } = renderWithApp(<NotificationSettingsSection />, { backend });

    const master = await screen.findByRole("switch", { name: "Show desktop notifications" });
    await waitFor(() => expect(master).toBeChecked());
    for (const label of CATEGORY_LABELS) {
      expect(screen.getByRole("checkbox", { name: label })).toBeChecked();
    }

    await user.click(screen.getByRole("checkbox", { name: "Import finished" }));
    await waitFor(() =>
      expect(backend.preferences.notifications.categories.import_finished).toBe(false),
    );
    expect(backend.preferences.notifications.enabled).toBe(true);
    await expectNoAxeViolations();
  });

  it("disables the categories when the master switch is off", async () => {
    const backend = new FakeBackend();
    const { user } = renderWithApp(<NotificationSettingsSection />, { backend });

    const master = await screen.findByRole("switch", { name: "Show desktop notifications" });
    await waitFor(() => expect(master).toBeChecked());
    await user.click(master);

    await waitFor(() => expect(backend.preferences.notifications.enabled).toBe(false));
    expect(screen.getByRole("checkbox", { name: "Requires attention" })).toBeDisabled();
  });

  it("explains a denied OS permission and that the activity history stays complete", async () => {
    const backend = new FakeBackend({
      osNotificationPermission: "denied",
      capabilities: { permissions: { notifications: "denied", bluetooth: "not_requested" } },
    });
    renderWithApp(<NotificationSettingsSection />, { backend });

    expect(
      await screen.findByText(/notifications are blocked for this app/),
    ).toBeInTheDocument();
    expect(screen.getByText(/activity history still records every event/)).toBeInTheDocument();
    expect(screen.getByText(/operating system's notification settings/)).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("says so plainly when the platform does not support notifications", async () => {
    const backend = new FakeBackend({
      osNotificationPermission: "unsupported",
      capabilities: { permissions: { notifications: "unsupported", bluetooth: "not_requested" } },
    });
    const { user } = renderWithApp(<NotificationSettingsSection />, { backend });

    const master = await screen.findByRole("switch", { name: "Show desktop notifications" });
    await waitFor(() => expect(master).toBeChecked());
    await user.click(master);

    expect(
      await screen.findByText(/does not support desktop notifications/),
    ).toBeInTheDocument();
  });
});
