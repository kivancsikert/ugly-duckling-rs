use std::process::Command;

fn main() {
    embuild::espidf::sysenv::output();

    // Run `git describe --tags` to get the version
    let output = Command::new("git")
        .args(["describe", "--tags", "--dirty"])
        .output()
        .expect("Failed to execute git command");

    let version = String::from_utf8(output.stdout).unwrap();
    println!("cargo:rustc-env=GIT_VERSION={}", version.trim());
}
