//! The machine a program is built for, named by a triple such as `x86_64-windows-gnu`,
//! and the C compiler that builds for it.

use std::fmt;

const ARCHES: [&str; 3] = ["x86_64", "aarch64", "riscv64"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub arch: String,
    pub os: String,
    /// `gnu` or `musl` on Linux, `gnu` on Windows, empty on macOS.
    pub env: String,
}

impl Platform {
    pub fn host() -> Self {
        let os = std::env::consts::OS.to_string();
        let env = if os == "macos" { "" } else { "gnu" }.to_string();
        Self { arch: std::env::consts::ARCH.into(), os, env }
    }

    /// `arch-os` or `arch-os-env`; the environment defaults to `gnu` where there is one.
    pub fn parse(triple: &str) -> Result<Self, String> {
        let unknown = || {
            format!(
                "unknown target `{triple}`: write <arch>-<os>[-<env>], with arch one of {}, os one of \
                 linux, windows, macos (for example x86_64-windows-gnu)",
                ARCHES.join(", ")
            )
        };
        let parts: Vec<&str> = triple.split('-').collect();
        let (arch, os, env) = match parts[..] {
            [arch, os] => (arch, os, None),
            [arch, os, env] => (arch, os, Some(env)),
            _ => return Err(unknown()),
        };
        if !ARCHES.contains(&arch) {
            return Err(unknown());
        }
        let env = match (os, env) {
            ("linux", None | Some("gnu")) | ("windows", None | Some("gnu")) => "gnu",
            ("linux", Some("musl")) => "musl",
            ("macos", None) => "",
            ("windows", Some("msvc")) => {
                return Err(format!(
                    "target `{triple}`: MSVC cannot build the generated C; use {arch}-windows-gnu"
                ));
            }
            _ => return Err(unknown()),
        };
        Ok(Self { arch: arch.into(), os: os.into(), env: env.into() })
    }

    pub fn is_host(&self) -> bool {
        *self == Self::host()
    }

    /// Added to the name of the binary.
    pub fn exe_suffix(&self) -> &'static str {
        if self.os == "windows" { ".exe" } else { "" }
    }

    /// `CC_<triple>` (with `_` for `-`), then `CC` when it is the only target built, then the
    /// usual name of a cross compiler.
    pub fn c_compiler(&self, only_target: bool) -> Result<String, String> {
        let variable = format!("CC_{}", self.to_string().replace('-', "_"));
        if let Ok(compiler) = std::env::var(&variable) {
            return Ok(compiler);
        }
        if only_target && let Ok(compiler) = std::env::var("CC") {
            return Ok(compiler);
        }
        if self.is_host() {
            return Ok("cc".into());
        }
        match self.os.as_str() {
            "linux" => Ok(format!("{}-linux-{}-gcc", self.arch, self.env)),
            "windows" => Ok(format!("{}-w64-mingw32-gcc", self.arch)),
            _ => Err(format!("no usual C compiler builds for {self} from here: set {variable}")),
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}-{}", self.arch, self.os)?;
        if !self.env.is_empty() {
            write!(f, "-{}", self.env)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn triples() {
        assert_eq!(Platform::parse("x86_64-windows").unwrap().to_string(), "x86_64-windows-gnu");
        assert_eq!(Platform::parse("aarch64-linux-musl").unwrap().env, "musl");
        assert_eq!(Platform::parse("aarch64-macos").unwrap().to_string(), "aarch64-macos");
        assert!(Platform::parse("x86_64-windows-msvc").unwrap_err().contains("MSVC"));
        assert!(Platform::parse("sparc-linux").is_err());
        assert!(Platform::parse("x86_64").is_err());
    }

    #[test]
    fn cross_compilers() {
        let windows = Platform::parse("x86_64-windows-gnu").unwrap();
        if !windows.is_host() && std::env::var("CC_x86_64_windows_gnu").is_err() {
            assert_eq!(windows.c_compiler(false).unwrap(), "x86_64-w64-mingw32-gcc");
        }
    }
}
