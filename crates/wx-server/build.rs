fn main() {
    // `sqlx::migrate!` embeds the migration files; rebuild when one is added.
    println!("cargo:rerun-if-changed=../../migrations");
}
