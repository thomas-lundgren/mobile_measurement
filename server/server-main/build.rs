// sqlx::migrate! embeds migrations/ at compile time; rebuild when a file is
// added or changed there (cargo does not track it otherwise).
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
