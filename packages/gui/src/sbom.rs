use std::path::Path;

use serde_json::Value;

/// CycloneDX SBOM of the third-party packages in the shipped binaries.
pub(crate) const CYCLONEDX: &str = include_str!(concat!(env!("OUT_DIR"), "/sbom.cdx.json"));
const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/notices.json"));

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
}

impl Component {
    pub(crate) fn all() -> Vec<Component> {
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
    pub(crate) fn all() -> Vec<Notice> {
        let notices: Value = serde_json::from_str(NOTICES).expect("embedded notices are JSON");
        notices
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| Notice {
                packages: n["packages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                text: n["text"].as_str().unwrap_or_default().to_owned(),
            })
            .collect()
    }
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
        assert!(!Notice::all().is_empty());
    }
}
