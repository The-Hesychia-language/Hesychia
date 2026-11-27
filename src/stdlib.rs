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
    ("scan", include_str!("../stdlib/scan.hsy")),
    ("csv", include_str!("../stdlib/csv.hsy")),
    ("json", include_str!("../stdlib/json.hsy")),
    ("thread", include_str!("../stdlib/thread.hsy")),
    ("testing", include_str!("../stdlib/testing.hsy")),
    ("net", include_str!("../stdlib/net.hsy")),
    ("re", include_str!("../stdlib/re.hsy")),
    ("url", include_str!("../stdlib/url.hsy")),
    ("html", include_str!("../stdlib/html.hsy")),
    ("http", include_str!("../stdlib/http.hsy")),
    ("args", include_str!("../stdlib/args.hsy")),
    ("binary", include_str!("../stdlib/binary.hsy")),
    ("elf", include_str!("../stdlib/elf.hsy")),
    // OffSec: tools for security work, shipped with the compiler like the rest.
    ("offsec.cyclic", include_str!("../stdlib/offsec/cyclic.hsy")),
    ("offsec.encode", include_str!("../stdlib/offsec/encode.hsy")),
    ("offsec.fmt", include_str!("../stdlib/offsec/fmt.hsy")),
    ("offsec.payload", include_str!("../stdlib/offsec/payload.hsy")),
    ("offsec.quine", include_str!("../stdlib/offsec/quine.hsy")),
    ("offsec.rop", include_str!("../stdlib/offsec/rop.hsy")),
    ("offsec.shellcode", include_str!("../stdlib/offsec/shellcode.hsy")),
    ("offsec.tube", include_str!("../stdlib/offsec/tube.hsy")),
];

/// The source of a standard module.
pub fn source(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|(module, _)| *module == name).map(|(_, source)| *source)
}
