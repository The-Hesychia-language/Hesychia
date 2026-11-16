//! `hsy`: the Hesychia compiler.

mod ast;
mod codegen;
mod comptime;
mod diag;
mod driver;
mod lexer;
mod loader;
mod parser;
mod platform;
mod project;
mod stdlib;
mod typeck;
mod types;

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use driver::BuildOptions;
use platform::Platform;

const USAGE: &str = "\
usage:
  hsy new <name>
  hsy build [<file.hsy>] [--release] [--target <triple>] [-o <output>] [--emit-c] [--warn-copies]
  hsy run [<file.hsy>] [--release] [--warn-copies]
  hsy test [<file.hsy>] [<part of a test name>]
  hsy --version

  Without a file, hsy builds the project whose hesychia.toml is in this folder or above,
  for each target of its [settings] targets (the host by default).

  --target       build for another machine, such as x86_64-windows-gnu or aarch64-linux-gnu
  --warn-copies  show each copy that `managed` code inserts";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let (command, rest) = args.split_first().ok_or(USAGE)?;
    if command == "--version" {
        println!("hsy {}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    if command == "new" {
        let [name] = rest else { return Err(USAGE.into()) };
        let dir = project::create(name)?;
        println!("created {}: build it with `cd {} && hsy run`", dir.display(), dir.display());
        return Ok(ExitCode::SUCCESS);
    }
    if command == "test" {
        return test(rest);
    }
    let (source, target, options) = parse_options(rest)?;
    let input = match source {
        Some(path) => driver::Input::file(&path),
        None => driver::Input::project(&std::env::current_dir().map_err(|e| e.to_string())?)?,
    };
    match command.as_str() {
        "build" => build(&input, target, &options),
        "run" if target.is_some() => Err("`hsy run` runs on this machine: use `hsy build --target`".into()),
        "run" => build_and_run(&input, options),
        _ => Err(USAGE.into()),
    }
}

/// Builds for `--target`, or for each target of the project.
fn build(
    input: &driver::Input,
    target: Option<Platform>,
    options: &BuildOptions,
) -> Result<ExitCode, String> {
    let targets = target.map_or_else(|| input.targets(), |t| vec![t]);
    if targets.len() > 1 && options.output.is_some() {
        return Err("`-o` names one binary, but the project has several targets: use `--target`".into());
    }
    for platform in &targets {
        let binary = driver::build(input, options, &driver::Build { platform, only: targets.len() == 1 })?;
        if let Some(label) = &input.label {
            let other = if platform.is_host() { String::new() } else { format!(" for {platform}") };
            eprintln!("built {label}{other}: {}", binary.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

type Options = (Option<PathBuf>, Option<Platform>, BuildOptions);

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut source = None;
    let mut target = None;
    let mut options = BuildOptions {
        release: false,
        output: None,
        emit_c: false,
        warn_copies: false,
        test: None,
        quiet: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--release" => options.release = true,
            "--emit-c" => options.emit_c = true,
            "--warn-copies" => options.warn_copies = true,
            "--target" => target = Some(Platform::parse(args.next().ok_or("`--target` needs a triple")?)?),
            "-o" => options.output = Some(args.next().ok_or("`-o` needs a path")?.into()),
            _ if source.is_none() && !arg.starts_with('-') => source = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument `{arg}`\n{USAGE}")),
        }
    }
    Ok((source, target, options))
}

/// `hsy test [file.hsy] [part of a name]`: builds and runs each `@test` function on its own,
/// and says which failed.
fn test(args: &[String]) -> Result<ExitCode, String> {
    let mut file = None;
    let mut filter = String::new();
    for arg in args {
        match arg {
            _ if arg.ends_with(".hsy") && file.is_none() => file = Some(PathBuf::from(arg)),
            _ if !arg.starts_with('-') && filter.is_empty() => filter.clone_from(arg),
            _ => return Err(format!("unexpected argument `{arg}`\n{USAGE}")),
        }
    }
    let mut input = match &file {
        Some(path) => driver::Input::file(path),
        None => driver::Input::project(&std::env::current_dir().map_err(|e| e.to_string())?)?,
    };
    let files = if file.is_some() { vec![input.entry.clone()] } else { input.test_files() };
    let binary = std::env::temp_dir().join(format!("hsy-test-{}", std::process::id()));
    let here = std::env::current_dir().unwrap_or_default();
    let (mut run, mut failed) = (0, 0);
    let mut last_error: Option<String> = None;
    for path in files {
        let shown = path.strip_prefix(&here).unwrap_or(&path).display().to_string();
        let names: Vec<String> =
            driver::tests_of(&path)?.into_iter().filter(|n| n.contains(&filter)).collect();
        input.entry.clone_from(&path);
        for (index, name) in names.iter().enumerate() {
            run += 1;
            let options = BuildOptions {
                release: false,
                output: Some(binary.clone()),
                emit_c: false,
                warn_copies: false,
                test: Some(name.clone()),
                quiet: index > 0,
            };
            let built =
                driver::build(&input, &options, &driver::Build { platform: &Platform::host(), only: true });
            if let Err(message) = built {
                failed += 1;
                println!("FAIL  {name}  ({shown}): it does not compile");
                // Code shared by the tests fails each of them the same way: said once.
                if last_error.as_ref() != Some(&message) {
                    eprintln!("{message}");
                    last_error = Some(message);
                }
                continue;
            }
            let output = Command::new(&binary).output().map_err(|e| format!("cannot run the test: {e}"))?;
            if output.status.success() {
                println!("ok    {name}");
                continue;
            }
            failed += 1;
            println!("FAIL  {name}  ({shown})");
            let said = [output.stdout, output.stderr].concat();
            for line in String::from_utf8_lossy(&said).lines() {
                println!("      {line}");
            }
            if said.is_empty() {
                let code = output.status.code().map_or("a signal".to_string(), |c| format!("code {c}"));
                println!("      stopped with {code}");
            }
        }
    }
    let _ = std::fs::remove_file(&binary);
    let plural = if run == 1 { "" } else { "s" };
    println!("{run} test{plural}, {failed} failed");
    Ok(if failed == 0 && run > 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Builds into a temporary binary, runs it, and forwards its exit code.
fn build_and_run(input: &driver::Input, mut options: BuildOptions) -> Result<ExitCode, String> {
    let binary = std::env::temp_dir().join(format!("hsy-run-{}", std::process::id()));
    options.output = Some(binary.clone());
    driver::build(input, &options, &driver::Build { platform: &Platform::host(), only: true })?;
    let status = Command::new(&binary).status().map_err(|e| format!("cannot run the program: {e}"));
    let _ = std::fs::remove_file(&binary);
    let code = status?.code().unwrap_or(1);
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}
