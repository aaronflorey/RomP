use std::io::{self, Read};
use std::path::{Component, Path};

pub const FILE: &str = "eden.zip";
const NAND: &str = "eden-nand";
const STAMP: &str = "eden-tree.hash";
// shortcut: sync caps saves at 1 GiB; stream archives if real games exceed it.
const LIMIT: u64 = 1024 * 1024 * 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}

fn files(root: &Path, relative: &Path, out: &mut Vec<std::path::PathBuf>) -> io::Result<()> {
    let path = root.join(relative);
    if !path.exists() {
        return Ok(());
    }
    let meta = path.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        return Err(invalid("Save directories must not contain symbolic links"));
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            files(root, &relative.join(entry?.file_name()), out)?;
        }
    } else if meta.is_file() {
        out.push(relative.to_path_buf());
    } else {
        return Err(invalid("Unsupported save file type"));
    }
    Ok(())
}

/// The archive is the sync file; NAND remains the working copy, including after a crash.
pub fn capture(dir: &Path) -> io::Result<()> {
    let nand = dir.join(NAND);
    let mut entries = Vec::new();
    for root in ["user/save", "system/save"] {
        files(&nand, Path::new(root), &mut entries)?;
    }
    if entries.is_empty() {
        return Ok(());
    }
    entries.sort();
    let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    let mut total = 0u64;
    for relative in entries {
        let path = nand.join(&relative);
        let size = path.metadata()?.len();
        total = total
            .checked_add(size)
            .ok_or_else(|| invalid("Save too large"))?;
        if total > LIMIT {
            return Err(invalid("Eden saves exceed the 1 GiB sync limit"));
        }
        zip.start_file(
            relative.to_string_lossy().replace('\\', "/"),
            zip::write::SimpleFileOptions::default(),
        )?;
        if io::copy(&mut std::fs::File::open(path)?.take(size + 1), &mut zip)? != size {
            return Err(invalid("Save changed while packing"));
        }
    }
    let bytes = zip.finish()?.into_inner();
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("Save archive too large"));
    }
    let hash = crate::saves::content_hash(&bytes);
    // A server download may be newer than this unchanged working copy.
    if std::fs::read_to_string(dir.join(STAMP)).ok().as_deref() == Some(&hash)
        && dir.join(FILE).is_file()
    {
        return Ok(());
    }
    crate::saves::replace_file(&dir.join(FILE), &bytes)?;
    crate::saves::replace_file(&dir.join(STAMP), hash.as_bytes())
}

fn unpack(bytes: &[u8], stage: &Path) -> io::Result<()> {
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes))?;
    let mut total = 0u64;
    let mut count = 0;
    if zip.len() > 100_000 {
        return Err(invalid("Too many save files"));
    }
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.name_raw().contains(&b'\\') {
            return Err(invalid("Unsafe save path"));
        }
        let path = entry
            .enclosed_name()
            .ok_or_else(|| invalid("Unsafe save path"))?;
        let name = path.to_string_lossy();
        if !path.components().all(|c| matches!(c, Component::Normal(_)))
            || name.contains(['\\', ':'])
            || !(path.starts_with("user/save") || path.starts_with("system/save"))
            || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err(invalid("The Eden save archive contains an unsafe entry"));
        }
        if entry.is_dir() {
            std::fs::create_dir_all(stage.join(path))?;
            continue;
        }
        if path == Path::new("user/save") || path == Path::new("system/save") {
            return Err(invalid("Invalid save file path"));
        }
        count += 1;
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| invalid("Save too large"))?;
        if total > LIMIT {
            return Err(invalid("Eden saves exceed the 1 GiB sync limit"));
        }
        let target = stage.join(&path);
        std::fs::create_dir_all(
            target
                .parent()
                .ok_or_else(|| invalid("Invalid save path"))?,
        )?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let size = entry.size();
        if io::copy(&mut entry.take(size + 1), &mut out)? != size {
            return Err(invalid("Invalid save size"));
        }
    }
    if count == 0 {
        return Err(invalid("Empty Eden save archive"));
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path, link: bool) -> io::Result<()> {
    let mut entries = Vec::new();
    files(source, Path::new(""), &mut entries)?;
    for relative in entries {
        let dest = target.join(&relative);
        std::fs::create_dir_all(
            dest.parent()
                .ok_or_else(|| invalid("Invalid destination"))?,
        )?;
        if !dest.exists() && (!link || std::fs::hard_link(source.join(&relative), &dest).is_err()) {
            std::fs::copy(source.join(relative), dest)?;
        }
    }
    Ok(())
}

const ACCOUNT: &str = "system/save/8000000000000010";

pub fn capture_after_play(exe: &Path, dir: &Path) -> io::Result<()> {
    // Eden constructs its account manager before reading the configured NAND path.
    if dir.join(NAND).join("user/save").exists() {
        let portable = exe
            .parent()
            .ok_or_else(|| invalid("Missing Eden directory"))?
            .join("user/nand");
        copy_tree(
            &portable.join(ACCOUNT),
            &dir.join(NAND).join(ACCOUNT),
            false,
        )?;
    }
    capture(dir)
}

/// Only saves and profiles travel to RomM, never firmware, keys or other games' NAND.
pub fn prepare(exe: &Path, dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let portable = exe
        .parent()
        .ok_or_else(|| invalid("Missing Eden directory"))?
        .join("user");
    let nand = dir.join(NAND);
    let archive = dir.join(FILE);
    if archive.exists() {
        if archive.metadata()?.len() > LIMIT {
            return Err(invalid("Save archive too large"));
        }
        let bytes = std::fs::read(&archive)?;
        let stage = dir.join("eden-restore");
        let previous = dir.join("eden-nand.previous");
        if previous.exists() {
            return Err(invalid(
                "An Eden NAND backup needs recovery before restoring",
            ));
        }
        if stage.exists() {
            std::fs::remove_dir_all(&stage)?;
        }
        std::fs::create_dir(&stage)?;
        unpack(&bytes, &stage)?;
        copy_tree(
            &portable.join("nand/system/Contents/registered"),
            &stage.join("system/Contents/registered"),
            true,
        )?;
        let had_old = nand.exists();
        if had_old {
            std::fs::rename(&nand, &previous)?;
        }
        if let Err(e) = std::fs::rename(&stage, &nand) {
            if had_old {
                let _ = std::fs::rename(&previous, &nand);
            }
            return Err(e);
        }
        crate::saves::replace_file(
            &dir.join(STAMP),
            crate::saves::content_hash(&bytes).as_bytes(),
        )?;
        if had_old {
            std::fs::remove_dir_all(previous)?;
        }
    } else {
        copy_tree(
            &portable.join("nand").join(ACCOUNT),
            &nand.join(ACCOUNT),
            false,
        )?;
        copy_tree(
            &portable.join("nand/system/Contents/registered"),
            &nand.join("system/Contents/registered"),
            true,
        )?;
    }
    // Seed the early account manager with this game's UUIDs, keeping the original profiles.
    let accounts = nand.join(ACCOUNT);
    if accounts.exists() {
        let early = portable.join("nand").join(ACCOUNT);
        let backup = early.with_extension("romp-backup");
        if early.exists() {
            if !backup.exists() {
                std::fs::rename(&early, &backup)?;
            } else {
                std::fs::remove_dir_all(&early)?;
            }
        }
        copy_tree(&accounts, &early, false)?;
    }
    let config = portable.join("config/qt-config.ini");
    let existing = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    crate::saves::replace_file(&config, config_text(&existing, &nand)?.as_bytes())
}

fn config_text(existing: &str, nand: &Path) -> io::Result<String> {
    let mut result = String::new();
    let mut settings = String::new();
    let mut storage = false;
    for line in existing.lines() {
        if line.trim_start().starts_with('[') {
            storage = matches!(line.trim(), "[Data%20Storage]" | "[Data Storage]");
            if storage {
                continue;
            }
        }
        if storage {
            let key = line
                .split_once('=')
                .map(|(key, _)| key.trim())
                .unwrap_or("");
            if [
                "nand_directory",
                "nand_directory\\default",
                "save_directory",
                "save_directory\\default",
            ]
            .contains(&key)
            {
                continue;
            }
            settings.push_str(line);
            settings.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    let path = nand.to_string_lossy().replace('\\', "/");
    if path.contains(['"', '\n', '\r']) {
        return Err(invalid(
            "Eden save paths cannot contain quotes or line breaks",
        ));
    }
    Ok(format!(
        "{}\n\n[Data%20Storage]\n{}\nnand_directory=\"{path}\"\nsave_directory=\"{path}\"\n",
        result.trim_end(),
        settings.trim_end()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn archive(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
        let mut bytes = zip.finish().unwrap().into_inner();
        if name.contains('\\') {
            let normalized = name.replace('\\', "/");
            for i in 0..=bytes.len() - name.len() {
                if &bytes[i..i + name.len()] == normalized.as_bytes() {
                    bytes[i..i + name.len()].copy_from_slice(name.as_bytes());
                }
            }
        }
        bytes
    }

    #[test]
    fn round_trip_is_per_game_and_excludes_firmware_and_keys() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("installed/eden");
        let portable = exe.parent().unwrap().join("user");
        write(&portable.join("keys/prod.keys"), b"secret");
        write(
            &portable.join("nand/system/Contents/registered/fw.nca"),
            b"firmware",
        );
        write(
            &portable.join("nand/system/save/8000000000000010/profiles.dat"),
            b"profile",
        );
        write(
            &portable.join("nand/user/save/legacy-other-game"),
            b"legacy",
        );
        let game = temp.path().join("game1");
        prepare(&exe, &game).unwrap();
        assert!(!game.join(NAND).join("user/save/legacy-other-game").exists());
        write(
            &game.join(NAND).join("user/save/account/title/progress"),
            b"progress",
        );
        write(
            &game.join(NAND).join("user/Contents/registered/game.nca"),
            b"installed game, not a save",
        );
        capture(&game).unwrap();
        let bytes = std::fs::read(game.join(FILE)).unwrap();
        let zip = zip::ZipArchive::new(io::Cursor::new(&bytes)).unwrap();
        assert_eq!(zip.len(), 2);
        let other = temp.path().join("game2");
        write(&other.join(FILE), &bytes);
        prepare(&exe, &other).unwrap();
        assert_eq!(
            std::fs::read(other.join(NAND).join("user/save/account/title/progress")).unwrap(),
            b"progress"
        );
        assert_eq!(
            std::fs::read(
                other
                    .join(NAND)
                    .join("system/save/8000000000000010/profiles.dat")
            )
            .unwrap(),
            b"profile"
        );
        assert!(other
            .join(NAND)
            .join("system/Contents/registered/fw.nca")
            .is_file());
        assert!(!other.join(NAND).join("keys").exists());
        // Retrying capture must not replace a newer archive downloaded by sync_pending.
        let newer = archive("user/save/account/title/progress", b"newer");
        write(&other.join(FILE), &newer);
        capture(&other).unwrap();
        assert_eq!(std::fs::read(other.join(FILE)).unwrap(), newer);
        prepare(&exe, &other).unwrap();
        assert_eq!(
            std::fs::read(other.join(NAND).join("user/save/account/title/progress")).unwrap(),
            b"newer"
        );
        write(
            &other.join(NAND).join("user/save/account/title/progress"),
            b"offline",
        );
        capture(&other).unwrap();
        assert_ne!(std::fs::read(other.join(FILE)).unwrap(), newer);
    }

    #[test]
    fn user_ids_travel_with_saves_and_seed_edens_early_account_manager() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("device1/eden");
        let game = temp.path().join("game");
        prepare(&first, &game).unwrap();
        let early = first
            .parent()
            .unwrap()
            .join("user/nand")
            .join(ACCOUNT)
            .join("su/avators/profiles.dat");
        // Simulate Eden generating the first profile before it reads qt-config.ini.
        write(&early, b"device1 UUID");
        write(
            &game.join(NAND).join("user/save/account/title/progress"),
            b"saved",
        );
        capture_after_play(&first, &game).unwrap();
        let second = temp.path().join("device2/eden");
        let second_early = second.parent().unwrap().join("user/nand").join(ACCOUNT);
        write(
            &second_early.join("su/avators/profiles.dat"),
            b"device2 UUID",
        );
        let remote_game = temp.path().join("remote-game");
        write(
            &remote_game.join(FILE),
            &std::fs::read(game.join(FILE)).unwrap(),
        );
        prepare(&second, &remote_game).unwrap();
        assert_eq!(
            std::fs::read(second_early.join("su/avators/profiles.dat")).unwrap(),
            b"device1 UUID"
        );
        assert_eq!(
            std::fs::read(
                second_early
                    .with_extension("romp-backup")
                    .join("su/avators/profiles.dat")
            )
            .unwrap(),
            b"device2 UUID"
        );
        let profiles = remote_game
            .join(NAND)
            .join(ACCOUNT)
            .join("su/avators/profiles.dat");
        write(&profiles, b"updated profile");
        capture_after_play(&second, &remote_game).unwrap();
        assert_eq!(std::fs::read(profiles).unwrap(), b"updated profile");
    }

    #[test]
    fn bad_downloads_never_replace_working_saves() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        let working = game.join(NAND).join("user/save/progress");
        write(&working, b"keep");
        for bad in [
            "../outside",
            "user/../../outside",
            "keys/prod.keys",
            "system/Contents/registered/fw.nca",
            "user\\evil",
            "user",
        ] {
            write(&game.join(FILE), &archive(bad, b"bad"));
            assert!(prepare(&temp.path().join("eden"), &game).is_err(), "{bad}");
            assert_eq!(std::fs::read(&working).unwrap(), b"keep");
        }
        let empty = zip::ZipWriter::new(io::Cursor::new(Vec::new()))
            .finish()
            .unwrap()
            .into_inner();
        write(&game.join(FILE), &empty);
        assert!(prepare(&temp.path().join("eden"), &game).is_err());
        assert_eq!(std::fs::read(&working).unwrap(), b"keep");
    }

    #[test]
    fn configuring_nand_preserves_controls_and_does_not_accumulate_sections() {
        let original = "[Controls]\nplayer_0_type=0\n[Data%20Storage]\nnand_directory=old\nsave_directory=old\nsdmc_directory=sd\n[Audio]\nvolume=50\n";
        let updated = config_text(original, Path::new("/game saves/nand")).unwrap();
        assert!(updated.contains("player_0_type=0"));
        assert!(updated.contains("volume=50"));
        assert!(updated.contains("sdmc_directory=sd"));
        assert!(updated.contains("nand_directory=\"/game saves/nand\""));
        assert!(!updated.contains("=old"));
        assert_eq!(
            config_text(&updated, Path::new("/game saves/nand")).unwrap(),
            updated
        );
    }

    #[cfg(unix)]
    #[test]
    fn does_not_upload_linked_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        write(&dir.join("secret"), b"secret");
        std::fs::create_dir_all(dir.join(NAND).join("user/save")).unwrap();
        std::os::unix::fs::symlink(dir.join("secret"), dir.join(NAND).join("user/save/link"))
            .unwrap();
        assert!(capture(dir).is_err());
        assert!(!dir.join(FILE).exists());
    }
}
