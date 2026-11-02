extern crate reuse_user;

fn main() {
    assert_eq!(reuse_user::call(4u8), 5);
    assert_eq!(reuse_user::wrap(3u16).0, 6);
}
