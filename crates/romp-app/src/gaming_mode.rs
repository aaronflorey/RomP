/// Detect the Gamescope desktop session, leaving ordinary desktop launches alone.
pub fn active() -> bool {
    cfg!(target_os = "linux")
        && is_game_session(
            std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
            std::env::var("XDG_SESSION_DESKTOP").ok().as_deref(),
            std::env::var("GAMESCOPE_WAYLAND_DISPLAY").ok().as_deref(),
        )
}

fn is_game_session(
    desktop: Option<&str>,
    session_desktop: Option<&str>,
    gamescope_display: Option<&str>,
) -> bool {
    // Steam launch wrappers can replace the desktop label while keeping Gamescope's display.
    is_gamescope(desktop)
        || is_gamescope(session_desktop)
        || gamescope_display.is_some_and(|display| !display.is_empty())
}

fn is_gamescope(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|desktop| {
        desktop
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("gamescope"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gamescope_in_desktop_lists() {
        assert!(is_gamescope(Some("gamescope")));
        assert!(is_gamescope(Some("Gamescope")));
        assert!(is_gamescope(Some("gamescope:KDE")));
        assert!(is_gamescope(Some("KDE:gamescope")));
    }

    #[test]
    fn ordinary_or_unknown_desktops_keep_desktop_behavior() {
        for desktop in [
            None,
            Some(""),
            Some("KDE"),
            Some("GNOME"),
            Some("my-gamescope"),
        ] {
            assert!(!is_gamescope(desktop));
        }
    }

    #[test]
    fn gamescope_session_survives_a_missing_or_overridden_desktop_label() {
        assert!(is_game_session(None, Some("gamescope"), None));
        assert!(is_game_session(Some("KDE"), Some("gamescope"), None));
        assert!(is_game_session(None, None, Some("gamescope-0")));
        assert!(is_game_session(
            Some("KDE"),
            Some("KDE"),
            Some("gamescope-0")
        ));
    }

    #[test]
    fn ordinary_desktop_launches_keep_their_dpi() {
        assert!(!is_game_session(None, None, None));
        assert!(!is_game_session(Some("KDE"), Some("KDE"), None));
        assert!(!is_game_session(Some("GNOME"), None, Some("")));
    }
}
