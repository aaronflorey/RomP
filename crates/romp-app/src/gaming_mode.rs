/// Detect the Gamescope desktop session, leaving ordinary desktop launches alone.
pub fn active() -> bool {
    cfg!(target_os = "linux") && is_gamescope(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref())
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
}
