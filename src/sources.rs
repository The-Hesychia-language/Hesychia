//! The files a program is built from, as `source()` and `offsec.quine` give them back: each
//! module of the project and of its `path` dependencies, their manifests and the lock, under
//! the paths they take in a copy of the project. A Git dependency is pinned by the lock and
//! the standard library comes with the compiler: neither is copied.

use std::fs;
use std::path::Path;

use crate::project::{LOCKFILE, MANIFEST, Project};

/// The files of a program, by their path in a copy, and the path of each module given.
pub struct Files {
    pub list: Vec<(String, String)>,
    pub keys: Vec<Option<String>>,
}

/// `modules` are the program's own modules: their package, file and text.
pub fn collect(project: &Project, modules: &[(usize, &str, &str)]) -> Result<Files, String> {
    let mut list = Vec::new();
    let mut keys = Vec::new();
    for &(package, path, text) in modules {
        let key = place(project, package).map(|place| {
            let base = project.packages[package].folder.as_ref().unwrap_or(&project.packages[package].root);
            let relative = Path::new(path).strip_prefix(base).unwrap_or(Path::new(path));
            format!("{place}{}", slashed(relative))
        });
        if let Some(key) = &key {
            list.push((key.clone(), text.to_string()));
        }
        keys.push(key);
    }
    for package in 0..project.packages.len() {
        let (Some(place), Some(folder)) = (place(project, package), &project.packages[package].folder) else {
            continue;
        };
        let manifest = read(&folder.join(MANIFEST))?;
        list.push((format!("{place}{MANIFEST}"), repoint_all(project, package, &manifest)?));
        if package == 0 && folder.join(LOCKFILE).is_file() {
            list.push((LOCKFILE.to_string(), read(&folder.join(LOCKFILE))?));
        }
    }
    Ok(Files { list, keys })
}

/// Where a package's files go in the copy: at its root for the project, in `deps/<name>/`
/// for a `path` dependency; `None` for a Git one.
fn place(project: &Project, package: usize) -> Option<String> {
    if package == 0 {
        return Some(String::new());
    }
    let package = &project.packages[package];
    package.folder.as_ref().map(|_| format!("deps/{}/", package.prefix))
}

/// The manifest with each `path` dependency pointing where the copy puts it.
fn repoint_all(project: &Project, package: usize, manifest: &str) -> Result<String, String> {
    let table: toml::Table = manifest.parse().map_err(|e| format!("{MANIFEST}: {e}"))?;
    let mut text = manifest.to_string();
    let Some(deps) = table.get("dependencies").and_then(toml::Value::as_table) else { return Ok(text) };
    for (name, value) in deps {
        let Some(old) = value.get("path").and_then(toml::Value::as_str) else { continue };
        let index = project.packages[package].dependencies[name];
        let new = if package == 0 {
            format!("deps/{}", project.packages[index].prefix)
        } else {
            format!("../{}", project.packages[index].prefix)
        };
        text = repoint(&text, name, old, &new);
    }
    Ok(text)
}

/// Replaces the `path` of dependency `name`, written `name = { path = "old" }` under
/// `[dependencies]` or `path = "old"` under `[dependencies.name]`; the rest is kept as written.
fn repoint(text: &str, name: &str, old: &str, new: &str) -> String {
    let mut section = String::new();
    let mut out = String::new();
    let mut done = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed.trim_matches(|c| c == '[' || c == ']').trim().to_string();
        }
        let names_it = (section == "dependencies"
            && trimmed.strip_prefix(name).is_some_and(|rest| rest.trim_start().starts_with('=')))
            || (section == format!("dependencies.{name}") && trimmed.starts_with("path"));
        if names_it && !done {
            for quote in ['"', '\''] {
                let quoted = format!("{quote}{old}{quote}");
                if line.contains(&quoted) {
                    out += &line.replacen(&quoted, &format!("{quote}{new}{quote}"), 1);
                    done = true;
                    break;
                }
            }
            if done {
                continue;
            }
        }
        out += line;
    }
    out
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// A relative path with `/` between its parts, on every system.
fn slashed(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::repoint;

    #[test]
    fn repoints_inline_and_table_forms() {
        let text = "[project]\nname = \"app\"\n\n[dependencies]\nmath = { path = \"../math\" }  # local\ntext = { git = \"x\" }\n\n[dependencies.util]\npath = '../../util'\n";
        let once = repoint(text, "math", "../math", "deps/math");
        let both = repoint(&once, "util", "../../util", "deps/util");
        assert_eq!(
            both,
            "[project]\nname = \"app\"\n\n[dependencies]\nmath = { path = \"deps/math\" }  # local\ntext = { git = \"x\" }\n\n[dependencies.util]\npath = 'deps/util'\n"
        );
    }
}
