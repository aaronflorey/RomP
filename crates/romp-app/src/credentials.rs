#[cfg(target_os = "linux")]
mod secret_service;

#[cfg(target_os = "linux")]
pub fn prepare(game_mode: bool) {
    if game_mode {
        if let Err(error) = secret_service::prepare() {
            tracing::warn!(error = %format_args!("{error:#}"), "could not prepare the system credential store");
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn prepare(_game_mode: bool) {}

pub trait TokenStore: Send + Sync {
    fn load(&self, server: &str) -> Option<String>;
    fn save(&self, server: &str, token: &str) -> Result<(), String>;
    fn delete(&self, server: &str);
}

pub struct Keychain {
    service: &'static str,
}

impl Keychain {
    pub fn retro_achievements() -> Keychain {
        Keychain {
            service: "Romp RetroAchievements",
        }
    }

    pub fn app() -> Migrating<Keychain, Keychain> {
        Migrating {
            current: Keychain { service: "Romp" },
            legacy: Keychain {
                service: "Cartridge",
            },
        }
    }
}

impl TokenStore for Keychain {
    fn load(&self, server: &str) -> Option<String> {
        match keyring::Entry::new(self.service, server).and_then(|entry| entry.get_password()) {
            Ok(token) => Some(token),
            Err(keyring::Error::NoEntry) => None,
            Err(error) => {
                let error = error_cause(&error);
                tracing::warn!(service = self.service, %error, "could not read saved sign-in");
                None
            }
        }
    }

    fn save(&self, server: &str, token: &str) -> Result<(), String> {
        keyring::Entry::new(self.service, server)
            .and_then(|e| e.set_password(token))
            .map_err(|error| {
                let e = error_cause(&error);
                tracing::warn!(service = self.service, error = %e, "could not store saved sign-in");
                tr::tr!("Could not store the sign-in in the keychain: {e}", e)
            })
    }

    fn delete(&self, server: &str) {
        if let Ok(entry) = keyring::Entry::new(self.service, server) {
            let _ = entry.delete_credential();
        }
    }
}

fn error_cause(error: &keyring::Error) -> &keyring::Error {
    // Entry::new replaces initialization failures with NoDefaultStore.
    keyring::Entry::store_status()
        .as_ref()
        .err()
        .unwrap_or(error)
}

pub struct Migrating<C, L> {
    current: C,
    legacy: L,
}

impl<C: TokenStore, L: TokenStore> TokenStore for Migrating<C, L> {
    fn load(&self, server: &str) -> Option<String> {
        if let Some(token) = self.current.load(server) {
            return Some(token);
        }
        let token = self.legacy.load(server)?;
        if self.current.save(server, &token).is_ok() {
            self.legacy.delete(server);
        }
        Some(token)
    }

    fn save(&self, server: &str, token: &str) -> Result<(), String> {
        self.current.save(server, token)
    }

    fn delete(&self, server: &str) {
        self.current.delete(server);
        self.legacy.delete(server);
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryTokens(std::sync::Mutex<std::collections::HashMap<String, String>>);

#[cfg(test)]
impl TokenStore for MemoryTokens {
    fn load(&self, server: &str) -> Option<String> {
        self.0.lock().unwrap().get(server).cloned()
    }
    fn save(&self, server: &str, token: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(server.into(), token.into());
        Ok(())
    }
    fn delete(&self, server: &str) {
        self.0.lock().unwrap().remove(server);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn unavailable_store_reports_its_initialization_error() {
        const CHILD: &str = "ROMP_TEST_KEYRING_FAILURE";
        if std::env::var_os(CHILD).is_some() {
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(std::io::stderr)
                .init();
            let tokens = Keychain::app();
            assert_eq!(tokens.load("https://debug.invalid/"), None);
            let error = tokens
                .save("https://debug.invalid/", "ROMP_TEST_SECRET")
                .unwrap_err();
            let cause = keyring::Entry::store_status().as_ref().unwrap_err();
            assert!(error.contains(&cause.to_string()), "{error}");
            return;
        }

        // A fresh process isolates keyring's cached initialization from other tests.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "credentials::tests::unavailable_store_reports_its_initialization_error",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/")
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{log}");
        assert!(log.contains("could not read saved sign-in"), "{log}");
        assert!(log.contains("could not store saved sign-in"), "{log}");
        assert!(log.contains("Platform failure:"), "{log}");
        assert!(!log.contains("No default store has been set"), "{log}");
        assert!(!log.contains("ROMP_TEST_SECRET"), "{log}");
    }

    #[test]
    fn a_sign_in_saved_under_the_old_name_moves_to_the_new_one() {
        let tokens = Migrating {
            current: MemoryTokens::default(),
            legacy: MemoryTokens::default(),
        };
        tokens.legacy.save("http://a/", "old").unwrap();
        assert_eq!(tokens.load("http://a/").as_deref(), Some("old"));
        assert_eq!(tokens.current.load("http://a/").as_deref(), Some("old"));
        assert_eq!(tokens.legacy.load("http://a/"), None);
        tokens.save("http://a/", "new").unwrap();
        assert_eq!(tokens.load("http://a/").as_deref(), Some("new"));
        tokens.legacy.save("http://a/", "stale").unwrap();
        tokens.delete("http://a/");
        assert_eq!(tokens.load("http://a/"), None);
    }

    #[test]
    fn memory_tokens_are_per_server() {
        let t = MemoryTokens::default();
        t.save("http://a/", "one").unwrap();
        t.save("http://b/", "two").unwrap();
        assert_eq!(t.load("http://a/").as_deref(), Some("one"));
        t.delete("http://a/");
        assert_eq!(t.load("http://a/"), None);
        assert_eq!(t.load("http://b/").as_deref(), Some("two"));
    }
}
