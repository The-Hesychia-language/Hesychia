//! End-to-end tests: every `.hsy` file under `tests/` is compiled with the real `hsy` binary.
//!
//! - `tests/run`: must compile and print the `# expect:` lines, in debug and release
//!   (`# expect-debug:` and `# expect-release:` lines apply to one mode only).
//! - `tests/fail`: must be rejected with a message containing the `# error:` text.
//! - `tests/panic`: must compile, then stop with a failure at runtime.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HSY: &str = env!("CARGO_BIN_EXE_hsy");

fn programs(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(dir);
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .expect("test directory exists")
        .map(|entry| entry.expect("readable entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "hsy"))
        .collect();
    files.sort();
    files
}

/// Collects, in order, the text after `# <key>: ` for each of `keys`.
fn directives(path: &Path, keys: &[&str]) -> Vec<String> {
    let prefixes: Vec<String> = keys.iter().map(|key| format!("# {key}: ")).collect();
    fs::read_to_string(path)
        .expect("readable program")
        .lines()
        .filter_map(|line| prefixes.iter().find_map(|prefix| line.strip_prefix(prefix.as_str())))
        .map(str::to_string)
        .collect()
}

fn build(path: &Path, release: bool) -> (Output, PathBuf) {
    let stem = path.file_stem().unwrap().to_string_lossy();
    let mode = if release { "release" } else { "debug" };
    let binary = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{stem}-{mode}"));
    let mut command = Command::new(HSY);
    command.arg("build").arg(path).arg("-o").arg(&binary).arg("--emit-c");
    if release {
        command.arg("--release");
    }
    (command.output().expect("hsy runs"), binary)
}

/// The generated C must also pass a strict compile; unused user variables are allowed.
fn assert_clean_c(c_file: &Path) {
    let intel = fs::read_to_string(c_file).expect("readable C").contains("__asm__ volatile");
    // An object file beside the C file: /dev/null is not on every system.
    let object = c_file.with_extension("o");
    let status = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-Wno-unused-variable", "-c", "-o"])
        .arg(&object)
        .args(intel.then_some("-masm=intel"))
        .arg(c_file)
        .status()
        .expect("cc runs");
    assert!(status.success(), "generated C has warnings: {}", c_file.display());
}

/// With valgrind installed, debug builds must free everything and touch no invalid memory.
fn assert_no_leaks(binary: &Path) {
    let Ok(output) = Command::new("valgrind")
        .args(["--leak-check=full", "--errors-for-leak-kinds=definite,indirect", "--error-exitcode=99", "-q"])
        .arg(binary)
        .output()
    else {
        return;
    };
    assert!(
        output.status.code() != Some(99),
        "{}:\n{}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn valid_programs_print_expected_output() {
    for path in programs("run") {
        for release in [false, true] {
            let mode = if release { "expect-release" } else { "expect-debug" };
            let expected: String =
                directives(&path, &["expect", mode]).iter().map(|line| format!("{line}\n")).collect();
            let (output, binary) = build(&path, release);
            assert!(
                output.status.success(),
                "{} failed:\n{}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_clean_c(&binary.with_extension("c"));
            let run = Command::new(&binary).output().expect("program runs");
            assert!(run.status.success(), "{} exited with {}", path.display(), run.status);
            // Windows' text mode ends lines with \r\n, as under Wine below.
            let printed = String::from_utf8_lossy(&run.stdout);
            let printed = if cfg!(windows) { printed.replace("\r\n", "\n") } else { printed.into_owned() };
            assert_eq!(printed, expected, "output of {}", path.display());
            if !release {
                assert_no_leaks(&binary);
            }
        }
    }
}

#[test]
fn invalid_programs_are_rejected() {
    for path in programs("fail") {
        let (output, _) = build(&path, false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{} should not compile", path.display());
        for message in directives(&path, &["error"]) {
            assert!(stderr.contains(&message), "{}: expected `{message}` in:\n{stderr}", path.display());
        }
    }
}

#[test]
fn runtime_faults_stop_the_program() {
    for path in programs("panic") {
        let (output, binary) = build(&path, true);
        assert!(output.status.success(), "{} failed to compile", path.display());
        let run = Command::new(&binary).output().expect("program runs");
        assert!(!run.status.success(), "{} should have stopped", path.display());
        let stdout = String::from_utf8_lossy(&run.stdout);
        let printed: Vec<&str> = stdout.lines().collect();
        assert_eq!(printed, directives(&path, &["expect"]), "{}: output before stopping", path.display());
    }
}

fn installed(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Built for Windows with MinGW and run under Wine: `cargo test -- --ignored`.
#[test]
#[ignore = "needs MinGW and Wine"]
fn valid_programs_run_on_windows() {
    assert!(installed("x86_64-w64-mingw32-gcc") && installed("wine"), "MinGW and Wine are needed");
    for path in programs("run") {
        let stem = path.file_stem().unwrap().to_string_lossy();
        let binary = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{stem}.exe"));
        let output = Command::new(HSY)
            .args(["build", "--target", "x86_64-windows-gnu", "-o"])
            .arg(&binary)
            .arg(&path)
            .output()
            .expect("hsy runs");
        assert!(output.status.success(), "{}:\n{}", path.display(), String::from_utf8_lossy(&output.stderr));
        let run = Command::new("wine").arg(&binary).env("WINEDEBUG", "-all").output().expect("wine runs");
        let expected: String =
            directives(&path, &["expect", "expect-debug"]).iter().map(|line| format!("{line}\n")).collect();
        let printed = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
        assert_eq!(printed, expected, "output of {} on Windows", path.display());
    }
}
