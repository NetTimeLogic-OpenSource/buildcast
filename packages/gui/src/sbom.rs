use std::path::Path;
use std::sync::LazyLock;

use serde_json::Value;

/// CycloneDX SBOM of the third-party packages in the shipped binaries.
pub(crate) const CYCLONEDX: &str = include_str!(concat!(env!("OUT_DIR"), "/sbom.cdx.json"));
const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/notices.json"));

static COMPONENTS: LazyLock<Vec<Component>> = LazyLock::new(Component::parse);
static NOTICE_LIST: LazyLock<Vec<Notice>> = LazyLock::new(Notice::parse);

/// Third-party package from the SBOM.
pub(crate) struct Component {
    pub(crate) license: String,
    pub(crate) name: String,
    pub(crate) version: String,
}

/// License text with the packages that ship it.
pub(crate) struct Notice {
    pub(crate) packages: String,
    pub(crate) text: String,
    /// First line of the text, for example "MIT License".
    pub(crate) title: String,
}

impl Component {
    pub(crate) fn all() -> &'static [Component] {
        &COMPONENTS
    }

    fn parse() -> Vec<Component> {
        let sbom: Value = serde_json::from_str(CYCLONEDX).expect("embedded SBOM is JSON");
        sbom["components"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| Component {
                license: c["licenses"][0]["expression"]
                    .as_str()
                    .unwrap_or("see license text")
                    .to_owned(),
                name: c["name"].as_str().unwrap_or_default().to_owned(),
                version: c["version"].as_str().unwrap_or_default().to_owned(),
            })
            .collect()
    }
}

impl Notice {
    pub(crate) fn all() -> &'static [Notice] {
        &NOTICE_LIST
    }

    fn parse() -> Vec<Notice> {
        let notices: Value = serde_json::from_str(NOTICES).expect("embedded notices are JSON");
        notices
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                let text = n["text"].as_str().unwrap_or_default().to_owned();
                let packages: Vec<&str> = n["packages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let mut title = license_name(&text);
                title += &format!(" ({} packages)", packages.len());
                Notice {
                    packages: packages.join(", "),
                    text,
                    title,
                }
            })
            .collect()
    }
}

// a readable name for well-known license texts, else their first line
fn license_name(text: &str) -> String {
    const KNOWN: [(&str, &str); 8] = [
        ("Apache License", "Apache License 2.0"),
        (
            "Permission is hereby granted, free of charge",
            "MIT License",
        ),
        (
            "Redistribution and use in source and binary forms",
            "BSD License",
        ),
        ("UNICODE LICENSE", "Unicode License v3"),
        (
            "Permission to use, copy, modify, and/or distribute",
            "ISC License",
        ),
        ("This software is provided 'as-is'", "zlib License"),
        ("Boost Software License", "Boost Software License 1.0"),
        ("CC0", "CC0 1.0"),
    ];
    if let Some((_, name)) = KNOWN.iter().find(|(needle, _)| text.contains(needle)) {
        return (*name).to_owned();
    }
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    let mut name: String = first.unwrap_or("License").chars().take(36).collect();
    if first.is_some_and(|l| l.chars().count() > 36) {
        name.push_str("...");
    }
    name
}

pub(crate) fn save(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, CYCLONEDX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_data_is_complete() {
        let components = Component::all();
        assert!(components.iter().any(|c| c.name == "egui"));
        assert!(components.iter().all(|c| !c.name.starts_with("buildcast")));
        assert!(
            Notice::all()
                .iter()
                .any(|n| n.title.starts_with("MIT License ("))
        );
    }
}
