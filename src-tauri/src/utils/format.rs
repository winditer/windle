//! Byte/duration formatting helpers shared by the command modules.

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Format a byte count using base-10 units, the way Finder reports sizes.
pub fn bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }

    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Convert a `SystemTime` into the epoch milliseconds the frontend expects.
pub fn epoch_millis(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// Best-effort display name for a path: the last component, else the path.
pub fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Lowercased extension without the dot, or an empty string.
pub fn extension(path: &std::path::Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// `used / total` as a 0–1 ratio, guarding against division by zero.
pub fn ratio(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) as f32
    }
}

/// Compact duration for progress labels: `45s`, `3m 20s`, `2h 5m`, `3d 4h`.
pub fn duration(total_seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    match total_seconds {
        s if s < MINUTE => format!("{s}s"),
        s if s < HOUR => {
            let (m, rest) = (s / MINUTE, s % MINUTE);
            if rest == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {rest}s")
            }
        }
        s if s < DAY => {
            let (h, rest) = (s / HOUR, (s % HOUR) / MINUTE);
            if rest == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {rest}m")
            }
        }
        s => {
            let (d, rest) = (s / DAY, (s % DAY) / HOUR);
            if rest == 0 {
                format!("{d}d")
            } else {
                format!("{d}d {rest}h")
            }
        }
    }
}

/// Same as [`duration`], for a `Duration` — used to label elapsed scan time.
pub fn elapsed(duration_value: std::time::Duration) -> String {
    duration(duration_value.as_secs())
}

/// Replace the home prefix with `~` so paths read the way Finder shows them.
pub fn tilde(path: &std::path::Path) -> String {
    let home = crate::utils::permissions::home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// Abbreviate a path to fit `max_chars`, keeping the first and last components
/// so the result stays recognisable: `~/Library/…/Caches/com.apple.Safari`.
pub fn shorten_path(path: &std::path::Path, max_chars: usize) -> String {
    let display = tilde(path);
    if display.chars().count() <= max_chars {
        return display;
    }

    let parts: Vec<&str> = display.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() <= 2 {
        // Nothing to elide — truncate the tail instead.
        let kept: String = display.chars().take(max_chars.saturating_sub(1)).collect();
        return format!("{kept}…");
    }

    let first = parts[0];
    let last = parts[parts.len() - 1];
    let mut candidate = format!("{first}/…/{last}");

    // Add back trailing components while they still fit.
    for extra in (1..parts.len() - 1).rev() {
        let tail = parts[extra..].join("/");
        let longer = format!("{first}/…/{tail}");
        if longer.chars().count() > max_chars {
            break;
        }
        candidate = longer;
    }

    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_byte_counts() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_500), "1.5 KB");
        assert_eq!(bytes(2_000_000), "2.0 MB");
    }

    #[test]
    fn ratio_handles_empty_volume() {
        assert_eq!(ratio(0, 0), 0.0);
        assert_eq!(ratio(1, 2), 0.5);
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(60), "1m");
        assert_eq!(duration(200), "3m 20s");
        assert_eq!(duration(3_600), "1h");
        assert_eq!(duration(7_500), "2h 5m");
        assert_eq!(duration(272_000), "3d 3h");
    }

    #[test]
    fn shortens_long_paths() {
        let path = std::path::Path::new("/a/bbbb/cccc/dddd/eeee/target.txt");
        assert_eq!(shorten_path(path, 100), "/a/bbbb/cccc/dddd/eeee/target.txt");

        let short = shorten_path(path, 20);
        assert!(short.chars().count() <= 20, "{short} is still too long");
        assert!(short.starts_with('a'), "{short} should keep the first part");
        assert!(short.ends_with("target.txt"), "{short} should keep the name");
    }

    #[test]
    fn tilde_collapses_the_home_prefix() {
        let home = crate::utils::permissions::home_dir();
        assert_eq!(tilde(&home), "~");
        assert_eq!(tilde(&home.join("Library/Caches")), "~/Library/Caches");
        assert_eq!(tilde(std::path::Path::new("/tmp")), "/tmp");
    }

    #[test]
    fn extension_is_lowercased() {
        assert_eq!(extension(std::path::Path::new("/a/App.DMG")), "dmg");
        assert_eq!(extension(std::path::Path::new("/a/noext")), "");
    }
}
