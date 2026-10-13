//! xxHash：字符串固定地映射成整数（不是解析数字）。算法固定（`i64` 用 xxh3-64，`i32` 用 xxh32，seed 都是 0），
//! 同一个字符串跨进程、跨机器、跨版本结果一样，可以存起来比较、拿去做锁的 key。
//! 和 `HashMap` 用的哈希不同，那个每个进程随机，换个进程结果就变了

/// xxh3-64 映射成 `i64`（不是把 `"42"` 解析成 42）。有损：不同字符串可能映射到同一个值
pub fn xxh3_64(s: &str) -> i64 {
    xxhash_rust::xxh3::xxh3_64(s.as_bytes()) as i64
}

/// xxh32 映射成 `i32`。位数少，撞的概率比 [`xxh3_64`] 大得多（几万个不同的字符串里就可能有撞的）
pub fn xxh32(s: &str) -> i32 {
    xxhash_rust::xxh32::xxh32(s.as_bytes(), 0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 算法固定：结果写死在这里，换了算法或 seed 会失败
    #[test]
    fn results_are_fixed() {
        let s = "001020002:settle:2026-10-13";
        assert_eq!(xxh3_64(s), 4973850726513738739);
        assert_eq!(xxh32(s), -531243377);
        let other = "001020002:settle:2026-10-14";
        assert_ne!(xxh3_64(s), xxh3_64(other));
        assert_ne!(xxh32(s), xxh32(other));
    }
}
