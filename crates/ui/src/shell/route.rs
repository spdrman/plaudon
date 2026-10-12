//! Destinations and routes (parity rows S1 and S2).
//!
//! The React shell addressed screens with hash routes; there is no URL in a
//! native window, so the same twelve variants become an enum in UI state. The
//! string forms are kept, because they are how a deep link arrives: a
//! notification activation names a recording or a job, and the shell has to
//! turn that into a destination.
//!
//! Ported from the React shell's `app/router.ts`, including the ordering
//! comment: Devices is last so `Mod+1`…`Mod+5` keep the meaning they had
//! before Devices existed.

/// A top-level destination in the navigation rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Destination {
    /// The recordings library.
    Library,
    /// Recently imported and unprocessed recordings.
    Inbox,
    /// Background work.
    Jobs,
    /// Output templates.
    Templates,
    /// Settings, activity history and sources.
    Settings,
    /// PLAUD devices.
    Devices,
}

impl Destination {
    /// Every destination, in navigation order. `Mod+1`…`Mod+6` index into
    /// this array, so the order is part of the contract.
    pub const ALL: [Self; 6] = [
        Self::Library,
        Self::Inbox,
        Self::Jobs,
        Self::Templates,
        Self::Settings,
        Self::Devices,
    ];

    /// The stable identifier, matching `router.ts`'s `DESTINATIONS`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Inbox => "inbox",
            Self::Jobs => "jobs",
            Self::Templates => "templates",
            Self::Settings => "settings",
            Self::Devices => "devices",
        }
    }

    /// The label shown in the navigation rail.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Inbox => "Inbox",
            Self::Jobs => "Jobs",
            Self::Templates => "Templates",
            Self::Settings => "Settings",
            Self::Devices => "Devices",
        }
    }

    /// The single-letter glyph the icon rail shows when it is collapsed.
    ///
    /// The React shell used the same letters; it is decoration, never the
    /// accessible name, which stays [`Destination::label`].
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Library => "L",
            Self::Inbox => "I",
            Self::Jobs => "J",
            Self::Templates => "T",
            Self::Settings => "S",
            Self::Devices => "D",
        }
    }

    /// The task that owns this destination's screen, named in the empty state
    /// the registry shows until that task's work lands.
    #[must_use]
    pub const fn owning_task(self) -> &'static str {
        match self {
            Self::Library | Self::Inbox => "O1.4 (#202)",
            Self::Jobs | Self::Templates => "O2.2 (#206)",
            Self::Devices => "O2.3 (#207)",
            Self::Settings => "O2.4 (#208)",
        }
    }

    /// The route a plain click on this destination opens.
    #[must_use]
    pub const fn route(self) -> Route {
        match self {
            Self::Library => Route::Library,
            Self::Inbox => Route::Inbox,
            Self::Jobs => Route::Jobs,
            Self::Templates => Route::Templates,
            Self::Settings => Route::Settings,
            Self::Devices => Route::Devices,
        }
    }

    /// The destination `Mod+n` selects, `n` being one-based as the shortcut
    /// is written.
    #[must_use]
    pub fn from_index(one_based: usize) -> Option<Self> {
        one_based
            .checked_sub(1)
            .and_then(|index| Self::ALL.get(index).copied())
    }
}

/// Where the shell is, including the sub-routes that are not destinations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Route {
    /// The library table. The route the shell opens on.
    #[default]
    Library,
    /// One recording, inside Library.
    LibraryRecording {
        /// The recording being shown.
        recording_id: String,
    },
    /// Cross-recording search, inside Library.
    LibrarySearch,
    /// The inbox.
    Inbox,
    /// The job list.
    Jobs,
    /// One job, inside Jobs.
    JobsJob {
        /// The job being shown.
        job_id: String,
    },
    /// Output templates.
    Templates,
    /// Settings.
    Settings,
    /// Activity history, inside Settings.
    SettingsActivity,
    /// PLAUD connections and watched folders, inside Settings.
    SettingsSources,
    /// The device list.
    Devices,
    /// One device, inside Devices.
    DevicesDevice {
        /// The peripheral being shown.
        peripheral_id: String,
    },
}

impl Route {
    /// The destination that owns this route, for navigation highlighting.
    #[must_use]
    pub const fn destination(&self) -> Destination {
        match self {
            Self::Library | Self::LibraryRecording { .. } | Self::LibrarySearch => {
                Destination::Library
            }
            Self::Inbox => Destination::Inbox,
            Self::Jobs | Self::JobsJob { .. } => Destination::Jobs,
            Self::Templates => Destination::Templates,
            Self::Settings | Self::SettingsActivity | Self::SettingsSources => {
                Destination::Settings
            }
            Self::Devices | Self::DevicesDevice { .. } => Destination::Devices,
        }
    }

    /// The title shown in the shell header, as `routeTitle` produced it.
    #[must_use]
    pub fn title(&self) -> &'static str {
        match self {
            Self::LibraryRecording { .. } => "Recording",
            Self::LibrarySearch => "Search",
            Self::SettingsActivity => "Activity history",
            Self::SettingsSources => "PLAUD and sources",
            Self::DevicesDevice { .. } => "Device",
            other => other.destination().label(),
        }
    }

    /// The path form, kept from `formatRoute` so a deep link is still a
    /// string that can be logged, compared and tested.
    #[must_use]
    pub fn path(&self) -> String {
        match self {
            Self::Library => "/library".to_owned(),
            Self::LibraryRecording { recording_id } => format!("/library/{recording_id}"),
            Self::LibrarySearch => "/library/search".to_owned(),
            Self::Inbox => "/inbox".to_owned(),
            Self::Jobs => "/jobs".to_owned(),
            Self::JobsJob { job_id } => format!("/jobs/{job_id}"),
            Self::Templates => "/templates".to_owned(),
            Self::Settings => "/settings".to_owned(),
            Self::SettingsActivity => "/settings/activity".to_owned(),
            Self::SettingsSources => "/settings/sources".to_owned(),
            Self::Devices => "/devices".to_owned(),
            Self::DevicesDevice { peripheral_id } => format!("/devices/{peripheral_id}"),
        }
    }

    /// The route a path names; an unknown path is the default route, exactly
    /// as `parseRoute` behaved.
    #[must_use]
    pub fn parse(path: &str) -> Self {
        let mut parts = path
            .trim_start_matches('#')
            .split('?')
            .next()
            .unwrap_or_default()
            .split('/')
            .filter(|part| !part.is_empty());
        let head = parts.next();
        let tail = parts.next();
        match (head, tail) {
            (None | Some("library"), None) => Self::Library,
            (Some("library"), Some("search")) => Self::LibrarySearch,
            (Some("library"), Some(id)) => Self::LibraryRecording {
                recording_id: id.to_owned(),
            },
            (Some("inbox"), _) => Self::Inbox,
            (Some("jobs"), None) => Self::Jobs,
            (Some("jobs"), Some(id)) => Self::JobsJob {
                job_id: id.to_owned(),
            },
            (Some("templates"), _) => Self::Templates,
            (Some("settings"), None) => Self::Settings,
            (Some("settings"), Some("activity")) => Self::SettingsActivity,
            (Some("settings"), Some("sources")) => Self::SettingsSources,
            (Some("settings"), Some(_)) => Self::Settings,
            (Some("devices"), None) => Self::Devices,
            (Some("devices"), Some(id)) => Self::DevicesDevice {
                peripheral_id: id.to_owned(),
            },
            _ => Self::Library,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_destinations_keep_the_order_the_shortcuts_depend_on() {
        let ids: Vec<&str> = Destination::ALL.iter().map(|d| d.id()).collect();
        assert_eq!(
            ids,
            [
                "library",
                "inbox",
                "jobs",
                "templates",
                "settings",
                "devices"
            ]
        );
        assert_eq!(Destination::from_index(1), Some(Destination::Library));
        assert_eq!(Destination::from_index(6), Some(Destination::Devices));
        assert_eq!(Destination::from_index(7), None);
        assert_eq!(Destination::from_index(0), None);
    }

    #[test]
    fn every_route_belongs_to_exactly_one_destination() {
        let routes = [
            Route::Library,
            Route::LibraryRecording {
                recording_id: "r1".to_owned(),
            },
            Route::LibrarySearch,
            Route::Inbox,
            Route::Jobs,
            Route::JobsJob {
                job_id: "j1".to_owned(),
            },
            Route::Templates,
            Route::Settings,
            Route::SettingsActivity,
            Route::SettingsSources,
            Route::Devices,
            Route::DevicesDevice {
                peripheral_id: "p1".to_owned(),
            },
        ];
        assert_eq!(routes.len(), 12, "router.ts publishes twelve variants");
        for route in &routes {
            assert!(Destination::ALL.contains(&route.destination()));
        }
    }

    #[test]
    fn a_route_round_trips_through_its_path() {
        for route in [
            Route::Library,
            Route::LibrarySearch,
            Route::LibraryRecording {
                recording_id: "rec-7".to_owned(),
            },
            Route::Inbox,
            Route::Jobs,
            Route::JobsJob {
                job_id: "job-9".to_owned(),
            },
            Route::Templates,
            Route::Settings,
            Route::SettingsActivity,
            Route::SettingsSources,
            Route::Devices,
            Route::DevicesDevice {
                peripheral_id: "dev-2".to_owned(),
            },
        ] {
            assert_eq!(Route::parse(&route.path()), route);
        }
    }

    #[test]
    fn an_unknown_path_lands_on_the_default_route_rather_than_nowhere() {
        assert_eq!(Route::parse(""), Route::Library);
        assert_eq!(Route::parse("/nope"), Route::Library);
        assert_eq!(Route::parse("#/library"), Route::Library);
        assert_eq!(Route::parse("/settings/unknown"), Route::Settings);
    }

    #[test]
    fn sub_routes_carry_their_own_titles() {
        assert_eq!(
            Route::LibraryRecording {
                recording_id: "r".to_owned()
            }
            .title(),
            "Recording"
        );
        assert_eq!(Route::SettingsActivity.title(), "Activity history");
        assert_eq!(Route::SettingsSources.title(), "PLAUD and sources");
        assert_eq!(Route::Library.title(), "Library");
    }
}
