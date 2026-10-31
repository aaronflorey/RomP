use anyhow::{bail, Context};
use std::process::Command;

fn available() -> std::io::Result<bool> {
    Command::new("busctl")
        .args([
            "--user",
            "--timeout=5",
            "call",
            "org.freedesktop.secrets",
            "/org/freedesktop/secrets",
            "org.freedesktop.DBus.Peer",
            "Ping",
        ])
        .output()
        .map(|output| output.status.success())
}

pub(super) fn prepare() -> anyhow::Result<()> {
    if available().context("checking Secret Service")? {
        return Ok(());
    }
    // Gaming Mode does not start Plasma's provider; wait for its bus name before keyring initializes.
    let output = Command::new("systemd-run")
        .args([
            "--user",
            "--collect",
            "--unit=romp-keyring",
            "--property=Type=dbus",
            "--property=BusName=org.freedesktop.secrets",
            "--property=TimeoutStartSec=10s",
            "--setenv=QT_QPA_PLATFORM=offscreen",
            "/usr/bin/ksecretd",
        ])
        .output()
        .context("starting KDE Secret Service")?;
    // Another launch may have started the same service while this one was checking.
    if !output.status.success() && !available().context("checking Secret Service after startup")? {
        bail!(
            "starting KDE Secret Service ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    tracing::info!("system credential store is ready for Gaming Mode");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn game_mode_prepares_a_missing_provider_and_preserves_an_existing_one() {
        const CHILD: &str = "ROMP_TEST_SECRET_SERVICE";
        if let Ok(mode) = std::env::var(CHILD) {
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(std::io::stderr)
                .init();
            crate::credentials::prepare(mode != "desktop");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let calls = dir.path().join("calls");
        for (name, body) in [
            (
                "busctl",
                "printf 'probe\\n' >> \"$ROMP_TEST_CALLS\"\ntest -f \"$ROMP_TEST_READY\"",
            ),
            (
                "systemd-run",
                "printf '%s\\n' \"$@\" >> \"$ROMP_TEST_CALLS\"\ncase \"$ROMP_TEST_SECRET_SERVICE\" in\nfailed) printf 'provider startup failed\\n' >&2; exit 1;;\nrace) touch \"$ROMP_TEST_READY\"; exit 1;;\n*) touch \"$ROMP_TEST_READY\";;\nesac",
            ),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths(std::iter::once(dir.path().to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        for mode in ["desktop", "existing", "missing", "failed", "race"] {
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_file(&calls);
            if mode == "existing" {
                std::fs::write(&ready, "").unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "credentials::secret_service::tests::game_mode_prepares_a_missing_provider_and_preserves_an_existing_one",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env("PATH", &path)
                .env("ROMP_TEST_READY", &ready)
                .env("ROMP_TEST_CALLS", &calls)
                .output()
                .unwrap();
            let log = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{mode}: {log}");
            let calls = std::fs::read_to_string(&calls).unwrap_or_default();
            match mode {
                "desktop" => assert!(calls.is_empty()),
                "existing" => assert_eq!(calls, "probe\n"),
                "missing" | "race" => {
                    assert!(ready.exists(), "{mode}");
                    assert!(calls.contains("--property=Type=dbus\n"));
                    assert!(calls.contains("--property=BusName=org.freedesktop.secrets\n"));
                    assert!(!log.contains("could not prepare"), "{mode}: {log}");
                }
                "failed" => {
                    assert!(!ready.exists());
                    assert!(log.contains("provider startup failed"), "{log}");
                }
                _ => unreachable!(),
            }
        }
    }
}
