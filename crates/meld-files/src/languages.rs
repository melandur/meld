//! An embedded table of GtkSourceView languages
//!
//! Meld used GtkSourceView's language manager to map file names and
//! content types to language IDs. Its `.lang` files aren't available
//! here, so this table replicates the IDs, names, file name globs and
//! MIME types of the common GtkSourceView 5 languages. IDs keep
//! GtkSourceView's vocabulary (`python3`, `sh`, `js`, `chdr`, ...), so
//! they can be used as stable keys for syntax highlighting.

use std::sync::OnceLock;

/// A language known to the language manager
#[derive(Debug)]
pub struct LanguageSpec {
    /// GtkSourceView language ID, e.g. `"python3"`
    pub id: &'static str,
    /// Display name, e.g. `"Python 3"`
    pub name: &'static str,
    /// File name globs, matched case-sensitively against base names
    pub globs: &'static [&'static str],
    /// MIME types handled by the language
    pub mime_types: &'static [&'static str],
}

macro_rules! lang {
    ($id:literal, $name:literal, [$($glob:literal),* $(,)?], [$($mime:literal),* $(,)?]) => {
        LanguageSpec { id: $id, name: $name, globs: &[$($glob),*], mime_types: &[$($mime),*] }
    };
}

/// All known languages
pub static LANGUAGES: &[LanguageSpec] = &[
    lang!(
        "ada",
        "Ada",
        ["*.adb", "*.ads"],
        ["text/x-ada", "text/x-adasrc"]
    ),
    lang!(
        "asciidoc",
        "AsciiDoc",
        ["*.asciidoc", "*.adoc"],
        ["text/asciidoc"]
    ),
    lang!(
        "automake",
        "Automake",
        ["Makefile.am", "GNUmakefile.am"],
        []
    ),
    lang!("awk", "awk", ["*.awk"], ["application/x-awk"]),
    lang!("bibtex", "BibTeX", ["*.bib"], ["text/x-bibtex"]),
    lang!(
        "c",
        "C",
        ["*.c"],
        ["text/x-c", "text/x-csrc", "image/x-xpixmap"]
    ),
    lang!(
        "c-sharp",
        "C#",
        ["*.cs"],
        ["text/x-csharp", "text/x-csharpsrc"]
    ),
    lang!(
        "changelog",
        "ChangeLog",
        ["ChangeLog*"],
        ["text/x-changelog"]
    ),
    lang!("chdr", "C/ObjC Header", ["*.h"], ["text/x-chdr"]),
    lang!(
        "cmake",
        "CMake",
        [
            "CMakeLists.txt",
            "*.cmake",
            "*.cmake.in",
            "*.ctest",
            "*.ctest.in"
        ],
        ["text/x-cmake"]
    ),
    lang!(
        "commonlisp",
        "Common Lisp",
        ["*.cl", "*.lisp", "*.el", "*.asd"],
        ["text/x-common-lisp", "text/x-emacs-lisp"]
    ),
    lang!(
        "cpp",
        "C++",
        ["*.cpp", "*.cxx", "*.cc", "*.C", "*.c++", "*.tpp"],
        ["text/x-c++", "text/x-cpp", "text/x-c++src"]
    ),
    lang!(
        "cpphdr",
        "C++ Header",
        ["*.hh", "*.hp", "*.hpp", "*.h++", "*.hxx"],
        ["text/x-c++hdr"]
    ),
    lang!("css", "CSS", ["*.css", "*.CSSL"], ["text/css"]),
    lang!("csv", "CSV", ["*.csv"], ["text/csv"]),
    lang!("cuda", "CUDA", ["*.cu", "*.cuh"], ["text/x-cuda"]),
    lang!("d", "D", ["*.d"], ["text/x-dsrc"]),
    lang!(
        "dart",
        "Dart",
        ["*.dart"],
        [
            "application/dart",
            "application/x-dart",
            "text/dart",
            "text/x-dart"
        ]
    ),
    lang!(
        "desktop",
        ".desktop",
        ["*.desktop", "*.kdelnk"],
        ["application/x-desktop"]
    ),
    lang!(
        "diff",
        "Diff",
        ["*.diff", "*.patch", "*.rej"],
        ["text/x-diff", "text/x-patch", "text/x-reject"]
    ),
    lang!(
        "docbook",
        "DocBook",
        ["*.docbook"],
        ["application/docbook+xml"]
    ),
    lang!(
        "docker",
        "Dockerfile",
        ["Dockerfile", "Containerfile", "*.dockerfile"],
        ["text/x-dockerfile"]
    ),
    lang!(
        "dosbatch",
        "DOS Batch",
        ["*.bat", "*.cmd", "*.sys"],
        ["text/x-dosbatch"]
    ),
    lang!(
        "dot",
        "Graphviz Dot",
        ["*.dot", "*.gv"],
        ["text/vnd.graphviz"]
    ),
    lang!("dtd", "DTD", ["*.dtd"], ["text/x-dtd"]),
    lang!("eiffel", "Eiffel", ["*.e", "*.eif"], ["text/x-eiffel"]),
    lang!("elixir", "Elixir", ["*.ex", "*.exs"], ["text/x-elixir"]),
    lang!("erlang", "Erlang", ["*.erl", "*.hrl"], ["text/x-erlang"]),
    lang!(
        "fortran",
        "Fortran 95",
        [
            "*.f", "*.f90", "*.f95", "*.for", "*.F", "*.F90", "*.f03", "*.f08"
        ],
        ["text/x-fortran"]
    ),
    lang!(
        "fsharp",
        "F#",
        ["*.fs", "*.fsx", "*.fsi"],
        ["text/x-fsharp"]
    ),
    lang!(
        "gettext-translation",
        "gettext translation",
        ["*.po", "*.pot"],
        [
            "text/x-po",
            "text/x-pot",
            "text/x-gettext-translation",
            "text/x-gettext-translation-template"
        ]
    ),
    lang!(
        "glsl",
        "OpenGL Shading Language",
        [
            "*.glsl", "*.glslv", "*.glslf", "*.vert", "*.frag", "*.geom", "*.comp"
        ],
        ["text/x-glsl"]
    ),
    lang!("go", "Go", ["*.go"], ["text/x-go"]),
    lang!(
        "groovy",
        "Groovy",
        ["*.groovy", "*.gradle"],
        ["text/x-groovy"]
    ),
    lang!("haskell", "Haskell", ["*.hs"], ["text/x-haskell"]),
    lang!(
        "haskell-literate",
        "Literate Haskell",
        ["*.lhs"],
        ["text/x-literate-haskell"]
    ),
    lang!("html", "HTML", ["*.html", "*.htm"], ["text/html"]),
    lang!("idl", "IDL", ["*.idl"], ["text/x-idl"]),
    lang!(
        "ini",
        ".ini",
        [
            "*.ini",
            "*.cfg",
            "*.conf",
            "*.editorconfig",
            ".editorconfig",
            "*.gitconfig",
            ".gitconfig",
            ".gitmodules"
        ],
        ["text/x-dosini", "text/x-ini"]
    ),
    lang!("java", "Java", ["*.java"], ["text/x-java"]),
    lang!(
        "js",
        "JavaScript",
        ["*.js", "*.mjs", "*.cjs", "*.node"],
        [
            "application/javascript",
            "application/x-javascript",
            "text/x-javascript",
            "text/javascript",
            "text/x-js"
        ]
    ),
    lang!(
        "json",
        "JSON",
        [
            "*.json",
            "*.geojson",
            "*.topojson",
            "*.jsonc",
            "*.ipynb",
            ".babelrc",
            ".eslintrc",
            "flake.lock"
        ],
        [
            "application/json",
            "application/geo+json",
            "application/x-ipynb+json"
        ]
    ),
    lang!(
        "jsx",
        "JavaScript (React)",
        ["*.jsx"],
        ["text/jsx", "text/x-jsx"]
    ),
    lang!(
        "julia",
        "Julia",
        ["*.jl"],
        ["text/x-julia", "application/x-julia"]
    ),
    lang!("kotlin", "Kotlin", ["*.kt", "*.kts"], ["text/x-kotlin"]),
    lang!(
        "latex",
        "LaTeX",
        [
            "*.tex", "*.ltx", "*.sty", "*.cls", "*.dtx", "*.ins", "*.bbl"
        ],
        ["text/x-tex"]
    ),
    lang!("less", "Less", ["*.less"], ["text/x-less"]),
    lang!(
        "libtool",
        "libtool",
        ["*.la", "*.lai", "*.lo"],
        ["text/x-libtool"]
    ),
    lang!("llvm", "LLVM IR", ["*.ll"], ["text/x-llvm"]),
    lang!("lua", "Lua", ["*.lua"], ["text/x-lua"]),
    lang!(
        "m4",
        "m4",
        ["*.m4", "configure.ac", "configure.in"],
        ["application/x-m4"]
    ),
    lang!(
        "makefile",
        "Makefile",
        ["[Mm]akefile", "GNUmakefile", "*.make", "*.mak", "*.mk"],
        ["text/x-makefile"]
    ),
    lang!(
        "markdown",
        "Markdown",
        ["*.markdown", "*.md", "*.mkd"],
        ["text/x-markdown", "text/markdown"]
    ),
    lang!("matlab", "Matlab", [], ["text/x-matlab"]),
    lang!(
        "meson",
        "Meson",
        ["meson.build", "meson_options.txt", "meson.options"],
        ["text/x-meson"]
    ),
    lang!("nsis", "NSIS", ["*.nsi", "*.nsh"], []),
    lang!("objc", "Objective-C", ["*.m"], ["text/x-objcsrc"]),
    lang!(
        "ocaml",
        "OCaml",
        ["*.ml", "*.mli", "*.mll", "*.mly"],
        ["text/x-ocaml"]
    ),
    lang!("octave", "Octave", [], ["text/x-octave"]),
    lang!("pascal", "Pascal", ["*.p", "*.pas"], ["text/x-pascal"]),
    lang!(
        "perl",
        "Perl",
        ["*.pl", "*.pm", "*.al", "*.perl", "*.t"],
        ["text/x-perl", "application/x-perl"]
    ),
    lang!(
        "php",
        "PHP",
        ["*.php", "*.php3", "*.php4", "*.phtml"],
        [
            "text/x-php",
            "application/x-php",
            "text/x-php-source",
            "application/x-php-source"
        ]
    ),
    lang!("pkgconfig", "pkg-config", ["*.pc"], ["text/x-pkg-config"]),
    lang!(
        "powershell",
        "PowerShell",
        ["*.ps1", "*.psm1", "*.psd1"],
        ["text/x-powershell"]
    ),
    lang!("prolog", "Prolog", ["*.prolog"], ["text/x-prolog"]),
    lang!("proto", "Protobuf", ["*.proto"], ["text/x-protobuf"]),
    lang!(
        "python",
        "Python 2",
        ["*.py2"],
        ["text/x-python2", "application/x-python2"]
    ),
    lang!(
        "python3",
        "Python 3",
        [
            "*.py",
            "*.py3",
            "*.pyi",
            "*.pyw",
            "SConstruct",
            "SConscript",
            "wscript"
        ],
        [
            "text/x-python",
            "application/x-python",
            "text/x-python3",
            "application/x-python3"
        ]
    ),
    lang!(
        "r",
        "R",
        [
            "*.R",
            "*.Rout",
            "*.r",
            "*.Rhistory",
            "*.Rt",
            "*.Rout.save",
            "*.Rout.fail"
        ],
        ["text/x-R"]
    ),
    lang!("rpmspec", "RPM spec", ["*.spec"], ["text/x-rpm-spec"]),
    lang!("rst", "reStructuredText", ["*.rst"], ["text/x-rst"]),
    lang!(
        "ruby",
        "Ruby",
        [
            "*.rb",
            "*.rake",
            "*.gemspec",
            "Rakefile",
            "Capfile",
            "Gemfile",
            "Guardfile",
            "Vagrantfile"
        ],
        ["application/x-ruby", "text/x-ruby"]
    ),
    lang!("rust", "Rust", ["*.rs"], ["text/rust", "text/x-rust"]),
    lang!("scala", "Scala", ["*.scala", "*.sc"], ["text/x-scala"]),
    lang!("scheme", "Scheme", ["*.scm", "*.ss"], ["text/x-scheme"]),
    lang!("scss", "SCSS", ["*.scss"], ["text/x-scss"]),
    lang!(
        "sh",
        "sh",
        [
            "*.sh",
            "*.bash",
            "*.zsh",
            "*.ksh",
            "*bashrc",
            ".profile",
            ".bash_profile",
            ".bash_login",
            ".bash_logout",
            ".zshrc",
            ".zprofile",
            ".zshenv",
            ".zlogin",
            ".zlogout",
            "PKGBUILD",
            "*.ebuild",
            "*.eclass",
            "*.bats"
        ],
        [
            "text/x-shellscript",
            "application/x-shellscript",
            "text/x-sh",
            "application/x-sh"
        ]
    ),
    lang!("sml", "Standard ML", ["*.sml", "*.sig"], ["text/x-sml"]),
    lang!("sql", "SQL", ["*.sql"], ["text/x-sql"]),
    lang!("swift", "Swift", ["*.swift"], ["text/x-swift"]),
    lang!(
        "systemverilog",
        "SystemVerilog",
        ["*.sv", "*.svh"],
        ["text/x-systemverilog"]
    ),
    lang!(
        "tcl",
        "Tcl",
        ["*.tcl", "*.tk"],
        ["text/x-tcl", "application/x-tcl"]
    ),
    lang!(
        "texinfo",
        "Texinfo",
        ["*.texi", "*.texinfo"],
        ["text/x-texinfo"]
    ),
    lang!(
        "toml",
        "TOML",
        [
            "*.toml",
            "*.tml",
            "Cargo.lock",
            "Pipfile",
            "poetry.lock",
            "uv.lock"
        ],
        ["application/toml", "text/x-toml"]
    ),
    lang!(
        "typescript",
        "TypeScript",
        ["*.ts", "*.mts", "*.cts"],
        [
            "application/typescript",
            "application/x-typescript",
            "text/x-typescript",
            "text/typescript"
        ]
    ),
    lang!(
        "typescript-jsx",
        "TypeScript JSX",
        ["*.tsx"],
        ["text/x-tsx"]
    ),
    lang!("vala", "Vala", ["*.vala", "*.vapi"], ["text/x-vala"]),
    lang!("vbnet", "VB.NET", ["*.vb"], ["text/x-vb", "text/x-vbnet"]),
    lang!("verilog", "Verilog", ["*.v"], ["text/x-verilog-src"]),
    lang!("vhdl", "VHDL", ["*.vhd", "*.vhdl"], ["text/x-vhdl"]),
    lang!(
        "xml",
        "XML",
        [
            "*.xml",
            "*.xspf",
            "*.siv",
            "*.smil",
            "*.smi",
            "*.kino",
            "*.xul",
            "*.xbel",
            "*.abw",
            "*.zabw",
            "*.glade",
            "*.jnlp",
            "*.xhtml",
            "*.svg",
            "*.mml",
            "*.rdf",
            "*.rss",
            "*.wsdl",
            "*.xlf",
            "*.xliff",
            "*.xmi",
            "*.xsd",
            "*.gir",
            "*.ui",
            "*.csproj",
            "*.vbproj",
            "*.fsproj",
            "*.vcxproj",
            "*.plist",
            "*.xsl",
            "*.xslt",
            "*.metainfo.xml",
            "*.policy"
        ],
        [
            "application/xml",
            "text/xml",
            "image/svg+xml",
            "application/xhtml+xml",
            "application/rss+xml",
            "application/rdf+xml",
            "application/x-glade",
            "application/xslt+xml"
        ]
    ),
    lang!(
        "yacc",
        "Yacc",
        ["*.y", "*.yacc"],
        ["text/x-yacc", "text/x-bison"]
    ),
    lang!(
        "lex",
        "Lex",
        ["*.l", "*.lex", "*.flex"],
        ["text/x-lex", "text/x-flex"]
    ),
    lang!(
        "yaml",
        "YAML",
        ["*.yaml", "*.yml", ".clang-format", ".clang-tidy"],
        [
            "application/x-yaml",
            "text/x-yaml",
            "application/yaml",
            "text/yaml"
        ]
    ),
];

/// Look up a language by ID
pub fn language(id: &str) -> Option<&'static LanguageSpec> {
    LANGUAGES.iter().find(|l| l.id == id)
}

/// All language IDs
#[cfg(test)]
pub fn language_ids() -> impl Iterator<Item = &'static str> {
    LANGUAGES.iter().map(|l| l.id)
}

struct CompiledGlob {
    regex: regex::Regex,
    literal_len: usize,
    lang: &'static LanguageSpec,
}

fn compiled_globs() -> &'static [CompiledGlob] {
    static GLOBS: OnceLock<Vec<CompiledGlob>> = OnceLock::new();
    GLOBS.get_or_init(|| {
        let mut globs = Vec::new();
        for lang in LANGUAGES {
            for glob in lang.globs {
                if let Some(regex) = crate::filters::compile_shell_pattern(glob) {
                    let literal_len = glob
                        .chars()
                        .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
                        .count();
                    globs.push(CompiledGlob {
                        regex,
                        literal_len,
                        lang,
                    });
                }
            }
        }
        globs
    })
}

/// Guess a language from a file's base name using the globs
///
/// When several globs match, the most specific one (with the most
/// literal characters, so `CMakeLists.txt` beats `*.txt`) wins.
pub fn language_for_filename(filename: &str) -> Option<&'static LanguageSpec> {
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    if basename.is_empty() {
        return None;
    }
    let mut best: Option<&CompiledGlob> = None;
    for glob in compiled_globs() {
        if glob.regex.is_match(basename) && best.is_none_or(|b| glob.literal_len > b.literal_len) {
            best = Some(glob);
        }
    }
    best.map(|g| g.lang)
}

/// MIME type parents for the types we sniff or that are commonly
/// reported, standing in for `g_content_type_is_a()`
const MIME_PARENTS: &[(&str, &str)] = &[
    ("image/svg+xml", "application/xml"),
    ("application/xhtml+xml", "application/xml"),
    ("text/xml", "application/xml"),
    ("application/x-docbook+xml", "application/xml"),
    ("text/x-csrc", "text/x-c"),
];

/// Guess a language from a MIME type
pub fn language_for_mime_type(mime: &str) -> Option<&'static LanguageSpec> {
    let mut current = mime;
    for _ in 0..4 {
        if let Some(lang) = LANGUAGES.iter().find(|l| l.mime_types.contains(&current)) {
            return Some(lang);
        }
        current = MIME_PARENTS.iter().find(|(child, _)| *child == current)?.1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = language_ids().collect();
        let count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn filename_lookup() {
        let id = |name: &str| language_for_filename(name).map(|l| l.id);
        assert_eq!(id("foo.py"), Some("python3"));
        assert_eq!(id("/a/b/main.rs"), Some("rust"));
        assert_eq!(id("Makefile"), Some("makefile"));
        assert_eq!(id("makefile"), Some("makefile"));
        assert_eq!(id("Makefile.am"), Some("automake"));
        assert_eq!(id("CMakeLists.txt"), Some("cmake"));
        assert_eq!(id("x.h"), Some("chdr"));
        assert_eq!(id("x.hpp"), Some("cpphdr"));
        assert_eq!(id("x.C"), Some("cpp"));
        assert_eq!(id(".bashrc"), Some("sh"));
        assert_eq!(id("meson.build"), Some("meson"));
        assert_eq!(id("Cargo.lock"), Some("toml"));
        assert_eq!(id("x.metainfo.xml"), Some("xml"));
        assert_eq!(id("notes.txt"), None);
        assert_eq!(id(""), None);
    }

    #[test]
    fn mime_lookup() {
        let id = |mime: &str| language_for_mime_type(mime).map(|l| l.id);
        assert_eq!(id("application/xml"), Some("xml"));
        assert_eq!(id("image/svg+xml"), Some("xml"));
        assert_eq!(id("text/x-python"), Some("python3"));
        assert_eq!(id("text/plain"), None);
    }
}
