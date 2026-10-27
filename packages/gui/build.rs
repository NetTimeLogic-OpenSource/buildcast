use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

// workspace packages that ship in the release
const SHIPPED: [&str; 2] = ["buildcast", "buildcast-cli"];

fn main() {
    // asInvoker: the GUI runs unelevated (drag and drop, no UAC at launch).
    // Device writes go through buildcast-cli started with the runas verb.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(new_manifest("BuildCast.App")).expect("failed to embed Windows manifest");

        winresource::WindowsResource::new()
            .set_icon("assets/buildcast.ico")
            .set("ProductName", "BuildCast")
            .set("FileDescription", "BuildCast disk image writer")
            .compile()
            .expect("failed to embed icon resource");
    }
    write_sbom();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    println!("cargo:rerun-if-changed=assets/buildcast.ico");
}

/// Writes a CycloneDX SBOM and the deduplicated license texts of all
/// third-party packages linked into the shipped binaries.
fn write_sbom() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new(std::env::var("CARGO").unwrap())
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .args(["--filter-platform", &std::env::var("TARGET").unwrap()])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .output()
        .expect("failed to run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).unwrap();

    let packages: BTreeMap<&str, &Value> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p))
        .collect();
    let members: HashSet<&str> = meta["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let deps: BTreeMap<&str, Vec<&str>> = meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let normal = n["deps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| {
                    d["dep_kinds"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|k| k["kind"].is_null())
                })
                .map(|d| d["pkg"].as_str().unwrap())
                .collect();
            (n["id"].as_str().unwrap(), normal)
        })
        .collect();

    let mut queue: VecDeque<&str> = packages
        .iter()
        .filter(|(id, p)| members.contains(*id) && SHIPPED.contains(&p["name"].as_str().unwrap()))
        .map(|(id, _)| *id)
        .collect();
    let mut seen: HashSet<&str> = queue.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        for &dep in deps.get(id).into_iter().flatten() {
            if seen.insert(dep) {
                queue.push_back(dep);
            }
        }
    }

    let mut third_party: Vec<&Value> = seen
        .iter()
        .filter(|id| !members.contains(*id))
        .map(|id| packages[id])
        .collect();
    third_party.sort_by_key(|p| (p["name"].as_str(), p["version"].as_str()));

    let components: Vec<Value> = third_party.iter().map(|p| component(p)).collect();
    let sbom = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": "buildcast",
                "version": env!("CARGO_PKG_VERSION"),
                "licenses": [{ "expression": env!("CARGO_PKG_LICENSE") }],
            },
        },
        "components": components,
    });

    // identical texts are listed once, with every package that uses them
    let mut notices: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for p in &third_party {
        let label = format!(
            "{} {}",
            p["name"].as_str().unwrap(),
            p["version"].as_str().unwrap()
        );
        for text in license_texts(p) {
            let key = text.split_whitespace().collect::<Vec<_>>().join(" ");
            notices
                .entry(key)
                .or_insert_with(|| (text, Vec::new()))
                .1
                .push(label.clone());
        }
    }
    let notices: Vec<Value> = notices
        .into_values()
        .map(|(text, packages)| json!({ "packages": packages, "text": text }))
        .collect();

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(
        out_dir.join("sbom.cdx.json"),
        serde_json::to_string_pretty(&sbom).unwrap(),
    )
    .unwrap();
    std::fs::write(
        out_dir.join("notices.json"),
        serde_json::to_string(&notices).unwrap(),
    )
    .unwrap();
}

fn component(p: &Value) -> Value {
    let name = p["name"].as_str().unwrap();
    let version = p["version"].as_str().unwrap();
    let purl = format!("pkg:cargo/{name}@{version}");
    let mut c = json!({
        "type": "library",
        "bom-ref": purl,
        "name": name,
        "version": version,
        "purl": purl,
    });
    if let Some(license) = p["license"].as_str() {
        c["licenses"] = json!([{ "expression": license.replace('/', " OR ") }]);
    }
    if let Some(repo) = p["repository"].as_str() {
        c["externalReferences"] = json!([{ "type": "vcs", "url": repo }]);
    }
    c
}

fn license_texts(p: &Value) -> Vec<String> {
    const PREFIXES: [&str; 5] = ["LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE"];
    let manifest = Path::new(p["manifest_path"].as_str().unwrap());
    let Ok(entries) = std::fs::read_dir(manifest.parent().unwrap()) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy().to_uppercase();
            path.is_file() && PREFIXES.iter().any(|pre| name.starts_with(pre))
        })
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect()
}
