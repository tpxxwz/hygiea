//! 路径模板：cassette 的 `path` 里 `/decks/{deck}` 这种写法

use std::sync::LazyLock;

use hygiea::{HyErr, err};
use regex::Regex;

use super::error::HttpMockErr;

static PLACEHOLDER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").ok());

/// 编译好的路径模板。`{名字}` 匹配一段不含 `/` 的路径，其他字符原样匹配，整条路径都要对上
#[derive(Clone)]
pub(crate) struct PathTemplate {
    pub regex: Regex,
}

impl PathTemplate {
    pub fn new(path: &str) -> Result<Self, HyErr> {
        let invalid = || err!(HttpMockErr::InvalidTemplate, path);
        let placeholder = PLACEHOLDER.as_ref().ok_or_else(invalid)?;
        let mut pattern = String::from("^");
        let mut last = 0;
        for caps in placeholder.captures_iter(path) {
            let (Some(whole), Some(name)) = (caps.get(0), caps.get(1)) else {
                continue;
            };
            let literal = &path[last..whole.start()];
            pattern.push_str(&regex::escape(literal));
            pattern.push_str(&format!("(?P<{}>[^/]+)", name.as_str()));
            last = whole.end();
        }
        pattern.push_str(&regex::escape(&path[last..]));
        pattern.push('$');
        let regex = Regex::new(&pattern).map_err(|e| invalid().with_source(e))?;
        Ok(Self { regex })
    }

    /// 实际路径匹配上时，返回各占位符的值
    pub fn params(&self, path: &str) -> Option<Vec<(String, String)>> {
        let caps = self.regex.captures(path)?;
        Some(
            self.regex
                .capture_names()
                .flatten()
                .filter_map(|name| Some((name.to_string(), caps.name(name)?.as_str().to_string())))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_whole_path_by_segment() {
        let t = PathTemplate::new("/tts/{voice}/{id}.mp3").unwrap();
        assert_eq!(
            t.params("/tts/f002/1.mp3").unwrap(),
            [("voice".into(), "f002".into()), ("id".into(), "1".into())]
        );
        assert!(t.params("/tts/f002/x/1.mp3").is_none());
        assert!(t.params("/tts/f002/1.wav").is_none());
        assert!(t.params("/tts/f002/1xmp3").is_none());
    }

    #[test]
    fn plain_path_has_no_placeholders() {
        let t = PathTemplate::new("/decks/folders").unwrap();
        assert!(t.params("/decks/folders").is_some());
        assert!(t.params("/decks/folders/x").is_none());
    }
}
