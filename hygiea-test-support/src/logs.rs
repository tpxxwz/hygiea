//! 捕获 tracing 输出，断言日志里实际打了什么

use std::sync::{Arc, Mutex};

/// 把 tracing 输出收进内存，看日志里实际打了什么
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    /// 非 UTF-8 字节转成 `�`，不 panic：打印 `Reply::bytes` 这类非法编码的响应时会遇到
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 在当前线程装一个写进内存的 subscriber，guard 活着期间有效。收 INFO 及以上，
/// 依赖库（hyper 等）的 DEBUG / TRACE 不会混进来。
/// `#[test]` 和 `#[tokio::test]`（默认单线程）都在同一个线程里跑完，所以够用；
/// 多线程 runtime 里别的线程打的日志收不到
pub fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let out = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(out.clone())
        .with_ansi(false)
        .finish();
    (out, tracing::subscriber::set_default(subscriber))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只收 INFO 及以上：`fmt()` 不配 filter 时默认的 max level 就是 INFO，
    /// DEBUG / TRACE（依赖库比如 hyper 打的那些）不会混进来
    #[test]
    fn only_info_and_above_are_captured() {
        let (out, _guard) = capture();
        tracing::trace!("trace-msg");
        tracing::debug!("debug-msg");
        tracing::info!("info-msg");
        tracing::warn!("warn-msg");
        tracing::error!("error-msg");
        let text = out.text();
        assert!(!text.contains("trace-msg"), "{text}");
        assert!(!text.contains("debug-msg"), "{text}");
        assert!(text.contains("info-msg"), "{text}");
        assert!(text.contains("warn-msg"), "{text}");
        assert!(text.contains("error-msg"), "{text}");
    }
}
