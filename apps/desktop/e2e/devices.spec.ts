import { auditPage, expect, gotoApp, test } from "./fixtures";

/**
 * L2.1: the Devices workspace over the in-memory device lab. Scanning is the
 * user's action, the five App. B.8 sections render with register-derived
 * labels, the wizard offers exactly three routes, and nothing leaves the
 * machine.
 */
test.describe("devices", () => {
  test("wizard and list: scan, five sections, honest labels", async ({ page, egress }) => {
    await gotoApp(page, { recordings: 1 });
    await page.evaluate(() => {
      const lab = window.__plaudonHarness!.devices;
      lab.addDevice({ peripheral_id: "dev-nearby", advertised_name: "Nearby Note" });
      lab.addDevice({
        peripheral_id: "dev-unsupported",
        advertised_name: "Newer Note",
        probed: true,
        section: "unsupported_firmware",
        support_status: "known_unsupported",
        firmware: "encrypted-mode",
      });
      lab.addDevice({ peripheral_id: "dev-linked", advertised_name: "Old Note", nearby: false, authorized: true });
    });

    await page.getByRole("tab", { name: /Devices/ }).click();
    await expect(page.getByRole("heading", { name: "No scan yet" })).toBeVisible();

    // Wizard: exactly three options, each with its own branch.
    await page.getByRole("button", { name: "Add PLAUD device" }).click();
    await expect(page.getByRole("radio")).toHaveCount(3);
    await page.getByRole("radio", { name: /Use PLAUD account/ }).check();
    await expect(page.getByRole("group", { name: "Use PLAUD account" })).toContainText("PLAUD Cloud, not a direct connection");
    await page.getByRole("radio", { name: /Import files/ }).check();
    await expect(page.getByRole("group", { name: "Import files" })).toContainText("without hardware, an account or a network connection");
    await page.getByRole("radio", { name: /Connect directly/ }).check();
    await page.getByRole("group", { name: "Connect directly" }).getByRole("button", { name: "Scan for nearby devices" }).click();

    for (const name of ["Nearby", "Connected", "Needs authorization", "Unsupported firmware", "Previously linked"]) {
      await expect(page.getByRole("region", { name: new RegExp(`^${name}`) })).toBeVisible();
    }
    await expect(page.getByRole("region", { name: /^Nearby/ })).toContainText("Unverified identity");
    await expect(page.getByRole("region", { name: /^Unsupported firmware/ })).toContainText("Known unsupported");
    await expect(page.getByRole("region", { name: /^Previously linked/ })).toContainText("Old Note");

    // Move the pointer away: the shared hover style on primary buttons is not under audit here.
    await page.mouse.move(0, 0);
    await auditPage(page, "devices list");

    // Detail: unknown stays unknown until a probe says otherwise; keyboard reachable.
    await page.getByRole("button", { name: "Open details of Nearby Note" }).focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("heading", { name: "Nearby Note", level: 3 })).toBeVisible();
    await expect(page.getByText("Cloud: not used")).toBeVisible();
    await expect(page.getByText("Device BLE · compressed ASR")).toBeVisible();
    await page.mouse.move(0, 0);
    await auditPage(page, "device detail");

    expect(egress.external).toEqual([]);
  });

  test("a denied radio is reported as denied with a way forward", async ({ page }) => {
    await gotoApp(page);
    await page.evaluate(() => window.__plaudonHarness!.devices.setRadio("permission_denied"));
    await page.getByRole("tab", { name: /Devices/ }).click();
    await page.getByRole("button", { name: "Scan for nearby devices" }).first().click();
    await expect(page.getByRole("alert")).toContainText("Allow Bluetooth for Plaudon in your system settings");
    await expect(page.getByText("No PLAUD device found")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Import audio files instead" })).toBeVisible();
  });
});
