//! 构造请求头

use http::{HeaderMap, HeaderName, HeaderValue};

/// 用 `(名字, 值)` 列表拼一个 `HeaderMap`。名字大小写随意（会统一成小写），同名的后一个覆盖前一个；
/// 名字或值不合法时直接 panic，只给测试用
pub fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
    // 用 insert 逐个写入才是覆盖语义；直接 collect 成 HeaderMap 走的是 append，
    // 同名会变成多值而不是覆盖，和上面的文档矛盾
    let mut map = HeaderMap::new();
    for (k, v) in pairs {
        map.insert(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字大小写随意，统一存成小写
    #[test]
    fn names_are_lowercased() {
        let map = header_map(&[("X-Foo", "1")]);
        assert!(map.contains_key("x-foo"));
    }

    /// 同名的后一个覆盖前一个，不是追加成多值
    #[test]
    fn later_pair_overrides_earlier_same_name() {
        let map = header_map(&[("x-a", "1"), ("X-A", "2")]);
        assert_eq!(map.len(), 1);
        assert_eq!(map["x-a"], "2");
    }
}
