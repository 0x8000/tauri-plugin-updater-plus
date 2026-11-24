#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
use flate2::Compression;
#[cfg(target_os = "linux")]
use flate2::write::GzEncoder;
#[cfg(target_os = "linux")]
use zip::ZipWriter;
#[cfg(target_os = "linux")]
use zip::write::SimpleFileOptions;

#[cfg(target_os = "linux")]
use super::linux::LinuxInstaller;
#[cfg(target_os = "linux")]
use super::{InstallContext, PlatformInstaller};
#[cfg(target_os = "linux")]
use crate::Error;

mod authenticode;
mod codesign;

#[cfg(target_os = "linux")]
fn sorted_dir_entries(dir: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();
    entries
}

#[cfg(target_os = "linux")]
#[test]
fn replaces_appimage_atomically_preserving_custom_executable_mode_and_open_handle() {
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("MyApp.AppImage");
    std::fs::write(&target, b"old-appimage-bytes").expect("write old");
    // Use 0o750 (distinct from the 0o755 fallback) to verify target mode preservation.
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let mut old_handle = std::fs::File::open(&target).expect("open old target");
    let old_ino = std::fs::metadata(&target).expect("old meta").ino();

    let new_pkg = dir.path().join("new.AppImage");
    std::fs::write(&new_pkg, b"new-appimage-bytes-v2").expect("write new");
    std::fs::set_permissions(&new_pkg, std::fs::Permissions::from_mode(0o644)).expect("chmod");

    let ctx = InstallContext {
        app_name: "myapp".into(),
        version: "0.2.0".into(),
        extract_path: target.clone(),
        restart_after_install: true,
        windows: None,
    };
    PlatformInstaller::install(&new_pkg, &ctx).expect("install raw AppImage");
    assert_eq!(
        std::fs::read(&target).expect("read"),
        b"new-appimage-bytes-v2"
    );
    let v2_ino = std::fs::metadata(&target).expect("v2 meta").ino();
    assert_ne!(v2_ino, old_ino, "atomic rename must replace target inode");
    assert_eq!(
        std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );

    // Open descriptor to the running binary still sees old unlinked inode bytes.
    let mut old_bytes = Vec::new();
    old_handle.read_to_end(&mut old_bytes).expect("read old fd");
    assert_eq!(old_bytes, b"old-appimage-bytes");

    // Cached source package stays intact (both bytes and non-executable mode).
    assert_eq!(
        std::fs::read(&new_pkg).expect("read cached source"),
        b"new-appimage-bytes-v2"
    );
    assert_eq!(
        std::fs::metadata(&new_pkg)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert_eq!(
        sorted_dir_entries(dir.path()),
        vec!["MyApp.AppImage", "new.AppImage"]
    );

    // Non-executable target gets upgraded to 0o755 and replaced with distinct v3 bytes.
    let v3_pkg = dir.path().join("v3.AppImage");
    std::fs::write(&v3_pkg, b"new-appimage-bytes-v3").expect("write v3");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod 644");
    PlatformInstaller::install(&v3_pkg, &ctx).expect("install onto 0644 target");
    assert_eq!(
        std::fs::read(&target).expect("read v3"),
        b"new-appimage-bytes-v3"
    );
    assert_ne!(std::fs::metadata(&target).expect("v3 meta").ino(), v2_ino);
    assert_eq!(
        std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(target_os = "linux")]
#[test]
fn extracts_appimage_from_targz_skips_directories_and_cleans_stage_on_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("MyApp.AppImage");
    std::fs::write(&target, b"original-v1").expect("write old");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let archive_path = dir.path().join("v2.AppImage.tar.gz");
    {
        let file = std::fs::File::create(&archive_path).expect("create tar.gz");
        let encoder = GzEncoder::new(file, Compression::fast());
        let mut builder = tar::Builder::new(encoder);

        // Directory named `fake.AppImage/` must be ignored in favor of the regular file.
        let mut dir_header = tar::Header::new_gnu();
        dir_header.set_entry_type(tar::EntryType::Directory);
        dir_header.set_size(0);
        dir_header.set_mode(0o755);
        dir_header.set_cksum();
        builder
            .append_data(&mut dir_header, "fake.AppImage/", std::io::empty())
            .expect("append dir");

        let payload = b"extracted-v2-appimage";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "bundle/MyApp.AppImage", payload.as_slice())
            .expect("append file");
        builder.into_inner().expect("tar").finish().expect("gz");
    }

    let header = std::fs::read(&archive_path).expect("read header");
    LinuxInstaller::install_appimage(&archive_path, &header[..64], &target)
        .expect("install tar.gz");
    assert_eq!(
        std::fs::read(&target).expect("read"),
        b"extracted-v2-appimage"
    );
    assert_eq!(
        std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    let v2_ino = std::fs::metadata(&target).expect("meta").ino();

    let no_binary_path = dir.path().join("no-binary.tar.gz");
    {
        let file = std::fs::File::create(&no_binary_path).expect("create tar.gz");
        let encoder = GzEncoder::new(file, Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let payload = b"not an appimage";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "README.txt", payload.as_slice())
            .expect("append");
        builder.into_inner().expect("tar").finish().expect("gz");
    }
    let no_bin_header = std::fs::read(&no_binary_path).expect("header");
    let err = LinuxInstaller::install_appimage(&no_binary_path, &no_bin_header, &target)
        .expect_err("missing AppImage entry must fail");
    assert!(matches!(
        err,
        Error::InvalidPackageFormat(ref msg) if msg.contains("AppImage binary not found in tar.gz archive")
    ));
    assert_eq!(std::fs::metadata(&target).expect("meta").ino(), v2_ino);

    let corrupt_path = dir.path().join("corrupt.tar.gz");
    {
        let file = std::fs::File::create(&corrupt_path).expect("create broken");
        let mut encoder = GzEncoder::new(file, Compression::fast());
        encoder.write_all(b"not a tar archive").expect("write");
        encoder.finish().expect("finish");
    }
    let corrupt_header = std::fs::read(&corrupt_path).expect("header");
    let corrupt_err = LinuxInstaller::install_appimage(&corrupt_path, &corrupt_header, &target)
        .expect_err("corrupt tar must fail");
    assert!(
        matches!(
            corrupt_err,
            Error::Io(ref io_err)
                if !matches!(
                    io_err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                )
        ),
        "expected archive decode I/O error, got {corrupt_err:?}"
    );
    assert_eq!(
        std::fs::read(&target).expect("untouched"),
        b"extracted-v2-appimage"
    );
    assert_eq!(std::fs::metadata(&target).expect("meta").ino(), v2_ino);
    assert_eq!(
        sorted_dir_entries(dir.path()),
        vec![
            "MyApp.AppImage",
            "corrupt.tar.gz",
            "no-binary.tar.gz",
            "v2.AppImage.tar.gz"
        ]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn extracts_appimage_from_zip_handles_corrupt_zip_missing_target_and_skips_dirs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("MyApp.AppImage");
    std::fs::write(&target, b"original-v1").expect("write old");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let zip_path = dir.path().join("v2.zip");
    {
        let file = std::fs::File::create(&zip_path).expect("create zip");
        let mut writer = ZipWriter::new(file);
        writer
            .add_directory("fake.AppImage/", SimpleFileOptions::default())
            .expect("add dir");
        writer
            .start_file("bundle/MyApp.AppImage", SimpleFileOptions::default())
            .expect("start file");
        writer.write_all(b"zipped-v2-appimage").expect("write");
        writer.finish().expect("finish");
    }
    let zip_header = std::fs::read(&zip_path).expect("header");
    LinuxInstaller::install_appimage(&zip_path, &zip_header[..64], &target).expect("install zip");
    assert_eq!(std::fs::read(&target).expect("read"), b"zipped-v2-appimage");
    assert_eq!(
        std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    let v2_ino = std::fs::metadata(&target).expect("meta").ino();

    let empty_zip = dir.path().join("no-binary.zip");
    {
        let file = std::fs::File::create(&empty_zip).expect("create zip");
        let mut writer = ZipWriter::new(file);
        writer
            .add_directory("only-dir.AppImage/", SimpleFileOptions::default())
            .expect("add dir");
        writer
            .start_file("README.md", SimpleFileOptions::default())
            .expect("start");
        writer.write_all(b"docs").expect("write");
        writer.finish().expect("finish");
    }
    let empty_header = std::fs::read(&empty_zip).expect("header");
    let err = LinuxInstaller::install_appimage(&empty_zip, &empty_header, &target)
        .expect_err("zip without regular AppImage file must fail");
    assert!(matches!(
        err,
        Error::InvalidPackageFormat(ref msg) if msg.contains("AppImage binary not found in zip archive")
    ));

    let corrupt_zip = dir.path().join("corrupt.zip");
    std::fs::write(&corrupt_zip, b"PK\x03\x04not-a-valid-zip-body").expect("write corrupt zip");
    let corrupt_header = std::fs::read(&corrupt_zip).expect("header");
    let zip_err = LinuxInstaller::install_appimage(&corrupt_zip, &corrupt_header, &target)
        .expect_err("corrupt zip must fail");
    assert!(matches!(zip_err, Error::Zip(_)), "got {zip_err:?}");

    assert_eq!(
        std::fs::read(&target).expect("untouched"),
        b"zipped-v2-appimage"
    );
    assert_eq!(std::fs::metadata(&target).expect("meta").ino(), v2_ino);
    assert_eq!(
        sorted_dir_entries(dir.path()),
        vec!["MyApp.AppImage", "corrupt.zip", "no-binary.zip", "v2.zip"]
    );

    let missing_target = dir.path().join("does-not-exist.AppImage");
    let missing_err =
        LinuxInstaller::install_appimage(&zip_path, &zip_header[..64], &missing_target)
            .expect_err("missing target must fail");
    assert!(matches!(
        missing_err,
        Error::Io(ref io_err) if io_err.kind() == std::io::ErrorKind::NotFound
    ));
}
