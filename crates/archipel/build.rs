//! The commit archipel is built from, shown with its version: `ARCHIPEL_COMMIT`
//! if set (CI), or git's.
fn main() {
    println!("cargo:rerun-if-env-changed=ARCHIPEL_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let commit = std::env::var("ARCHIPEL_COMMIT").ok().or_else(|| {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "--short=7", "HEAD"])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    if let Some(commit) = commit.filter(|c| !c.is_empty()) {
        println!(
            "cargo:rustc-env=ARCHIPEL_COMMIT={}",
            &commit[..commit.len().min(7)]
        );
    }
}
