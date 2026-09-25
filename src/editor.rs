use crate::proxy::{Direction, Limits, PerDirection};
use crate::settings::{PORT_ERROR, Profile, RATE_ERROR, Settings, parse_port, parse_rate};

const NEW_PROFILE: &str = "New profile";

/// Why the form could not be used. The UI shows each kind next to a different section.
pub enum Invalid {
    Rates(String),
    Profile(String),
}

/// A profile to open in the form.
#[derive(Clone)]
pub enum Choice {
    Saved(String),
    New,
}

impl Choice {
    pub fn name(&self) -> &str {
        match self {
            Self::Saved(name) => name,
            Self::New => NEW_PROFILE,
        }
    }
}

/// The form as typed. Any text is allowed here. `Editor` checks it when it is used.
pub struct Form {
    pub name: String,
    pub host: String,
    pub upstream_port: String,
    pub local_port: String,
    pub download: String,
    pub upload: String,
    pub limited: bool,
}

impl Form {
    fn new(profile: &Profile) -> Self {
        Self {
            name: profile.name.clone(),
            host: profile.host.clone(),
            upstream_port: profile.upstream_port.to_string(),
            local_port: profile.local_port.to_string(),
            download: profile.download_kib.to_string(),
            upload: profile.upload_kib.to_string(),
            limited: profile.limited,
        }
    }

    pub fn rate_mut(&mut self, direction: Direction) -> &mut String {
        match direction {
            Direction::Download => &mut self.download,
            Direction::Upload => &mut self.upload,
        }
    }
}

/// The profile form and the saved profiles behind it.
pub struct Editor {
    pub form: Form,
    settings: Settings,
    // The rates in KiB/s that the proxy uses. Typed rates apply only on commit.
    rates: PerDirection<u64>,
    // The profile as last loaded or saved, for detecting unsaved edits.
    baseline: Profile,
    // Always the name of a profile in `settings`.
    selected: Option<String>,
}

impl Editor {
    pub fn new(settings: Settings) -> Self {
        let (profile, selected) = match settings.profiles().first() {
            Some(profile) => (profile.clone(), Some(profile.name.clone())),
            None => (Profile::default(), None),
        };
        Self {
            form: Form::new(&profile),
            rates: rates_of(&profile),
            baseline: profile,
            selected,
            settings,
        }
    }

    pub fn profiles(&self) -> &[Profile] {
        self.settings.profiles()
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The active rates in KiB/s.
    pub fn rates(&self) -> PerDirection<u64> {
        self.rates
    }

    pub fn limits(&self) -> Limits {
        Limits {
            enabled: self.form.limited,
            bytes_per_second: PerDirection {
                download: self.rates.download * 1024,
                upload: self.rates.upload * 1024,
            },
        }
    }

    fn draft(&self) -> Option<Profile> {
        Some(Profile {
            name: self.form.name.clone(),
            host: self.form.host.clone(),
            upstream_port: parse_port(&self.form.upstream_port)?,
            local_port: parse_port(&self.form.local_port)?,
            download_kib: self.rates.download,
            upload_kib: self.rates.upload,
            limited: self.form.limited,
        })
    }

    pub fn rates_pending(&self) -> bool {
        self.form.download.trim() != self.rates.download.to_string()
            || self.form.upload.trim() != self.rates.upload.to_string()
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.draft().as_ref() != Some(&self.baseline) || self.rates_pending()
    }

    /// Replaces the form with a saved or new profile, discarding any edits.
    pub fn open(&mut self, choice: &Choice) {
        let (profile, selected) = match choice {
            Choice::Saved(name) => {
                let Some(profile) = self.profiles().iter().find(|saved| &saved.name == name) else {
                    return;
                };
                (profile.clone(), Some(name.clone()))
            }
            Choice::New => {
                let profile = Profile {
                    name: NEW_PROFILE.into(),
                    ..Profile::default()
                };
                (profile, None)
            }
        };
        self.form = Form::new(&profile);
        self.rates = rates_of(&profile);
        self.baseline = profile;
        self.selected = selected;
    }

    /// Types `rate` into both rate fields and applies it.
    pub fn apply_preset(&mut self, rate: u64) -> Result<(), String> {
        self.form.download = rate.to_string();
        self.form.upload = rate.to_string();
        self.commit_rates()
    }

    /// Applies typed rates, or restores the active ones if the text is invalid.
    pub fn commit_rates(&mut self) -> Result<(), String> {
        let parsed = parse_rate(&self.form.download).zip(parse_rate(&self.form.upload));
        if let Some((download, upload)) = parsed {
            self.rates = PerDirection { download, upload };
        }
        self.form.download = self.rates.download.to_string();
        self.form.upload = self.rates.upload.to_string();
        parsed
            .map(|_| ())
            .ok_or_else(|| format!("{RATE_ERROR} The previous rates are still active."))
    }

    /// Commits the form and returns the cleaned-up profile, which the form then shows.
    pub fn commit(&mut self) -> Result<Profile, Invalid> {
        self.commit_rates().map_err(Invalid::Rates)?;
        let mut profile = self
            .draft()
            .ok_or_else(|| Invalid::Profile(PORT_ERROR.into()))?;
        profile.name = profile.name.trim().into();
        profile.host = profile.host.trim().into();
        profile
            .validate()
            .map_err(|error| Invalid::Profile(error.into()))?;
        self.form = Form::new(&profile);
        Ok(profile)
    }

    /// Saves the form under its name, replacing the selected profile if there is one.
    pub fn save(&mut self) -> Result<String, Invalid> {
        let profile = self.commit()?;
        let mut profiles = self.profiles().to_vec();
        let existing = self
            .selected
            .as_ref()
            .and_then(|name| profiles.iter().position(|saved| &saved.name == name));
        let taken = profiles
            .iter()
            .enumerate()
            .any(|(index, saved)| saved.name == profile.name && Some(index) != existing);
        if taken {
            return Err(Invalid::Profile(format!(
                "Another profile is already named \"{}\".",
                profile.name
            )));
        }
        match existing {
            Some(index) => profiles[index] = profile.clone(),
            None => profiles.push(profile.clone()),
        }
        self.settings
            .save(profiles)
            .map_err(|error| Invalid::Profile(format!("Could not save: {error}")))?;
        self.selected = Some(profile.name.clone());
        self.baseline = profile.clone();
        Ok(profile.name)
    }

    /// Deletes a saved profile. Its values stay in the form, unsaved.
    pub fn delete(&mut self, name: &str) -> std::io::Result<()> {
        let profiles = self
            .profiles()
            .iter()
            .filter(|profile| profile.name != name)
            .cloned()
            .collect();
        self.settings.save(profiles)?;
        if self.selected.as_deref() == Some(name) {
            self.selected = None;
        }
        Ok(())
    }
}

fn rates_of(profile: &Profile) -> PerDirection<u64> {
    PerDirection {
        download: profile.download_kib,
        upload: profile.upload_kib,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::testing::TempProfiles;

    fn saved(name: &str) -> Profile {
        Profile {
            name: name.into(),
            ..Profile::default()
        }
    }

    fn editor_with(file: &TempProfiles, profiles: &[&str]) -> Editor {
        let (mut settings, _) = Settings::load_from(file.path());
        settings
            .save(profiles.iter().map(|name| saved(name)).collect())
            .unwrap();
        Editor::new(settings)
    }

    #[test]
    fn invalid_rates_restore_the_active_ones() {
        let mut editor = Editor::new(Settings::default());
        editor.form.download = "0".into();
        assert!(editor.commit_rates().is_err());
        assert_eq!(editor.form.download, "128");
        editor.form.download = " 64 ".into();
        editor.commit_rates().unwrap();
        assert_eq!(editor.rates().download, 64);
        assert!(editor.has_unsaved_changes());
    }

    #[test]
    fn presets_apply_to_both_directions() {
        let mut editor = Editor::new(Settings::default());
        editor.apply_preset(16).unwrap();
        assert_eq!(
            editor.limits().bytes_per_second,
            PerDirection {
                download: 16 * 1024,
                upload: 16 * 1024
            }
        );
        assert!(!editor.rates_pending());
    }

    #[test]
    fn commit_trims_and_validates() {
        let mut editor = Editor::new(Settings::default());
        editor.form.name = "  Prod  ".into();
        assert_eq!(editor.commit().ok().unwrap().name, "Prod");
        assert_eq!(editor.form.name, "Prod");
        editor.form.local_port = "0".into();
        assert!(matches!(editor.commit(), Err(Invalid::Profile(_))));
    }

    #[test]
    fn saving_the_selected_profile_renames_it_in_place() {
        let file = TempProfiles::new("rename");
        let mut editor = editor_with(&file, &["First", "Second"]);
        assert_eq!(editor.selected(), Some("First"));
        editor.form.name = "Renamed".into();
        assert_eq!(editor.save().ok().unwrap(), "Renamed");
        let names: Vec<_> = editor.profiles().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Renamed", "Second"]);
        assert!(!editor.has_unsaved_changes());
        let (reloaded, problem) = Settings::load_from(file.path());
        assert!(problem.is_none());
        assert_eq!(reloaded.profiles(), editor.profiles());
    }

    #[test]
    fn saving_under_another_profiles_name_is_refused() {
        let file = TempProfiles::new("collision");
        let mut editor = editor_with(&file, &["First", "Second"]);
        editor.form.name = "Second".into();
        assert!(matches!(editor.save(), Err(Invalid::Profile(_))));
        assert_eq!(editor.profiles().len(), 2);
        assert_eq!(editor.selected(), Some("First"));
    }

    #[test]
    fn a_new_profile_is_added_and_selected_on_save() {
        let file = TempProfiles::new("new");
        let mut editor = editor_with(&file, &["First"]);
        editor.open(&Choice::New);
        assert_eq!(editor.selected(), None);
        assert_eq!(editor.form.name, NEW_PROFILE);
        editor.save().ok().unwrap();
        assert_eq!(editor.profiles().len(), 2);
        assert_eq!(editor.selected(), Some(NEW_PROFILE));
    }

    #[test]
    fn deleting_keeps_the_form_and_clears_the_selection() {
        let file = TempProfiles::new("delete");
        let mut editor = editor_with(&file, &["First", "Second"]);
        editor.delete("First").unwrap();
        assert_eq!(editor.selected(), None);
        assert_eq!(editor.form.name, "First");
        assert_eq!(editor.profiles().len(), 1);
        let (reloaded, _) = Settings::load_from(file.path());
        assert_eq!(reloaded.profiles().len(), 1);
    }

    #[test]
    fn opening_a_missing_profile_changes_nothing() {
        let mut editor = Editor::new(Settings::default());
        editor.form.name = "Edited".into();
        editor.open(&Choice::Saved("Gone".into()));
        assert_eq!(editor.form.name, "Edited");
    }
}
