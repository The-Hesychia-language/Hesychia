//! Mutation fuzzing: every test program, damaged in small ways, must be compiled or rejected
//! with a message. The compiler must never crash, and C it accepts must always compile.
//!
//! `cargo test --test fuzz -- --ignored`; `HSY_FUZZ=n` sets the mutants per program (8 by
//! default), `HSY_FUZZ_SEED` the seed, and `HSY_FUZZ_ONLY` a part of the names of the programs
//! to damage.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const HSY: &str = env!("CARGO_BIN_EXE_hsy");

/// xorshift64: the same seed gives the same mutants, so a failure can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Pieces of the language, put in at random places.
const PIECES: &[&str] = &[
    "&",
    "&mut ",
    "owned ",
    "mut ",
    "const ",
    "None",
    "0",
    "-1",
    "\"\"",
    "[]",
    "()",
    "[i]",
    ".copy()",
    " + ",
    " * ",
    " // ",
    " == ",
    " and ",
    " not ",
    ":",
    ",",
    "(",
    ")",
    "[",
    "]",
    "return",
    "break",
    "continue",
    "pass",
    "raise ",
    "check ",
    "try:",
    "except:",
    "lambda: 0",
    "f\"{x}\"",
    "i64",
    "str",
    "vector[str]",
    "Optional[i32]",
    "shared(1)",
    "heap",
    "comptime ",
    "match x:",
    "case _:",
    "parallel ",
    "select:",
    "\n",
    "\n    ",
    "\t",
    "é",
    "#",
];

fn mutate(source: &str, rng: &mut Rng) -> String {
    let mut lines: Vec<String> = source.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return String::new();
    }
    for _ in 0..1 + rng.below(3) {
        let at = rng.below(lines.len());
        match rng.below(7) {
            0 => {
                lines.remove(at);
                if lines.is_empty() {
                    break;
                }
            }
            1 => {
                let copy = lines[at].clone();
                lines.insert(rng.below(lines.len() + 1), copy);
            }
            2 => {
                let other = rng.below(lines.len());
                lines.swap(at, other);
            }
            // A character taken out, or a piece put in, inside a line.
            3 | 4 => {
                let line = &mut lines[at];
                let chars: Vec<char> = line.chars().collect();
                let pos = rng.below(chars.len() + 1);
                let mut changed: String = chars[..pos].iter().collect();
                if rng.below(2) == 0 && pos < chars.len() {
                    changed.extend(&chars[pos + 1..]);
                } else {
                    changed += PIECES[rng.below(PIECES.len())];
                    changed.extend(&chars[pos..]);
                }
                *line = changed;
            }
            // A word replaced by another word of the program.
            5 => {
                let words: Vec<String> = source
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|w| !w.is_empty())
                    .map(str::to_string)
                    .collect();
                let line = &mut lines[at];
                if let (Some(word), Some(new)) = (
                    line.split(|c: char| !c.is_alphanumeric() && c != '_').find(|w| !w.is_empty()),
                    words.get(rng.below(words.len())),
                ) {
                    *line = line.replacen(word, new, 1);
                }
            }
            _ => {
                let cut = rng.below(lines[at].len() + 1);
                let keep: String = lines[at].chars().take(cut).collect();
                lines[at] = keep;
            }
        }
    }
    lines.join("\n") + "\n"
}

fn programs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = Vec::new();
    for dir in ["run", "fail", "panic"] {
        for entry in fs::read_dir(root.join(dir)).expect("test directory exists") {
            let path = entry.expect("readable entry").path();
            if path.extension().is_some_and(|ext| ext == "hsy") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Builds `file`, or gives up after 20 seconds: the compiler is then stuck.
fn build(file: &Path) -> Option<Output> {
    let mut child = Command::new(HSY)
        .arg("build")
        .arg(file)
        .arg("-o")
        .arg(file.with_extension("bin"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hsy runs");
    let start = Instant::now();
    while child.try_wait().expect("hsy can be waited for").is_none() {
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Some(child.wait_with_output().expect("hsy output"))
}

#[test]
#[ignore = "long: run with `cargo test --test fuzz -- --ignored`"]
fn damaged_programs_never_crash_the_compiler() {
    let count: usize = std::env::var("HSY_FUZZ").ok().and_then(|n| n.parse().ok()).unwrap_or(8);
    let seed: u64 = std::env::var("HSY_FUZZ_SEED").ok().and_then(|n| n.parse().ok()).unwrap_or(0x5eed);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fuzz-{seed}"));
    fs::create_dir_all(&dir).expect("fuzz directory");
    let mut failures = Vec::new();
    let only = std::env::var("HSY_FUZZ_ONLY").unwrap_or_default();
    for (index, path) in programs().iter().enumerate() {
        if !path.to_string_lossy().contains(&only) {
            continue;
        }
        let source = fs::read_to_string(path).expect("readable program");
        let mut rng = Rng(seed ^ (index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        for n in 0..count {
            let mutant = mutate(&source, &mut rng);
            let stem = path.file_stem().unwrap().to_string_lossy();
            let file = dir.join(format!("{stem}-{n}.hsy"));
            fs::write(&file, &mutant).expect("mutant written");
            let Some(output) = build(&file) else {
                failures.push(format!("hang: {}", file.display()));
                continue;
            };
            let stderr = String::from_utf8_lossy(&output.stderr);
            let crashed = output.status.code().is_none_or(|code| code == 101) || stderr.contains("panicked");
            // Lines of `asm` reach the assembler as written, and `extern` names the linker:
            // an error there is the program's.
            let theirs = stderr.contains("Assembler messages") || stderr.contains("undefined reference");
            let bad_c = stderr.contains("failed on the generated code") && !theirs;
            if crashed || bad_c {
                let kind = if crashed { "crash" } else { "bad C" };
                let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                failures.push(format!("{kind}: {} ({first})", file.display()));
            } else {
                let _ = fs::remove_file(&file);
            }
            let _ = fs::remove_file(file.with_extension("bin"));
            let _ = fs::remove_file(file.with_extension("c"));
        }
    }
    assert!(failures.is_empty(), "{} mutants broke the compiler:\n{}", failures.len(), failures.join("\n"));
}
