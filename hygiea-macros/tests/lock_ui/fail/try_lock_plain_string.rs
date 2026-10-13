// try_lock 只接受 lock_key! 生成的 LockKey，不能直接传字符串
use hygiea::lock::DistributedLock;

async fn f(lock: &impl DistributedLock) {
    let _ = lock.try_lock("job").await;
}

fn main() {}
