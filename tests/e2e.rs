use std::process::Command;

fn binary_path() -> std::path::PathBuf {
    let mut dir = std::env::current_exe().unwrap();
    dir.pop(); // remove test binary name (e.g. e2e-...)
    if dir.ends_with("deps") {
        dir.pop(); // target/debug/ or target/release/
    }
    dir.join(if cfg!(target_os = "windows") {
        "mac-screen-cast.exe"
    } else {
        "mac-screen-cast"
    })
}

#[test]
fn help_shows_usage() {
    let output = Command::new(binary_path())
        .arg("--help")
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mac-screen-cast"));
    assert!(stdout.contains("-l"));
    assert!(stdout.contains("--fps"));
    assert!(stdout.contains("--port"));
}

#[test]
fn help_via_h() {
    let output = Command::new(binary_path())
        .arg("-h")
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
}

#[test]
fn list_windows() {
    let output = Command::new(binary_path())
        .arg("--list")
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
}

#[test]
#[ignore = "requires Screen Recording permission"]
fn stream_default_port() {
    let mut child = Command::new(binary_path())
        .arg("--list")
        .spawn()
        .expect("failed to start binary");
    std::thread::sleep(std::time::Duration::from_secs(2));
    child.kill().ok();
}
