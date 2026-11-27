//! Loads the main file and every module it imports, directly or not, from the project,
//! its dependencies and the standard library.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;

use crate::ast::{FnDecl, Item, Module, Program};
use crate::diag::{Diagnostic, Result, SourceMap, Span};
use crate::project::Project;
use crate::{lexer, parser, stdlib};

/// Parses `main_file` (already in `sources`), the main module of the project's first
/// package, and the modules it imports. Each import is rewritten to the module's full name:
/// `json.parse` for `import parse` inside the `json` dependency.
/// The prelude is loaded with the main module.
/// `None` builds the program; `Some(name)` builds a program that runs the `@test` function
/// `name` of the main file in place of its `main`.
pub fn load(
    main_file: usize,
    project: &Project,
    sources: &mut SourceMap,
    test: Option<&str>,
) -> Result<Program> {
    let prelude = stdlib_file(stdlib::PRELUDE, sources);
    let mut seen = HashSet::from(["main".to_string(), stdlib::PRELUDE.to_string()]);
    let mut queue = VecDeque::from([
        ("main".to_string(), main_file, Some(0)),
        (stdlib::PRELUDE.to_string(), prelude, None),
    ]);
    let mut modules = Vec::new();
    let mut own = Vec::new();
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
        let mut items = parsed.items;
        if !stdlib {
            let root = modules.is_empty();
            items = keep_tests(items, if root { test } else { None })?;
            if let (true, Some(name)) = (root, test) {
                items.retain(|item| !matches!(item, Item::Fn(f) if f.name == "main"));
                let runner =
                    sources.add(format!("<test {name}>"), format!("managed fn main():\n    {name}()\n"));
                items.extend(parser::parse(lexer::lex(sources.source(runner), runner)?)?.items);
            }
        }
        if let Some(package) = package {
            own.push((modules.len(), package, file));
        }
        let path = package.map(|_| PathBuf::from(sources.path(file)));
        modules.push(Module { name, stdlib, file: path, key: None, imports: parsed.imports, items });
    }
    let given: Vec<(usize, &str, &str)> =
        own.iter().map(|&(_, package, file)| (package, sources.path(file), sources.source(file))).collect();
    let files = crate::sources::collect(project, &given)
        .map_err(|message| Diagnostic::error(Span::new(main_file, 1, 1), message))?;
    for (&(index, _, _), key) in own.iter().zip(files.keys) {
        modules[index].key = key;
    }
    let mut program = Program { modules, files: files.list };
    crate::derive::expand(&mut program, sources)?;
    Ok(program)
}

/// The `@test` functions of the file, checked to take and give nothing.
pub fn tests(source: &str, file: usize) -> Result<Vec<String>> {
    let items = parser::parse(lexer::lex(source, file)?)?.items;
    let mut names = Vec::new();
    for item in &items {
        if let Item::Fn(f) = item
            && is_test(f)
        {
            check_test(f)?;
            names.push(f.name.clone());
        }
    }
    Ok(names)
}

fn is_test(f: &FnDecl) -> bool {
    f.decorators.iter().any(|d| d.name == "test")
}

fn check_test(f: &FnDecl) -> Result<()> {
    let span = f.decorators.iter().find(|d| d.name == "test").map_or(f.span, |d| d.span);
    if !f.params.is_empty() || f.ret.is_some() || !f.generics.is_empty() || f.decorators.len() > 1 {
        return Err(Diagnostic::error(
            span,
            format!("a test is `fn {}():`, with no parameters and no result", f.name),
        )
        .with_hint("`@test` goes alone above the function"));
    }
    Ok(())
}

/// A program leaves its `@test` functions out; a test program keeps only the one it runs.
fn keep_tests(items: Vec<Item>, test: Option<&str>) -> Result<Vec<Item>> {
    let mut kept = Vec::new();
    for item in items {
        match item {
            Item::Fn(mut f) if is_test(&f) => {
                check_test(&f)?;
                if test == Some(f.name.as_str()) {
                    f.decorators.clear();
                    kept.push(Item::Fn(f));
                }
            }
            item => kept.push(item),
        }
    }
    Ok(kept)
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
