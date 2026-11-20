//! Whether a newer release is out: one anonymous call to GitHub a day,
//! cached on disk, and never a reason for a command to fail or wait long.

use std::path::PathBuf;
use std::time::Duration;

use cp_api::Version;

/// Where releases are published.
pub(crate) const RELEASES: &str = "https://github.com/Savid/clanker-proxy/releases";
/// GitHub's API for the newest stable release.
pub(crate) const LATEST_API: &str =
    "https://api.github.com/repos/Savid/clanker-proxy/releases/latest";

const DAY_SECS: i64 = 86_400;

/// What the cache says.
enum Cached {
    /// Under a day old; the answer may be "unknown" after a failed lookup.
    Fresh(Option<Version>),
    Stale,
}

/// How to look up the newest release.
#[derive(Clone, Debug)]
pub(crate) struct Updates {
    pub(crate) url: String,
    /// The cache file; without one, every check asks GitHub.
    pub(crate) cache: Option<PathBuf>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl Updates {
    /// The newest release, from the cache when it is under a day old. A
    /// failed lookup is cached too, so an offline machine asks once a day.
    pub(crate) async fn latest(&self) -> Option<Version> {
        if let Cached::Fresh(answer) = self.cached() {
            return answer;
        }
        let found = self.fetch().await;
        if let Some(path) = &self.cache {
            let line = format!("{} {}\n", now(), found.as_ref().map_or("", Version::as_str));
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, line);
        }
        found
    }

    fn cached(&self) -> Cached {
        let Some(text) = self
            .cache
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
        else {
            return Cached::Stale;
        };
        let (at, tag) = text.trim().split_once(' ').unwrap_or((text.trim(), ""));
        match at.parse::<i64>() {
            Ok(at) if at <= now() && now() - at < DAY_SECS => {
                Cached::Fresh(Version::try_from(tag.to_string()).ok())
            }
            _ => Cached::Stale,
        }
    }

    async fn fetch(&self) -> Option<Version> {
        #[derive(serde::Deserialize)]
        struct Release {
            tag_name: String,
        }
        let client = reqwest::Client::builder()
            .user_agent("cpctl")
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;
        let resp = client
            .get(&self.url)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let r: Release = serde_json::from_slice(&resp.bytes().await.ok()?).ok()?;
        Version::try_from(r.tag_name).ok()
    }
}

/// A line saying `latest` is out, when it is newer than `running`.
pub(crate) fn notice(latest: Option<&Version>, running: &str) -> Option<String> {
    let running = Version::try_from(running.to_string()).ok()?;
    let latest = latest.filter(|l| l.newer_than(&running))?;
    Some(format!(
        "update available: {latest} (this cpd runs {running}); see {RELEASES}/tag/{latest}"
    ))
}
