use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use piik_protocol::{QualitySettings, decode, encode};

use crate::config::save_file;
use crate::{Error, Result};

pub const MAX_QUALITY_PREFERENCE_BYTES: usize = 1024;

/// Optional App-wide sharing quality, not a live capture profile. A separate
/// `<client.json>.quality` file leaves the strict configuration readable by
/// published Apps.
#[derive(Debug)]
pub struct QualityPreference {
    path: PathBuf,
    value: Mutex<Option<QualitySettings>>,
}

impl QualityPreference {
    /// A missing or unreadable preference must not prevent the App from
    /// starting: even with an error the returned store accepts the user's next
    /// explicit choice.
    pub fn load(config_path: &Path) -> (Self, Result<()>) {
        let mut path = config_path.as_os_str().to_owned();
        path.push(".quality");
        let path = PathBuf::from(path);
        let (value, result) = match read(&path) {
            Ok(value) => (value, Ok(())),
            Err(error) => (None, Err(error)),
        };
        (
            Self {
                path,
                value: Mutex::new(value),
            },
            result,
        )
    }

    pub fn read(&self) -> Option<QualitySettings> {
        *self.lock()
    }

    pub fn save(&self, quality: QualitySettings) -> Result<()> {
        // Reuse the wire validator instead of a second list of accepted settings.
        let mut payload = encode(&quality);
        let quality: QualitySettings =
            decode(&payload).map_err(|error| Error::invalid(error.to_string()))?;
        let mut value = self.lock();
        if *value == Some(quality) {
            return Ok(());
        }
        payload.push(b'\n');
        save_file(&self.path, &payload)?;
        *value = Some(quality);
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<QualitySettings>> {
        self.value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn read(path: &Path) -> Result<Option<QualitySettings>> {
    let file = match std::fs::File::open(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        file => file.map_err(Error::io("open saved sharing quality"))?,
    };
    let mut payload = Vec::new();
    file.take(MAX_QUALITY_PREFERENCE_BYTES as u64 + 1)
        .read_to_end(&mut payload)
        .map_err(Error::io("read saved sharing quality"))?;
    if payload.len() > MAX_QUALITY_PREFERENCE_BYTES {
        return Err(Error::invalid("invalid saved sharing quality"));
    }
    decode(&payload)
        .map(Some)
        .map_err(|_| Error::invalid("invalid saved sharing quality"))
}

#[cfg(test)]
mod tests {
    use piik_protocol::{DEFAULT_QUALITY_SETTINGS, QualityResolution};

    use super::*;
    use crate::load_or_create;

    #[test]
    fn quality_preference_persists_without_changing_app_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        load_or_create(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let (preference, result) = QualityPreference::load(&path);
        result.unwrap();
        assert_eq!(preference.read(), None);

        let mut quality = DEFAULT_QUALITY_SETTINGS;
        quality.resolution = QualityResolution {
            width: 1920,
            height: 1200,
        };
        quality.max_audio_bitrate = Some(320_000);
        preference.save(quality).unwrap();
        let (loaded, result) = QualityPreference::load(&path);
        result.unwrap();
        assert_eq!(loaded.read(), Some(quality));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let mut invalid = quality;
        invalid.resolution.width = 0x1_0000_0000;
        assert!(preference.save(invalid).is_err());
        assert_eq!(preference.read(), Some(quality));

        let quality_path = directory.path().join("client.json.quality");
        std::fs::remove_file(&quality_path).unwrap();
        std::fs::create_dir(&quality_path).unwrap();
        quality.resolution = QualityResolution {
            width: 1280,
            height: 720,
        };
        assert!(preference.save(quality).is_err(), "failed write must fail");
        assert_eq!(
            preference.read().unwrap().resolution,
            QualityResolution {
                width: 1920,
                height: 1200
            }
        );
    }

    #[test]
    fn unavailable_quality_preference_can_accept_the_next_choice() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        let oversized = " ".repeat(MAX_QUALITY_PREFERENCE_BYTES + 1);
        for payload in ["null", "{}", "{", oversized.as_str()] {
            std::fs::write(directory.path().join("client.json.quality"), payload).unwrap();
            let (preference, result) = QualityPreference::load(&path);
            assert!(result.is_err(), "accepted {payload:?}");
            assert_eq!(preference.read(), None);
            preference.save(DEFAULT_QUALITY_SETTINGS).unwrap();
            assert!(preference.read().is_some(), "choice was not saved");
        }
    }
}
