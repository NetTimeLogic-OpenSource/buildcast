use std::path::PathBuf;

use egui::RichText;

use crate::version::Version;

const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// One released version in the changelog.
pub(crate) struct Release {
    body: &'static str,
    date: &'static str,
    pub(crate) version: Version,
}

impl Release {
    /// All released versions, newest first. `Unreleased` is skipped.
    pub(crate) fn all() -> Vec<Release> {
        CHANGELOG
            .split("\n## [")
            .skip(1)
            .filter_map(|section| {
                let (heading, body) = section.split_once('\n').unwrap_or((section, ""));
                let (version, rest) = heading.split_once(']')?;
                Some(Release {
                    body: body.split("\n[").next().unwrap_or_default().trim(),
                    date: rest.trim().trim_start_matches('-').trim(),
                    version: version.parse().ok()?,
                })
            })
            .collect()
    }

    pub(crate) fn show(&self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(format!("{}  ·  {}", self.version, self.date))
                .strong()
                .size(15.0),
        );
        let mut item = String::new();
        for line in self.body.lines().chain([""]) {
            let line = line.trim_end();
            if let Some(cont) = line.strip_prefix("  ")
                && !item.is_empty()
            {
                item.push(' ');
                item.push_str(cont.trim());
                continue;
            }
            if !item.is_empty() {
                ui.label(format!("•  {}", item.replace('`', "")));
                item.clear();
            }
            if let Some(kind) = line.strip_prefix("### ") {
                ui.add_space(4.0);
                ui.label(RichText::new(kind).weak().strong());
            } else if let Some(text) = line.strip_prefix("- ") {
                item.push_str(text);
            }
        }
        ui.add_space(8.0);
    }

    /// Releases the user has not seen yet, newest first, and records the
    /// current version as seen.
    ///
    /// Without a record of an earlier run, only the current release is
    /// returned. Earlier versions did not keep one.
    pub(crate) fn unseen() -> Vec<Release> {
        let current = Version::current();
        let path = seen_path();
        let last: Option<Version> = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| s.parse().ok());
        if last == Some(current) {
            return Vec::new();
        }
        if let Some(p) = &path
            && let Some(dir) = p.parent()
        {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(p, current.to_string());
        }
        Release::all()
            .into_iter()
            .filter(|r| r.version <= current && last.is_none_or(|l| r.version > l))
            .take(if last.is_some() { usize::MAX } else { 1 })
            .collect()
    }
}

fn seen_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("buildcast").join("last-version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_parses() {
        let all = Release::all();
        assert!(all.windows(2).all(|w| w[0].version > w[1].version));
        let first = all.last().unwrap();
        assert_eq!(first.version, "0.1.1".parse().unwrap());
        assert!(first.body.starts_with("### Added"));
        assert!(!first.body.contains("https://"));
    }

    #[test]
    fn current_version_is_released_or_unreleased() {
        let current = Version::current();
        assert!(Release::all().iter().all(|r| r.version <= current));
    }
}
