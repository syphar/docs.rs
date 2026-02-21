use anyhow::Result;
use std::sync::LazyLock;
use syntect::{
    html::{ClassStyle, ClassedHTMLGenerator},
    parsing::{SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};
use tracing::{debug, error};

const TOTAL_CODE_BYTE_LENGTH_LIMIT: usize = 2 * 1024 * 1024;
const PER_LINE_BYTE_LENGTH_LIMIT: usize = 512;

#[derive(Debug, thiserror::Error)]
#[error("the code exceeded a highlighting limit")]
pub struct LimitsExceeded;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(|| {
    let mut builder = SyntaxSet::load_defaults_nonewlines().into_builder();

    for syntax in two_face::syntax::extra_no_newlines()
        .into_builder()
        .syntaxes()
    {
        builder.add(syntax.clone());
    }

    let syntaxes = builder.build();

    let names = syntaxes
        .syntaxes()
        .iter()
        .map(|s| &s.name)
        .collect::<Vec<_>>();
    debug!(?names, "known syntaxes");

    syntaxes
});

fn try_with_syntax(syntax: &SyntaxReference, code: &str) -> Result<String> {
    if code.len() > TOTAL_CODE_BYTE_LENGTH_LIMIT {
        return Err(LimitsExceeded.into());
    }

    let mut html_generator = ClassedHTMLGenerator::new_with_class_style(
        syntax,
        &SYNTAXES,
        ClassStyle::SpacedPrefixed { prefix: "syntax-" },
    );

    for line in LinesWithEndings::from(code) {
        if line.len() > PER_LINE_BYTE_LENGTH_LIMIT {
            return Err(LimitsExceeded.into());
        }
        html_generator.parse_html_for_line_which_includes_newline(line)?;
    }

    Ok(html_generator.finalize())
}

fn select_syntax(
    name: Option<&str>,
    code: &str,
    default: Option<&str>,
) -> &'static SyntaxReference {
    name.and_then(|name| {
        if name.is_empty()
            && let Some(default) = default
        {
            return SYNTAXES.find_syntax_by_token(default);
        }
        SYNTAXES.find_syntax_by_token(name).or_else(|| {
            name.rsplit_once('.')
                .and_then(|(_, ext)| SYNTAXES.find_syntax_by_token(ext))
        })
    })
    .or_else(|| SYNTAXES.find_syntax_by_first_line(code))
    .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text())
}

pub fn try_with_lang(lang: Option<&str>, code: &str, default: Option<&str>) -> Result<String> {
    try_with_syntax(select_syntax(lang, code, default), code)
}

pub fn with_lang(lang: Option<&str>, code: &str, default: Option<&str>) -> String {
    match try_with_lang(lang, code, default) {
        Ok(highlighted) => highlighted,
        Err(err) => {
            if err.is::<LimitsExceeded>() {
                debug!("hit limit while highlighting code");
            } else {
                error!(?err, "failed while highlighting code");
            }
            crate::page::templates::filters::escape_html_inner(code)
                .map(|s| s.to_string())
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::highlight::SYNTAXES;
    use pretty_assertions::assert_eq;

    use super::{
        LimitsExceeded, PER_LINE_BYTE_LENGTH_LIMIT, TOTAL_CODE_BYTE_LENGTH_LIMIT, select_syntax,
        try_with_lang, with_lang,
    };

    #[test]
    fn custom_filetypes() {
        let toml = select_syntax(Some("toml"), "", None);

        assert_eq!(
            select_syntax(Some("Cargo.toml.orig"), "", None).name,
            toml.name
        );
        assert_eq!(select_syntax(Some("Cargo.lock"), "", None).name, toml.name);
    }

    #[test]
    fn dotfile_with_extension() {
        let toml = select_syntax(Some("toml"), "", None);

        assert_eq!(
            select_syntax(Some(".rustfmt.toml"), "", None).name,
            toml.name
        );
    }

    #[test]
    fn limits() {
        let is_limited = |s: String| {
            try_with_lang(Some("toml"), &s, None)
                .unwrap_err()
                .is::<LimitsExceeded>()
        };
        assert!(is_limited("a\n".repeat(TOTAL_CODE_BYTE_LENGTH_LIMIT)));
        assert!(is_limited("aa".repeat(PER_LINE_BYTE_LENGTH_LIMIT)));
    }

    #[test]
    fn limited_escaped() {
        let text = "<p>\n".to_string() + "aa".repeat(PER_LINE_BYTE_LENGTH_LIMIT).as_str();
        let highlighted = with_lang(Some("toml"), &text, None);
        assert!(highlighted.starts_with("&lt;p&gt;\n"));
    }

    #[test]
    fn all_discovered_syntaxes() {
        let mut names = SYNTAXES
            .syntaxes()
            .iter()
            .map(|s| &s.name)
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(
            vec![
                "ASP",
                "ActionScript",
                "AppleScript",
                "Batch File",
                "BibTeX",
                "Bourne Again Shell (bash)",
                "C",
                "C#",
                "C++",
                "CSS",
                "Cargo Build Results",
                "Clojure",
                "D",
                "DMD Output",
                "Diff",
                "Erlang",
                "Git Attributes",
                "Git Commit",
                "Git Common",
                "Git Config",
                "Git Ignore",
                "Git Link",
                "Git Log",
                "Git Mailmap",
                "Git Rebase Todo",
                "Go",
                "Graphviz (DOT)",
                "Groovy",
                "HTML",
                "HTML (ASP)",
                "HTML (Erlang)",
                "HTML (Rails)",
                "HTML (Tcl)",
                "Haskell",
                "JSON",
                "Java",
                "Java Properties",
                "Java Server Page (JSP)",
                "Javadoc",
                "JavaScript",
                "JavaScript (Babel)",
                "JavaScript (Rails)",
                "LaTeX",
                "LaTeX Log",
                "Lisp",
                "Literate Haskell",
                "Lua",
                "MATLAB",
                "Make Output",
                "Makefile",
                "Markdown",
                "MultiMarkdown",
                "NAnt Build File",
                "OCaml",
                "OCamllex",
                "OCamlyacc",
                "Objective-C",
                "Objective-C++",
                "PHP",
                "PHP Source",
                "Pascal",
                "Perl",
                "Plain Text",
                "Python",
                "R",
                "R Console",
                "Rd (R Documentation)",
                "Regular Expression",
                "Regular Expressions (Javascript)",
                "Regular Expressions (PHP)",
                "Regular Expressions (Python)",
                "Ruby",
                "Ruby Haml",
                "Ruby on Rails",
                "Rust",
                "SQL",
                "SQL (Rails)",
                "Scala",
                "Shell-Unix-Generic",
                "TOML",
                "Tcl",
                "TeX",
                "Textile",
                "XML",
                "YAML",
                "camlp4",
                "commands-builtin-shell-bash",
                "reStructuredText",
            ],
            names,
        );
    }
}
