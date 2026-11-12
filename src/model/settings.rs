// Preferences, and where they live.
//
// Same shape as `store`: hand-editable TOML next to the connections file,
// written atomically. Anything missing falls back to the default, so an older
// file keeps working and a newer one does not have to be migrated.

use serde::{Deserialize, Serialize};

use crate::model::order::Sort;

const FILE_NAME: &str = "settings.toml";

/// Which clock timestamps are shown against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Clock {
    /// The machine's own time zone, including daylight saving.
    #[default]
    Local,
    /// Useful when comparing against server logs.
    Utc,
}

impl Clock {
    pub const ALL: [Self; 2] = [Self::Local, Self::Utc];

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local time",
            Self::Utc => "UTC",
        }
    }

    /// What the column heading says, so the reading is never ambiguous.
    pub fn column(self) -> &'static str {
        match self {
            Self::Local => "Modified",
            Self::Utc => "Modified (UTC)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Follow the system",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// Remembered so the window comes back the size it was left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub width: f32,
    pub height: f32,
    pub sidebar: f32,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            width: 1180.0,
            height: 720.0,
            sidebar: 220.0,
        }
    }
}

impl Window {
    /// Guards against a saved size that would open off-screen or too small to
    /// use — a corrupt file or a monitor that is no longer attached.
    pub fn sane(self) -> Self {
        Self {
            width: self.width.clamp(640.0, 10_000.0),
            height: self.height.clamp(400.0, 10_000.0),
            sidebar: self.sidebar.clamp(170.0, 600.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Dotfiles are hidden by default, as they are everywhere else.
    pub show_hidden: bool,
    pub sort: Sort,
    /// Asking before a delete is the default; it is destructive and remote.
    pub confirm_delete: bool,
    pub clock: Clock,
    pub theme: Theme,
    pub window: Window,
    /// Where downloads land. Empty means `~/Downloads`.
    pub download_dir: String,
    /// Whether the transfers drawer is open.
    pub show_transfers: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_hidden: false,
            sort: Sort::default(),
            confirm_delete: true,
            clock: Clock::default(),
            theme: Theme::default(),
            window: Window::default(),
            download_dir: String::new(),
            show_transfers: true,
        }
    }
}

impl Settings {
    pub fn path() -> Option<std::path::PathBuf> {
        crate::model::store::config_dir().map(|dir| dir.join(FILE_NAME))
    }

    /// Never fails: a missing or malformed file means defaults, so a bad edit
    /// cannot stop the app starting.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(settings) => settings,
                Err(error) => {
                    tracing::error!(path = %path.display(), %error, "settings are malformed; using defaults");
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "could not read settings");
                Self::default()
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no home directory"))?;
        std::fs::create_dir_all(path.parent().expect("config path has a parent"))?;
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, &path)
    }
}

impl Settings {
    /// The download folder, resolved: the configured one, else `~/Downloads`,
    /// else home.
    pub fn downloads(&self) -> std::path::PathBuf {
        if !self.download_dir.trim().is_empty() {
            let dir = self.download_dir.trim();
            if let Some(rest) = dir.strip_prefix("~/")
                && let Some(home) = std::env::var_os("HOME")
            {
                return std::path::PathBuf::from(home).join(rest);
            }
            return std::path::PathBuf::from(dir);
        }
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let downloads = home.join("Downloads");
        if downloads.is_dir() { downloads } else { home }
    }
}

/// Whether a name is hidden, by the Unix convention. S3 keys can start with a
/// dot too, and the same rule reads sensibly there.
pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::order::SortKey;

    #[test]
    fn defaults_are_the_unsurprising_ones() {
        let settings = Settings::default();
        assert!(!settings.show_hidden);
        assert!(
            settings.confirm_delete,
            "deleting is destructive and remote"
        );
        assert_eq!(settings.clock, Clock::Local);
        assert_eq!(settings.theme, Theme::System);
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let settings = Settings {
            show_hidden: true,
            sort: Sort {
                key: SortKey::Modified,
                descending: true,
                directories_first: false,
            },
            confirm_delete: false,
            clock: Clock::Utc,
            theme: Theme::Dark,
            window: Window {
                width: 900.0,
                height: 600.0,
                sidebar: 250.0,
            },
            download_dir: "~/Incoming".into(),
            show_transfers: false,
        };
        let text = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
    }

    #[test]
    fn a_partial_file_fills_in_the_rest() {
        // Only one key set: everything else must take its default rather than
        // failing to parse.
        let settings: Settings = toml::from_str("show_hidden = true").unwrap();
        assert!(settings.show_hidden);
        assert!(settings.confirm_delete);
        assert_eq!(settings.window, Window::default());
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        assert_eq!(toml::from_str::<Settings>("").unwrap(), Settings::default());
    }

    #[test]
    fn an_implausible_window_is_clamped_rather_than_used() {
        let absurd = Window {
            width: 1.0,
            height: -50.0,
            sidebar: 100_000.0,
        };
        let sane = absurd.sane();
        assert_eq!(sane.width, 640.0);
        assert_eq!(sane.height, 400.0);
        assert_eq!(sane.sidebar, 600.0);
        // A reasonable one passes through untouched.
        assert_eq!(Window::default().sane(), Window::default());
    }

    #[test]
    fn hidden_is_the_dot_convention() {
        assert!(is_hidden(".bashrc"));
        assert!(is_hidden(".config"));
        assert!(!is_hidden("notes.txt"));
        assert!(!is_hidden("archive.tar.gz"));
        assert!(!is_hidden(""));
    }
}
