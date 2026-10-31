pub mod saves;

use crate::external::{Asset, Emulator, Running};
use std::path::{Path, PathBuf};

pub const PLATFORM: &str = "switch";
const VERSION: &str = "v0.2.1";

// shortcut: Forgejo has no asset digests; update these verified pins when adopting a newer stable release.
fn download() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "Eden-Linux-v0.2.1-legacy-gcc-standard.AppImage",
            "2003a408753b9663a3cb72080085df7fe5f09b7825f542c1a01ad123bdb7c8e2",
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some((
            "Eden-Linux-v0.2.1-aarch64-gcc-standard.AppImage",
            "494014f67401bbbe7d48da7128d769734cf97863bfb2349d48cd2a118173e769",
        ))
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some((
            "Eden-Windows-v0.2.1-amd64-msvc-standard.zip",
            "ff498e5da9630216926ac3cbe9fb493b14930665c728a40a8f5b59507fdd7ebf",
        ))
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some((
            "Eden-Windows-v0.2.1-arm64-clang-standard.zip",
            "7df16b64bb0a0225acddcb498e524aa7854c24708022ecbea599cc1aebf010dc",
        ))
    } else {
        None
    }
}

pub fn available() -> bool {
    download().is_some()
}

pub fn emulator(dir: PathBuf) -> Emulator {
    Emulator {
        name: "Eden",
        dir,
        pick: |_| None,
        exe_in: |dir, asset| dir.join(if cfg!(windows) { "eden.exe" } else { asset }),
    }
}

pub async fn install(emulator: &Emulator, http: &reqwest::Client) -> Result<PathBuf, String> {
    let (name, hash) =
        download().ok_or_else(|| "Eden has no download for this computer".to_string())?;
    emulator
        .install_asset(
            http,
            VERSION,
            &Asset {
                name: name.into(),
                browser_download_url: format!("https://stable.eden-emu.dev/{VERSION}/{name}"),
                digest: Some(format!("sha256:{hash}")),
            },
        )
        .await
}

pub fn missing(source: &Path) -> Vec<String> {
    [
        (!source.join("prod.keys").is_file(), "prod.keys"),
        (matches!(find_firmware(source), Ok(None)), "firmware.zip"),
    ]
    .into_iter()
    .filter(|(absent, _)| *absent)
    .map(|(_, name)| name.to_string())
    .collect()
}

pub fn is_firmware_archive(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "firmware.zip"
        || name
            .strip_prefix("firmware.")
            .and_then(|version| version.strip_suffix(".zip"))
            .is_some_and(|version| {
                version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            })
}

fn find_firmware(source: &Path) -> Result<Option<PathBuf>, String> {
    let canonical = source.join("firmware.zip");
    if canonical.is_file() {
        return Ok(Some(canonical));
    }
    let Ok(entries) = std::fs::read_dir(source) else {
        return Ok(None);
    };
    let mut archives = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_file() && is_firmware_archive(&name) {
            if name.eq_ignore_ascii_case("firmware.zip") {
                return Ok(Some(path));
            }
            archives.push(path);
        }
    }
    match archives.len() {
        0 => Ok(None),
        1 => Ok(archives.pop()),
        _ => Err(tr::tr!(
            "Several Switch firmware archives are available in {}. Rename the one to use to firmware.zip in that folder.",
            source.display()
        )),
    }
}

fn valid_keys(text: &str) -> bool {
    let mut count = 0;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let Some((name, value)) = line.split_once('=') else {
            return false;
        };
        let value = value.trim();
        if name.trim().is_empty()
            || value.is_empty()
            || value.len() % 2 != 0
            || !value.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return false;
        }
        count += 1;
    }
    count > 0
}

/// Portable mode keeps Eden's keys, firmware and saves separate from any existing Eden installation.
pub fn prepare(exe: &Path, source: &Path) -> Result<(), String> {
    let home = exe
        .parent()
        .ok_or("Eden has no parent directory")?
        .join("user");
    let keys = home.join("keys");
    std::fs::create_dir_all(&keys).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let mut imports = Vec::new();
    for name in ["prod.keys", "title.keys"] {
        let path = source.join(name);
        if name == "title.keys" && !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        if !valid_keys(&text) {
            return Err(format!("{name} is not a valid key file"));
        }
        imports.push((name, text));
    }
    let firmware = find_firmware(source)?.ok_or("Switch firmware.zip is missing")?;
    tracing::info!(firmware_name = ?firmware.file_name(), "Switch firmware selected");
    install_firmware(&firmware, &home)
        .map_err(|e| format!("Could not install Switch firmware: {e}"))?;
    for (name, text) in imports {
        let path = keys.join(name);
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, text).map_err(|e| e.to_string())?;
        std::fs::rename(temp, path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn install_firmware(archive: &Path, home: &Path) -> Result<(), String> {
    if std::fs::metadata(archive).map_err(|e| e.to_string())?.len() > 1024 * 1024 * 1024 {
        return Err("Firmware archive is too large".into());
    }
    let bytes = std::fs::read(archive).map_err(|e| e.to_string())?;
    let hash = crate::cores::sha256_hex(&bytes);
    let content = home.join("nand/system/Contents");
    let registered = content.join("registered");
    let stamp = home.join("firmware.sha256");
    let firmware_name = archive.file_name().unwrap_or_default().to_string_lossy();
    if registered.is_dir() && std::fs::read_to_string(&stamp).ok().as_deref() == Some(&hash) {
        tracing::info!(%firmware_name, "Switch firmware already installed");
        return Ok(());
    }
    let stage = content.join("romp-firmware");
    if stage.exists() {
        std::fs::remove_dir_all(&stage).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    unpack_firmware(&bytes, &stage)?;
    let backup = content.join("romp-firmware-backup");
    // Never discard a backup left by an interrupted installation.
    if backup.exists() {
        return Err("A firmware backup already exists; restore it before retrying".into());
    }
    let had_old = registered.exists();
    if had_old {
        std::fs::rename(&registered, &backup).map_err(|e| e.to_string())?;
    }
    if let Err(e) = std::fs::rename(&stage, &registered) {
        if had_old {
            let _ = std::fs::rename(&backup, &registered);
        }
        return Err(e.to_string());
    }
    if had_old {
        std::fs::remove_dir_all(&backup).map_err(|e| e.to_string())?;
    }
    std::fs::write(stamp, hash).map_err(|e| e.to_string())?;
    tracing::info!(%firmware_name, "Switch firmware installed");
    Ok(())
}

fn unpack_firmware(bytes: &[u8], stage: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut count = 0;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let path = entry
            .enclosed_name()
            .ok_or("Unsafe firmware archive path")?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid firmware filename")?;
        let Some(id) = name.strip_suffix(".nca") else {
            continue;
        };
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid firmware NCA filename".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Firmware too large")?;
        if entry.size() == 0 || total > 1024 * 1024 * 1024 {
            return Err("Invalid firmware size".into());
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.join(name))
            .map_err(|e| e.to_string())?;
        let size = entry.size();
        let copied = std::io::copy(&mut std::io::Read::take(entry, size + 1), &mut out)
            .map_err(|e| e.to_string())?;
        if copied != size {
            return Err("Invalid firmware entry size".into());
        }
        count += 1;
    }
    if count == 0 {
        return Err("The firmware zip contains no NCA files".into());
    }
    Ok(())
}

pub fn launch(
    exe: &Path,
    rom: &Path,
    fullscreen: bool,
    on_exit: impl FnOnce(Option<i32>, Vec<String>) + Send + 'static,
) -> std::io::Result<Running> {
    let mut command = std::process::Command::new(exe);
    command.current_dir(exe.parent().expect("installed executable has a parent"));
    #[cfg(target_os = "linux")]
    command.env("APPIMAGE_EXTRACT_AND_RUN", "1");
    if fullscreen {
        command.arg("-f");
    }
    command.arg("-g").arg(rom);
    crate::external::launch(command, "Eden", on_exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn firmware(name: &str, data: &[u8]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn prepares_a_private_portable_install_and_optional_title_keys() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let exe = dir.path().join("eden");
        std::fs::write(source.join("prod.keys"), "master_key_00 = 0123abcd").unwrap();
        std::fs::write(source.join("title.keys"), "abcd = 0123abcd").unwrap();
        std::fs::write(
            source.join("firmware.zip"),
            firmware("0123456789abcdef0123456789abcdef.nca", b"nca"),
        )
        .unwrap();
        prepare(&exe, &source).unwrap();
        let home = dir.path().join("user");
        assert_eq!(
            std::fs::read_to_string(home.join("keys/prod.keys")).unwrap(),
            "master_key_00 = 0123abcd"
        );
        assert!(home.join("keys/title.keys").is_file());
        std::fs::create_dir_all(home.join("nand/user/save")).unwrap();
        std::fs::write(home.join("nand/user/save/game"), b"save").unwrap();
        prepare(&exe, &source).unwrap();
        assert_eq!(
            std::fs::read(home.join("nand/user/save/game")).unwrap(),
            b"save"
        );
    }

    #[cfg(unix)]
    #[test]
    fn launches_the_game_in_portable_mode_with_fullscreen() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("eden");
        std::fs::write(&exe, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > args\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _running = launch(&exe, Path::new("/roms/a game.nsp"), true, move |code, _| {
            tx.send(code).unwrap();
        })
        .unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            Some(0)
        );
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [dir.path().to_str().unwrap(), "-f", "-g", "/roms/a game.nsp"]
        );
    }

    #[test]
    fn validates_keys_and_requires_firmware() {
        assert!(valid_keys("# comment\nmaster_key_00 = 0123abcd\n"));
        for bad in ["", "not keys", "key = zz", "key = 123", " = ab"] {
            assert!(!valid_keys(bad));
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(missing(dir.path()), ["prod.keys", "firmware.zip"]);
    }

    #[test]
    fn imports_a_versioned_firmware_dump_and_reports_ambiguous_versions() {
        for accepted in ["firmware.zip", "FIRMWARE.ZIP", "Firmware.18.0.1.zip"] {
            assert!(is_firmware_archive(accepted));
        }
        for rejected in [
            "unrelated.zip",
            "firmware.backup.zip",
            "Firmware..zip",
            "firmware.18.zip.part",
        ] {
            assert!(!is_firmware_archive(rejected));
        }
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("prod.keys"), "master_key_00 = 0123abcd").unwrap();
        std::fs::write(source.join("unrelated.zip"), b"unrelated").unwrap();
        assert_eq!(missing(&source), ["firmware.zip"]);
        let name = "0123456789abcdef0123456789abcdef.nca";
        let archive = firmware(name, b"versioned");
        std::fs::write(source.join("Firmware.18.0.1.zip"), &archive).unwrap();
        assert!(missing(&source).is_empty());
        let exe = dir.path().join("eden");
        prepare(&exe, &source).unwrap();
        let installed = dir
            .path()
            .join("user/nand/system/Contents/registered")
            .join(name);
        assert_eq!(std::fs::read(&installed).unwrap(), b"versioned");
        std::fs::write(source.join("Firmware.19.0.0.zip"), &archive).unwrap();
        assert!(prepare(&exe, &source).unwrap_err().contains("firmware.zip"));
        assert_eq!(std::fs::read(&installed).unwrap(), b"versioned");
        std::fs::write(source.join("firmware.zip"), firmware(name, b"selected")).unwrap();
        prepare(&exe, &source).unwrap();
        assert_eq!(std::fs::read(installed).unwrap(), b"selected");
    }

    #[test]
    fn imports_firmware_without_destroying_the_previous_install_on_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("user");
        let archive = dir.path().join("firmware.zip");
        let name = "0123456789abcdef0123456789abcdef.nca";
        std::fs::write(&archive, firmware(&format!("firmware/{name}"), b"nca")).unwrap();
        install_firmware(&archive, &home).unwrap();
        let installed = home.join("nand/system/Contents/registered").join(name);
        assert_eq!(std::fs::read(&installed).unwrap(), b"nca");
        install_firmware(&archive, &home).unwrap();
        for bad in [
            "readme.txt",
            "../0123456789abcdef0123456789abcdef.nca",
            "bad.nca",
        ] {
            std::fs::write(&archive, firmware(bad, b"bad")).unwrap();
            assert!(install_firmware(&archive, &home).is_err());
            assert_eq!(std::fs::read(&installed).unwrap(), b"nca");
        }
    }
}
