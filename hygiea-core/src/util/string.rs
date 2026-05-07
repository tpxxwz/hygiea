use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use parking_lot::RwLock;
use regex::Regex;
use crate::{BaseFmtErr, FmtErr};

static PATTERN_CACHE: LazyLock<RwLock<HashMap<String, Arc<Regex>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn get_pattern(regex: &str) -> Result<Arc<Regex>, FmtErr> {
    if let Some(p) = PATTERN_CACHE.read().get(regex) {
        return Ok(Arc::clone(p));
    }
    let mut cache = PATTERN_CACHE.write();
    if let Some(p) = cache.get(regex) {
        return Ok(Arc::clone(p));
    }
    let pattern = Arc::new(Regex::new(regex).map_err(|e| {
        BaseFmtErr::RegexError.to_err(serde_json::json!({ "cause": e.to_string() }))
    })?);
    cache.insert(regex.to_string(), Arc::clone(&pattern));
    Ok(pattern)
}

pub fn is_match(resource: &str, regex: &str) -> Result<bool, FmtErr> {
    Ok(get_pattern(regex)?.is_match(resource))
}

pub fn regex_replace(resource: &str, regex: &str, replacement: &str) -> Result<String, FmtErr> {
    Ok(get_pattern(regex)?.replace_all(resource, replacement).into_owned())
}

pub fn regex_find(resource: &str, regex: &str) -> Result<Option<String>, FmtErr> {
    let pattern = get_pattern(regex)?;
    Ok(pattern.captures(resource).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()))
}

pub fn regex_find_double(resource: &str, regex: &str) -> Result<(Option<String>, Option<String>), FmtErr> {
    let pattern = get_pattern(regex)?;
    let caps = pattern.captures(resource);
    let g1 = caps.as_ref().and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
    let g2 = caps.as_ref().and_then(|c| c.get(2)).map(|m| m.as_str().to_string());
    Ok((g1, g2))
}

pub fn regex_find_all(resource: &str, regex: &str) -> Result<Vec<String>, FmtErr> {
    Ok(get_pattern(regex)?.find_iter(resource).map(|m| m.as_str().to_string()).collect())
}
