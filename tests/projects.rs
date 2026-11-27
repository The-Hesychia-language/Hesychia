//! Projects: `hsy new`, `hsy run` without a file, path and Git dependencies, the lockfile.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HSY: &str = env!("CARGO_BIN_EXE_hsy");

/// A fresh folder under `target/tmp`.
fn workspace(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/tmp/projects").join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("workspace");
    dir
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("a folder")).expect("folder");
    fs::write(path, text).expect("written");
}

fn hsy(dir: &Path, args: &[&str]) -> Output {
    Command::new(HSY).args(args).current_dir(dir).output().expect("hsy runs")
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "hsy failed: {}", String::from_utf8_lossy(&output.stderr));
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    // Windows' text mode ends lines with \r\n.
    if cfg!(windows) { printed.replace("\r\n", "\n") } else { printed }
}

/// A program's file name as each system names it: `app`, or `app.exe` on Windows.
fn program(path: &str) -> String {
    format!("{path}{}", std::env::consts::EXE_SUFFIX)
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.email=test@hesychia", "-c", "user.name=test"])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

/// A library package at `dir` whose `lib.hsy` holds `body`.
fn library(dir: &Path, name: &str, body: &str) {
    write(&dir.join("hesychia.toml"), &format!("[project]\nname = \"{name}\"\nversion = \"1.0.0\"\n"));
    write(&dir.join("src/lib.hsy"), body);
}

#[test]
fn new_project_runs() {
    let dir = workspace("new");
    stdout(&hsy(&dir, &["new", "hello"]));
    assert_eq!(stdout(&hsy(&dir.join("hello"), &["run"])), "Hello, Hesychia!\n");
    let built = hsy(&dir.join("hello/src"), &["build"]);
    assert!(built.status.success());
    assert!(dir.join(program("hello/target/debug/hello")).is_file());
}

#[test]
fn dependencies_by_path_and_git() {
    let dir = workspace("deps");
    library(
        &dir.join("numbers"),
        "numbers",
        "import ops\n\nmanaged fn double(x: i64) -> i64:\n    return ops.twice(x)\n",
    );
    write(&dir.join("numbers/src/ops.hsy"), "managed fn twice(x: i64) -> i64:\n    return x * 2\n");
    let text = dir.join("text");
    library(&text, "text", "managed fn shout(s: str) -> str:\n    return f\"{s}!\"\n");
    git(&text, &["init", "-q"]);
    git(&text, &["add", "."]);
    git(&text, &["commit", "-qm", "one"]);
    git(&text, &["tag", "v1"]);
    write(&text.join("src/lib.hsy"), "managed fn shout(s: str) -> str:\n    return f\"{s}?\"\n");
    git(&text, &["commit", "-qam", "two"]);

    let app = dir.join("app");
    // A file URL with `/` only, as TOML reads `\` as an escape: file:///D:/... on Windows.
    let path = text.display().to_string().replace('\\', "/");
    let url = if path.starts_with('/') { format!("file://{path}") } else { format!("file:///{path}") };
    write(
        &app.join("hesychia.toml"),
        &format!(
            "[project]\nname = \"app\"\n\n[dependencies]\nnumbers = {{ path = \"../numbers\" }}\ntext = {{ git = \"{url}\", tag = \"v1\" }}\n"
        ),
    );
    write(
        &app.join("src/main.hsy"),
        "from io import print\nimport numbers\nfrom text import shout\n\nmanaged fn main():\n    print(f\"{numbers.double(21)} {shout(\"hi\")}\")\n",
    );
    assert_eq!(stdout(&hsy(&app, &["run"])), "42 hi!\n");
    let lock = fs::read_to_string(app.join("hesychia.lock")).expect("a lockfile");
    assert!(lock.contains("name = \"text\""), "{lock}");

    // The lock keeps the commit even when the tag moves.
    git(&text, &["tag", "-f", "v1"]);
    assert_eq!(stdout(&hsy(&app, &["run"])), "42 hi!\n");
}

#[test]
fn a_name_in_two_places_is_refused() {
    let dir = workspace("clash");
    library(&dir.join("text"), "text", "managed fn f() -> i32:\n    return 1\n");
    write(
        &dir.join("app/hesychia.toml"),
        "[project]\nname = \"app\"\n\n[dependencies]\ntext = { path = \"../text\" }\n",
    );
    write(&dir.join("app/src/main.hsy"), "import text\n\nmanaged fn main():\n    text.f()\n");
    write(&dir.join("app/src/text.hsy"), "managed fn f() -> i32:\n    return 2\n");
    let output = hsy(&dir.join("app"), &["run"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("names both a module of this package"));
}

#[test]
fn large_locals_go_to_the_heap() {
    let dir = workspace("heap");
    write(&dir.join("hesychia.toml"), "[project]\nname = \"heap\"\n\n[settings]\nheap_threshold = 16\n");
    write(
        &dir.join("src/main.hsy"),
        "from io import print\n\nmanaged struct Big:\n    a: i64\n    b: i64\n    c: i64\n\n\
         managed fn total(big: Big) -> i64:\n    return big.a + big.b + big.c\n\n\
         managed fn main():\n    big = Big(a=1, b=2, c=3)\n    big.b = 20\n    print(f\"{total(big)} {big}\")\n    \
         big = Big(a=0, b=0, c=7)\n    print(f\"{big.c}\")\n",
    );
    let built = hsy(&dir, &["build", "--emit-c", "-o", "target/heap"]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let c = fs::read_to_string(dir.join("target/heap.c")).expect("the C file");
    assert!(c.contains("hsy_alloc(sizeof("), "the variable is not on the heap");
    assert_eq!(stdout(&hsy(&dir, &["run"])), "24 Big(a=1, b=20, c=3)\n7\n");
}

fn installed(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok_and(|o| o.status.success())
}

#[test]
fn settings_name_the_targets() {
    let dir = workspace("targets");
    stdout(&hsy(&dir, &["new", "app"]));
    let app = dir.join("app");
    let manifest = fs::read_to_string(app.join("hesychia.toml")).expect("manifest");
    write(&app.join("hesychia.toml"), &format!("{manifest}\n[settings]\ntargets = [\"sparc-linux\"]\n"));
    let refused = hsy(&app, &["build"]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("unknown target `sparc-linux`"));

    // A build for Windows beside the host's: on Windows, the two are the same.
    if !installed("x86_64-w64-mingw32-gcc") || cfg!(windows) {
        return;
    }
    let host = format!("{}-{}-gnu", std::env::consts::ARCH, std::env::consts::OS);
    let targets = format!("targets = [\"{host}\", \"x86_64-windows-gnu\"]");
    write(&app.join("hesychia.toml"), &format!("{manifest}\n[settings]\n{targets}\n"));
    stdout(&hsy(&app, &["build"]));
    assert!(app.join("target/debug/app").is_file());
    assert!(app.join("target/x86_64-windows-gnu/debug/app.exe").is_file());
    let one = hsy(&app, &["build", "-o", "app"]);
    assert!(String::from_utf8_lossy(&one.stderr).contains("several targets"));
}

#[test]
fn settings_link_c_libraries() {
    let dir = workspace("links");
    // A C library of our own, `libgreet.a`, found by the C compiler through `LIBRARY_PATH`.
    let libs = dir.join("libs");
    write(&libs.join("greet.c"), "int greet_twice(int x) { return 2 * x; }\n");
    let cc = |args: &[&str]| Command::new("cc").args(args).current_dir(&libs).status().expect("cc runs");
    assert!(cc(&["-c", "greet.c", "-o", "greet.o"]).success());
    let ar = Command::new("ar").args(["rcs", "libgreet.a", "greet.o"]).current_dir(&libs).status();
    assert!(ar.expect("ar runs").success());

    // The library declares the C function and names the C library; the app only imports it.
    library(
        &dir.join("greeting"),
        "greeting",
        "extern fn greet_twice(x: i32) -> i32\n\nmanual fn twice(x: i32) -> i32:\n    return greet_twice(x)\n",
    );
    let manifest = fs::read_to_string(dir.join("greeting/hesychia.toml")).expect("manifest");
    write(&dir.join("greeting/hesychia.toml"), &format!("{manifest}\n[settings]\nlinks = [\"greet\"]\n"));
    let app = dir.join("app");
    write(
        &app.join("hesychia.toml"),
        "[project]\nname = \"app\"\n\n[dependencies]\ngreeting = { path = \"../greeting\" }\n",
    );
    write(
        &app.join("src/main.hsy"),
        "from io import print\nimport greeting\n\nmanaged fn main():\n    print(f\"{greeting.twice(21)}\")\n",
    );
    let run = Command::new(HSY).arg("run").current_dir(&app).env("LIBRARY_PATH", &libs).output();
    assert_eq!(stdout(&run.expect("hsy runs")), "42\n");

    // `:file` names the library's file, as installed without its development files: GNU ld's
    // form, which Apple's linker does not take.
    if !cfg!(target_os = "macos") {
        write(
            &dir.join("greeting/hesychia.toml"),
            &format!("{manifest}\n[settings]\nlinks = [\":libgreet.a\"]\n"),
        );
        let run = Command::new(HSY).arg("run").current_dir(&app).env("LIBRARY_PATH", &libs).output();
        assert_eq!(stdout(&run.expect("hsy runs")), "42\n");
    }

    // Without the library, the C compiler is told about it, and fails.
    let missing = hsy(&app, &["build"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("failed on the generated code"));

    // A flag or a path is not a library name.
    write(&app.join("hesychia.toml"), "[project]\nname = \"app\"\n\n[settings]\nlinks = [\"-Wl,--evil\"]\n");
    let refused = hsy(&app, &["build"]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("`links` is a list of C library names"));
}

/// `offsec.quine.write` copies the project with its path dependencies, Git ones pinned by the
/// lock and left out; the copy builds and runs, and is itself a quine.
#[test]
fn a_quine_copies_the_project() {
    let dir = workspace("quine");
    library(
        &dir.join("numbers"),
        "numbers",
        "import ops\n\nmanaged fn double(x: i64) -> i64:\n    return ops.twice(x)\n",
    );
    write(&dir.join("numbers/src/ops.hsy"), "managed fn twice(x: i64) -> i64:\n    return x * 2\n");
    let text = dir.join("text");
    library(&text, "text", "managed fn shout(s: str) -> str:\n    return f\"{s}!\"\n");
    git(&text, &["init", "-q"]);
    git(&text, &["add", "."]);
    git(&text, &["commit", "-qm", "one"]);
    let app = dir.join("app");
    write(
        &app.join("hesychia.toml"),
        &format!(
            "[project]\nname = \"app\"\n\n[dependencies]\nnumbers = {{ path = \"../numbers\" }}  # kept\ntext = {{ git = \"{}\" }}\n",
            text.display().to_string().replace('\\', "/")
        ),
    );
    write(
        &app.join("src/main.hsy"),
        "import numbers\nimport text\nimport offsec.quine\nfrom io import print\n\nmanaged fn main():\n    print(text.shout(f\"{numbers.double(21)}\"))\n    quine.write(to=\"../copy\", overwrite=True)\n",
    );
    assert_eq!(stdout(&hsy(&app, &["run"])), "42!\n");
    let copy = dir.join("copy");
    let mut files: Vec<String> = Vec::new();
    for entry in [
        "hesychia.toml",
        "hesychia.lock",
        "src/main.hsy",
        "deps/numbers/hesychia.toml",
        "deps/numbers/src/lib.hsy",
        "deps/numbers/src/ops.hsy",
    ] {
        assert!(copy.join(entry).is_file(), "{entry} is copied");
        files.push(entry.into());
    }
    assert!(!copy.join("deps/text").exists(), "a Git dependency is not copied");
    let manifest = fs::read_to_string(copy.join("hesychia.toml")).expect("manifest");
    assert!(manifest.contains("numbers = { path = \"deps/numbers\" }  # kept"), "{manifest}");
    assert_eq!(
        fs::read_to_string(copy.join("src/main.hsy")).expect("main"),
        fs::read_to_string(app.join("src/main.hsy")).expect("main")
    );
    assert_eq!(stdout(&hsy(&copy, &["run"])), "42!\n");
}
