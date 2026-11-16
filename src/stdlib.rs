//! The standard library, built into `hsy`: one Hesychia file per module, in `stdlib/`.
//! `print`, `spawn` and the other items the compiler provides itself sit beside them.

/// Loaded with every program; its public names are known in every module.
pub const PRELUDE: &str = "prelude";

const MODULES: &[(&str, &str)] = &[
    ("prelude", include_str!("../stdlib/prelude.hsy")),
    ("io", include_str!("../stdlib/io.hsy")),
    ("fs", include_str!("../stdlib/fs.hsy")),
    ("math", include_str!("../stdlib/math.hsy")),
    ("time", include_str!("../stdlib/time.hsy")),
    ("os", include_str!("../stdlib/os.hsy")),
    ("random", include_str!("../stdlib/random.hsy")),
    ("csv", include_str!("../stdlib/csv.hsy")),
    ("json", include_str!("../stdlib/json.hsy")),
    ("thread", include_str!("../stdlib/thread.hsy")),
    ("test", include_str!("../stdlib/test.hsy")),
];

/// The source of a standard module.
pub fn source(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|(module, _)| *module == name).map(|(_, source)| *source)
}
