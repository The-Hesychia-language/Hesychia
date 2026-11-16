//! Projects: `hesychia.toml`, the packages a program is built from, and fetching the
//! Git dependencies, pinned in `hesychia.lock`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::platform::Platform;

pub const MANIFEST: &str = "hesychia.toml";
const LOCKFILE: &str = "hesychia.lock";

/// What `hesychia.toml` says.
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub dependencies: Vec<Dependency>,
    /// `[settings] heap_threshold`, in bytes.
    pub heap_threshold: Option<u64>,
    /// `[settings] targets`: what `hsy build` builds for; the host when empty.
    pub targets: Vec<Platform>,
}

pub struct Dependency {
    pub name: String,
    pub source: Source,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Source {
    Path(PathBuf),
    /// A repository, at a tag, a branch or a commit.
    Git {
        url: String,
        reference: GitRef,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub enum GitRef {
    Tag(String),
    Branch(String),
    Rev(String),
    Default,
}

/// One package of the program: where its modules live, and the packages it depends on.
pub struct Package {
    /// The prefix of its modules' names: empty for the project, the dependency's name otherwise.
    pub prefix: String,
    /// Its `src/` folder (or the folder of a lone `.hsy` file).
    pub root: PathBuf,
    pub dependencies: HashMap<String, usize>,
}

/// The project being built, with every package it uses; package 0 is the project.
pub struct Project {
    pub manifest: Option<Manifest>,
    pub packages: Vec<Package>,
}

impl Project {
    /// A single file, outside any project: its folder is the only package.
    pub fn single_file(path: &Path) -> Self {
        let root = path.parent().unwrap_or(Path::new("")).to_path_buf();
        Self {
            manifest: None,
            packages: vec![Package { prefix: String::new(), root, dependencies: HashMap::new() }],
        }
    }

    /// The project at `dir`, its dependencies fetched as needed.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let manifest = read_manifest(dir)?;
        let mut lock = Lock::read(dir);
        let mut loader = PackageLoader {
            packages: Vec::new(),
            by_name: HashMap::new(),
            cache: dir.join(".hesychia/deps"),
        };
        loader.packages.push(Package {
            prefix: String::new(),
            root: dir.join("src"),
            dependencies: HashMap::new(),
        });
        loader.add_dependencies(0, &manifest, &mut lock)?;
        lock.write(dir)?;
        Ok(Self { manifest: Some(manifest), packages: loader.packages })
    }
}

/// The folder holding `hesychia.toml`, from `start` up.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|dir| dir.join(MANIFEST).is_file()).map(Path::to_path_buf)
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST);
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let table: toml::Table = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    let shown = path.display().to_string();
    let project =
        table.get("project").and_then(toml::Value::as_table).ok_or(format!("{shown}: missing [project]"))?;
    let text_of = |key: &str| project.get(key).and_then(toml::Value::as_str).map(str::to_string);
    let name = text_of("name").ok_or(format!("{shown}: [project] needs a `name`"))?;
    let version = text_of("version").unwrap_or_else(|| "0.1.0".into());
    let mut dependencies = Vec::new();
    if let Some(deps) = table.get("dependencies").and_then(toml::Value::as_table) {
        for (dep, value) in deps {
            let source =
                dependency_source(value, dir).map_err(|e| format!("{shown}: dependency `{dep}`: {e}"))?;
            dependencies.push(Dependency { name: dep.clone(), source });
        }
    }
    let heap_threshold = match table.get("settings").and_then(|s| s.get("heap_threshold")) {
        None => None,
        Some(value) => Some(
            value
                .as_integer()
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or(format!("{shown}: `heap_threshold` is a size in bytes"))?,
        ),
    };
    let targets = match table.get("settings").and_then(|s| s.get("targets")) {
        None => Vec::new(),
        Some(value) => {
            let names = value.as_array().ok_or(format!("{shown}: `targets` is a list of target names"))?;
            names
                .iter()
                .map(|name| {
                    let name =
                        name.as_str().ok_or(format!("{shown}: `targets` is a list of target names"))?;
                    Platform::parse(name).map_err(|e| format!("{shown}: {e}"))
                })
                .collect::<Result<_, _>>()?
        }
    };
    Ok(Manifest { name, version, dependencies, heap_threshold, targets })
}

/// `{ path = "../lib" }` or `{ git = "https://...", tag = "v1.0" }` (or `branch`, `rev`).
fn dependency_source(value: &toml::Value, dir: &Path) -> Result<Source, String> {
    let table = value.as_table().ok_or("expected `{ git = \"...\" }` or `{ path = \"...\" }`")?;
    let text = |key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
    if let Some(path) = text("path") {
        return Ok(Source::Path(dir.join(path)));
    }
    let url = text("git").ok_or("expected a `git` or a `path` key")?;
    let reference = match (text("tag"), text("branch"), text("rev")) {
        (Some(tag), None, None) => GitRef::Tag(tag),
        (None, Some(branch), None) => GitRef::Branch(branch),
        (None, None, Some(rev)) => GitRef::Rev(rev),
        (None, None, None) => GitRef::Default,
        _ => return Err("give one of `tag`, `branch` and `rev`".into()),
    };
    Ok(Source::Git { url, reference })
}

struct PackageLoader {
    packages: Vec<Package>,
    /// Each dependency by name, with its source: one name is one package.
    by_name: HashMap<String, (Source, usize)>,
    cache: PathBuf,
}

impl PackageLoader {
    fn add_dependencies(
        &mut self,
        package: usize,
        manifest: &Manifest,
        lock: &mut Lock,
    ) -> Result<(), String> {
        for dep in &manifest.dependencies {
            let index = match self.by_name.get(&dep.name) {
                Some((source, index)) if *source == dep.source => *index,
                Some(_) => {
                    return Err(format!(
                        "two dependencies are named `{}` but come from different places",
                        dep.name
                    ));
                }
                None => self.add_package(dep, lock)?,
            };
            self.packages[package].dependencies.insert(dep.name.clone(), index);
        }
        Ok(())
    }

    fn add_package(&mut self, dep: &Dependency, lock: &mut Lock) -> Result<usize, String> {
        let folder = match &dep.source {
            Source::Path(path) => path.clone(),
            Source::Git { url, reference } => fetch(&dep.name, url, reference, &self.cache, lock)?,
        };
        let index = self.packages.len();
        self.by_name.insert(dep.name.clone(), (dep.source.clone(), index));
        let root = folder.join("src");
        self.packages.push(Package { prefix: dep.name.clone(), root, dependencies: HashMap::new() });
        let manifest = read_manifest(&folder).map_err(|e| format!("dependency `{}`: {e}", dep.name))?;
        self.add_dependencies(index, &manifest, lock)?;
        Ok(index)
    }
}

/// Clones the repository once into the cache, then checks out the locked commit, or the
/// one the reference names now (which the lock then records).
fn fetch(
    name: &str,
    url: &str,
    reference: &GitRef,
    cache: &Path,
    lock: &mut Lock,
) -> Result<PathBuf, String> {
    let folder = cache.join(name);
    if !folder.join(".git").is_dir() {
        fs::create_dir_all(cache).map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
        git(None, &["clone", "--quiet", url, &folder.display().to_string()])
            .map_err(|e| format!("cannot fetch dependency `{name}` from {url}: {e}"))?;
    }
    let commit = if let Some(commit) = lock.commit(name, url, reference) {
        commit
    } else {
        let target = match reference {
            GitRef::Tag(tag) => format!("refs/tags/{tag}"),
            GitRef::Branch(branch) => format!("origin/{branch}"),
            GitRef::Rev(rev) => rev.clone(),
            GitRef::Default => "origin/HEAD".into(),
        };
        let _ = git(Some(&folder), &["fetch", "--quiet", "--tags", "origin"]);
        git(Some(&folder), &["rev-parse", &format!("{target}^{{commit}}")])
            .map_err(|_| format!("dependency `{name}`: {} not found in {url}", shown(reference)))?
    };
    if git(Some(&folder), &["checkout", "--quiet", "--detach", &commit]).is_err() {
        let _ = git(Some(&folder), &["fetch", "--quiet", "--tags", "origin"]);
        git(Some(&folder), &["checkout", "--quiet", "--detach", &commit])
            .map_err(|e| format!("dependency `{name}`: cannot check out {commit}: {e}"))?;
    }
    lock.record(name, url, reference, &commit);
    Ok(folder)
}

fn shown(reference: &GitRef) -> String {
    match reference {
        GitRef::Tag(tag) => format!("tag `{tag}`"),
        GitRef::Branch(branch) => format!("branch `{branch}`"),
        GitRef::Rev(rev) => format!("commit `{rev}`"),
        GitRef::Default => "the default branch".into(),
    }
}

/// Runs git, and gives what it printed.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let output = command.args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `hesychia.lock`: the commit each Git dependency was built from, so builds repeat.
struct Lock {
    entries: Vec<LockEntry>,
    used: Vec<LockEntry>,
}

#[derive(Clone)]
struct LockEntry {
    name: String,
    source: String,
    commit: String,
}

impl Lock {
    fn read(dir: &Path) -> Self {
        let entries = fs::read_to_string(dir.join(LOCKFILE))
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
            .and_then(|table| table.get("package").and_then(toml::Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|p| {
                let field = |key: &str| p.get(key).and_then(toml::Value::as_str).map(str::to_string);
                Some(LockEntry { name: field("name")?, source: field("source")?, commit: field("commit")? })
            })
            .collect();
        Self { entries, used: Vec::new() }
    }

    fn commit(&self, name: &str, url: &str, reference: &GitRef) -> Option<String> {
        let source = lock_source(url, reference);
        self.entries.iter().find(|e| e.name == name && e.source == source).map(|e| e.commit.clone())
    }

    fn record(&mut self, name: &str, url: &str, reference: &GitRef, commit: &str) {
        let entry =
            LockEntry { name: name.into(), source: lock_source(url, reference), commit: commit.into() };
        self.used.push(entry);
    }

    /// Writes the entries used by this build, when there are any.
    fn write(&self, dir: &Path) -> Result<(), String> {
        if self.used.is_empty() {
            return Ok(());
        }
        let mut text = String::from("# Written by hsy: the commit of each Git dependency.\n");
        for e in &self.used {
            text += &format!(
                "\n[[package]]\nname = {:?}\nsource = {:?}\ncommit = {:?}\n",
                e.name, e.source, e.commit
            );
        }
        fs::write(dir.join(LOCKFILE), text).map_err(|e| format!("cannot write {LOCKFILE}: {e}"))
    }
}

fn lock_source(url: &str, reference: &GitRef) -> String {
    match reference {
        GitRef::Tag(tag) => format!("{url}#tag={tag}"),
        GitRef::Branch(branch) => format!("{url}#branch={branch}"),
        GitRef::Rev(rev) => format!("{url}#rev={rev}"),
        GitRef::Default => url.to_string(),
    }
}

/// `hsy new <name>`: a project folder with its manifest and a `main`.
pub fn create(name: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(name);
    if dir.exists() {
        return Err(format!("`{name}` already exists"));
    }
    let project = dir.file_name().and_then(|n| n.to_str()).unwrap_or(name);
    let write = |path: PathBuf, text: String| {
        fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    };
    fs::create_dir_all(dir.join("src")).map_err(|e| format!("cannot create {name}: {e}"))?;
    write(
        dir.join(MANIFEST),
        format!("[project]\nname = \"{project}\"\nversion = \"0.1.0\"\n\n[dependencies]\n"),
    )?;
    write(
        dir.join("src/main.hsy"),
        "from io import print\n\nmanaged fn main():\n    print(\"Hello, Hesychia!\")\n".into(),
    )?;
    write(dir.join(".gitignore"), "/target/\n/.hesychia/\n".into())?;
    Ok(dir)
}
