//! What `fetch`, `read` and `search` print first about a work the registry
//! already holds, so that no agent fetches or reads it a second time.

use serde::Serialize;

use crate::config::Config;
use crate::http;
use crate::registry::Reference;

/// A recorded text and whether its file still is the recorded one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextState {
    /// Relative to the project root.
    pub path: String,
    /// `ok`, `missing`, or `changed` (the file's sha256 is not the recorded one).
    pub state: &'static str,
}

/// A registry entry as the commands report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Registered {
    pub key: String,
    pub title: Option<String>,
    pub depth: Option<String>,
    pub duplicate_of: Option<String>,
    pub texts: Vec<TextState>,
}

impl Registered {
    /// Reads each recorded text's file to compare its sha256.
    pub fn new(config: &Config, r: &Reference) -> Self {
        let texts = r
            .texts
            .iter()
            .map(|t| TextState {
                path: t.path.clone(),
                state: match std::fs::read(config.root.join(&t.path)) {
                    Ok(bytes) if http::sha256_hex(&bytes) == t.sha256 => "ok",
                    Ok(_) => "changed",
                    Err(_) => "missing",
                },
            })
            .collect();
        Registered {
            key: r.key.clone(),
            title: r.title.clone(),
            depth: r.depth.clone(),
            duplicate_of: r.duplicate_of.clone(),
            texts,
        }
    }

    /// The block, without a final newline:
    ///
    /// ```text
    /// registered: key <key> · depth <depth> · <title>
    /// text: <path>
    /// ```
    ///
    /// One `text:` line per recorded text; ` (missing)` or ` (changed)`
    /// follows a path whose file is not the recorded one.
    pub fn block(&self) -> String {
        let mut out = format!(
            "registered: key {} · depth {}",
            self.key,
            self.depth.as_deref().unwrap_or("unknown")
        );
        if let Some(target) = &self.duplicate_of {
            out += &format!(" · duplicate of {target}");
        }
        out += &format!(" · {}", self.title.as_deref().unwrap_or("(no title)"));
        for t in &self.texts {
            out += &format!("\ntext: {}", t.path);
            if t.state != "ok" {
                out += &format!(" ({})", t.state);
            }
        }
        out
    }
}

/// The first line of a search result that the archive holds: `seen: key 87`,
/// or `seen: key 87, key 90` for several keys. `None` without keys.
pub fn seen_line(keys: &[String]) -> Option<String> {
    (!keys.is_empty()).then(|| {
        let keys: Vec<String> = keys.iter().map(|k| format!("key {k}")).collect();
        format!("seen: {}", keys.join(", "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Text;

    #[test]
    fn block_names_key_depth_title_and_each_text_with_its_state() {
        let dir = std::env::temp_dir().join(format!("papers-registered-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("texts")).unwrap();
        std::fs::write(dir.join("research.toml"), "[papers]\n").unwrap();
        std::fs::write(dir.join("texts/a.txt"), "first").unwrap();
        std::fs::write(dir.join("texts/b.txt"), "edited").unwrap();
        let config = Config::load(Some(&dir.join("research.toml")), &dir).unwrap();
        let text = |path: &str, content: &str| Text {
            path: path.into(),
            sha256: http::sha256_hex(content.as_bytes()),
            origin: None,
            retrieved: None,
            extra: serde_json::Map::new(),
        };
        let mut r = Reference::new("87".into());
        r.title = Some("A Synthetic Title".into());
        r.depth = Some("full".into());
        r.texts = vec![
            text("texts/a.txt", "first"),
            text("texts/b.txt", "second"),
            text("texts/c.txt", "third"),
        ];
        let block = Registered::new(&config, &r).block();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            block,
            "registered: key 87 · depth full · A Synthetic Title\n\
             text: texts/a.txt\n\
             text: texts/b.txt (changed)\n\
             text: texts/c.txt (missing)"
        );
        let mut bare = Reference::new("3".into());
        bare.duplicate_of = Some("87".into());
        assert_eq!(
            Registered::new(&config, &bare).block(),
            "registered: key 3 · depth unknown · duplicate of 87 · (no title)"
        );
    }

    #[test]
    fn seen_line_lists_each_key() {
        assert_eq!(seen_line(&[]), None);
        assert_eq!(seen_line(&["87".into()]).unwrap(), "seen: key 87");
        assert_eq!(
            seen_line(&["87".into(), "lee2026".into()]).unwrap(),
            "seen: key 87, key lee2026"
        );
    }
}
