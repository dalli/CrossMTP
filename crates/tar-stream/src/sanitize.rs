//! Rename pattern + timestamp sanitisation.
//!
//! plan.md §5: rename pattern variables are `{name}`, `{ext}`, `{n}`,
//! `{timestamp}` only. Output must be safe for FAT-family and Android
//! shared storage. `{timestamp}` default format is `yyyyMMdd-HHmmss`
//! (no `:` because FAT bans it).

/// Characters that are unsafe on FAT/exFAT or in Android scoped storage
/// path tokens. Replaced with `_` during sanitisation.
const UNSAFE_FILENAME_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*', '\0'];

/// Strip filesystem-unsafe characters from a single filename component.
/// Returns a string that is safe to use as a tar entry final segment on
/// FAT-derived filesystems.
pub fn sanitize_tar_path(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if UNSAFE_FILENAME_CHARS.contains(&ch) || (ch as u32) < 0x20 {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim_end_matches(&[' ', '.'] as &[char]).to_string();
    if trimmed.is_empty() {
        "_".into()
    } else {
        trimmed
    }
}

/// Render a sanitised default timestamp in `yyyyMMdd-HHmmss` form from a
/// Unix epoch second. Pure function so tests don't depend on wall clock.
pub fn sanitize_timestamp(epoch_seconds: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(epoch_seconds);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Sanitise a user-supplied rename pattern. We don't *expand* it here
/// (the caller does, plugging in `{name}` / `{ext}` / `{n}` / `{timestamp}`).
/// We only refuse patterns that reference unknown variables or sneak in
/// unsafe literal characters that survive after variable expansion.
///
/// Returns the cleaned pattern. Unknown `{var}` tokens cause an error so
/// the user sees the mistake immediately rather than getting `{foo}` in
/// their filename.
pub fn sanitize_rename_pattern(pattern: &str) -> Result<String, String> {
    let allowed = ["name", "ext", "n", "timestamp"];
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            // collect until '}'
            let mut var = String::new();
            let mut closed = false;
            for nc in chars.by_ref() {
                if nc == '}' {
                    closed = true;
                    break;
                }
                var.push(nc);
            }
            if !closed {
                return Err(format!("unterminated `{{` in pattern: {pattern}"));
            }
            if !allowed.contains(&var.as_str()) {
                return Err(format!(
                    "unknown variable `{{{var}}}` (allowed: {})",
                    allowed.join(", ")
                ));
            }
            out.push('{');
            out.push_str(&var);
            out.push('}');
        } else if c == '}' {
            return Err(format!("unbalanced `}}` in pattern: {pattern}"));
        } else if UNSAFE_FILENAME_CHARS.contains(&c) {
            // literal unsafe char in pattern → reject early
            return Err(format!(
                "pattern literal contains unsafe character `{c}`: {pattern}"
            ));
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

// ---- minimal civil-from-unix conversion (no chrono dep) ----
//
// Howard Hinnant's algorithm. Good for any int64 epoch seconds, no
// leap-second handling (we don't need it for filename timestamps).
fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_fat_unsafe_chars() {
        let cleaned = sanitize_tar_path(r#"a:b<c>d"e|f?g*h\i/j"#);
        assert!(!cleaned.contains(':'));
        assert!(!cleaned.contains('<'));
        assert!(!cleaned.contains('>'));
        assert!(!cleaned.contains('"'));
        assert!(!cleaned.contains('|'));
        assert!(!cleaned.contains('?'));
        assert!(!cleaned.contains('*'));
        assert!(!cleaned.contains('\\'));
        assert!(!cleaned.contains('/'));
    }

    #[test]
    fn sanitize_keeps_korean_and_spaces() {
        assert_eq!(sanitize_tar_path("한글 파일.txt"), "한글 파일.txt");
    }

    #[test]
    fn sanitize_trims_trailing_dots_and_spaces() {
        // Windows hates trailing `.` and ` `; FAT does too. We strip
        // them defensively even though our target is Android.
        assert_eq!(sanitize_tar_path("name.  "), "name");
        assert_eq!(sanitize_tar_path("name..."), "name");
    }

    #[test]
    fn sanitize_empty_after_strip_yields_placeholder() {
        assert_eq!(sanitize_tar_path("..."), "_");
        assert_eq!(sanitize_tar_path(""), "_");
    }

    #[test]
    fn timestamp_has_no_colon_and_is_fixed_width() {
        // 2026-05-14 00:00:00 UTC = 1778716800
        let ts = sanitize_timestamp(1_778_716_800);
        assert_eq!(ts.len(), 15);
        assert!(!ts.contains(':'));
        assert_eq!(ts, "20260514-000000");
    }

    #[test]
    fn timestamp_handles_pre_epoch() {
        // 1969-12-31 23:59:59 UTC = -1
        let ts = sanitize_timestamp(-1);
        assert_eq!(ts, "19691231-235959");
    }

    #[test]
    fn rename_pattern_accepts_documented_vars() {
        assert!(sanitize_rename_pattern("{name} ({n}){ext}").is_ok());
        assert!(sanitize_rename_pattern("{name} - {timestamp}{ext}").is_ok());
        assert!(sanitize_rename_pattern("{name} - copy{ext}").is_ok());
    }

    #[test]
    fn rename_pattern_rejects_unknown_var() {
        let e = sanitize_rename_pattern("{name}-{user}").unwrap_err();
        assert!(e.contains("unknown variable"));
    }

    #[test]
    fn rename_pattern_rejects_unterminated_brace() {
        assert!(sanitize_rename_pattern("{name").is_err());
    }

    #[test]
    fn rename_pattern_rejects_unsafe_literal() {
        assert!(sanitize_rename_pattern("{name}:{n}").is_err());
        assert!(sanitize_rename_pattern("{name}/{n}").is_err());
    }
}
