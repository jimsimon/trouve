//! Language detection and content-type classification.
//!
//! Port of `semble/index/files.py`: the extension-to-language table, the
//! docs/config/data language sets, and file-status checks.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use crate::types::ContentType;

/// 1 MB max file size to read and index.
pub const MAX_FILE_BYTES: u64 = 1_000_000;
/// Files smaller than this are checked for being effectively empty.
pub const EMPTY_FILE_BYTES: u64 = 128;

pub static EXTENSION_TO_LANGUAGE: &[(&str, &str)] = &[
    (".4th", "forth"),
    (".ada", "ada"),
    (".adb", "ada"),
    (".adoc", "asciidoc"),
    (".ads", "ada"),
    (".agda", "agda"),
    (".al", "al"),
    (".as", "actionscript"),
    (".asciidoc", "asciidoc"),
    (".asm", "asm"),
    (".astro", "astro"),
    (".awk", "awk"),
    (".axi", "netlinx"),
    (".axs", "netlinx"),
    (".bash", "bash"),
    (".bat", "batch"),
    (".bb", "bitbake"),
    (".bbappend", "bitbake"),
    (".bbclass", "bitbake"),
    (".beancount", "beancount"),
    (".bib", "bibtex"),
    (".bicep", "bicep"),
    (".blade", "blade"),
    (".bq", "sql_bigquery"),
    (".brs", "brightscript"),
    (".bsl", "bsl"),
    (".bzl", "starlark"),
    (".c", "c"),
    (".c3", "c3"),
    (".c3i", "c3"),
    (".c3t", "c3"),
    (".caddyfile", "caddy"),
    (".cairo", "cairo"),
    (".capnp", "capnp"),
    (".cbl", "cobol"),
    (".cc", "cpp"),
    (".cedar", "cedar"),
    (".cedarschema", "cedarschema"),
    (".cel", "cel"),
    (".cfc", "cfml"),
    (".cfg", "ini"),
    (".chatito", "chatito"),
    (".circom", "circom"),
    (".cjs", "javascript"),
    (".ck", "chuck"),
    (".cl", "commonlisp"),
    (".clar", "clarity"),
    (".clj", "clojure"),
    (".cljc", "clojure"),
    (".cljs", "clojure"),
    (".cls", "abl"),
    (".cmake", "cmake"),
    (".cmd", "batch"),
    (".cob", "cobol"),
    (".cobol", "cobol"),
    (".conf", "nginx"),
    (".cook", "cooklang"),
    (".corn", "corn"),
    (".cpon", "cpon"),
    (".cpp", "cpp"),
    (".cr", "crystal"),
    (".cs", "csharp"),
    (".cshtml", "razor"),
    (".css", "css"),
    (".cst", "cst"),
    (".csv", "csv"),
    (".cts", "typescript"),
    (".cu", "cuda"),
    (".cuda", "cuda"),
    (".cue", "cue"),
    (".cxx", "cpp"),
    (".cylc", "cylc"),
    (".d", "d"),
    (".dart", "dart"),
    (".desktop", "desktop"),
    (".dhall", "dhall"),
    (".diff", "diff"),
    (".dj", "djot"),
    (".dl", "souffle"),
    (".dockerfile", "dockerfile"),
    (".dot", "dot"),
    (".dsp", "faust"),
    (".dtd", "dtd"),
    (".dts", "devicetree"),
    (".dtsi", "devicetree"),
    (".ebnf", "ebnf"),
    (".eds", "eds"),
    (".eex", "eex"),
    (".el", "elisp"),
    (".elm", "elm"),
    (".elv", "elvish"),
    (".enforce", "enforce"),
    (".eps", "postscript"),
    (".erb", "embeddedtemplate"),
    (".erl", "erlang"),
    (".ex", "elixir"),
    (".exs", "elixir"),
    (".f", "fortran"),
    (".f03", "fortran"),
    (".f08", "fortran"),
    (".f90", "fortran"),
    (".f95", "fortran"),
    (".fc", "func"),
    (".fidl", "fidl"),
    (".filter", "poe_filter"),
    (".fir", "firrtl"),
    (".fish", "fish"),
    (".fnl", "fennel"),
    (".fs", "fsharp"),
    (".fsd", "facility"),
    (".fsi", "fsharp_signature"),
    (".fsx", "fsharp"),
    (".fth", "forth"),
    (".fun", "sml"),
    (".g", "gap"),
    (".gd", "gdscript"),
    (".gdshader", "gdshader"),
    (".gi", "gap"),
    (".gitattributes", "gitattributes"),
    (".gitignore", "gitignore"),
    (".gleam", "gleam"),
    (".glsl", "glsl"),
    (".gn", "gn"),
    (".gni", "gn"),
    (".gnuplot", "gnuplot"),
    (".go", "go"),
    (".gotmpl", "gotmpl"),
    (".gp", "gnuplot"),
    (".gql", "graphql"),
    (".gradle", "groovy"),
    (".graphql", "graphql"),
    (".gren", "gren"),
    (".groovy", "groovy"),
    (".gv", "dot"),
    (".h", "c"),
    (".hack", "hack"),
    (".hare", "hare"),
    (".hbs", "glimmer"),
    (".hcl", "hcl"),
    (".heex", "heex"),
    (".hjson", "hjson"),
    (".hlsl", "hlsl"),
    (".hocon", "hocon"),
    (".hoon", "hoon"),
    (".hpp", "cpp"),
    (".hrl", "erlang"),
    (".hs", "haskell"),
    (".htm", "html"),
    (".html", "html"),
    (".http", "http"),
    (".hurl", "hurl"),
    (".hx", "haxe"),
    (".hxx", "cpp"),
    (".idr", "idris"),
    (".inc", "sourcepawn"),
    (".ini", "ini"),
    (".ino", "arduino"),
    (".ispc", "ispc"),
    (".j2", "jinja2"),
    (".jai", "jai"),
    (".janet", "janet"),
    (".java", "java"),
    (".jinja2", "jinja2"),
    (".jl", "julia"),
    (".journal", "ledger"),
    (".jq", "jq"),
    (".js", "javascript"),
    (".json", "json"),
    (".json5", "json5"),
    (".jsonnet", "jsonnet"),
    (".jsx", "javascript"),
    (".just", "just"),
    (".k", "kcl"),
    (".kdl", "kdl"),
    (".kt", "kotlin"),
    (".kts", "kotlin"),
    (".lc", "elsa"),
    (".ldg", "ledger"),
    (".lds", "linkerscript"),
    (".lean", "lean"),
    (".ledger", "ledger"),
    (".leex", "eex"),
    (".less", "less"),
    (".libsonnet", "jsonnet"),
    (".liquid", "liquid"),
    (".lisp", "commonlisp"),
    (".ll", "llvm"),
    (".lua", "lua"),
    (".luau", "luau"),
    (".m", "objc"),
    (".magik", "magik"),
    (".makefile", "make"),
    (".markdown", "markdown"),
    (".matlab", "matlab"),
    (".md", "markdown"),
    (".mermaid", "mermaid"),
    (".meson", "meson"),
    (".mjs", "javascript"),
    (".mk", "make"),
    (".ml", "ocaml"),
    (".mli", "ocaml_interface"),
    (".mlir", "mlir"),
    (".mll", "ocamllex"),
    (".mmd", "mermaid"),
    (".mod", "gomod"),
    (".mojo", "mojo"),
    (".move", "move"),
    (".mts", "typescript"),
    (".nasm", "nasm"),
    (".ncl", "nickel"),
    (".nginx", "nginx"),
    (".nim", "nim"),
    (".nims", "nim"),
    (".ninja", "ninja"),
    (".nix", "nix"),
    (".norg", "norg"),
    (".nqc", "nqc"),
    (".nu", "nushell"),
    (".nut", "squirrel"),
    (".odin", "odin"),
    (".org", "org"),
    (".p", "abl"),
    (".pas", "pascal"),
    (".patch", "diff"),
    (".pbtxt", "textproto"),
    (".pem", "pem"),
    (".pgn", "pgn"),
    (".php", "php"),
    (".pkl", "pkl"),
    (".pl", "perl"),
    (".plt", "gnuplot"),
    (".pm", "perl"),
    (".po", "po"),
    (".pony", "pony"),
    (".pot", "po"),
    (".pp", "puppet"),
    (".prisma", "prisma"),
    (".pro", "prolog"),
    (".promql", "promql"),
    (".properties", "properties"),
    (".proto", "proto"),
    (".prql", "prql"),
    (".ps", "postscript"),
    (".ps1", "powershell"),
    (".psd1", "powershell"),
    (".psm1", "powershell"),
    (".psv", "psv"),
    (".pug", "pug"),
    (".purs", "purescript"),
    (".py", "python"),
    (".pyi", "python"),
    (".pyw", "python"),
    (".ql", "ql"),
    (".qml", "qmljs"),
    (".r", "r"),
    (".rasi", "rasi"),
    (".razor", "razor"),
    (".rb", "ruby"),
    (".rbs", "rbs"),
    (".re", "re2c"),
    (".rego", "rego"),
    (".res", "rescript"),
    (".resi", "rescript"),
    (".rkt", "racket"),
    (".robot", "robot"),
    (".roc", "roc"),
    (".ron", "ron"),
    (".rs", "rust"),
    (".rst", "rst"),
    (".rtf", "rtf"),
    (".s", "asm"),
    (".scad", "openscad"),
    (".scala", "scala"),
    (".scm", "scheme"),
    (".scss", "scss"),
    (".sh", "bash"),
    (".shtml", "superhtml"),
    (".sig", "sml"),
    (".slang", "slang"),
    (".smali", "smali"),
    (".smithy", "smithy"),
    (".smk", "snakemake"),
    (".sml", "sml"),
    (".sol", "solidity"),
    (".sp", "sourcepawn"),
    (".sparql", "sparql"),
    (".sql", "sql"),
    (".squirrel", "squirrel"),
    (".st", "smalltalk"),
    (".stan", "stan"),
    (".star", "starlark"),
    (".sv", "systemverilog"),
    (".svelte", "svelte"),
    (".svh", "systemverilog"),
    (".sw", "sway"),
    (".swift", "swift"),
    (".tact", "tact"),
    (".tal", "uxntal"),
    (".tape", "vhs"),
    (".tcl", "tcl"),
    (".td", "tablegen"),
    (".templ", "templ"),
    (".tera", "tera"),
    (".tex", "latex"),
    (".textproto", "textproto"),
    (".tf", "terraform"),
    (".tfvars", "terraform"),
    (".thrift", "thrift"),
    (".tl", "teal"),
    (".tla", "tlaplus"),
    (".todotxt", "todotxt"),
    (".toml", "toml"),
    (".tres", "godot_resource"),
    (".trigger", "apex"),
    (".ts", "typescript"),
    (".tscn", "godot_resource"),
    (".tsconfig", "typoscript"),
    (".tsp", "typespec"),
    (".tsv", "tsv"),
    (".tsx", "tsx"),
    (".ttl", "turtle"),
    (".twig", "twig"),
    (".typoscript", "typoscript"),
    (".typst", "typst"),
    (".v", "v"),
    (".vb", "vb"),
    (".verilog", "verilog"),
    (".vhd", "vhdl"),
    (".vhdl", "vhdl"),
    (".vim", "vim"),
    (".vrl", "vrl"),
    (".vue", "vue"),
    (".w", "abl"),
    (".wast", "wast"),
    (".wat", "wat"),
    (".wgsl", "wgsl"),
    (".wit", "wit"),
    (".wl", "wolfram"),
    (".xml", "xml"),
    (".xsl", "xml"),
    (".xslt", "xml"),
    (".yaml", "yaml"),
    (".yml", "yaml"),
    (".yuck", "yuck"),
    (".zig", "zig"),
    (".ziggy", "ziggy"),
    (".zsh", "zsh"),
];

static DOC_LANGUAGES: &[&str] = &[
    "asciidoc",
    "bibtex",
    "djot",
    "doxygen",
    "html",
    "javadoc",
    "jsdoc",
    "latex",
    "luadoc",
    "markdown",
    "markdown_inline",
    "mermaid",
    "norg",
    "norg_meta",
    "org",
    "phpdoc",
    "po",
    "rst",
    "rtf",
    "vimdoc",
];

static CONFIG_LANGUAGES: &[&str] = &[
    "beancount",
    "capnp",
    "cedarschema",
    "comment",
    "cooklang",
    "cpon",
    "desktop",
    "devicetree",
    "diff",
    "dtd",
    "editorconfig",
    "ebnf",
    "git_config",
    "gitattributes",
    "gitcommit",
    "gitignore",
    "godot_resource",
    "gomod",
    "gosum",
    "gowork",
    "gpg",
    "hjson",
    "hocon",
    "ini",
    "kdl",
    "ledger",
    "pem",
    "pgn",
    "properties",
    "proto",
    "requirements",
    "ron",
    "smithy",
    "ssh_config",
    "textproto",
    "thrift",
    "todotxt",
    "toml",
    "turtle",
    "typespec",
    "wit",
    "xcompose",
    "xml",
    "yaml",
    "ziggy_schema",
];

static DATA_LANGUAGES: &[&str] = &["csv", "json", "json5", "psv", "tsv"];

fn extension_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| EXTENSION_TO_LANGUAGE.iter().copied().collect())
}

fn language_sets() -> &'static HashMap<ContentType, HashSet<&'static str>> {
    static SETS: OnceLock<HashMap<ContentType, HashSet<&'static str>>> = OnceLock::new();
    SETS.get_or_init(|| {
        let all: HashSet<&'static str> = EXTENSION_TO_LANGUAGE.iter().map(|(_, l)| *l).collect();
        let docs: HashSet<&'static str> = DOC_LANGUAGES.iter().copied().collect();
        let config: HashSet<&'static str> = CONFIG_LANGUAGES.iter().copied().collect();
        let data: HashSet<&'static str> = DATA_LANGUAGES.iter().copied().collect();
        let code: HashSet<&'static str> = all
            .iter()
            .copied()
            .filter(|l| !docs.contains(l) && !config.contains(l) && !data.contains(l))
            .collect();
        let mut map = HashMap::new();
        map.insert(ContentType::Code, code);
        map.insert(
            ContentType::Docs,
            docs.intersection(&all).copied().collect(),
        );
        map.insert(
            ContentType::Config,
            config.intersection(&all).copied().collect(),
        );
        map
    })
}

/// Detect the language of a file from its extension (lowercased).
pub fn detect_language(file_name: &Path) -> Option<&'static str> {
    let ext = file_name.extension()?.to_str()?.to_lowercase();
    extension_map().get(format!(".{ext}").as_str()).copied()
}

/// Return the sorted list of supported file extensions for the given content types.
pub fn get_extensions(types: &[ContentType]) -> Vec<String> {
    let sets = language_sets();
    let mut languages: HashSet<&'static str> = HashSet::new();
    for t in types {
        if let Some(s) = sets.get(t) {
            languages.extend(s.iter().copied());
        }
    }
    let mut extensions: BTreeSet<String> = BTreeSet::new();
    for (ext, lang) in EXTENSION_TO_LANGUAGE {
        if languages.contains(lang) {
            extensions.insert((*ext).to_string());
        }
    }
    extensions.into_iter().collect()
}

/// Whether a file should be indexed based on its size and content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    TooLarge,
    Empty,
    Valid,
}

/// Read a file's text content, replacing invalid UTF-8 sequences.
pub fn read_file_text(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Classify file content read from disk (size checks mirror upstream `get_file_status`).
pub fn file_status_for_bytes(bytes: &[u8]) -> FileStatus {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return FileStatus::TooLarge;
    }
    if (bytes.len() as u64) < EMPTY_FILE_BYTES && String::from_utf8_lossy(bytes).trim().is_empty() {
        return FileStatus::Empty;
    }
    FileStatus::Valid
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_common_languages() {
        assert_eq!(detect_language(&PathBuf::from("foo.py")), Some("python"));
        assert_eq!(detect_language(&PathBuf::from("foo.RS")), Some("rust"));
        assert_eq!(detect_language(&PathBuf::from("a/b/foo.tsx")), Some("tsx"));
        assert_eq!(detect_language(&PathBuf::from("foo.unknownext")), None);
        assert_eq!(detect_language(&PathBuf::from("Makefile")), None);
    }

    #[test]
    fn code_extensions_exclude_docs_and_config() {
        let code = get_extensions(&[ContentType::Code]);
        assert!(code.contains(&".py".to_string()));
        assert!(code.contains(&".rs".to_string()));
        assert!(!code.contains(&".md".to_string()));
        assert!(!code.contains(&".toml".to_string()));
        assert!(!code.contains(&".json".to_string())); // data language

        let docs = get_extensions(&[ContentType::Docs]);
        assert!(docs.contains(&".md".to_string()));
        assert!(docs.contains(&".rst".to_string()));
        assert!(!docs.contains(&".py".to_string()));

        let config = get_extensions(&[ContentType::Config]);
        assert!(config.contains(&".toml".to_string()));
        assert!(config.contains(&".yaml".to_string()));
        assert!(!config.contains(&".py".to_string()));
    }

    #[test]
    fn all_content_types_union() {
        let all = get_extensions(&ContentType::ALL);
        let code = get_extensions(&[ContentType::Code]);
        assert!(all.len() > code.len());
        assert!(all.contains(&".md".to_string()));
        assert!(all.contains(&".rs".to_string()));
    }

    #[test]
    fn file_status_checks() {
        assert_eq!(file_status_for_bytes(b"   \n \t "), FileStatus::Empty);
        assert_eq!(file_status_for_bytes(b"fn main() {}"), FileStatus::Valid);
        let big = vec![b'a'; (MAX_FILE_BYTES + 1) as usize];
        assert_eq!(file_status_for_bytes(&big), FileStatus::TooLarge);
    }
}
