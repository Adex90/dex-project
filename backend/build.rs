// Las migraciones se incrustan con `sqlx::migrate!`; recompilar si cambian.
fn main() {
    println!("cargo:rerun-if-changed=../migrations");
}
