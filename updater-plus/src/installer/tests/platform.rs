use std::path::{Path, PathBuf};

use crate::installer::{InstallContext, UnsupportedInstaller, UpdateInstaller};

#[test]
fn unsupported_installer_returns_unsupported_platform() {
    let ctx = InstallContext {
        app_name: "myapp".into(),
        version: "0.2.0".into(),
        extract_path: PathBuf::from("/tmp/myapp"),
        bundle_type: None,
        restart_after_install: false,
        windows: None,
    };
    assert!(matches!(
        UnsupportedInstaller.install(Path::new("/tmp/pkg"), &ctx),
        Err(crate::Error::UnsupportedPlatform)
    ));
}

#[cfg(feature = "native-installer")]
#[test]
fn android_installer_validates_apk_zip_magic_before_launching() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use crate::installer::{AndroidApkLauncher, AndroidInstaller, PlatformInstaller};

    let dir = tempfile::tempdir().expect("temp dir");
    let valid_apk = dir.path().join("fastnet.apk");
    {
        let file = std::fs::File::create(&valid_apk).expect("create apk");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("AndroidManifest.xml", SimpleFileOptions::default())
            .expect("start entry");
        writer.write_all(b"manifest-bytes").expect("write entry");
        writer.finish().expect("finish zip");
    }

    let launched = Arc::new(Mutex::new(Vec::new()));
    let launched_clone = Arc::clone(&launched);
    let launcher = AndroidApkLauncher::new(move |path| {
        launched_clone
            .lock()
            .expect("lock")
            .push(path.to_path_buf());
        Ok(())
    });
    let ctx = InstallContext {
        app_name: "fastnet".into(),
        version: "0.2.0".into(),
        extract_path: PathBuf::from("/tmp"),
        bundle_type: None,
        restart_after_install: false,
        windows: None,
    };

    let installer = PlatformInstaller::android(launcher.clone());
    installer
        .install(&valid_apk, &ctx)
        .expect("valid apk installs");
    assert_eq!(launched.lock().expect("lock").as_slice(), &[valid_apk]);

    let invalid_apk = dir.path().join("not-an-apk.bin");
    std::fs::write(&invalid_apk, b"plain-text-not-a-zip").expect("write invalid");
    let err = AndroidInstaller::new(launcher)
        .install(&invalid_apk, &ctx)
        .expect_err("non-zip must fail");
    assert!(matches!(
        err,
        crate::Error::InvalidPackageFormat(ref msg) if msg.contains(".apk")
    ));
    assert_eq!(
        launched.lock().expect("lock").len(),
        1,
        "invalid package must not invoke Android launcher"
    );
}
