//! Headless-browser fallback for `read`. Some open-access hosts answer plain
//! HTTP clients with a JavaScript check page (such as an AWS WAF challenge)
//! and serve the content once it has run. `read` loads only such pages here
//! (see `pdf::is_js_challenge`), never ones refused with 403 or 429, and
//! captures PDF downloads. The browser keeps its default automation flag and
//! user agent: hosts that block automated browsers outright are reported as
//! blocked, never worked around.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use headless_chrome::protocol::cdp::Browser::{
    SetDownloadBehavior, SetDownloadBehaviorBehaviorOption,
};
use headless_chrome::{LaunchOptions, Tab};
use serde::Deserialize;

/// Path to a Chromium-family binary, overriding discovery.
pub const BROWSER_VAR: &str = "PAPERS_CHROME";
/// How long one URL may take to download or settle (JavaScript checks included).
const LOAD_TIMEOUT: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(500);
/// Consecutive polls a page must look ready, so an interim page that is about
/// to reload is not mistaken for the destination.
const READY_POLLS: u32 = 2;
/// Chrome's suffix for a download still in progress.
const PARTIAL: &str = ".crdownload";

pub enum Loaded {
    /// A finished download (not yet checked to be a PDF).
    Download(Vec<u8>),
    /// A rendered page with no check in progress.
    Page { html: String, url: String },
    /// The host refused the automated browser.
    Blocked(String),
}

#[derive(Debug, PartialEq, Eq)]
enum PageState {
    Loading,
    Challenge,
    Blocked(String),
    Ready,
}

/// One headless Chromium with a private download folder, removed on drop.
pub struct Browser {
    // Field order matters: the tab must close before the process exits.
    tab: Arc<Tab>,
    _chrome: headless_chrome::Browser,
    downloads: PathBuf,
}

impl Browser {
    pub fn launch() -> Result<Self, String> {
        let path = find_chrome()?;
        let downloads = std::env::temp_dir().join(format!("papers-browser-{}", std::process::id()));
        std::fs::create_dir_all(&downloads).map_err(|e| format!("{}: {e}", downloads.display()))?;
        let options = LaunchOptions::default_builder()
            .path(Some(path.clone()))
            .headless(true)
            .idle_browser_timeout(LOAD_TIMEOUT * 4)
            .build()
            .map_err(|e| format!("browser options: {e}"))?;
        let chrome = headless_chrome::Browser::new(options)
            .map_err(|e| format!("launching {}: {e}", path.display()))?;
        let tab = chrome
            .new_tab()
            .map_err(|e| format!("opening a browser tab: {e}"))?;
        tab.set_default_timeout(LOAD_TIMEOUT);
        tab.call_method(SetDownloadBehavior {
            behavior: SetDownloadBehaviorBehaviorOption::AllowAndName,
            browser_context_id: None,
            download_path: Some(downloads.to_string_lossy().into_owned()),
            events_enabled: None,
        })
        .map_err(|e| format!("enabling browser downloads: {e}"))?;
        Ok(Browser {
            tab,
            _chrome: chrome,
            downloads,
        })
    }

    /// Loads `url` and waits until it downloads a file, settles on a page with
    /// no check in progress, or shows a block page.
    pub fn load(&self, url: &str, byte_limit: u64) -> Result<Loaded, String> {
        let before = self.download_names()?;
        let previous = self.tab.get_url();
        // A URL that downloads aborts its navigation, but the file still lands,
        // so a navigation error is only reported if nothing else happens.
        let navigation = self.tab.navigate_to(url).err();
        let deadline = Instant::now() + LOAD_TIMEOUT;
        let mut ready_polls = 0;
        let mut committed = false;
        let last = loop {
            if let Some(bytes) = self.new_download(&before, byte_limit)? {
                return Ok(Loaded::Download(bytes));
            }
            // A download leaves the previous page on screen; only the page of a
            // committed navigation counts, so a stale page is never reported.
            committed = committed || previous == url || self.tab.get_url() != previous;
            // A probe can fail while the page navigates; it is retried until the deadline.
            let state = if committed {
                self.page_state()
            } else {
                Ok(PageState::Loading)
            };
            match &state {
                Ok(PageState::Blocked(why)) => return Ok(Loaded::Blocked(why.clone())),
                Ok(PageState::Ready) => {
                    ready_polls += 1;
                    if ready_polls >= READY_POLLS {
                        let html = self
                            .tab
                            .get_content()
                            .map_err(|e| format!("reading the page: {e}"))?;
                        return Ok(Loaded::Page {
                            html,
                            url: self.tab.get_url(),
                        });
                    }
                }
                _ => ready_polls = 0,
            }
            if Instant::now() >= deadline {
                break state;
            }
            sleep(POLL);
        };
        let secs = LOAD_TIMEOUT.as_secs();
        Err(match (last, navigation) {
            (Ok(PageState::Challenge), _) => {
                format!("a browser check did not clear within {secs}s (it may need a human)")
            }
            (_, Some(e)) => format!("navigation failed: {e}"),
            (Err(e), None) => format!("timed out after {secs}s; last page probe failed: {e}"),
            (Ok(_), None) if !committed => {
                format!("neither a page nor a finished download within {secs}s")
            }
            (Ok(_), None) => format!("timed out after {secs}s"),
        })
    }

    fn page_state(&self) -> Result<PageState, String> {
        const PROBE: &str = "JSON.stringify({ready: document.readyState, title: document.title, \
            status: (performance.getEntriesByType('navigation')[0] || {}).responseStatus || 0, \
            text: (document.body ? document.body.innerText : '').slice(0, 3000)})";
        #[derive(Deserialize)]
        struct Probe {
            ready: String,
            title: String,
            status: u16,
            text: String,
        }
        let value = self
            .tab
            .evaluate(PROBE, false)
            .map_err(|e| format!("probing the page: {e}"))?
            .value
            .ok_or("probing the page returned nothing")?;
        let json = value
            .as_str()
            .ok_or("probing the page returned a non-string")?;
        let p: Probe = serde_json::from_str(json).map_err(|e| format!("page probe: {e}"))?;
        Ok(classify(&p.ready, &p.title, p.status, &p.text))
    }

    fn download_names(&self) -> Result<HashSet<String>, String> {
        let entries = std::fs::read_dir(&self.downloads)
            .map_err(|e| format!("{}: {e}", self.downloads.display()))?;
        Ok(entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect())
    }

    /// The bytes of a download that finished since `before`, if any.
    fn new_download(
        &self,
        before: &HashSet<String>,
        byte_limit: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        for name in self.download_names()? {
            if before.contains(&name) || name.ends_with(PARTIAL) {
                continue;
            }
            let path = self.downloads.join(&name);
            let size = std::fs::metadata(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .len();
            if size > byte_limit {
                return Err(format!("download is larger than {} MiB", byte_limit >> 20));
            }
            return std::fs::read(&path)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display()));
        }
        Ok(None)
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.downloads) {
            eprintln!("papers: could not remove {}: {e}", self.downloads.display());
        }
    }
}

/// Reads a page's state from its load state, title, HTTP status, and leading text.
fn classify(ready: &str, title: &str, status: u16, text: &str) -> PageState {
    const CHALLENGE: [&str; 7] = [
        "just a moment",
        "checking your browser",
        "verify you are human",
        "verifying you are human",
        "human verification",
        "enable javascript and cookies",
        "please wait while",
    ];
    const BLOCKED: [&str; 6] = [
        "access denied",
        "content blocked",
        "automated script",
        "unusual traffic",
        "request blocked",
        "forbidden",
    ];
    let seen = format!("{title}\n{text}").to_lowercase();
    // AWS WAF answers a challenge with 202 until its script reloads the page.
    if status == 202 || CHALLENGE.iter().any(|m| seen.contains(m)) {
        return PageState::Challenge;
    }
    if matches!(status, 401 | 403 | 451) || BLOCKED.iter().any(|m| seen.contains(m)) {
        let what = if title.trim().is_empty() {
            "no title"
        } else {
            title.trim()
        };
        return PageState::Blocked(format!("HTTP {status}: {what}"));
    }
    if ready == "complete" {
        PageState::Ready
    } else {
        PageState::Loading
    }
}

/// The browser binary: `PAPERS_CHROME`, else the newest Playwright-installed
/// Chromium, else an installed Google Chrome.
pub fn find_chrome() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var(BROWSER_VAR) {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("{BROWSER_VAR}={} is not a file", path.display()))
        };
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let caches = [
        home.as_ref()
            .map(|h| h.join("Library/Caches/ms-playwright")),
        home.as_ref().map(|h| h.join(".cache/ms-playwright")),
    ];
    for cache in caches.into_iter().flatten() {
        if let Some(path) = newest_playwright_chromium(&cache) {
            return Ok(path);
        }
    }
    let chrome = Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    if chrome.is_file() {
        return Ok(chrome.to_owned());
    }
    Err(format!(
        "no Chromium found: install one with `bunx playwright install chromium-headless-shell`, \
         or set {BROWSER_VAR} to a Chrome or Chromium binary"
    ))
}

/// The newest browser binary in a Playwright cache, preferring the headless shell.
fn newest_playwright_chromium(cache: &Path) -> Option<PathBuf> {
    let mut installs: Vec<(u32, u8, PathBuf)> = std::fs::read_dir(cache)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (rank, revision) = playwright_install(&name)?;
            Some((revision, rank, e.path()))
        })
        .collect();
    // Newest revision first; the headless shell before full Chromium.
    installs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    installs
        .into_iter()
        .find_map(|(_, _, dir)| browser_binary(&dir))
}

/// `(preference rank, revision)` for a Playwright Chromium install directory name.
fn playwright_install(name: &str) -> Option<(u8, u32)> {
    let (rank, revision) = if let Some(r) = name.strip_prefix("chromium_headless_shell-") {
        (0, r)
    } else {
        (1, name.strip_prefix("chromium-")?)
    };
    Some((rank, revision.parse().ok()?))
}

/// The executable inside one Playwright install directory, on macOS or Linux.
fn browser_binary(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .flat_map(|platform| {
            let p = platform.path();
            [
                p.join("chrome-headless-shell"),
                p.join("Chromium.app/Contents/MacOS/Chromium"),
                p.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"),
                p.join("chrome"),
            ]
        })
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_challenges_blocks_and_ready_pages() {
        assert_eq!(
            classify("complete", "Item - figshare", 202, ""),
            PageState::Challenge
        );
        assert_eq!(
            classify("complete", "Just a moment...", 403, ""),
            PageState::Challenge
        );
        assert_eq!(
            classify(
                "complete",
                "Content Blocked",
                403,
                "We have detected that you may be using an automated script"
            ),
            PageState::Blocked("HTTP 403: Content Blocked".to_owned())
        );
        assert_eq!(
            classify("complete", "Access Denied", 403, ""),
            PageState::Blocked("HTTP 403: Access Denied".to_owned())
        );
        assert_eq!(
            classify("interactive", "Paper", 200, ""),
            PageState::Loading
        );
        assert_eq!(
            classify("complete", "Paper", 200, "Abstract ..."),
            PageState::Ready
        );
    }

    #[test]
    fn parses_playwright_install_names() {
        assert_eq!(
            playwright_install("chromium_headless_shell-1248"),
            Some((0, 1248))
        );
        assert_eq!(playwright_install("chromium-1250"), Some((1, 1250)));
        assert_eq!(playwright_install("ffmpeg-1013"), None);
        assert_eq!(playwright_install("chromium-tip-of-tree-1"), None);
    }
}
