use std::sync::{Arc, Mutex};

use buildcast_core::worker::Notify;

const REPO: &str = "NetTimeLogic-OpenSource/buildcast";
pub const RELEASES_PAGE: &str = "https://github.com/NetTimeLogic-OpenSource/buildcast/releases";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub installer_url: Option<String>,
}

#[derive(Clone)]
pub enum UpdateState {
    Unknown,
    UpToDate,
    Available(UpdateInfo),
    Downloading,
    Error(String),
}

pub type SharedUpdate = Arc<Mutex<UpdateState>>;

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().trim_start_matches('v').splitn(3, '.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        // tolerate suffixes like "3-rc1"
        it.next()?
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()?,
    ))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn fetch_latest() -> Result<UpdateInfo, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut resp = ureq::get(&url)
        .header("User-Agent", "buildcast")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let version = v["tag_name"]
        .as_str()
        .ok_or("no tag_name in release")?
        .to_string();
    let installer_url = v["assets"].as_array().and_then(|assets| {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            (name.starts_with("buildcast-setup-") && name.ends_with(".exe"))
                .then(|| a["browser_download_url"].as_str().map(str::to_string))
                .flatten()
        })
    });
    Ok(UpdateInfo {
        version,
        installer_url,
    })
}

pub fn check(state: SharedUpdate, notify: Notify) {
    std::thread::spawn(move || {
        let next = match fetch_latest() {
            Ok(info) if is_newer(&info.version, CURRENT) => UpdateState::Available(info),
            Ok(_) => UpdateState::UpToDate,
            // offline or rate-limited: stay quiet, no banner
            Err(_) => UpdateState::Unknown,
        };
        *state.lock().unwrap() = next;
        notify();
    });
}

// download the installer and hand off to it; the setup exe elevates itself
// (or installs per-user) so a plain spawn is fine
#[cfg(windows)]
pub fn install(info: UpdateInfo, state: SharedUpdate, notify: Notify) {
    std::thread::spawn(move || {
        *state.lock().unwrap() = UpdateState::Downloading;
        notify();
        let result = (|| -> Result<(), String> {
            let url = info
                .installer_url
                .ok_or("release has no Windows installer")?;
            let mut resp = ureq::get(&url)
                .header("User-Agent", "buildcast")
                .call()
                .map_err(|e| e.to_string())?;
            let mut data = Vec::new();
            // as_reader streams without ureq's 10 MB read_to_vec limit
            std::io::copy(&mut resp.body_mut().as_reader(), &mut data)
                .map_err(|e| e.to_string())?;
            let path = std::env::temp_dir().join(format!("buildcast-setup-{}.exe", info.version));
            std::fs::write(&path, data).map_err(|e| e.to_string())?;
            std::process::Command::new(&path)
                .spawn()
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        match result {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                *state.lock().unwrap() = UpdateState::Error(format!("update failed: {e}"));
                notify();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("v0.2.0-rc1", "0.1.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(!is_newer("garbage", "0.1.1"));
        assert!(!is_newer("v1.0.0", "junk"));
    }
}
