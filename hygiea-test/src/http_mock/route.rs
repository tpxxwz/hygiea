//! 路由冲突检查：两个正则转成 DFA，同时走一遍，看有没有两边都能完整匹配的串。
//! Rust 的 regex 没有反向引用和环视，写得出来的都是正则语言，交集能精确判定；
//! 有交集时顺带给出一个最短的例子，报错时用

use std::collections::{HashSet, VecDeque};

use regex_automata::dfa::{Automaton, dense};
use regex_automata::{Anchored, Input};

/// 单个 DFA 的大小上限；Unicode 类（`\w`、`.`）可能很大，超了就报无法判断
const DFA_SIZE_LIMIT: usize = 4 << 20;
/// 最多走多少个状态对
const MAX_PAIRS: usize = 200_000;

/// 例子里优先用的字节：字母、数字，然后才是别的
fn preferred_bytes() -> impl Iterator<Item = u8> {
    (b'a'..=b'z')
        .chain(b'0'..=b'9')
        .chain(b'A'..=b'Z')
        .chain((0..=255u8).filter(|b| !b.is_ascii_alphanumeric()))
}

fn build(pattern: &str) -> Result<dense::DFA<Vec<u32>>, String> {
    dense::Builder::new()
        .configure(
            dense::Config::new()
                .dfa_size_limit(Some(DFA_SIZE_LIMIT))
                .determinize_size_limit(Some(DFA_SIZE_LIMIT)),
        )
        .build(pattern)
        .map_err(|e| format!("can't check regex `{pattern}`: {e}"))
}

/// `a`、`b` 是带首尾锚定的正则，两边都能完整匹配的串存在时返回一个例子
pub(crate) fn intersection(a: &str, b: &str) -> Result<Option<String>, String> {
    let (da, db) = (build(a)?, build(b)?);
    let input = Input::new("").anchored(Anchored::Yes);
    let start = |dfa: &dense::DFA<Vec<u32>>, pattern: &str| {
        dfa.start_state_forward(&input)
            .map_err(|e| format!("can't check regex `{pattern}`: {e}"))
    };
    let first = (start(&da, a)?, start(&db, b)?);
    let mut seen = HashSet::from([first]);
    let mut queue = VecDeque::from([(first, Vec::new())]);
    while let Some(((sa, sb), bytes)) = queue.pop_front() {
        // DFA 的匹配晚一个字节，走到输入结尾再看
        if da.is_match_state(da.next_eoi_state(sa)) && db.is_match_state(db.next_eoi_state(sb)) {
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
        for byte in preferred_bytes() {
            let (na, nb) = (da.next_state(sa, byte), db.next_state(sb, byte));
            if da.is_dead_state(na)
                || db.is_dead_state(nb)
                || da.is_quit_state(na)
                || db.is_quit_state(nb)
            {
                continue;
            }
            if seen.insert((na, nb)) {
                if seen.len() > MAX_PAIRS {
                    return Err(format!("can't check `{a}` and `{b}`: too many states"));
                }
                let mut next = bytes.clone();
                next.push(byte);
                queue.push_back(((na, nb), next));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_example_or_none() {
        let seg = r"^/a/(?:[^/]+)$";
        assert_eq!(
            intersection(seg, r"^/a/(?:[0-9]+)$").unwrap().as_deref(),
            Some("/a/0")
        );
        assert_eq!(
            intersection(r"^/a/(?:[0-9]+)$", r"^/a/(?:[a-z]+)$").unwrap(),
            None
        );
        assert_eq!(
            intersection(r"^/a/(?:d_.*)$", seg).unwrap().as_deref(),
            Some("/a/d_")
        );
        assert_eq!(
            intersection(r"^/a/(?:[^/]+)\.mp3$", r"^/a/(?:x)$").unwrap(),
            None
        );
        assert_eq!(
            intersection(r"^/a/(?:[^/]+)\.mp3$", seg)
                .unwrap()
                .as_deref(),
            Some("/a/a.mp3")
        );
    }
}
