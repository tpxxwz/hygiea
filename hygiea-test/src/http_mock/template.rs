//! 路径模板：cassette 的 `path` 里 `/decks/{deck}`、`/tts/{id:[0-9a-z]+}.mp3` 这种写法。
//!
//! - `{名字}`：匹配一段非空、不含 `/` 的路径
//! - `{名字:正则}`：整段要满足正则（自动加首尾锚定），正则本身不能匹配空串或带 `/` 的串，启动时检查
//! - 其他字符原样匹配，整条路径都要对上；正则里可以有 `{1,32}` 这种花括号，按配对找变量的结尾

use std::sync::LazyLock;

use hygiea_core::{Result, err};
use regex::Regex;

use super::error::HttpMockErr;
use super::route;

static NAME: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").ok());

/// 编译好的路径模板
#[derive(Clone)]
pub(crate) struct PathTemplate {
    /// 整条路径的正则，带首尾锚定
    pub regex: Regex,
    /// 没有变量：路由时比模板优先
    pub literal: bool,
}

impl PathTemplate {
    pub fn new(path: &str) -> Result<Self> {
        let invalid = |cause: String| err!(HttpMockErr::InvalidTemplate, path).with_source(cause);
        let (pattern, vars) = parse(path).map_err(invalid)?;
        for (name, constraint) in &vars {
            if let Some(constraint) = constraint {
                check_constraint(name, constraint).map_err(invalid)?;
            }
        }
        let regex = Regex::new(&pattern).map_err(|e| invalid(e.to_string()))?;
        Ok(Self {
            regex,
            literal: vars.is_empty(),
        })
    }

    /// 实际路径匹配上时，返回各变量的值
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

/// 变量的正则不能匹配空串，也不能匹配带 `/` 的串
fn check_constraint(name: &str, constraint: &str) -> Result<(), String> {
    regex::Regex::new(constraint).map_err(|e| format!("regex of `{name}`: {e}"))?;
    let example = route::intersection(&format!("^(?:{constraint})$"), r"^(?s:|.*/.*)$")?;
    match example {
        Some(example) => Err(format!(
            "{{{name}:{constraint}}} can match empty or `/`, e.g. `{example}`"
        )),
        None => Ok(()),
    }
}

/// 路径里的变量：名字、正则（没写是 None）
type Vars = Vec<(String, Option<String>)>;

/// 解析成整条路径的正则，和各变量的名字、正则
fn parse(path: &str) -> Result<(String, Vars), String> {
    let name_re = NAME.as_ref().ok_or("name regex")?;
    let mut pattern = String::from("^");
    let mut vars: Vars = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let literal = &rest[..open];
        if literal.contains('}') {
            return Err("unmatched `}`".into());
        }
        pattern.push_str(&regex::escape(literal));
        let (name, constraint, len) = parse_var(&rest[open + 1..])?;
        if !name_re.is_match(&name) {
            return Err(format!("invalid variable name `{name}`"));
        }
        if vars.iter().any(|(n, _)| *n == name) {
            return Err(format!("duplicate variable `{name}`"));
        }
        match &constraint {
            Some(constraint) => pattern.push_str(&format!("(?P<{name}>(?:{constraint}))")),
            None => pattern.push_str(&format!("(?P<{name}>[^/]+)")),
        }
        vars.push((name, constraint));
        rest = &rest[open + 1 + len..];
    }
    if rest.contains('}') {
        return Err("unmatched `}`".into());
    }
    pattern.push_str(&regex::escape(rest));
    pattern.push('$');
    Ok((pattern, vars))
}

/// `{` 之后的部分：返回名字、正则（没写是 None）、连同结尾 `}` 一共多少字节
fn parse_var(s: &str) -> Result<(String, Option<String>, usize), String> {
    let mut depth = 1;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let body = &s[..i];
                    let (name, constraint) = match body.split_once(':') {
                        Some((name, constraint)) => (name, Some(constraint.to_string())),
                        None => (body, None),
                    };
                    if constraint.as_deref() == Some("") {
                        return Err(format!("empty regex for `{name}`"));
                    }
                    return Ok((name.to_string(), constraint, i + 1));
                }
            }
            _ => {}
        }
    }
    Err("unclosed `{`".into())
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
        // 不能为空
        assert!(t.params("/tts//1.mp3").is_none());
        assert!(!t.literal);
    }

    #[test]
    fn plain_path_is_literal() {
        let t = PathTemplate::new("/decks/folders").unwrap();
        assert!(t.literal);
        assert!(t.params("/decks/folders").is_some());
        assert!(t.params("/decks/folders/x").is_none());
    }

    #[test]
    fn regex_constraint() {
        let t = PathTemplate::new("/items/{id:[0-9]{1,3}}/{name:[a-z]+}.txt").unwrap();
        assert_eq!(
            t.params("/items/42/abc.txt").unwrap(),
            [("id".into(), "42".into()), ("name".into(), "abc".into())]
        );
        assert!(t.params("/items/1234/abc.txt").is_none());
        assert!(t.params("/items/4x/abc.txt").is_none());
    }

    #[test]
    fn invalid_templates() {
        for path in [
            "/a/{id:.*}",      // 能匹配空串
            "/a/{id:[a-z/]+}", // 能匹配 /
            "/a/{id",
            "/a/id}",
            "/a/{1d}",
            "/a/{id}/{id}",
            "/a/{id:}",
            "/a/{id:(}",
        ] {
            assert!(PathTemplate::new(path).is_err(), "{path}");
        }
    }
}
