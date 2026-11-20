//! Configuration environment variables and their legacy aliases.
//!
//! Configuration names use the `MEMQUIRA_` prefix. Releases up to 0.2.x used `MEMCORE_`. Every
//! `MEMQUIRA_*` name still accepts its `MEMCORE_*` form as a deprecated alias:
//! - the new name wins when both are set to the same value;
//! - startup fails when both are set to different values ([`legacy_report`], [`conflict_error`]).

use std::env::VarError;

/// Prefix of every configuration variable.
pub const PREFIX: &str = "MEMQUIRA_";
/// Prefix accepted as a deprecated alias (releases up to 0.2.x).
pub const LEGACY_PREFIX: &str = "MEMCORE_";

/// The deprecated alias of `key`, when `key` is a `MEMQUIRA_*` name.
pub fn legacy_name(key: &str) -> Option<String> {
    key.strip_prefix(PREFIX)
        .map(|rest| format!("{LEGACY_PREFIX}{rest}"))
}

/// The current name for a legacy `MEMCORE_*` name.
pub fn current_name(legacy: &str) -> Option<String> {
    legacy
        .strip_prefix(LEGACY_PREFIX)
        .map(|rest| format!("{PREFIX}{rest}"))
}

/// Reads `key`; when it is unset and is a `MEMQUIRA_*` name, reads its `MEMCORE_*` alias.
/// Other names behave exactly like [`std::env::var`].
pub fn var(key: &str) -> Result<String, VarError> {
    match std::env::var(key) {
        Err(VarError::NotPresent) => match legacy_name(key) {
            Some(legacy) => std::env::var(legacy),
            None => Err(VarError::NotPresent),
        },
        other => other,
    }
}

/// Legacy variables found in the process environment.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LegacyReport {
    /// `(legacy, current)` pairs where only the legacy name is set, or both are set to the same value.
    pub in_use: Vec<(String, String)>,
    /// `(legacy, current)` pairs set to different values. Values are never recorded.
    pub conflicts: Vec<(String, String)>,
}

impl LegacyReport {
    /// One warning line per legacy name in use (no values).
    pub fn deprecation_warnings(&self) -> Vec<String> {
        self.in_use
            .iter()
            .map(|(legacy, current)| format!("{legacy} is deprecated; rename it to {current}"))
            .collect()
    }
}

/// Scans the process environment for `MEMCORE_*` names (sorted, names only).
pub fn legacy_report() -> LegacyReport {
    let mut report = LegacyReport::default();
    let mut names: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| k.starts_with(LEGACY_PREFIX))
        .collect();
    names.sort();
    for legacy in names {
        let Some(current) = current_name(&legacy) else {
            continue;
        };
        match (std::env::var_os(&legacy), std::env::var_os(&current)) {
            (Some(old), Some(new)) if old != new => report.conflicts.push((legacy, current)),
            _ => report.in_use.push((legacy, current)),
        }
    }
    report
}

/// The configuration error for conflicting legacy and current names, if any.
pub fn conflict_error(report: &LegacyReport) -> Option<String> {
    if report.conflicts.is_empty() {
        return None;
    }
    let pairs = report
        .conflicts
        .iter()
        .map(|(legacy, current)| format!("{legacy} and {current}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "conflicting configuration: {pairs} are both set to different values; remove the deprecated \
         {LEGACY_PREFIX}* name"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    fn set(key: &str, value: Option<&str>) {
        // SAFETY: tests that touch the environment hold LOCK.
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn names_map_both_ways() {
        assert_eq!(
            legacy_name("MEMQUIRA_PORT").as_deref(),
            Some("MEMCORE_PORT")
        );
        assert_eq!(
            current_name("MEMCORE_PORT").as_deref(),
            Some("MEMQUIRA_PORT")
        );
        assert_eq!(legacy_name("OPENAI_API_KEY"), None);
    }

    #[test]
    fn current_name_wins_and_legacy_is_a_fallback() {
        let _guard = LOCK.lock().unwrap();
        set("MEMQUIRA_ENVTEST_A", None);
        set("MEMCORE_ENVTEST_A", Some("legacy"));
        assert_eq!(var("MEMQUIRA_ENVTEST_A").as_deref(), Ok("legacy"));
        set("MEMQUIRA_ENVTEST_A", Some("current"));
        assert_eq!(var("MEMQUIRA_ENVTEST_A").as_deref(), Ok("current"));
        set("MEMQUIRA_ENVTEST_A", None);
        set("MEMCORE_ENVTEST_A", None);
        assert_eq!(var("MEMQUIRA_ENVTEST_A"), Err(VarError::NotPresent));
    }

    #[test]
    fn report_lists_aliases_and_conflicts_without_values() {
        let _guard = LOCK.lock().unwrap();
        set("MEMCORE_ENVTEST_B", Some("same"));
        set("MEMQUIRA_ENVTEST_B", Some("same"));
        set("MEMCORE_ENVTEST_C", Some("only-legacy"));
        set("MEMCORE_ENVTEST_D", Some("secret-old"));
        set("MEMQUIRA_ENVTEST_D", Some("secret-new"));
        let report = legacy_report();
        let pair = |s: &str| (format!("MEMCORE_{s}"), format!("MEMQUIRA_{s}"));
        assert!(report.in_use.contains(&pair("ENVTEST_B")));
        assert!(report.in_use.contains(&pair("ENVTEST_C")));
        assert!(report.conflicts.contains(&pair("ENVTEST_D")));
        let err = conflict_error(&report).expect("conflict");
        assert!(
            err.contains("MEMCORE_ENVTEST_D and MEMQUIRA_ENVTEST_D"),
            "{err}"
        );
        assert!(!err.contains("secret"), "{err}");
        assert!(report.deprecation_warnings().contains(
            &"MEMCORE_ENVTEST_C is deprecated; rename it to MEMQUIRA_ENVTEST_C".to_string()
        ));
        for k in ["B", "C", "D"] {
            set(&format!("MEMCORE_ENVTEST_{k}"), None);
            set(&format!("MEMQUIRA_ENVTEST_{k}"), None);
        }
    }
}
