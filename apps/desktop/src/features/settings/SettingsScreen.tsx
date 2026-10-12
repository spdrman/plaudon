import "./settings.css";
import type { ReactNode } from "react";
import { useAppState } from "../../app/AppState";
import { Button } from "../../components/Button";
import { AppearanceSection } from "./AppearanceSection";
import { PrivacySection } from "./PrivacySection";
import { AiKeySection } from "./AiKeySection";
import { NotificationSettingsSection } from "./NotificationSettingsSection";
import { formatBytes } from "./bytes";

/** Settings destination (§5.1, §5.2): privacy, AI providers, notifications, storage. */
export function SettingsScreen() {
  const { capabilities, navigate } = useAppState();
  const storage = capabilities?.storage ?? null;

  return (
    <div className="settings">
      <h2 className="settings__title">Settings</h2>

      <SettingsSection title="Appearance">
        <AppearanceSection />
      </SettingsSection>

      <SettingsSection title="Privacy & network">
        <PrivacySection />
      </SettingsSection>

      <SettingsSection title="AI key">
        <AiKeySection />
      </SettingsSection>

      <SettingsSection title="Notifications">
        <NotificationSettingsSection />
      </SettingsSection>

      <SettingsSection title="Storage">
        {storage ? (
          <dl className="settings-facts">
            <dt>Library location</dt>
            <dd>{storage.library_path}</dd>
            <dt>Free space</dt>
            <dd>{formatBytes(storage.free_bytes)}</dd>
            <dt>Total space</dt>
            <dd>{formatBytes(storage.total_bytes)}</dd>
          </dl>
        ) : (
          <p className="settings-section__note">Storage facts are not available yet.</p>
        )}
      </SettingsSection>

      <SettingsSection title="PLAUD and sources">
        <p className="settings-section__note">
          Connect the official PLAUD command line tool to list and import your cloud recordings.
          Importing files by hand always works.
        </p>
        <Button onClick={() => navigate({ name: "settings.sources" })}>
          Open PLAUD and sources
        </Button>
      </SettingsSection>

      <SettingsSection title="Activity history">
        <p className="settings-section__note">
          Every import, transcription and attention notice is kept in the app, whether or not the
          operating system showed a notification.
        </p>
        <Button onClick={() => navigate({ name: "settings.activity" })}>
          Open activity history
        </Button>
      </SettingsSection>
    </div>
  );
}

function SettingsSection({ title, children }: { title: string; children: ReactNode }) {
  const id = `settings-${title.toLowerCase().replace(/[^a-z]+/g, "-")}`;
  return (
    <section className="settings-section" aria-labelledby={id}>
      <h2 className="settings-section__title" id={id}>
        {title}
      </h2>
      {children}
    </section>
  );
}
