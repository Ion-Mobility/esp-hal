use std::{env, path::Path};

use log_04::LevelFilter;

#[macro_export]
macro_rules! assert_unique_used_features {
    ($($feature:literal),+ $(,)?) => {
        assert!(
            (0 $(+ cfg!(feature = $feature) as usize)+ ) == 1,
            "Exactly one of the following features must be enabled: {}",
            [$($feature),+].join(", ")
        );
    };
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure that only a single communication method is specified
    assert_unique_used_features!("jtag-serial", "uart", "auto");

    let chip = esp_metadata_generated::Chip::from_cargo_feature()?;
    // Ensure that, if the `jtag-serial` communication method feature is enabled,
    // a compatible chip feature is also enabled.
    let has_jtag_serial = chip.contains("soc_has_usb_device");

    if cfg!(feature = "jtag-serial") && !has_jtag_serial {
        panic!(
            "The `jtag-serial` feature is only supported by the ESP32-C3, ESP32-C6, ESP32-H2, ESP32-P4, and ESP32-S3"
        );
    }

    // Ensure that, if the `colors` is used with `log-04`.
    if cfg!(feature = "colors") && !cfg!(feature = "log-04") {
        println!(
            "cargo:warning=The `colors` feature is only effective when using the `log-04` feature"
        );
    }

    if std::env::var("ESP_LOGLEVEL").is_ok() || std::env::var("ESP_LOGFILTER").is_ok() {
        panic!(
            "`ESP_LOGLEVEL` and `ESP_LOGFILTER` is not supported anymore. Please use `ESP_LOG` instead."
        );
    }

    generate_filter_snippet();

    #[cfg(target_os = "windows")]
    println!("cargo:rustc-cfg=host_is_windows");

    println!("cargo:rerun-if-env-changed=ESP_LOG");
    println!("cargo:rustc-check-cfg=cfg(host_is_windows)");

    Ok(())
}

fn generate_filter_snippet() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("log_filter.rs");

    let filter = env::var("ESP_LOG");
    let snippet = if let Ok(filter) = filter {
        let res = parse_spec(&filter);

        if !res.errors.is_empty() {
            panic!("Error parsing `ESP_LOG`: {:?}", res.errors);
        } else {
            let max = res
                .directives
                .iter()
                .map(|v| v.level)
                .max()
                .unwrap_or(LevelFilter::Off);
            let max = match max {
                LevelFilter::Off => "Off",
                LevelFilter::Error => "Error",
                LevelFilter::Warn => "Warn",
                LevelFilter::Info => "Info",
                LevelFilter::Debug => "Debug",
                LevelFilter::Trace => "Trace",
            };

            let mut snippet = String::new();

            snippet.push_str(&format!(
                "pub(crate) const FILTER_MAX: log_04::LevelFilter = log_04::LevelFilter::{max};"
            ));

            snippet.push_str(
                "pub(crate) fn is_enabled(level: log_04::Level, _target: &str) -> bool {",
            );

            let mut global_level = None;
            for directive in res.directives {
                let level = match directive.level {
                    LevelFilter::Off => "Off",
                    LevelFilter::Error => "Error",
                    LevelFilter::Warn => "Warn",
                    LevelFilter::Info => "Info",
                    LevelFilter::Debug => "Debug",
                    LevelFilter::Trace => "Trace",
                };

                if let Some(name) = directive.name {
                    // If a prefix matches, don't continue to the next directive
                    snippet.push_str(&format!(
                        "if _target.starts_with(\"{}\") {{ return level <= log_04::LevelFilter::{}; }}",
                        &name, level
                    ));
                } else {
                    if global_level.is_some() {
                        panic!("Multiple global log levels specified in `ESP_LOG`");
                    }
                    global_level = Some(level);
                }
            }

            // Place the fallback rule at the end
            if let Some(level) = global_level {
                snippet.push_str(&format!("level <= log_04::LevelFilter::{level}"));
            } else {
                snippet.push_str(" false");
            }
            snippet.push('}');
            snippet
        }
    } else {
        "pub(crate) const FILTER_MAX: log_04::LevelFilter = log_04::LevelFilter::Off; pub(crate) fn is_enabled(_level: log_04::Level, _target: &str) -> bool { true }".to_string()
    };

    std::fs::write(&dest_path, &snippet).unwrap();
}

#[derive(Default, Debug)]
struct ParseResult {
    pub(crate) directives: Vec<Directive>,
    pub(crate) errors: Vec<String>,
}

impl ParseResult {
    fn add_directive(&mut self, directive: Directive) {
        self.directives.push(directive);
    }

    fn add_error(&mut self, message: String) {
        self.errors.push(message);
    }
}

#[derive(Debug)]
struct Directive {
    pub(crate) name: Option<String>,
    pub(crate) level: LevelFilter,
}

/// Parse a logging specification string (e.g:
/// `crate1,crate2::mod3,crate3::x=error/foo`) and return a vector with log
/// directives.
fn parse_spec(spec: &str) -> ParseResult {
    let mut result = ParseResult::default();

    let mut parts = spec.split('/');
    let mods = parts.next();

    if let Some(m) = mods {
        for s in m.split(',').map(|ss| ss.trim()) {
            if s.is_empty() {
                continue;
            }
            let mut parts = s.split('=');
            let (log_level, name) =
                match (parts.next(), parts.next().map(|s| s.trim()), parts.next()) {
                    (Some(part0), None, None) => {
                        // if the single argument is a log-level string or number,
                        // treat that as a global fallback
                        match part0.parse() {
                            Ok(num) => (num, None),
                            Err(_) => (LevelFilter::max(), Some(part0)),
                        }
                    }
                    (Some(part0), Some(""), None) => (LevelFilter::max(), Some(part0)),
                    (Some(part0), Some(part1), None) => {
                        if let Ok(num) = part1.parse() {
                            (num, Some(part0))
                        } else {
                            result.add_error(format!("invalid logging spec '{part1}'"));
                            continue;
                        }
                    }
                    _ => {
                        result.add_error(format!("invalid logging spec '{s}'"));
                        continue;
                    }
                };

            result.add_directive(Directive {
                name: name.map(|s| s.to_owned()),
                level: log_level,
            });
        }
    }

    // Sort by length so that the most specific prefixes come first
    result
        .directives
        .sort_by(|a, b| match (a.name.as_ref(), b.name.as_ref()) {
            (Some(a), Some(b)) => b.len().cmp(&a.len()),
            _ => std::cmp::Ordering::Equal,
        });

    result
}
