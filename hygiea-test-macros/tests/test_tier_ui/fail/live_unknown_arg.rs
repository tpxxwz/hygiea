// #[live] 只收 env = [..]
#[hygiea_test::live(envs = ["A"])]
mod own_bucket {
    #[test]
    fn put_get() {}
}

fn main() {}
