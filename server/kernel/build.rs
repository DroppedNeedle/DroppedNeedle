fn main() {
    // sqlx's migrate! cannot see a newly added migration file, so rebuild when the folder changes.
    println!("cargo:rerun-if-changed=../migrations");
}
