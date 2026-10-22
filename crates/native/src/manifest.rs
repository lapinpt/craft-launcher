//! The launcher's record of what it installed itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use artcraft_launcher_core::PackageKind;
use serde::{Deserialize, Serialize};

/// One app the launcher installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    pub kind: PackageKind,
    /// The `.app`, the exe, the AppImage, or the folder of an unpacked tarball.
    pub path: PathBuf,
    /// Unix seconds.
    pub installed_at: i64,
    /// Every file installed, for tree installs (FreeBSD), so removal takes exactly those.
    #[serde(default)]
    pub files: Vec<PathBuf>,
    /// The files are in a system prefix and removing them needs root.
    #[serde(default)]
    pub needs_root: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub apps: BTreeMap<String, Record>,
}

impl Manifest {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Portable installs are stored beside the launcher. If the user renames or moves that
    /// folder, update recorded paths under the previous root before app detection starts.
    pub fn load_portable(path: &Path, root: &Path) -> Self {
        let mut manifest = Self::load(path);
        let Some(data) = path.parent() else { return manifest };
        let root_file = data.join("portable-root.txt");
        let previous_root = std::fs::read_to_string(&root_file).ok().map(|value| PathBuf::from(value.trim()));
        let changed = previous_root.as_deref().is_some_and(|previous| {
            if previous == root {
                return false;
            }
            rebase_records(&mut manifest, previous, root)
        });

        // Older portable builds didn't record their root. If a previously recorded app path is
        // gone but its executable is present in this portable folder's App directory, repair it.
        let inferred = if previous_root.is_none() { infer_portable_records(&mut manifest, root) } else { false };
        if (changed || inferred)
            && let Err(error) = manifest.save(path)
        {
            log::warn!("couldn't update portable app paths in {} ({error})", path.display());
            return manifest;
        }
        if previous_root.as_deref() != Some(root)
            && let Err(error) = crate::paths::write_atomic(&root_file, root.to_string_lossy().as_bytes())
        {
            log::warn!("couldn't save portable launcher folder in {} ({error})", root_file.display());
        }
        manifest
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::paths::write_atomic(path, &bytes)
    }
}

fn rebase_records(manifest: &mut Manifest, previous: &Path, current: &Path) -> bool {
    let mut changed = false;
    for record in manifest.apps.values_mut() {
        if let Ok(relative) = record.path.strip_prefix(previous) {
            record.path = current.join(relative);
            changed = true;
        }
        for file in &mut record.files {
            if let Ok(relative) = file.strip_prefix(previous) {
                *file = current.join(relative);
                changed = true;
            }
        }
    }
    changed
}

fn infer_portable_records(manifest: &mut Manifest, root: &Path) -> bool {
    let mut changed = false;
    for (id, record) in &mut manifest.apps {
        if record.kind != PackageKind::PortableZip || record.path.exists() {
            continue;
        }
        let Some(executable) = record.path.file_name().map(|name| name.to_os_string()) else { continue };
        let new_app_root = crate::paths::portable_apps_dir(root).join(id);
        if !new_app_root.join(&executable).is_file() {
            continue;
        }
        let old_app_root = record.path.parent().map(Path::to_path_buf);
        record.path = new_app_root.join(executable);
        if let Some(old_app_root) = old_app_root {
            for file in &mut record.files {
                if let Ok(relative) = file.strip_prefix(&old_app_root) {
                    *file = new_app_root.join(relative);
                }
            }
        }
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_survives_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installed.json");
        assert_eq!(Manifest::load(&path), Manifest::default());
        let mut m = Manifest::default();
        m.apps.insert(
            "photocraft".into(),
            Record { version: "0.5.0".into(), kind: PackageKind::AppImage, path: "/x/y".into(), installed_at: 1, files: vec![], needs_root: false },
        );
        m.save(&path).unwrap();
        assert_eq!(Manifest::load(&path), m);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Manifest::load(&path), Manifest::default());
    }

    #[test]
    fn portable_manifest_paths_follow_a_renamed_launcher_folder() {
        let dir = tempfile::tempdir().unwrap();
        let old_root = dir.path().join("artcraft-launcher");
        let new_root = dir.path().join("launcher-renamed");
        let app_root = old_root.join("App/photocraft");
        std::fs::create_dir_all(&app_root).unwrap();
        let exe = app_root.join("photocraft.exe");
        let data_file = app_root.join("portable.txt");
        std::fs::write(&exe, b"exe").unwrap();
        std::fs::write(&data_file, b"marker").unwrap();
        let mut manifest = Manifest::default();
        manifest.apps.insert(
            "photocraft".into(),
            Record { version: "0.5.0".into(), kind: PackageKind::PortableZip, path: exe, installed_at: 1, files: vec![data_file], needs_root: false },
        );
        let old_data = old_root.join("Data");
        std::fs::create_dir_all(&old_data).unwrap();
        manifest.save(&old_data.join("installed.json")).unwrap();
        std::fs::write(old_data.join("portable-root.txt"), old_root.to_string_lossy().as_bytes()).unwrap();

        std::fs::rename(&old_root, &new_root).unwrap();
        let new_data = new_root.join("Data");
        let loaded = Manifest::load_portable(&new_data.join("installed.json"), &new_root);
        let record = &loaded.apps["photocraft"];
        assert_eq!(record.path, new_root.join("App/photocraft/photocraft.exe"));
        assert_eq!(record.files, [new_root.join("App/photocraft/portable.txt")]);
        assert_eq!(std::fs::read_to_string(new_data.join("portable-root.txt")).unwrap(), new_root.to_string_lossy());
    }

    #[test]
    fn portable_manifest_without_a_saved_root_repairs_an_app_in_the_new_launcher_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("renamed-launcher");
        let app_root = crate::paths::portable_apps_dir(&root).join("photocraft");
        std::fs::create_dir_all(&app_root).unwrap();
        std::fs::write(app_root.join("photocraft.exe"), b"exe").unwrap();
        std::fs::write(app_root.join("portable.txt"), b"marker").unwrap();
        let data = root.join("Data");
        std::fs::create_dir_all(&data).unwrap();
        let mut manifest = Manifest::default();
        manifest.apps.insert(
            "photocraft".into(),
            Record {
                version: "0.5.0".into(),
                kind: PackageKind::PortableZip,
                path: dir.path().join("old-launcher/App/photocraft/photocraft.exe"),
                installed_at: 1,
                files: vec![dir.path().join("old-launcher/App/photocraft/portable.txt")],
                needs_root: false,
            },
        );
        manifest.save(&data.join("installed.json")).unwrap();

        let loaded = Manifest::load_portable(&data.join("installed.json"), &root);
        let record = &loaded.apps["photocraft"];
        assert_eq!(record.path, app_root.join("photocraft.exe"));
        assert_eq!(record.files, [app_root.join("portable.txt")]);
    }
}
