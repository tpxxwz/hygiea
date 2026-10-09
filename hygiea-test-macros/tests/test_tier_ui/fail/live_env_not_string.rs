// env 里只能是字符串字面量
#[hygiea_test::live(env = [A])]
mod own_bucket_live {
    #[test]
    fn put_get() {}
}

fn main() {}
