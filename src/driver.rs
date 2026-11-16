//! Compilation pipeline: source, C, then a native binary through the system C compiler.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::comptime::Target;
use crate::diag::{Diagnostic, SourceMap};
use crate::platform::Platform;
use crate::project::{self, Project};
use crate::{codegen, loader, typeck};

/// What to build: a lone file, or the project around a folder.
pub struct Input {
    pub entry: PathBuf,
    pub project: Project,
    /// The binary's name, without `.exe`.
    name: String,
    /// The project's `target` folder; binaries of a lone file go in the current folder.
    target_dir: Option<PathBuf>,
    /// `name version` of a project, said once it is built.
    pub label: Option<String>,
}

impl Input {
    pub fn file(path: &Path) -> Self {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
        Self {
            entry: path.to_path_buf(),
            project: Project::single_file(path),
            name: stem.into(),
            target_dir: None,
            label: None,
        }
    }

    /// The project whose `hesychia.toml` is in `dir` or above, built from `src/main.hsy`.
    pub fn project(dir: &Path) -> Result<Self, String> {
        let root = project::find_root(dir)
            .ok_or("no `hesychia.toml` in this folder or above: give a `.hsy` file, or create a project with `hsy new <name>`")?;
        let project = Project::open(&root)?;
        let (name, version) = project
            .manifest
            .as_ref()
            .map_or(("out".into(), String::new()), |m| (m.name.clone(), m.version.clone()));
        let label = Some(format!("{name} {version}"));
        Ok(Self {
            entry: root.join("src/main.hsy"),
            project,
            name,
            target_dir: Some(root.join("target")),
            label,
        })
    }

    /// The targets of `[settings] targets`, or the host.
    pub fn targets(&self) -> Vec<Platform> {
        match self.project.manifest.as_ref().map(|m| m.targets.clone()) {
            Some(targets) if !targets.is_empty() => targets,
            _ => vec![Platform::host()],
        }
    }

    /// `target/debug/<name>` for the host, `target/<triple>/release/<name>.exe` for example.
    fn binary(&self, platform: &Platform, release: bool) -> PathBuf {
        let file = format!("{}{}", self.name, platform.exe_suffix());
        let Some(target_dir) = &self.target_dir else { return PathBuf::from(file) };
        let mut dir = target_dir.clone();
        if !platform.is_host() {
            dir.push(platform.to_string());
        }
        dir.push(if release { "release" } else { "debug" });
        dir.join(file)
    }
}

pub struct BuildOptions {
    pub release: bool,
    pub output: Option<PathBuf>,
    pub emit_c: bool,
    /// Show each copy `managed` code inserted.
    pub warn_copies: bool,
}

/// One build: the machine it is for, and whether it is the only one this command makes.
pub struct Build<'a> {
    pub platform: &'a Platform,
    pub only: bool,
}

/// Compiles the input into a binary and returns its path.
pub fn build(input: &Input, options: &BuildOptions, build: &Build) -> Result<PathBuf, String> {
    let compiler = build.platform.c_compiler(build.only)?;
    let c_code = compile_to_c(input, options, build.platform)?;
    let stem = input.name.clone();
    let output = match &options.output {
        Some(output) => output.clone(),
        None => input.binary(build.platform, options.release),
    };
    if let Some(dir) = output.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let c_path = if options.emit_c {
        output.with_extension("c")
    } else {
        std::env::temp_dir().join(format!("hsy-{stem}-{}.c", std::process::id()))
    };
    fs::write(&c_path, &c_code).map_err(|e| format!("cannot write {}: {e}", c_path.display()))?;
    let intel = c_code.contains("__asm__ volatile");
    let threads = c_code.contains("#include <pthread.h>");
    let flags = CFlags { release: options.release, intel_asm: intel, threads };
    let result = run_c_compiler(&compiler, build.platform, &c_path, &output, &flags);
    if !options.emit_c {
        let _ = fs::remove_file(&c_path);
    }
    result.map(|()| output)
}

/// Runs the front end and code generator, printing warnings as they come.
fn compile_to_c(input: &Input, options: &BuildOptions, platform: &Platform) -> Result<String, String> {
    let debug = !options.release;
    let name = input.entry.display().to_string();
    let source = fs::read_to_string(&input.entry).map_err(|e| format!("cannot read {name}: {e}"))?;
    let mut sources = SourceMap::default();
    let main_file = sources.add(name, source);

    let program = loader::load(main_file, &input.project, &mut sources);
    let render = |diag: Diagnostic| diag.render(&sources);
    let mut program = program.map_err(render)?;
    let heap_threshold = input
        .project
        .manifest
        .as_ref()
        .and_then(|m| m.heap_threshold)
        .unwrap_or(crate::comptime::HEAP_THRESHOLD);
    let checked = typeck::check(&mut program, &Target { platform: platform.clone(), debug, heap_threshold })
        .map_err(render)?;
    for warning in checked.warnings.iter().cloned() {
        eprint!("{}", render(warning));
    }
    if options.warn_copies {
        for copy in checked.copies.iter().cloned() {
            eprint!("{}", render(copy));
        }
    }
    let file_names = (0..sources.len()).map(|file| sources.path(file).to_string()).collect();
    Ok(codegen::generate(&program, &checked, &codegen::Options { debug, file_names }))
}

struct CFlags {
    release: bool,
    /// `asm` bodies are written in Intel syntax.
    intel_asm: bool,
    threads: bool,
}

fn run_c_compiler(
    compiler: &str,
    platform: &Platform,
    c_path: &Path,
    output: &Path,
    flags: &CFlags,
) -> Result<(), String> {
    let status = Command::new(compiler)
        .args(["-std=c11", "-o"])
        .arg(output)
        .arg(c_path)
        .args(c_flags(platform, flags.release))
        .args(flags.intel_asm.then_some("-masm=intel"))
        .args(flags.threads.then_some("-pthread"))
        .arg("-lm")
        .status()
        .map_err(|e| format!("cannot run the C compiler `{compiler}` for {platform}: {e}"))?;
    if !status.success() {
        return Err(format!("the C compiler `{compiler}` failed on the generated code"));
    }
    Ok(())
}

/// Debug keeps symbols for debuggers; release optimizes for size and drops unused code.
/// Windows binaries are linked statically, so they run without MinGW's DLLs.
fn c_flags(platform: &Platform, release: bool) -> Vec<&'static str> {
    let mut flags =
        if release { vec!["-Os", "-ffunction-sections", "-fdata-sections"] } else { vec!["-O0", "-g"] };
    if platform.os == "windows" {
        flags.push("-static");
    }
    if release {
        if platform.os == "macos" {
            flags.push("-Wl,-dead_strip");
        } else {
            flags.extend(["-Wl,--gc-sections", "-s"]);
        }
    }
    flags
}
