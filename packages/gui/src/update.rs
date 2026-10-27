use std::sync::{Arc, Mutex};

use buildcast_core::worker::Notify;

use crate::version::Version;

const REPO: &str = "NetTimeLogic-OpenSource/buildcast";
pub(crate) const RELEASES_PAGE: &str =
    "https://github.com/NetTimeLogic-OpenSource/buildcast/releases";

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("cannot download the update")]
    Http(#[from] ureq::Error),
    #[cfg(windows)]
    #[error("cannot start the installer")]
    Io(#[from] std::io::Error),
    #[error("unexpected release data")]
    Json(#[from] serde_json::Error),
    #[error("the latest release has no tag")]
    MissingTag,
    #[cfg(windows)]
    #[error("the release has no Windows installer")]
    NoInstaller,
}

#[derive(Clone)]
pub(crate) struct UpdateInfo {
    // read by install(), which exists only on Windows
    #[cfg_attr(not(windows), expect(dead_code))]
    installer_url: Option<String>,
    pub(crate) version: Version,
}

#[derive(Clone)]
pub(crate) enum UpdateState {
    Available(UpdateInfo),
    // constructed only by install(), which exists only on Windows
    #[cfg_attr(not(windows), expect(dead_code))]
    Downloading,
    #[cfg_attr(not(windows), expect(dead_code))]
    Failed(Arc<Error>),
    Unknown,
    UpToDate,
}

/// Background check for a newer GitHub release.
#[derive(Clone)]
pub(crate) struct Updater {
    state: Arc<Mutex<UpdateState>>,
}

impl UpdateInfo {
    #[cfg(windows)]
    pub(crate) fn has_installer(&self) -> bool {
        self.installer_url.is_some()
    }

    fn fetch_latest() -> Result<UpdateInfo, Error> {
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let body = ureq::get(&url)
            .header("User-Agent", "buildcast")
            .header("Accept", "application/vnd.github+json")
            .call()?
            .body_mut()
            .read_to_string()?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        let version = v["tag_name"]
            .as_str()
            .and_then(|t| t.parse().ok())
            .ok_or(Error::MissingTag)?;
        let installer_url = v["assets"].as_array().and_then(|assets| {
            assets.iter().find_map(|a| {
                let name = a["name"].as_str()?;
                (name.starts_with("buildcast-setup-") && name.ends_with(".exe"))
                    .then(|| a["browser_download_url"].as_str().map(str::to_string))
                    .flatten()
            })
        });
        Ok(UpdateInfo {
            installer_url,
            version,
        })
    }

    // the setup exe elevates itself or installs per user, so a plain spawn
    // is enough
    #[cfg(windows)]
    fn download_and_run(self) -> Result<(), Error> {
        let url = self.installer_url.ok_or(Error::NoInstaller)?;
        let mut resp = ureq::get(&url).header("User-Agent", "buildcast").call()?;
        let path = std::env::temp_dir().join(format!("buildcast-setup-{}.exe", self.version));
        // as_reader streams without the 10 MB limit of read_to_vec
        let mut file = std::fs::File::create(&path)?;
        std::io::copy(&mut resp.body_mut().as_reader(), &mut file)?;
        drop(file);
        std::process::Command::new(&path).spawn()?;
        Ok(())
    }
}

impl Updater {
    /// Starts the check on a background thread.
    pub(crate) fn check(notify: Notify) -> Updater {
        let updater = Updater {
            state: Arc::new(Mutex::new(UpdateState::Unknown)),
        };
        let state = updater.state.clone();
        std::thread::spawn(move || {
            let next = match UpdateInfo::fetch_latest() {
                Ok(info) if info.version > Version::current() => UpdateState::Available(info),
                Ok(_) => UpdateState::UpToDate,
                // offline or rate-limited: stay quiet
                Err(_) => UpdateState::Unknown,
            };
            *state.lock().unwrap() = next;
            notify();
        });
        updater
    }

    /// Downloads and starts the installer, then exits the app.
    #[cfg(windows)]
    pub(crate) fn install(&self, info: UpdateInfo, notify: Notify) {
        let state = self.state.clone();
        std::thread::spawn(move || {
            *state.lock().unwrap() = UpdateState::Downloading;
            notify();
            match info.download_and_run() {
                Ok(()) => std::process::exit(0),
                Err(err) => {
                    *state.lock().unwrap() = UpdateState::Failed(Arc::new(err));
                    notify();
                }
            }
        });
    }

    pub(crate) fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }
}
