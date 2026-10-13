// lock_type 里只能写 code
use hygiea::lock::hy_lock_type;

#[derive(hy_lock_type)]
enum E {
    #[lock_type(id = "000001")]
    A,
}

fn main() {}
