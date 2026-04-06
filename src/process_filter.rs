//! Regex-based process filtering.
//!
//! Users can provide several regex patterns; a process matches the filter
//! set if *any* pattern matches its name or full command line. Matching is
//! case-insensitive by default (wrapped with `(?i)`) unless the user opts
//! out by using explicit inline flags in the pattern.

use regex::{Regex, RegexBuilder};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FilterError {
    #[error("invalid regex '{pattern}': {source}")]
    InvalidRegex {
        pattern: String,
        #[source]
        source: regex::Error,
    },
}

/// A single user-specified regex filter.
#[derive(Debug, Clone)]
pub struct ProcessFilter {
    raw: String,
    regex: Regex,
    /// If true, children (and transitive descendants) of matched processes
    /// are also included.
    pub include_children: bool,
    /// If true, this filter is active; otherwise it's skipped.
    pub enabled: bool,
}

impl ProcessFilter {
    pub fn new(pattern: impl Into<String>) -> Result<Self, FilterError> {
        let raw = pattern.into();
        let regex = RegexBuilder::new(&raw)
            .case_insensitive(true)
            .build()
            .map_err(|e| FilterError::InvalidRegex {
                pattern: raw.clone(),
                source: e,
            })?;
        Ok(Self {
            raw,
            regex,
            include_children: true,
            enabled: true,
        })
    }

    pub fn pattern(&self) -> &str {
        &self.raw
    }

    pub fn matches(&self, name: &str, cmdline: &str) -> bool {
        if !self.enabled {
            return false;
        }
        self.regex.is_match(name) || self.regex.is_match(cmdline)
    }
}

/// A collection of filters combined with logical OR.
#[derive(Debug, Clone, Default)]
pub struct FilterSet {
    pub filters: Vec<ProcessFilter>,
}

impl FilterSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, filter: ProcessFilter) {
        self.filters.push(filter);
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.filters.len() {
            self.filters.remove(index);
        }
    }

    pub fn clear(&mut self) {
        self.filters.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.filters.iter().all(|f| !f.enabled)
    }

    /// Returns true if *any* active filter matches the process.
    pub fn matches(&self, name: &str, cmdline: &str) -> bool {
        self.filters.iter().any(|f| f.matches(name, cmdline))
    }

    /// Returns true if at least one filter requests child inclusion.
    pub fn wants_children(&self) -> bool {
        self.filters.iter().any(|f| f.enabled && f.include_children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_regex_compiles() {
        let f = ProcessFilter::new("python.*").unwrap();
        assert_eq!(f.pattern(), "python.*");
        assert!(f.enabled);
        assert!(f.include_children);
    }

    #[test]
    fn invalid_regex_reports_pattern() {
        let err = ProcessFilter::new("(unclosed").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("(unclosed"), "error msg: {msg}");
    }

    #[test]
    fn case_insensitive_by_default() {
        let f = ProcessFilter::new("PyThOn").unwrap();
        assert!(f.matches("python3", ""));
        assert!(f.matches("PYTHON3", ""));
    }

    #[test]
    fn matches_against_name_or_cmdline() {
        let f = ProcessFilter::new("myapp").unwrap();
        assert!(f.matches("myapp", ""));
        assert!(f.matches("bash", "/usr/bin/myapp --flag"));
        assert!(!f.matches("bash", "/usr/bin/other"));
    }

    #[test]
    fn disabled_filter_never_matches() {
        let mut f = ProcessFilter::new(".*").unwrap();
        f.enabled = false;
        assert!(!f.matches("anything", "anything"));
    }

    #[test]
    fn filter_set_is_logical_or() {
        let mut set = FilterSet::new();
        set.push(ProcessFilter::new("^python$").unwrap());
        set.push(ProcessFilter::new("^node$").unwrap());
        assert!(set.matches("python", ""));
        assert!(set.matches("node", ""));
        assert!(!set.matches("rustc", ""));
    }

    #[test]
    fn empty_filter_set_matches_nothing() {
        let set = FilterSet::new();
        assert!(set.is_empty());
        assert!(!set.matches("anything", "anything"));
    }

    #[test]
    fn filter_set_remove_and_clear() {
        let mut set = FilterSet::new();
        set.push(ProcessFilter::new("a").unwrap());
        set.push(ProcessFilter::new("b").unwrap());
        set.remove(5); // out of bounds: no panic
        assert_eq!(set.filters.len(), 2);
        set.remove(0);
        assert_eq!(set.filters.len(), 1);
        assert_eq!(set.filters[0].pattern(), "b");
        set.clear();
        assert!(set.filters.is_empty());
    }

    #[test]
    fn wants_children_reflects_active_filter_flags() {
        let mut set = FilterSet::new();
        let mut f1 = ProcessFilter::new("a").unwrap();
        f1.include_children = false;
        set.push(f1);
        assert!(!set.wants_children());

        let f2 = ProcessFilter::new("b").unwrap();
        set.push(f2);
        assert!(set.wants_children());

        // Disabling the only child-including filter disables the answer.
        set.filters[1].enabled = false;
        assert!(!set.wants_children());
    }

    #[test]
    fn set_is_empty_when_all_filters_disabled() {
        let mut set = FilterSet::new();
        let mut f = ProcessFilter::new("a").unwrap();
        f.enabled = false;
        set.push(f);
        assert!(set.is_empty());
    }
}
