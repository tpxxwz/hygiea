// 项目前缀（fixture 里没配，是 000）+ 模块前缀 + code，共 9 位；不写模块前缀时 code 6 位。
// 锁类型和字符串 key 共用一套编号
use hygiea::lock::{LockType, hy_lock_key, hy_lock_type, lock_key};

#[derive(hy_lock_type)]
#[lock_module_prefix = "02"]
enum WithModule {
    #[lock_type(code = "0001")]
    A,
    #[lock_type(code = "9999")]
    B,
}

#[derive(hy_lock_type)]
enum NoModule {
    #[lock_type(code = "000042")]
    C,
}

#[derive(hy_lock_key)]
#[lock_module_prefix = "02"]
enum Key {
    #[lock_key(code = "0002", tpl = "fixed")]
    Fixed,
    #[lock_key(code = "0003", tpl = "user:{{ user }}")]
    One,
    #[lock_key(code = "0004", tpl = "{{ shop }}:{{ day }}")]
    Two,
}

fn main() {
    assert_eq!(LockType::from(WithModule::A).key().get(), 20001);
    assert_eq!(LockType::from(WithModule::B).key().get(), 29999);
    assert_eq!(LockType::from(NoModule::C).key().get(), 42);

    assert_eq!(lock_key!(Key::Fixed).as_str().unwrap(), "000020002:fixed");
    assert_eq!(lock_key!(Key::One, "alice").as_str().unwrap(), "000020003:user:alice");
    assert_eq!(
        lock_key!(Key::Two, { "shop": 42, "day": "2026-10-13" }).as_str().unwrap(),
        "000020004:42:2026-10-13"
    );
}
