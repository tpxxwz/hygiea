//! 构造请求头

use http::{HeaderMap, HeaderName, HeaderValue};

/// 用 `(名字, 值)` 列表拼一个 `HeaderMap`。名字大小写随意（会统一成小写），同名的后一个覆盖前一个；
/// 名字或值不合法时直接 panic，只给测试用
pub fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
    pairs
        .iter()
        .map(|(k, v)| {
            (
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            )
        })
        .collect()
}
