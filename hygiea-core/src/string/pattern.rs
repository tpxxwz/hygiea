//! 正则工具，编译结果按 LRU 缓存。

use crate::{BaseErr, HyErr, ResultExt, err};
use lru::LruCache;
use parking_lot::RwLock;
use regex::Regex;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock};

/// 编译后的正则最多缓存这么多条，超出后按 LRU 淘汰最久未插入/未失效命中的条目。
const PATTERN_CACHE_CAPACITY: usize = 1024;

static PATTERN_CACHE: LazyLock<RwLock<LruCache<String, Arc<Regex>>>> = LazyLock::new(|| {
    RwLock::new(LruCache::new(
        NonZeroUsize::new(PATTERN_CACHE_CAPACITY).expect("pattern cache capacity must be non-zero"),
    ))
});

/// 对应 `Regex::is_match`：`resource` 中是否存在匹配。
pub fn is_match(regex: &str, resource: &str) -> Result<bool, HyErr> {
    Ok(get_pattern(regex)?.is_match(resource))
}

/// 对应 `Regex::replace`：只替换第一个匹配。
pub fn replace(regex: &str, resource: &str, replacement: &str) -> Result<String, HyErr> {
    Ok(get_pattern(regex)?
        .replace(resource, replacement)
        .into_owned())
}

/// 对应 `Regex::replace_all`：替换所有匹配。
pub fn replace_all(regex: &str, resource: &str, replacement: &str) -> Result<String, HyErr> {
    Ok(get_pattern(regex)?
        .replace_all(resource, replacement)
        .into_owned())
}

/// 对应 `Regex::captures`：第一次匹配的所有捕获组（下标从 0 对应 group 1，不含
/// group 0 整体匹配），regex 没有匹配上时返回 `None`。
pub fn captures(regex: &str, resource: &str) -> Result<Option<Vec<Option<String>>>, HyErr> {
    let pattern = get_pattern(regex)?;
    Ok(pattern.captures(resource).map(|caps| capture_groups(&caps)))
}

/// 对应 `Regex::captures_iter`：每一次匹配各自的所有捕获组（某次匹配里某个组没捕获到就是 None）。
pub fn captures_all(regex: &str, resource: &str) -> Result<Vec<Vec<Option<String>>>, HyErr> {
    let pattern = get_pattern(regex)?;
    Ok(pattern
        .captures_iter(resource)
        .map(|caps| capture_groups(&caps))
        .collect())
}

/// 把一次匹配的 `Captures` 拆成各个捕获组（跳过 group 0 整体匹配），`captures`/`captures_all` 共用。
fn capture_groups(caps: &regex::Captures) -> Vec<Option<String>> {
    (1..caps.len())
        .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
        .collect()
}

/// 对应 `Regex::find_iter`：所有匹配的整体文本（不是捕获组）。
pub fn find_all(regex: &str, resource: &str) -> Result<Vec<String>, HyErr> {
    Ok(get_pattern(regex)?
        .find_iter(resource)
        .map(|m| m.as_str().to_string())
        .collect())
}

/// `captures` 的薄封装：只取第一个捕获组。`Regex` 本身没有这个方法。
pub fn regex_find(regex: &str, resource: &str) -> Result<Option<String>, HyErr> {
    Ok(captures(regex, resource)?
        .and_then(|groups| groups.into_iter().next())
        .flatten())
}

/// `captures` 的薄封装：取前两个捕获组。`Regex` 本身没有这个方法。
pub fn regex_find_double(
    regex: &str,
    resource: &str,
) -> Result<(Option<String>, Option<String>), HyErr> {
    let mut groups = captures(regex, resource)?.unwrap_or_default().into_iter();
    Ok((groups.next().flatten(), groups.next().flatten()))
}

/// 命中路径只用 `peek`（不提升 LRU 顺序），避免每次命中都要抢写锁；
/// 未命中才升级写锁，`get` 这里只是为了应对并发下两个线程同时编译同一个新正则的竞态。
fn get_pattern(regex: &str) -> Result<Arc<Regex>, HyErr> {
    if let Some(p) = PATTERN_CACHE.read().peek(regex) {
        return Ok(Arc::clone(p));
    }
    let mut cache = PATTERN_CACHE.write();
    if let Some(p) = cache.get(regex) {
        return Ok(Arc::clone(p));
    }
    let pattern = Arc::new(Regex::new(regex).wrap_err(|| err!(BaseErr::RegexError, regex))?);
    cache.put(regex.to_string(), Arc::clone(&pattern));
    Ok(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== is_match =====

    #[test]
    fn test_is_match_true() {
        assert!(is_match(r"\d+", "abc123").unwrap());
    }

    #[test]
    fn test_is_match_false() {
        assert!(!is_match(r"\d+", "abc").unwrap());
    }

    #[test]
    fn test_is_match_invalid_regex() {
        assert!(is_match("(", "abc").is_err());
    }

    // ===== replace / replace_all =====

    #[test]
    fn test_replace_only_first() {
        let result = replace(r"\d", "a1b2c3", "_").unwrap();
        assert_eq!(result, "a_b2c3");
    }

    #[test]
    fn test_replace_all() {
        let result = replace_all(r"\d", "a1b2c3", "_").unwrap();
        assert_eq!(result, "a_b_c_");
    }

    #[test]
    fn test_replace_all_no_match() {
        let result = replace_all(r"\d", "abc", "_").unwrap();
        assert_eq!(result, "abc");
    }

    // ===== captures =====

    #[test]
    fn test_captures_basic() {
        // 正则 `user=(\w+)(?:\s+role=(\w+))?` 拆解：
        // - `user=`               字面量前缀
        // - `(\w+)`               group 1：用户名
        // - `(?:\s+role=(\w+))?`  外层 `(?:...)?` 是非捕获组，表示 " role=角色" 这一段整体可有可无；
        //                         里面嵌套的 `(\w+)` 才是 group 2（角色），这段缺失时 group 2 为 None
        //   - `\s+`               一个或多个空白字符（空格/制表符等），匹配 "role=" 前面的分隔空格
        //   - `role=`             字面量
        //   - `(\w+)`             group 2：角色名
        //
        // resource 里其实有两处能匹配上的片段，但 `captures` 只看第一次匹配，
        // 后面那个 "user=bob" 完全不会被看到——这就是它和 `captures_all` 的核心区别。
        let regex = r"user=(\w+)(?:\s+role=(\w+))?";
        let resource = "user=alice role=admin; user=bob";

        let groups = captures(regex, resource).unwrap().unwrap();

        assert_eq!(
            groups,
            vec![Some("alice".to_string()), Some("admin".to_string())]
        );
    }

    #[test]
    fn test_captures_optional_group_missing() {
        // 同一个正则，resource 这次没有 " role=xxx" 这一段，
        // 对应 group 2（角色）在这次匹配里没捕获到，是 None。
        let regex = r"user=(\w+)(?:\s+role=(\w+))?";
        let groups = captures(regex, "user=bob").unwrap().unwrap();
        assert_eq!(groups, vec![Some("bob".to_string()), None]);
    }

    #[test]
    fn test_captures_no_match() {
        assert_eq!(captures(r"(\d+)", "abc").unwrap(), None);
    }

    #[test]
    fn test_captures_no_groups() {
        assert_eq!(captures(r"abc", "abc").unwrap(), Some(vec![]));
    }

    // ===== captures_all =====

    #[test]
    fn test_captures_all_basic() {
        // 正则 `v(\d+)\.(\d+)(?:\.(\d+))?` 拆解：
        // - `v`               字面量前缀
        // - `(\d+)`           group 1：主版本号
        // - `\.`              字面量 `.`（转义，区别于匹配任意字符的 `.`）
        // - `(\d+)`           group 2：次版本号
        // - `(?:\.(\d+))?`    外层 `(?:...)?` 是非捕获组，表示这一段（".修订号"）整体可有可无；
        //                     里面嵌套的 `(\d+)` 才是 group 3（修订号），这次匹配没这一段时 group 3 就是 None
        let regex = r"v(\d+)\.(\d+)(?:\.(\d+))?";
        let resource = "v1.2.3 v2.5 v10.0.1";

        let result = captures_all(regex, resource).unwrap();

        assert_eq!(
            result,
            vec![
                vec![
                    Some("1".to_string()),
                    Some("2".to_string()),
                    Some("3".to_string())
                ],
                vec![Some("2".to_string()), Some("5".to_string()), None],
                vec![
                    Some("10".to_string()),
                    Some("0".to_string()),
                    Some("1".to_string())
                ],
            ]
        );
    }

    #[test]
    fn test_captures_all_no_match() {
        let result = captures_all(r"v(\d+)", "no version here").unwrap();
        assert!(result.is_empty());
    }

    // ===== find_all =====

    #[test]
    fn test_find_all_basic() {
        // 跟 `test_captures_all_basic` 用同一个正则 `v(\d+)\.(\d+)(?:\.(\d+))?`，
        // 但 `find_all` 不关心括号里的捕获组，每次匹配只要整体匹配到的文本（相当于 group 0），
        // 括号在这里只是用来限定"版本号"这个形状，不会被拆开返回。
        let regex = r"v(\d+)\.(\d+)(?:\.(\d+))?";
        let resource = "v1.2.3 v2.5 v10.0.1";

        let result = find_all(regex, resource).unwrap();

        assert_eq!(result, vec!["v1.2.3", "v2.5", "v10.0.1"]);
    }

    #[test]
    fn test_find_all_no_match() {
        let result = find_all(r"\d+", "abc").unwrap();
        assert!(result.is_empty());
    }

    // ===== regex_find / regex_find_double =====

    #[test]
    fn test_regex_find_basic() {
        assert_eq!(
            regex_find(r"id=(\d+)", "id=42").unwrap(),
            Some("42".to_string())
        );
    }

    #[test]
    fn test_regex_find_no_match() {
        assert_eq!(regex_find(r"id=(\d+)", "no match").unwrap(), None);
    }

    #[test]
    fn test_regex_find_double_basic() {
        let result = regex_find_double(r"(\w+)@(\w+)", "user@host").unwrap();
        assert_eq!(result, (Some("user".to_string()), Some("host".to_string())));
    }

    #[test]
    fn test_regex_find_double_second_group_missing() {
        let result = regex_find_double(r"(\w+)", "user").unwrap();
        assert_eq!(result, (Some("user".to_string()), None));
    }

    #[test]
    fn test_regex_find_double_no_match() {
        let result = regex_find_double(r"(\w+)@(\w+)", "no match").unwrap();
        assert_eq!(result, (None, None));
    }
}
