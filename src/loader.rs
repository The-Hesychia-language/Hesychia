//! Loads the main file and every module it imports, directly or not, from the project,
//! its dependencies and the standard library.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;

use crate::ast::{Module, Program};
use crate::diag::{Diagnostic, Result, SourceMap, Span};
use crate::project::Project;
use crate::{lexer, parser, stdlib};

/// Parses `main_file` (already in `sources`), the main module of the project's first
/// package, and the modules it imports. Each import is rewritten to the module's full name:
/// `json.parse` for `import parse` inside the `json` dependency.
/// The prelude is loaded with the main module.
pub fn load(main_file: usize, project: &Project, sources: &mut SourceMap) -> Result<Program> {
    let prelude = stdlib_file(stdlib::PRELUDE, sources);
    let mut seen = HashSet::from(["main".to_string(), stdlib::PRELUDE.to_string()]);
    let mut queue = VecDeque::from([
        ("main".to_string(), main_file, Some(0)),
        (stdlib::PRELUDE.to_string(), prelude, None),
    ]);
    let mut modules = Vec::new();
    while let Some((name, file, package)) = queue.pop_front() {
        let mut parsed = parser::parse(lexer::lex(sources.source(file), file)?)?;
        for import in &mut parsed.imports {
            let found = match package {
                Some(package) => locate(project, package, &import.module, import.span)?,
                None => locate_stdlib(&import.module, import.span)?,
            };
            import.module.clone_from(&found.name);
            let full = found.name.join(".");
            if seen.insert(full.clone()) {
                let file = read(&found, import.span, sources)?;
                queue.push_back((full, file, found.package));
            }
        }
        let stdlib = package.is_none();
        modules.push(Module { name, stdlib, imports: parsed.imports, items: parsed.items });
    }
    Ok(Program { modules })
}

/// Where an import leads.
struct Found {
    /// The module's full name, its package's prefix first.
    name: Vec<String>,
    /// Its file; `None` for a standard module.
    path: Option<PathBuf>,
    /// `None` for a standard module.
    package: Option<usize>,
}

/// `a.b` in a package: the file `a/b.hsy` of the package, a dependency named `a`, or the
/// standard module `a`. A name found in two of these places is an error.
fn locate(project: &Project, package: usize, parts: &[String], span: Span) -> Result<Found> {
    if parts.join(".") == stdlib::PRELUDE {
        return Err(Diagnostic::error(span, "the prelude is known in every module: no need to import it")
            .with_hint("a module of your own cannot be named `prelude`"));
    }
    let here = &project.packages[package];
    let mut found = Vec::new();
    let local = file_in(&here.root, parts);
    if local.is_file() {
        found.push(Found { name: prefixed(&here.prefix, parts), path: Some(local), package: Some(package) });
    }
    if let Some(&dep) = here.dependencies.get(&parts[0]) {
        let rest = &parts[1..];
        let root = &project.packages[dep].root;
        let path = if rest.is_empty() { root.join("lib.hsy") } else { file_in(root, rest) };
        found.push(Found {
            name: prefixed(&project.packages[dep].prefix, rest),
            path: Some(path),
            package: Some(dep),
        });
    }
    let name = parts.join(".");
    if let Ok(standard) = locate_stdlib(parts, span) {
        found.push(standard);
    }
    match found.len() {
        0 => {
            let expected = file_in(&here.root, parts);
            Err(Diagnostic::error(span, format!("module `{name}` not found")).with_hint(format!(
                "expected the file `{}`, or a dependency named `{}`",
                expected.display(),
                parts[0]
            )))
        }
        1 => Ok(found.pop().expect("one")),
        _ => Err(Diagnostic::error(
            span,
            format!("`{name}` names both a module of this package and a dependency or a standard module"),
        )
        .with_hint("rename the local module")),
    }
}

/// A module of the standard library, which only sees the standard library.
fn locate_stdlib(parts: &[String], span: Span) -> Result<Found> {
    let name = parts.join(".");
    if name == stdlib::PRELUDE || stdlib::source(&name).is_none() {
        return Err(Diagnostic::error(span, format!("no standard module `{name}`")));
    }
    Ok(Found { name: parts.to_vec(), path: None, package: None })
}

fn stdlib_file(name: &str, sources: &mut SourceMap) -> usize {
    let source = stdlib::source(name).expect("a standard module").to_string();
    sources.add(format!("<stdlib>/{name}.hsy"), source)
}

fn file_in(root: &std::path::Path, parts: &[String]) -> PathBuf {
    let mut path: PathBuf = parts.iter().fold(root.to_path_buf(), |path, part| path.join(part));
    path.set_extension("hsy");
    path
}

fn prefixed(prefix: &str, parts: &[String]) -> Vec<String> {
    prefix.split('.').filter(|p| !p.is_empty()).map(str::to_string).chain(parts.iter().cloned()).collect()
}

fn read(found: &Found, span: Span, sources: &mut SourceMap) -> Result<usize> {
    let Some(path) = &found.path else { return Ok(stdlib_file(&found.name.join("."), sources)) };
    let source = fs::read_to_string(path).map_err(|_| {
        Diagnostic::error(span, format!("module `{}` not found", found.name.join(".")))
            .with_hint(format!("expected the file `{}`", path.display()))
    })?;
    Ok(sources.add(path.display().to_string(), source))
}
