//! Parse Embarcadero `rsvars.bat` files into a variable map.
//!
//! `rsvars.bat` lives in the `bin` directory of a RAD Studio / Delphi
//! installation and contains `@SET KEY=VALUE` lines that define environment
//! variables such as `BDS`, `BDSINCLUDE`, `BDSCOMMONDIR`, etc.
//!
//! These variables appear as `$(BDS)` / `$(BDSCOMMONDIR)` references inside
//! `.dproj` files and need to be expanded for correct path resolution.

use std::collections::HashMap;

/// Expand `%VAR%` references in a value using the already-accumulated map.
/// Unknown variables expand to the empty string.
fn expand_percent_vars(s: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let var_name: String = chars.by_ref().take_while(|&ch| ch != '%').collect();
            if let Some(val) = vars.get(&var_name.to_ascii_uppercase()) {
                result.push_str(val);
            }
            // Unknown variables expand to the empty string.
        } else {
            result.push(c);
        }
    }

    result
}

/// Parse the **contents** of an `rsvars.bat` file into a variable map.
///
/// The returned map is seeded with **all current process environment variables**
/// first; entries from the file then override them.  This means `%VAR%`
/// references in the file can expand against any variable already present in
/// the environment, and the caller does not need to merge system env vars
/// separately.
///
/// Each line of the form `@SET KEY=VALUE` or `SET KEY=VALUE` (case-insensitive)
/// is parsed.  `%VAR%` references inside values are expanded using the
/// variables accumulated so far (document order).
///
/// Lines that don't match the `@SET` / `SET` pattern are silently skipped.
///
/// # Example
/// ```
/// let content = r#"
/// @SET BDS=C:\Delphi
/// @SET BDSBIN=%BDS%\bin
/// "#;
/// let vars = dproj_rs::rsvars::parse_rsvars(content);
/// assert_eq!(vars["BDS"], r"C:\Delphi");
/// assert_eq!(vars["BDSBIN"], r"C:\Delphi\bin");
/// ```
pub fn parse_rsvars(content: &str) -> HashMap<String, String> {
    // Seed with the full process environment so that %VAR% references in the
    // file can expand against any already-set variable.  File entries are
    // inserted afterwards and therefore override duplicates.
    // All keys are stored uppercased to match Windows' case-insensitive
    // environment variable semantics (e.g. `Path` and `PATH` are the same).
    let mut vars: HashMap<String, String> =
        std::env::vars().map(|(k, v)| (k.to_ascii_uppercase(), v)).collect();

    for line in content.lines() {
        let trimmed = line.trim();

        // Strip optional leading '@'.
        let rest = trimmed.strip_prefix('@').unwrap_or(trimmed);

        // Must start with SET (case-insensitive), then whitespace or '='.
        let rest = if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case("set") {
            &rest[3..]
        } else {
            continue;
        };

        // Skip optional whitespace between SET and KEY.
        let rest = rest.trim_start();

        // Find '=' separator.
        let Some(eq_pos) = rest.find('=') else {
            continue;
        };

        let key = rest[..eq_pos].trim().to_ascii_uppercase();
        if key.is_empty() {
            continue;
        }

        let raw_value = rest[eq_pos + 1..].to_string();

        // Expand %VAR% references using variables collected so far.
        let value = if raw_value.contains('%') {
            expand_percent_vars(&raw_value, &vars)
        } else {
            raw_value
        };

        vars.insert(key, value);
    }

    vars
}

/// Parse an `rsvars.bat` file from disk into a variable map.
///
/// This is a convenience wrapper around [`parse_rsvars`] that reads the file
/// first.
pub fn parse_rsvars_file(
    path: impl AsRef<std::path::Path>,
) -> Result<HashMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_rsvars(&content))
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_set_lines() {
        let content = "\
@SET BDS=C:\\Program Files\\Delphi
@SET BDSINCLUDE=C:\\Program Files\\Delphi\\include
";
        let vars = parse_rsvars(content);
        assert_eq!(vars["BDS"], "C:\\Program Files\\Delphi");
        assert_eq!(vars["BDSINCLUDE"], "C:\\Program Files\\Delphi\\include");
    }

    #[test]
    fn expand_percent_references() {
        let content = "\
@SET BDS=C:\\Delphi
@SET BDSBIN=%BDS%\\bin
@SET BDSLIB=%BDS%\\lib
";
        let vars = parse_rsvars(content);
        assert_eq!(vars["BDSBIN"], "C:\\Delphi\\bin");
        assert_eq!(vars["BDSLIB"], "C:\\Delphi\\lib");
    }

    #[test]
    fn handles_empty_value() {
        let content = "@SET PLATFORM=\n@SET SDK=\n";
        let vars = parse_rsvars(content);
        assert_eq!(vars["PLATFORM"], "");
        assert_eq!(vars["SDK"], "");
    }

    #[test]
    fn case_insensitive_set_keyword() {
        let content = "SET BDS=C:\\Delphi\nset FOO=bar\n@Set BAZ=qux\n";
        let vars = parse_rsvars(content);
        assert_eq!(vars["BDS"], "C:\\Delphi");
        assert_eq!(vars["FOO"], "bar");
        assert_eq!(vars["BAZ"], "qux");
    }

    #[test]
    fn skips_non_set_lines() {
        let content = "\
@echo off
REM This is a comment
@SET BDS=C:\\Delphi
:: another comment
";
        let vars = parse_rsvars(content);
        // Non-SET lines must not produce entries; only the one BDS entry
        // should have been added on top of the pre-seeded system env vars.
        assert_eq!(vars["BDS"], "C:\\Delphi");
        assert!(!vars.contains_key("ECHO"), "@echo off must not be parsed as a variable");
        assert!(!vars.contains_key("REM"), "comment lines must not produce entries");
    }

    #[test]
    fn unknown_percent_var_expands_to_empty() {
        let content = "@SET FOO=%TOTALLY_NONEXISTENT_VAR_12345%;rest\n";
        let vars = parse_rsvars(content);
        // Not in the map and not a real env var → empty string
        assert_eq!(vars["FOO"], ";rest");
    }

    #[test]
    fn percent_var_resolves_from_seeded_env() {
        // %PATH% is pre-seeded from the process environment, so the expansion
        // works without any explicit fallback lookup.
        let content = "@SET MY_PATH=%PATH%\n";
        let vars = parse_rsvars(content);
        let real_path = std::env::var("PATH").unwrap_or_default();
        assert_eq!(vars["MY_PATH"], real_path);
    }

    #[test]
    fn parse_real_rsvars_file() {
        let vars = parse_rsvars_file("rsvars.bat").unwrap();
        assert!(vars.contains_key("BDS"), "expected BDS key in rsvars.bat");
        assert!(
            vars["BDS"].contains("Embarcadero"),
            "expected Embarcadero in BDS value: {}",
            vars["BDS"]
        );
    }

    #[test]
    fn path_expands_framework_dir() {
        let content = "\
@SET FrameworkDir=C:\\Windows\\Microsoft.NET\\Framework\\v4.0.30319
@SET PATH=%FrameworkDir%;C:\\Delphi\\bin;%PATH%
";
        let vars = parse_rsvars(content);
        assert!(
            vars["PATH"].starts_with("C:\\Windows\\Microsoft.NET"),
            "PATH should start with expanded FrameworkDir: {}",
            vars["PATH"]
        );
    }
}
