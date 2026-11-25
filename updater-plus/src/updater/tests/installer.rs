use std::path::Path;
use std::sync::{Arc, Mutex};

use semver::Version;

use super::super::UpdaterBuilder;
use super::{RecordingListener, ReleaseServerState, spawn_release_server};
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::chunker::ChunkerConfig;
use crate::installer::{InstallContext, UpdateInstaller};

type InstalledRecords = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

#[cfg(feature = "native-installer")]
#[tokio::test]
async fn downloads_and_hands_off_verified_apk_to_android_installer() {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use crate::installer::{AndroidApkLauncher, PlatformInstaller};

    let state = ReleaseServerState::default();
    let base_url = spawn_release_server(state.clone()).await;

    let mut apk_buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut apk_buf);
        writer
            .start_file("AndroidManifest.xml", SimpleFileOptions::default())
            .expect("start entry");
        writer.write_all(b"apk-v2-payload").expect("write entry");
        writer.finish().expect("finish");
    }
    let apk_bytes = apk_buf.into_inner();
    let apk_art =
        BlockMap::generate(apk_bytes.as_slice(), "file", ChunkerConfig::default()).expect("map");

    let manifest = serde_json::json!({
        "version": "0.2.0",
        "notes": "Android APK update",
        "pub_date": "2026-11-24T02:00:00Z",
        "platforms": {
            "android-universal": {
                "url": base_url.join("fastnet_0.2.0_universal.apk").expect("url"),
                "sha512": apk_art.sha512,
                "size": apk_art.size,
            }
        }
    });
    {
        let mut files = state.files.lock().expect("lock");
        files.insert(
            "latest.json".into(),
            serde_json::to_vec(&manifest).expect("json"),
        );
        files.insert("fastnet_0.2.0_universal.apk".into(), apk_bytes.clone());
        files.insert(
            "fastnet_0.2.0_universal.apk.blockmap".into(),
            apk_art.blockmap.to_gzip_bytes().expect("gz"),
        );
    }

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);
    let launched_path = Arc::new(Mutex::new(None));
    let launched_clone = Arc::clone(&launched_path);
    let launcher = AndroidApkLauncher::new(move |path| {
        *launched_clone.lock().expect("lock") = Some(path.to_path_buf());
        Ok(())
    });

    let updater = UpdaterBuilder::new(
        "fastnet".into(),
        Version::new(0, 1, 0),
        temp.path().join("cache"),
        cache,
    )
    .target("android-universal")
    .endpoints(vec![base_url.join("latest.json").expect("url")])
    .installer(Arc::new(PlatformInstaller::android(launcher)))
    .build()
    .expect("updater");

    let update = updater.check().await.expect("check").expect("update");
    assert_eq!(update.version, "0.2.0");
    let listener = RecordingListener::default();
    let summary = update
        .download_and_install(&listener)
        .await
        .expect("download and install apk");
    assert_eq!(summary.total_bytes, apk_bytes.len() as u64);
    let installed_path = launched_path
        .lock()
        .expect("lock")
        .clone()
        .expect("android launcher called");
    assert_eq!(std::fs::read(&installed_path).expect("read"), apk_bytes);
}

#[tokio::test]
async fn custom_update_installer_overrides_default_platform_backend() {
    #[derive(Debug)]
    struct RecordingInstaller {
        installed: InstalledRecords,
    }

    impl UpdateInstaller for RecordingInstaller {
        fn install(&self, package_path: &Path, context: &InstallContext) -> crate::Result<()> {
            let bytes = std::fs::read(package_path)?;
            self.installed
                .lock()
                .expect("lock")
                .push((context.version.clone(), bytes));
            Ok(())
        }
    }

    let state = ReleaseServerState::default();
    let base_url = spawn_release_server(state.clone()).await;

    let bundle_bytes = b"hotfix-lua-or-web-bundle-v2".to_vec();
    let art =
        BlockMap::generate(bundle_bytes.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "platforms": {
            "lua-bundle": {
                "url": base_url.join("bundle-0.2.0.zip").expect("url"),
                "sha512": art.sha512,
                "size": art.size,
            }
        }
    });
    {
        let mut files = state.files.lock().expect("lock");
        files.insert(
            "latest.json".into(),
            serde_json::to_vec(&manifest).expect("json"),
        );
        files.insert("bundle-0.2.0.zip".into(), bundle_bytes.clone());
        files.insert(
            "bundle-0.2.0.zip.blockmap".into(),
            art.blockmap.to_gzip_bytes().expect("gz"),
        );
    }

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);
    let installed = Arc::new(Mutex::new(Vec::new()));
    let custom = Arc::new(RecordingInstaller {
        installed: Arc::clone(&installed),
    });

    let updater = UpdaterBuilder::new(
        "scripts".into(),
        Version::new(0, 1, 0),
        temp.path().join("scripts"),
        cache,
    )
    .target("lua-bundle")
    .endpoints(vec![base_url.join("latest.json").expect("url")])
    .installer(custom)
    .build()
    .expect("updater");

    let update = updater.check().await.expect("check").expect("update");
    let listener = RecordingListener::default();
    update
        .download_and_install(&listener)
        .await
        .expect("download and install via custom installer");

    assert_eq!(
        installed.lock().expect("lock").as_slice(),
        &[("0.2.0".to_string(), bundle_bytes)]
    );
}
