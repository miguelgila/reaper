//! Pod annotation-based configuration for Reaper.
//!
//! Allows users to influence Reaper behavior per-pod via Kubernetes annotations
//! on the pod spec. Annotations use the prefix `reaper.runtime/`.
//!
//! Example pod annotation:
//! ```yaml
//! metadata:
//!   annotations:
//!     reaper.runtime/dns-mode: "kubernetes"
//! ```
//!
//! # Security Model
//!
//! - Only annotations in the **user-overridable allowlist** are honored.
//! - Admin-only parameters (overlay paths, filter settings, etc.) can NEVER
//!   be overridden via annotations regardless of configuration.
//! - The admin can disable all annotation processing via
//!   `REAPER_ANNOTATIONS_ENABLED=false`.
//! - Unknown annotation keys are silently ignored.
//! - Invalid values for known keys are logged and ignored.

use std::collections::HashMap;

/// Annotation key prefix. Pod annotations must start with this to be considered.
pub const ANNOTATION_PREFIX: &str = "reaper.runtime/";

/// Known annotation keys that users may override (stripped of prefix).
/// These map to specific Reaper configuration parameters.
const USER_OVERRIDABLE_KEYS: &[&str] = &["dns-mode", "overlay-name"];

/// Parsed Reaper annotations from a pod spec.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReaperAnnotations {
    /// DNS resolution mode override: "host", "kubernetes", or "k8s".
    pub dns_mode: Option<String>,
    /// Named overlay group override. DNS label format: [a-z0-9][a-z0-9-]*, max 63 chars.
    /// Pods with the same overlay-name (within the same namespace) share an overlay.
    pub overlay_name: Option<String>,
}

/// Check whether annotation-based configuration is enabled.
///
/// Reads `REAPER_ANNOTATIONS_ENABLED`. Default is `true`.
/// Set to `false`, `0`, `no`, or `off` (case-insensitive) to disable all annotation processing.
pub fn annotations_enabled() -> bool {
    std::env::var("REAPER_ANNOTATIONS_ENABLED")
        .map(|v| {
            let lower = v.to_ascii_lowercase();
            lower != "false" && lower != "0" && lower != "no" && lower != "off"
        })
        .unwrap_or(true)
}

/// Valid values for the `dns-mode` annotation.
const VALID_DNS_MODES: &[&str] = &["host", "kubernetes", "k8s"];

/// Validate an overlay name: DNS label format ([a-z0-9][a-z0-9-]*, max 63 chars).
fn is_valid_overlay_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 63 {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse Reaper annotations from an OCI config.json annotations map.
///
/// Filters for the `reaper.runtime/` prefix, validates keys against the
/// user-overridable allowlist, and validates values. Unknown or denied
/// keys are logged and ignored.
///
/// Returns `None` if annotations are disabled via `REAPER_ANNOTATIONS_ENABLED=false`.
pub fn parse_annotations(all_annotations: &HashMap<String, String>) -> Option<ReaperAnnotations> {
    if !annotations_enabled() {
        return None;
    }

    let mut result = ReaperAnnotations::default();

    for (key, value) in all_annotations {
        let stripped = match key.strip_prefix(ANNOTATION_PREFIX) {
            Some(k) => k,
            None => continue, // Not a reaper annotation
        };

        validate_and_apply_annotation(stripped, value, key, &mut result);
    }

    Some(result)
}

/// Parse already-stripped Reaper annotations (without the `reaper.runtime/` prefix).
///
/// This is used by the runtime when loading annotations from state, where keys
/// are already stored without the prefix. Avoids a wasteful strip-then-re-add round-trip.
///
/// Returns `None` if annotations are disabled via `REAPER_ANNOTATIONS_ENABLED=false`.
pub fn parse_stripped_annotations(
    stripped_annotations: &HashMap<String, String>,
) -> Option<ReaperAnnotations> {
    if !annotations_enabled() {
        return None;
    }

    let mut result = ReaperAnnotations::default();

    for (key, value) in stripped_annotations {
        validate_and_apply_annotation(key, value, key, &mut result);
    }

    Some(result)
}

/// Validate a single annotation key/value against the allowlist and apply it.
/// `display_key` is used in log messages (may include the prefix for context).
fn validate_and_apply_annotation(
    stripped_key: &str,
    value: &str,
    display_key: &str,
    result: &mut ReaperAnnotations,
) {
    if !USER_OVERRIDABLE_KEYS.contains(&stripped_key) {
        eprintln!(
            "reaper: annotation: ignoring unknown or non-overridable key {:?}",
            display_key
        );
        return;
    }

    if stripped_key == "dns-mode" {
        let normalized = value.to_ascii_lowercase();
        if VALID_DNS_MODES.contains(&normalized.as_str()) {
            result.dns_mode = Some(normalized);
        } else {
            eprintln!(
                "reaper: annotation: ignoring invalid value {:?} for {:?} (valid: {:?})",
                value, display_key, VALID_DNS_MODES
            );
        }
    } else if stripped_key == "overlay-name" {
        if value.is_empty() {
            // Empty string treated as "not set" (backward compatible)
            return;
        }
        let normalized = value.to_ascii_lowercase();
        if is_valid_overlay_name(&normalized) {
            result.overlay_name = Some(normalized);
        } else {
            eprintln!(
                "reaper: annotation: ignoring invalid overlay-name {:?} for {:?} \
                 (must be DNS label: [a-z0-9-], max 63 chars)",
                value, display_key
            );
        }
    }
}

/// Extract allowlisted `reaper.runtime/*` annotations from an OCI config.json annotations map.
///
/// Returns a filtered map containing only annotations with the reaper prefix
/// whose keys are in the user-overridable allowlist. Keys are stripped of the prefix.
/// Unknown keys are logged and excluded to prevent state pollution.
pub fn extract_reaper_annotations(
    all_annotations: &HashMap<String, String>,
) -> HashMap<String, String> {
    all_annotations
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix(ANNOTATION_PREFIX).and_then(|stripped| {
                if USER_OVERRIDABLE_KEYS.contains(&stripped) {
                    Some((stripped.to_string(), value.clone()))
                } else {
                    if !stripped.is_empty() {
                        eprintln!(
                            "reaper: annotation: filtering out non-allowlisted key {:?}",
                            key
                        );
                    }
                    None
                }
            })
        })
        .collect()
}

/// Serialize annotations to CLI `--annotation key=value` format.
pub fn annotations_to_cli_args(annotations: &HashMap<String, String>) -> Vec<String> {
    annotations
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect()
}

/// Parse CLI `--annotation key=value` arguments back into a HashMap.
pub fn parse_cli_annotations(args: &[String]) -> HashMap<String, String> {
    args.iter()
        .filter_map(|arg| {
            arg.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn make_annotations(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // --- annotations_enabled tests ---

    #[test]
    #[serial]
    fn test_annotations_enabled_default() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        assert!(annotations_enabled());
    }

    #[test]
    #[serial]
    fn test_annotations_enabled_true() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "true");
        assert!(annotations_enabled());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    #[test]
    #[serial]
    fn test_annotations_enabled_false() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "false");
        assert!(!annotations_enabled());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    #[test]
    #[serial]
    fn test_annotations_enabled_zero() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "0");
        assert!(!annotations_enabled());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    #[test]
    #[serial]
    fn test_annotations_enabled_no() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "no");
        assert!(!annotations_enabled());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    #[test]
    #[serial]
    fn test_annotations_enabled_off() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "off");
        assert!(!annotations_enabled());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    #[test]
    #[serial]
    fn test_annotations_enabled_false_case_insensitive() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "FALSE");
        assert!(!annotations_enabled());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    // --- parse_annotations tests ---

    #[test]
    #[serial]
    fn test_parse_dns_mode_host() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/dns-mode", "host")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, Some("host".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_dns_mode_kubernetes() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/dns-mode", "kubernetes")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, Some("kubernetes".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_dns_mode_k8s() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/dns-mode", "k8s")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, Some("k8s".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_dns_mode_case_insensitive() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/dns-mode", "Kubernetes")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, Some("kubernetes".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_dns_mode_invalid_value() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/dns-mode", "invalid")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, None);
    }

    #[test]
    #[serial]
    fn test_parse_unknown_key_ignored() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/unknown-key", "value")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result, ReaperAnnotations::default());
    }

    #[test]
    #[serial]
    fn test_parse_non_reaper_annotations_ignored() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[
            ("io.kubernetes.pod.namespace", "default"),
            ("some.other/annotation", "value"),
        ]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result, ReaperAnnotations::default());
    }

    #[test]
    #[serial]
    fn test_parse_empty_annotations() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = HashMap::new();
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result, ReaperAnnotations::default());
    }

    #[test]
    #[serial]
    fn test_parse_returns_none_when_disabled() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "false");
        let annots = make_annotations(&[("reaper.runtime/dns-mode", "kubernetes")]);
        let result = parse_annotations(&annots);
        assert!(result.is_none());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    #[test]
    #[serial]
    fn test_parse_mixed_valid_and_invalid() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[
            ("reaper.runtime/dns-mode", "kubernetes"),
            ("reaper.runtime/unknown", "ignored"),
            ("io.kubernetes.pod.namespace", "default"),
        ]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, Some("kubernetes".to_string()));
    }

    // --- extract_reaper_annotations tests ---

    #[test]
    fn test_extract_reaper_annotations_allowlisted_only() {
        let annots = make_annotations(&[
            ("reaper.runtime/dns-mode", "kubernetes"),
            ("reaper.runtime/unknown", "value"),
            ("io.kubernetes.pod.namespace", "default"),
        ]);
        let extracted = extract_reaper_annotations(&annots);
        // Only allowlisted keys are extracted; "unknown" is filtered out
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted.get("dns-mode"), Some(&"kubernetes".to_string()));
        assert!(!extracted.contains_key("unknown"));
    }

    #[test]
    fn test_extract_reaper_annotations_empty() {
        let annots = make_annotations(&[("io.kubernetes.pod.namespace", "default")]);
        let extracted = extract_reaper_annotations(&annots);
        assert!(extracted.is_empty());
    }

    // --- parse_stripped_annotations tests ---

    #[test]
    #[serial]
    fn test_parse_stripped_dns_mode() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("dns-mode", "kubernetes")]);
        let result = parse_stripped_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, Some("kubernetes".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_stripped_unknown_key_ignored() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("unknown-key", "value")]);
        let result = parse_stripped_annotations(&annots).unwrap();
        assert_eq!(result, ReaperAnnotations::default());
    }

    #[test]
    #[serial]
    fn test_parse_stripped_returns_none_when_disabled() {
        std::env::set_var("REAPER_ANNOTATIONS_ENABLED", "false");
        let annots = make_annotations(&[("dns-mode", "kubernetes")]);
        let result = parse_stripped_annotations(&annots);
        assert!(result.is_none());
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
    }

    #[test]
    #[serial]
    fn test_parse_stripped_invalid_value() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("dns-mode", "invalid")]);
        let result = parse_stripped_annotations(&annots).unwrap();
        assert_eq!(result.dns_mode, None);
    }

    // --- overlay-name annotation tests ---

    #[test]
    #[serial]
    fn test_parse_overlay_name_valid() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/overlay-name", "pippo")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.overlay_name, Some("pippo".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_overlay_name_with_hyphens_and_digits() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/overlay-name", "my-group-42")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.overlay_name, Some("my-group-42".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_overlay_name_case_normalized() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/overlay-name", "MyGroup")]);
        let result = parse_annotations(&annots).unwrap();
        // Uppercase is normalized to lowercase (same as dns-mode)
        assert_eq!(result.overlay_name, Some("mygroup".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_overlay_name_empty_treated_as_unset() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/overlay-name", "")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.overlay_name, None);
    }

    #[test]
    #[serial]
    fn test_parse_overlay_name_invalid_chars() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("reaper.runtime/overlay-name", "bad/name")]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.overlay_name, None);
    }

    #[test]
    #[serial]
    fn test_parse_overlay_name_too_long() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let long = "a".repeat(64);
        let annots = make_annotations(&[("reaper.runtime/overlay-name", &long)]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.overlay_name, None);
    }

    #[test]
    #[serial]
    fn test_parse_overlay_name_with_dns_mode() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[
            ("reaper.runtime/overlay-name", "pippo"),
            ("reaper.runtime/dns-mode", "kubernetes"),
        ]);
        let result = parse_annotations(&annots).unwrap();
        assert_eq!(result.overlay_name, Some("pippo".to_string()));
        assert_eq!(result.dns_mode, Some("kubernetes".to_string()));
    }

    #[test]
    #[serial]
    fn test_parse_stripped_overlay_name() {
        std::env::remove_var("REAPER_ANNOTATIONS_ENABLED");
        let annots = make_annotations(&[("overlay-name", "pippo")]);
        let result = parse_stripped_annotations(&annots).unwrap();
        assert_eq!(result.overlay_name, Some("pippo".to_string()));
    }

    #[test]
    fn test_extract_reaper_annotations_includes_overlay_name() {
        let annots = make_annotations(&[
            ("reaper.runtime/overlay-name", "pippo"),
            ("reaper.runtime/dns-mode", "kubernetes"),
        ]);
        let extracted = extract_reaper_annotations(&annots);
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted.get("overlay-name"), Some(&"pippo".to_string()));
        assert_eq!(extracted.get("dns-mode"), Some(&"kubernetes".to_string()));
    }

    // --- CLI serialization round-trip tests ---

    #[test]
    fn test_cli_annotations_round_trip() {
        let mut original = HashMap::new();
        original.insert("dns-mode".to_string(), "kubernetes".to_string());

        let cli_args = annotations_to_cli_args(&original);
        assert_eq!(cli_args.len(), 1);
        assert_eq!(cli_args[0], "dns-mode=kubernetes");

        let parsed = parse_cli_annotations(&cli_args);
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_parse_cli_annotations_malformed() {
        let args = vec![
            "dns-mode=kubernetes".to_string(),
            "no-equals-sign".to_string(),
            "has=equals=in=value".to_string(),
        ];
        let parsed = parse_cli_annotations(&args);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get("dns-mode"), Some(&"kubernetes".to_string()));
        assert_eq!(parsed.get("has"), Some(&"equals=in=value".to_string()));
    }

    #[test]
    fn test_parse_cli_annotations_empty() {
        let args: Vec<String> = vec![];
        let parsed = parse_cli_annotations(&args);
        assert!(parsed.is_empty());
    }
}
