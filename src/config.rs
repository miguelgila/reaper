//! Shared configuration file loader for Reaper binaries.
//!
//! Reads `/etc/reaper/reaper.conf` (or path from `REAPER_CONFIG` env var)
//! and sets environment variables for any keys not already present.
//!
//! File format: simple `KEY=VALUE` lines. Comments (`#`) and blank lines
//! are ignored. Environment variables always take precedence over file values.

/// Default config file path, works on any Linux distribution.
const DEFAULT_CONFIG_PATH: &str = "/etc/reaper/reaper.conf";

/// Load configuration from the Reaper config file.
///
/// Search order:
/// 1. `REAPER_CONFIG` env var (explicit path override)
/// 2. `/etc/reaper/reaper.conf`
///
/// For each `KEY=VALUE` line, sets the environment variable only if it
/// is not already set. This ensures env vars always win over file values.
///
/// Silently returns if the config file doesn't exist (not an error).
pub fn load_config() {
    let path = std::env::var("REAPER_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return, // File doesn't exist or unreadable — silently continue
    };

    for line in contents.lines() {
        let trimmed = line.trim();

        // Skip comments and blank lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Split on first '=' only (values may contain '=')
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            if key.is_empty() {
                continue;
            }

            // Only set if not already present in environment (env wins)
            if std::env::var(key).is_err() {
                std::env::set_var(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_config_missing_file() {
        // Should not panic when file doesn't exist
        std::env::set_var("REAPER_CONFIG", "/nonexistent/path/reaper.conf");
        load_config();
        std::env::remove_var("REAPER_CONFIG");
    }

    #[test]
    fn test_load_config_parses_values() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("reaper.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "# Comment line").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "REAPER_TEST_KEY_A=hello").unwrap();
        writeln!(f, "REAPER_TEST_KEY_B = world ").unwrap();
        writeln!(f, "REAPER_TEST_KEY_C=has=equals").unwrap();

        // Ensure clean state
        std::env::remove_var("REAPER_TEST_KEY_A");
        std::env::remove_var("REAPER_TEST_KEY_B");
        std::env::remove_var("REAPER_TEST_KEY_C");

        std::env::set_var("REAPER_CONFIG", conf.to_str().unwrap());
        load_config();

        assert_eq!(std::env::var("REAPER_TEST_KEY_A").unwrap(), "hello");
        assert_eq!(std::env::var("REAPER_TEST_KEY_B").unwrap(), "world");
        assert_eq!(std::env::var("REAPER_TEST_KEY_C").unwrap(), "has=equals");

        // Cleanup
        std::env::remove_var("REAPER_CONFIG");
        std::env::remove_var("REAPER_TEST_KEY_A");
        std::env::remove_var("REAPER_TEST_KEY_B");
        std::env::remove_var("REAPER_TEST_KEY_C");
    }

    #[test]
    fn test_env_var_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("reaper.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "REAPER_TEST_PRECEDENCE=from_file").unwrap();

        // Set env var BEFORE loading config
        std::env::set_var("REAPER_TEST_PRECEDENCE", "from_env");
        std::env::set_var("REAPER_CONFIG", conf.to_str().unwrap());

        load_config();

        // Env var should win
        assert_eq!(std::env::var("REAPER_TEST_PRECEDENCE").unwrap(), "from_env");

        // Cleanup
        std::env::remove_var("REAPER_CONFIG");
        std::env::remove_var("REAPER_TEST_PRECEDENCE");
    }

    #[test]
    fn test_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("reaper.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "no_equals_sign").unwrap();
        writeln!(f, "=empty_key").unwrap();
        writeln!(f, "  =also_empty").unwrap();
        writeln!(f, "REAPER_TEST_VALID=ok").unwrap();

        std::env::remove_var("REAPER_TEST_VALID");
        std::env::set_var("REAPER_CONFIG", conf.to_str().unwrap());

        load_config(); // Should not panic

        assert_eq!(std::env::var("REAPER_TEST_VALID").unwrap(), "ok");

        std::env::remove_var("REAPER_CONFIG");
        std::env::remove_var("REAPER_TEST_VALID");
    }
}
