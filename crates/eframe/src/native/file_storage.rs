use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The folder where `eframe` will store its state.
///
/// The given `app_id` is either the
/// [`egui::ViewportBuilder::app_id`] of [`crate::NativeOptions::viewport`]
/// or the title argument to [`crate::run_native`].
///
/// On native, the path is:
/// * Linux:   `/home/UserName/.local/share/APP_ID`
/// * macOS:   `/Users/UserName/Library/Application Support/APP_ID`
/// * Windows: `C:\Users\UserName\AppData\Roaming\APP_ID\data`
pub fn storage_dir(app_id: &str) -> Option<PathBuf> {
    use egui::os::OperatingSystem as OS;
    use std::env::var_os;
    match OS::from_target_os() {
        OS::Nix => var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::home_dir().map(|p| p.join(".local").join("share")))
            .map(|p| {
                p.join(
                    app_id
                        .to_lowercase()
                        .replace(|c: char| c.is_ascii_whitespace(), ""),
                )
            }),
        OS::Mac => std::env::home_dir().map(|p| {
            p.join("Library")
                .join("Application Support")
                .join(app_id.replace(|c: char| c.is_ascii_whitespace(), "-"))
        }),
        OS::Windows => roaming_appdata().map(|p| p.join(app_id).join("data")),
        OS::Unknown | OS::Android | OS::IOS => None,
    }
}

// Adapted from
// https://github.com/rust-lang/cargo/blob/6e11c77384989726bb4f412a0e23b59c27222c34/crates/home/src/windows.rs#L19-L37
#[cfg(all(windows, not(target_vendor = "uwp")))]
#[expect(unsafe_code)]
fn roaming_appdata() -> Option<PathBuf> {
    use core::ptr;
    use core::slice;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;

    use windows_sys::Win32::Foundation::S_OK;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_RoamingAppData, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath,
    };

    unsafe extern "C" {
        fn wcslen(buf: *const u16) -> usize;
    }
    let mut path_raw = ptr::null_mut();

    // SAFETY: SHGetKnownFolderPath allocates for us, we don't pass any pointers to it.
    // See https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath
    let result = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_RoamingAppData,
            KF_FLAG_DONT_VERIFY as u32,
            core::ptr::null_mut(),
            &raw mut path_raw,
        )
    };

    let path = if result == S_OK {
        // SAFETY: SHGetKnownFolderPath indicated success and is supposed to allocate a null-terminated string for us.
        let path_slice = unsafe { slice::from_raw_parts(path_raw, wcslen(path_raw)) };
        Some(PathBuf::from(OsString::from_wide(path_slice)))
    } else {
        None
    };

    // SAFETY:
    // This memory got allocated by SHGetKnownFolderPath, we didn't touch anything in the process.
    // A null ptr is a no-op for `CoTaskMemFree`, so in case this failed we're still good.
    // https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-cotaskmemfree
    unsafe { CoTaskMemFree(path_raw.cast()) };

    path
}

#[cfg(any(not(windows), target_vendor = "uwp"))]
fn roaming_appdata() -> Option<PathBuf> {
    None
}

// ----------------------------------------------------------------------------

/// The schema version written by this version of `eframe`.
///
/// Increment this whenever the on-disk layout changes in a way that older
/// readers can't understand. Older files are upgraded by [`StorageFile::parse`].
pub const STORAGE_SCHEMA_VERSION: u32 = 1;

/// Key holding the schema version inside the storage file.
const SCHEMA_VERSION_KEY: &str = "eframe_storage_version";

/// Key holding the key-value map inside the storage file.
const VALUES_KEY: &str = "values";

/// Why loading the storage file failed.
///
/// This is deliberately richer than "missing": a missing file is not an error
/// at all, while a corrupt or too-new file must be reported distinctly so the
/// caller can decide whether to warn, back up, or start fresh.
#[derive(Debug)]
pub enum StorageLoadError {
    /// The file exists but could not be read.
    Io(std::io::Error),

    /// The file is not valid RON, or does not have the storage-file shape.
    Corrupt {
        /// Human-readable description of what was wrong.
        message: String,
    },

    /// The file was written by a newer `eframe` and uses a schema version we
    /// don't know how to read.
    ///
    /// The file is left untouched on disk so a newer version can still use it.
    UnsupportedVersion {
        /// The version found in the file.
        found: u32,
        /// The newest version this build understands.
        supported: u32,
    },
}

impl core::fmt::Display for StorageLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "failed to read storage file: {err}"),
            Self::Corrupt { message } => write!(f, "storage file is corrupt: {message}"),
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "storage file schema version {found} is newer than the supported version {supported}"
            ),
        }
    }
}

impl core::error::Error for StorageLoadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Corrupt { .. } | Self::UnsupportedVersion { .. } => None,
        }
    }
}

/// The parsed content of a storage file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StorageFile {
    /// Schema version the file was written with.
    pub schema_version: u32,

    /// The key-value pairs.
    pub values: BTreeMap<String, String>,

    /// Fields written by a newer `eframe` that this version doesn't understand.
    ///
    /// They are kept verbatim (as raw RON) and written back on save, so that
    /// downgrading or round-tripping through an older version doesn't lose
    /// data. See [`StorageFile::load`].
    pub unknown: BTreeMap<String, Box<ron::value::RawValue>>,
}

impl StorageFile {
    /// Parse a storage file from its text.
    ///
    /// Accepts both the current versioned format and the unversioned format
    /// written by older `eframe` versions (a bare map of key-values), which is
    /// upgraded in memory to [`STORAGE_SCHEMA_VERSION`].
    ///
    /// Unknown fields are kept in [`StorageFile::unknown`] rather than
    /// rejected, so a file written by a newer version still loads here.
    ///
    /// # Errors
    /// [`StorageLoadError::Corrupt`] if `text` is not valid RON or doesn't have
    /// the storage-file shape, and [`StorageLoadError::UnsupportedVersion`] if
    /// it was written by a newer schema version.
    pub fn parse(text: &str) -> Result<Self, StorageLoadError> {
        profiling::function_scope!();

        let fields = Fields::parse(text)?;

        let version = match fields.get(SCHEMA_VERSION_KEY) {
            Some(raw) => {
                parse_version(raw).map_err(|message| StorageLoadError::Corrupt { message })?
            }
            // No version field: written by an `eframe` from before versioning.
            None => 0,
        };

        if version > STORAGE_SCHEMA_VERSION {
            return Err(StorageLoadError::UnsupportedVersion {
                found: version,
                supported: STORAGE_SCHEMA_VERSION,
            });
        }

        if version == 0 {
            // Legacy format: a bare map from key to value, with no envelope.
            // A key literally named `values` is just a value here.
            let values = fields.into_legacy_values()?;
            return Ok(Self {
                schema_version: migrate(0),
                values,
                unknown: BTreeMap::new(),
            });
        }

        let values = match fields.get(VALUES_KEY) {
            Some(raw) => {
                parse_values(raw).map_err(|message| StorageLoadError::Corrupt { message })?
            }
            None => {
                return Err(StorageLoadError::Corrupt {
                    message: format!("schema version {version} file is missing `{VALUES_KEY}`"),
                });
            }
        };

        Ok(Self {
            schema_version: migrate(version),
            values,
            unknown: fields.into_unknown(),
        })
    }

    /// Read and parse a storage file.
    ///
    /// Returns `Ok(None)` if the file does not exist (that is a normal first
    /// run, not an error), and a [`StorageLoadError`] if it exists but can't be
    /// used.
    ///
    /// # Errors
    /// [`StorageLoadError::Io`] if the file can't be read,
    /// [`StorageLoadError::Corrupt`] if it isn't a valid storage file, and
    /// [`StorageLoadError::UnsupportedVersion`] if it was written by a newer
    /// schema version.
    pub fn load(path: impl AsRef<Path>) -> Result<Option<Self>, StorageLoadError> {
        profiling::function_scope!();
        let path = path.as_ref();

        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(StorageLoadError::Io(err)),
        };
        let text =
            std::io::read_to_string(std::io::BufReader::new(file)).map_err(StorageLoadError::Io)?;

        Self::parse(&text).map(Some)
    }

    /// Serialize to the on-disk text.
    ///
    /// The output is deterministic: keys are sorted, so writing the same state
    /// twice produces byte-identical files.
    pub fn to_ron_string(&self) -> String {
        profiling::function_scope!();

        // Keys are stored in `BTreeMap`s, so the field order is already stable;
        // RON's pretty-printer then produces the same bytes for the same state.
        let config = ron::ser::PrettyConfig::default()
            .new_line("\n")
            .indentor("    ");

        let envelope = SerializeEnvelope {
            version: self.schema_version,
            values: &self.values,
            unknown: &self.unknown,
        };

        // The envelope only holds strings, string maps and raw RON, all of
        // which always serialize. If that ever changes, panicking is better
        // than silently saving an empty file over the user's state.
        #[expect(clippy::expect_used)]
        let mut text = ron::Options::default()
            .to_string_pretty(&envelope, config)
            .expect("serializing a storage file cannot fail");

        // The pretty-printer omits a trailing newline; add one so the file ends
        // with a newline like any other text file.
        text.push('\n');
        text
    }
}

/// On-disk shape of a storage file: the version, the values, and any unknown
/// fields (kept verbatim) from a newer version.
struct SerializeEnvelope<'a> {
    version: u32,
    values: &'a BTreeMap<String, String>,
    unknown: &'a BTreeMap<String, Box<ron::value::RawValue>>,
}

impl serde::Serialize for SerializeEnvelope<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;

        let mut map = serializer.serialize_map(Some(2 + self.unknown.len()))?;
        map.serialize_entry(SCHEMA_VERSION_KEY, &self.version)?;
        map.serialize_entry(VALUES_KEY, self.values)?;
        for (key, value) in self.unknown {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Upgrade an older file's schema version to [`STORAGE_SCHEMA_VERSION`].
///
/// Both the legacy (unversioned) layout and the current one store the same
/// key-value pairs, so version 0 needs no field rewriting; only the envelope
/// is added on the next save. Future format changes get a new `match` arm here.
fn migrate(from_version: u32) -> u32 {
    match from_version {
        // Legacy, unversioned: values have already been extracted.
        0 => STORAGE_SCHEMA_VERSION,
        version => {
            debug_assert!(version <= STORAGE_SCHEMA_VERSION, "checked by the caller");
            version
        }
    }
}

fn parse_version(raw: &ron::value::RawValue) -> Result<u32, String> {
    raw.to_owned()
        .into_rust::<u32>()
        .map_err(|err| format!("`{SCHEMA_VERSION_KEY}` is not a valid version number: {err}"))
}

fn parse_values(raw: &ron::value::RawValue) -> Result<BTreeMap<String, String>, String> {
    raw.to_owned()
        .into_rust::<BTreeMap<String, String>>()
        .map_err(|err| format!("`{VALUES_KEY}` is not a map of strings: {err}"))
}

/// Raw fields of a storage file, before they are interpreted.
struct Fields {
    fields: BTreeMap<String, Box<ron::value::RawValue>>,
}

impl Fields {
    fn parse(text: &str) -> Result<Self, StorageLoadError> {
        let fields = ron::from_str::<BTreeMap<String, _>>(text).map_err(|err| {
            StorageLoadError::Corrupt {
                message: err.to_string(),
            }
        })?;
        Ok(Self { fields })
    }

    fn get(&self, key: &str) -> Option<&ron::value::RawValue> {
        self.fields.get(key).map(core::ops::Deref::deref)
    }

    fn into_legacy_values(self) -> Result<BTreeMap<String, String>, StorageLoadError> {
        self.fields
            .into_iter()
            .map(|(key, raw)| {
                let value = raw
                    .into_rust::<String>()
                    .map_err(|err| StorageLoadError::Corrupt {
                        message: format!(
                            "unversioned storage file value for key {key:?} is not a string: {err}"
                        ),
                    })?;
                Ok((key, value))
            })
            .collect()
    }

    fn into_unknown(self) -> BTreeMap<String, Box<ron::value::RawValue>> {
        self.fields
            .into_iter()
            .filter(|(key, _)| key != SCHEMA_VERSION_KEY && key != VALUES_KEY)
            .collect()
    }
}

// ----------------------------------------------------------------------------

/// A key-value store backed by a [RON](https://github.com/ron-rs/ron) file on disk.
/// Used to restore egui state, glow window position/size and app state.
pub struct FileStorage {
    ron_filepath: PathBuf,
    kv: BTreeMap<String, String>,
    /// Fields from a newer `eframe` that we don't understand, written back verbatim.
    unknown: BTreeMap<String, Box<ron::value::RawValue>>,
    dirty: bool,
    last_save_join_handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FileStorage {
    fn drop(&mut self) {
        if let Some(join_handle) = self.last_save_join_handle.take() {
            profiling::scope!("wait_for_save");
            join_handle.join().ok();
        }
    }
}

impl FileStorage {
    /// Store the state in this .ron file.
    pub(crate) fn from_ron_filepath(ron_filepath: impl Into<PathBuf>) -> Self {
        profiling::function_scope!();
        let ron_filepath: PathBuf = ron_filepath.into();
        log::debug!("Loading app state from {}…", ron_filepath.display());

        let loaded = match StorageFile::load(&ron_filepath) {
            Ok(Some(file)) => file,
            Ok(None) => StorageFile {
                schema_version: STORAGE_SCHEMA_VERSION,
                ..Default::default()
            },
            Err(err) => {
                // Keep going with an empty store, but never silently: log which
                // kind of failure it was, so a corrupt or too-new file can be
                // told apart from a missing one. Use `StorageFile::load` to
                // handle the error programmatically instead.
                log::warn!(
                    "Failed to load app state from {}: {err}. Starting with empty state.",
                    ron_filepath.display()
                );
                StorageFile {
                    schema_version: STORAGE_SCHEMA_VERSION,
                    ..Default::default()
                }
            }
        };

        Self {
            kv: loaded.values,
            unknown: loaded.unknown,
            ron_filepath,
            dirty: false,
            last_save_join_handle: None,
        }
    }

    /// Find a good place to put the files that the OS likes.
    #[cfg_attr(not(feature = "persistence"), expect(dead_code))]
    pub fn from_app_id(app_id: &str) -> Option<Self> {
        profiling::function_scope!();
        if let Some(data_dir) = storage_dir(app_id) {
            if let Err(err) = std::fs::create_dir_all(&data_dir) {
                log::warn!(
                    "Saving disabled: Failed to create app path at {}: {err}",
                    data_dir.display()
                );
                None
            } else {
                Some(Self::from_ron_filepath(data_dir.join("app.ron")))
            }
        } else {
            log::warn!("Saving disabled: Failed to find path to data_dir.");
            None
        }
    }
}

impl crate::Storage for FileStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.kv.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        if self.kv.get(key) != Some(&value) {
            self.kv.insert(key.to_owned(), value);
            self.dirty = true;
        }
    }

    fn remove_string(&mut self, key: &str) {
        self.kv.remove(key);
        self.dirty = true;
    }

    fn flush(&mut self) {
        if self.dirty {
            profiling::scope!("FileStorage::flush");
            self.dirty = false;

            let file_path = self.ron_filepath.clone();
            let kv = self.kv.clone();
            let unknown = self.unknown.clone();

            if let Some(join_handle) = self.last_save_join_handle.take() {
                // wait for previous save to complete.
                join_handle.join().ok();
            }

            let result = std::thread::Builder::new()
                .name("eframe_persist".to_owned())
                .spawn(move || {
                    save_to_disk(&file_path, &kv, &unknown);
                });
            match result {
                Ok(join_handle) => {
                    self.last_save_join_handle = Some(join_handle);
                }
                Err(err) => {
                    log::warn!("Failed to spawn thread to save app state: {err}");
                }
            }
        }
    }
}

fn save_to_disk(
    file_path: &Path,
    kv: &BTreeMap<String, String>,
    unknown: &BTreeMap<String, Box<ron::value::RawValue>>,
) {
    profiling::function_scope!();

    if let Some(parent_dir) = file_path.parent()
        && !parent_dir.exists()
        && let Err(err) = std::fs::create_dir_all(parent_dir)
    {
        log::warn!("Failed to create directory {}: {err}", parent_dir.display());
    }

    let file = StorageFile {
        schema_version: STORAGE_SCHEMA_VERSION,
        values: kv.clone(),
        unknown: unknown.clone(),
    };
    let text = file.to_ron_string();

    // Write to a temporary file and rename it into place, so an interrupted
    // save can never leave a half-written (corrupt) storage file behind.
    let tmp_path = file_path.with_extension("ron.tmp");
    match std::fs::write(&tmp_path, text) {
        Ok(()) => match std::fs::rename(&tmp_path, file_path) {
            Ok(()) => {
                log::trace!("Persisted to {}", file_path.display());
            }
            Err(err) => {
                log::warn!(
                    "Failed to rename {} to {}: {err}",
                    tmp_path.display(),
                    file_path.display()
                );
                std::fs::remove_file(&tmp_path).ok();
            }
        },
        Err(err) => {
            log::warn!("Failed to write {}: {err}", tmp_path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests

    use super::*;

    fn directories_storage_dir(app_id: &str) -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", app_id)
            .map(|proj_dirs| proj_dirs.data_dir().to_path_buf())
    }

    #[test]
    fn storage_path_matches_directories() {
        use super::storage_dir;
        for app_id in [
            "MyApp", "My App", "my_app", "my-app", "My.App", "my/app", "my:app", r"my\app",
        ] {
            assert_eq!(directories_storage_dir(app_id), storage_dir(app_id));
        }
    }

    // ------------------------------------------------------------------------
    // Persistence format fixtures

    const LEGACY_V0: &str = include_str!("storage_fixtures/legacy_v0.ron");
    const VERSIONED_UNKNOWN_FIELDS: &str =
        include_str!("storage_fixtures/versioned_unknown_fields.ron");
    const CORRUPT_TRUNCATED: &str = include_str!("storage_fixtures/corrupt_truncated.ron");

    /// Writes a fixture to a fresh temp dir and returns the path.
    fn write_fixture(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn legacy_fixture_loads_unchanged() {
        // An unversioned file from an older eframe must load with exactly the
        // same key-values, and be upgraded to the current schema version.
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "app.ron", LEGACY_V0);

        let loaded = StorageFile::load(&path).unwrap().expect("file exists");

        assert_eq!(loaded.schema_version, STORAGE_SCHEMA_VERSION);
        assert_eq!(loaded.values.len(), 2);
        assert_eq!(
            loaded.values["app"],
            "(counter:3,name:\"hello\",note:\"Ünïcode — 東京 \\\"quoted\\\"\")"
        );
        assert_eq!(
            loaded.values["window"],
            "(fullscreen:false,inner_size:(1280.0,720.0),theme:\"dark\")"
        );
        assert!(loaded.unknown.is_empty());
    }

    #[test]
    fn versioned_fixture_keeps_unknown_fields() {
        // Fields written by a newer eframe are neither rejected nor dropped:
        // they are kept verbatim and written back on save.
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "app.ron", VERSIONED_UNKNOWN_FIELDS);

        let loaded = StorageFile::load(&path).unwrap().expect("file exists");
        assert_eq!(loaded.schema_version, STORAGE_SCHEMA_VERSION);
        assert_eq!(loaded.values.len(), 2);
        assert_eq!(loaded.unknown.len(), 2);
        assert_eq!(loaded.unknown["future_list"].get_ron().trim(), "[1, 2, 3]");
        assert!(
            loaded.unknown["future_settings"]
                .get_ron()
                .contains("Named(Blue)"),
            "unknown enum values must survive: {:?}",
            loaded.unknown["future_settings"].get_ron()
        );

        // Re-serializing and loading again must preserve the unknown fields.
        let roundtripped = StorageFile::parse(&loaded.to_ron_string()).unwrap();
        assert_eq!(roundtripped, loaded);
    }

    #[test]
    fn corrupt_fixture_is_a_distinct_error() {
        // Corruption must not be silently treated like "no file".
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "app.ron", CORRUPT_TRUNCATED);

        let err = StorageFile::load(&path).unwrap_err();
        assert!(
            matches!(err, StorageLoadError::Corrupt { .. }),
            "expected Corrupt, got {err:?}"
        );
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.ron");
        assert!(StorageFile::load(&path).unwrap().is_none());
    }

    #[test]
    fn newer_schema_version_is_rejected_distinctly() {
        let text = format!(
            "{{\"{SCHEMA_VERSION_KEY}\": {}, \"{VALUES_KEY}\": {{}}}}",
            STORAGE_SCHEMA_VERSION + 1
        );
        let err = StorageFile::parse(&text).unwrap_err();
        match err {
            StorageLoadError::UnsupportedVersion { found, supported } => {
                assert_eq!(found, STORAGE_SCHEMA_VERSION + 1);
                assert_eq!(supported, STORAGE_SCHEMA_VERSION);
            }
            err => panic!("expected UnsupportedVersion, got {err:?}"),
        }
    }

    #[test]
    fn same_state_writes_identical_bytes() {
        // Deterministic output: same state, same bytes, regardless of the order
        // that keys were inserted in.
        let mut a = StorageFile {
            schema_version: STORAGE_SCHEMA_VERSION,
            ..Default::default()
        };
        a.values.insert("z".to_owned(), "1".to_owned());
        a.values.insert("a".to_owned(), "2".to_owned());
        a.unknown.insert(
            "newer".to_owned(),
            ron::value::RawValue::from_ron("[1, 2]").unwrap().to_owned(),
        );

        let mut b = StorageFile {
            schema_version: STORAGE_SCHEMA_VERSION,
            ..Default::default()
        };
        b.unknown.insert(
            "newer".to_owned(),
            ron::value::RawValue::from_ron("[1, 2]").unwrap().to_owned(),
        );
        b.values.insert("a".to_owned(), "2".to_owned());
        b.values.insert("z".to_owned(), "1".to_owned());

        assert_eq!(a, b);
        assert_eq!(a.to_ron_string(), b.to_ron_string());

        // And the bytes are stable across a load → save cycle.
        let reloaded = StorageFile::parse(&a.to_ron_string()).unwrap();
        assert_eq!(reloaded.to_ron_string(), a.to_ron_string());
    }

    #[test]
    fn legacy_file_written_back_is_versioned_and_equivalent() {
        // Loading a legacy file and saving it produces a versioned file that
        // holds the same key-values.
        let loaded = StorageFile::parse(LEGACY_V0).unwrap();
        let resaved = StorageFile::parse(&loaded.to_ron_string()).unwrap();
        assert_eq!(resaved.values, loaded.values);
        assert_eq!(resaved.schema_version, STORAGE_SCHEMA_VERSION);
        assert!(resaved.to_ron_string().contains(SCHEMA_VERSION_KEY));
    }

    #[test]
    fn values_round_trip_through_flush_and_reload() {
        // End-to-end through `FileStorage` + `Storage::flush` (which saves on a
        // background thread; `Drop` joins it).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.ron");

        {
            let mut storage = FileStorage::from_ron_filepath(&path);
            crate::Storage::set_string(&mut storage, "app", "(a:1)".to_owned());
            crate::Storage::set_string(&mut storage, "window", "(b:2)".to_owned());
            crate::Storage::flush(&mut storage);
        } // `Drop` waits for the save to finish.

        let reloaded = StorageFile::load(&path).unwrap().unwrap();
        assert_eq!(reloaded.schema_version, STORAGE_SCHEMA_VERSION);
        assert_eq!(reloaded.values["app"], "(a:1)");
        assert_eq!(reloaded.values["window"], "(b:2)");

        // Saving the same state again writes identical bytes.
        let first_bytes = std::fs::read(&path).unwrap();
        let mut storage = FileStorage::from_ron_filepath(&path);
        crate::Storage::set_string(&mut storage, "window", "(b:2)".to_owned());
        crate::Storage::flush(&mut storage);
        drop(storage);
        assert_eq!(std::fs::read(&path).unwrap(), first_bytes);
    }

    #[test]
    fn legacy_key_named_values_is_not_mistaken_for_envelope() {
        // A legacy file that happens to have a key called `values` must still
        // load as plain key-values (the envelope only exists with a version).
        let loaded = StorageFile::parse(r#"{ "values": "not a map", "app": "(a:1)" }"#).unwrap();
        assert_eq!(loaded.values["values"], "not a map");
        assert_eq!(loaded.values["app"], "(a:1)");
        assert!(loaded.unknown.is_empty());
        assert_eq!(loaded.schema_version, STORAGE_SCHEMA_VERSION);
    }
}
