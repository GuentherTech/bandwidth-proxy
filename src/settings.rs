use serde::{Deserialize, Serialize};
use std::{fs, io, net::Ipv6Addr, ops::RangeInclusive, path::PathBuf};

pub const RATE_RANGE: RangeInclusive<u64> = 1..=1_000_000;
pub const RATE_ERROR: &str = "Rates must be whole numbers from 1 to 1,000,000 KiB/s.";
pub const PORT_ERROR: &str = "Ports must be whole numbers from 1 to 65535.";

pub fn parse_port(text: &str) -> Option<u16> {
    text.trim().parse().ok().filter(|port| *port > 0)
}

pub fn parse_rate(text: &str) -> Option<u64> {
    text.trim()
        .parse()
        .ok()
        .filter(|rate| RATE_RANGE.contains(rate))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub host: String,
    pub upstream_port: u16,
    pub local_port: u16,
    pub download_kib: u64,
    pub upload_kib: u64,
    #[serde(default)]
    pub limited: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Local SQL Server".into(),
            host: "127.0.0.1".into(),
            upstream_port: 1433,
            local_port: 11433,
            download_kib: 128,
            upload_kib: 128,
            limited: false,
        }
    }
}

impl Profile {
    fn bare_host(&self) -> &str {
        self.host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() {
            return Err("Enter a profile name.");
        }
        let host = self.bare_host();
        if host.is_empty() {
            return Err("Enter a destination host.");
        }
        if host.contains(['/', '\\', ' ', '\t', '\n', '\r']) {
            return Err("Enter a hostname or IP address, not a URL or instance name.");
        }
        let address = host.split('%').next().unwrap_or(host);
        if host.contains(',') || (host.contains(':') && address.parse::<Ipv6Addr>().is_err()) {
            return Err("Enter the destination port in its own field, not in the host.");
        }
        if self.upstream_port == 0 || self.local_port == 0 {
            return Err(PORT_ERROR);
        }
        if !RATE_RANGE.contains(&self.download_kib) || !RATE_RANGE.contains(&self.upload_kib) {
            return Err(RATE_ERROR);
        }
        Ok(())
    }

    pub fn target(&self) -> String {
        let host = self.bare_host();
        if host.contains(':') {
            format!("[{host}]:{}", self.upstream_port)
        } else {
            format!("{host}:{}", self.upstream_port)
        }
    }
}

#[derive(Default)]
pub struct Settings {
    // None when the configuration directory cannot be found. Saving then fails.
    path: Option<PathBuf>,
    profiles: Vec<Profile>,
    // Set when the file on disk holds data this version could not load, so the
    // next save copies it aside instead of silently discarding it.
    backup_pending: bool,
}

#[derive(Deserialize)]
struct StoredSettings {
    #[serde(default)]
    profiles: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct SavedSettings<'a> {
    profiles: &'a [Profile],
}

const NO_CONFIG_DIRECTORY: &str = "Cannot locate the user configuration directory";

fn default_path() -> Option<PathBuf> {
    let root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(root.join("bandwidth-proxy").join("profiles.json"))
}

impl Settings {
    /// Loads saved profiles. The message explains anything that could not be loaded.
    pub fn load() -> (Self, Option<String>) {
        match default_path() {
            Some(path) => Self::load_from(path),
            None => (
                Self::default(),
                Some(format!(
                    "Could not open saved profiles: {NO_CONFIG_DIRECTORY}."
                )),
            ),
        }
    }

    pub fn load_from(path: PathBuf) -> (Self, Option<String>) {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (Self::empty(path, false), None);
            }
            Err(error) => {
                return (
                    Self::empty(path, true),
                    Some(format!("Could not open saved profiles: {error}")),
                );
            }
        };
        match Self::parse(&bytes) {
            Ok((settings, 0)) => (
                Self {
                    path: Some(path),
                    ..settings
                },
                None,
            ),
            Ok((settings, skipped)) => (
                Self {
                    path: Some(path),
                    ..settings
                },
                Some(format!(
                    "Skipped {skipped} invalid saved profile(s). The next save keeps the \
                     original file as profiles.json.bak."
                )),
            ),
            Err(error) => (
                Self::empty(path, true),
                Some(format!(
                    "Could not read saved profiles ({error}). The next save keeps the \
                     original file as profiles.json.bak."
                )),
            ),
        }
    }

    fn empty(path: PathBuf, backup_pending: bool) -> Self {
        Self {
            path: Some(path),
            profiles: Vec::new(),
            backup_pending,
        }
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    fn parse(bytes: &[u8]) -> serde_json::Result<(Self, usize)> {
        let stored: StoredSettings = serde_json::from_slice(bytes)?;
        let total = stored.profiles.len();
        let profiles: Vec<Profile> = stored
            .profiles
            .into_iter()
            .filter_map(|value| serde_json::from_value::<Profile>(value).ok())
            .filter(|profile| profile.validate().is_ok())
            .collect();
        let skipped = total - profiles.len();
        let settings = Self {
            path: None,
            profiles,
            backup_pending: skipped > 0,
        };
        Ok((settings, skipped))
    }

    /// Writes `profiles` to disk and, only if that succeeds, makes them current.
    pub fn save(&mut self, profiles: Vec<Profile>) -> io::Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| io::Error::other(NO_CONFIG_DIRECTORY))?;
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        if self.backup_pending {
            match fs::copy(path, path.with_extension("json.bak")) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        let temporary = path.with_extension("tmp");
        let saved = SavedSettings {
            profiles: &profiles,
        };
        fs::write(&temporary, serde_json::to_vec_pretty(&saved)?)?;
        fs::rename(temporary, path)?;
        self.profiles = profiles;
        self.backup_pending = false;
        Ok(())
    }
}

#[cfg(test)]
pub mod testing {
    use std::{fs, path::PathBuf};

    /// A `profiles.json` path in its own temporary folder, removed on drop.
    pub struct TempProfiles(PathBuf);

    impl TempProfiles {
        pub fn new(name: &str) -> Self {
            let folder = std::env::temp_dir().join(format!(
                "bandwidth-proxy-test-{}-{name}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&folder);
            Self(folder.join("profiles.json"))
        }

        pub fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for TempProfiles {
        fn drop(&mut self) {
            if let Some(folder) = self.0.parent() {
                let _ = fs::remove_dir_all(folder);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempProfiles;
    use super::*;

    #[test]
    fn saved_profiles_load_back_from_disk() {
        let file = TempProfiles::new("round-trip");
        let (mut settings, problem) = Settings::load_from(file.path());
        assert!(problem.is_none());
        assert!(settings.profiles().is_empty());
        settings.save(vec![Profile::default()]).unwrap();
        let (reloaded, problem) = Settings::load_from(file.path());
        assert!(problem.is_none());
        assert_eq!(reloaded.profiles(), [Profile::default()]);
        assert!(!file.path().with_extension("json.bak").exists());
    }

    #[test]
    fn unreadable_file_is_backed_up_before_the_next_save() {
        let file = TempProfiles::new("backup");
        fs::create_dir_all(file.path().parent().unwrap()).unwrap();
        fs::write(file.path(), "not json").unwrap();
        let (mut settings, problem) = Settings::load_from(file.path());
        assert!(problem.is_some());
        assert!(settings.profiles().is_empty());
        settings.save(vec![Profile::default()]).unwrap();
        let backup = fs::read_to_string(file.path().with_extension("json.bak")).unwrap();
        assert_eq!(backup, "not json");
        let (reloaded, problem) = Settings::load_from(file.path());
        assert!(problem.is_none());
        assert_eq!(reloaded.profiles().len(), 1);
    }

    #[test]
    fn saving_without_a_path_fails_and_keeps_the_profiles() {
        let mut settings = Settings::default();
        assert!(settings.save(vec![Profile::default()]).is_err());
        assert!(settings.profiles().is_empty());
    }

    #[test]
    fn profiles_round_trip() {
        let profiles = [Profile::default()];
        let bytes = serde_json::to_vec(&SavedSettings {
            profiles: &profiles,
        })
        .unwrap();
        let (restored, skipped) = Settings::parse(&bytes).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(restored.profiles, profiles);
        assert!(!restored.backup_pending);
    }

    #[test]
    fn invalid_profiles_are_skipped_and_the_rest_kept() {
        let bytes = br#"{"profiles": [
            {"name": "Good", "host": "db", "upstream_port": 1433, "local_port": 11433,
             "download_kib": 64, "upload_kib": 64},
            {"name": "Zero rate", "host": "db", "upstream_port": 1433, "local_port": 11433,
             "download_kib": 0, "upload_kib": 64},
            {"name": "Missing fields"}
        ]}"#;
        let (settings, skipped) = Settings::parse(bytes).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.profiles[0].name, "Good");
        assert!(settings.backup_pending);
    }

    #[test]
    fn invalid_rates_and_endpoints_are_rejected() {
        let mut profile = Profile {
            download_kib: 0,
            ..Profile::default()
        };
        assert!(profile.validate().is_err());
        profile.download_kib = 128;
        for host in [
            "https://localhost",
            "localhost:1433",
            "localhost,1433",
            "db\\SQL",
        ] {
            profile.host = host.into();
            assert!(profile.validate().is_err(), "{host}");
        }
    }

    #[test]
    fn ipv6_hosts_are_accepted_and_bracketed() {
        let profile = Profile {
            host: "::1".into(),
            ..Profile::default()
        };
        profile.validate().unwrap();
        assert_eq!(profile.target(), "[::1]:1433");
        let bracketed = Profile {
            host: "[::1]".into(),
            ..Profile::default()
        };
        assert_eq!(bracketed.target(), "[::1]:1433");
        assert_eq!(Profile::default().target(), "127.0.0.1:1433");
    }

    #[test]
    fn field_parsing_uses_shared_ranges() {
        assert_eq!(parse_port(" 1433 "), Some(1433));
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("65536"), None);
        assert_eq!(parse_rate("1000000"), Some(1_000_000));
        assert_eq!(parse_rate("1000001"), None);
        assert_eq!(parse_rate("0"), None);
    }
}
