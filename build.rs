fn main() -> Result<(), Box<dyn std::error::Error>> {
    let commit_info = std::process::Command::new("git")
        .args(&["show", "-s", "--format=%h %ci"])
        .output()?;
    println!(
        "cargo:rustc-env=COMMIT_INFO={}",
        std::str::from_utf8(&commit_info.stdout)?
    );
    Ok(())
}
