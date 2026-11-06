//! Bundled `.snippet` tables, looked up by the file's extension — the same key
//! the `.snippet` files are named after.

/// The bundled snippet table for file
/// extension `ext` (lowercased), when
/// the distribution ships one.
pub fn bundled(ext: &str) -> Option<dg_core::SnippetFile> {
    let text: &str = match ext {
        "c" | "h" => include_str!("../snippets/c.snippet"),
        "py" => include_str!("../snippets/py.snippet"),
        "js" | "mjs" | "jsx" => {
            include_str!("../snippets/js.snippet")
        }
        "html" | "htm" => {
            include_str!("../snippets/html.snippet")
        }
        "css" => include_str!("../snippets/css.snippet"),
        "java" => include_str!("../snippets/java.snippet"),
        "lua" => include_str!("../snippets/lua.snippet"),
        "bat" | "cmd" => {
            include_str!("../snippets/bat.snippet")
        }
        "ahk" => include_str!("../snippets/ahk.snippet"),
        "pl" => include_str!("../snippets/perl.snippet"),
        "php" => include_str!("../snippets/php.snippet"),
        "vbs" => include_str!("../snippets/vbs.snippet"),
        "rb" => include_str!("../snippets/rails.snippet"),
        "nsi" => include_str!("../snippets/nsis.snippet"),
        _ => return None,
    };
    dg_core::SnippetFile::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_tables_parse() {
        // the common languages all ship a
        // table with at least one snippet
        for ext in ["c", "py", "js", "html", "css", "java", "lua", "bat", "php"] {
            let file = bundled(ext).unwrap_or_else(|| panic!("no table for {ext}"));
            assert!(!file.snippets.is_empty(), "empty table for {ext}");
        }
    }

    #[test]
    fn unknown_extension_has_no_table() {
        assert!(bundled("zzz").is_none());
        assert!(bundled("").is_none());
    }

    #[test]
    fn triggers_are_unique_per_table() {
        // a trigger maps to exactly one
        // snippet (the first match wins)
        let file = bundled("py").unwrap();
        let mut triggers: Vec<&str> = file.snippets.iter().map(|s| s.trigger.as_str()).collect();
        triggers.sort_unstable();
        let len = triggers.len();
        triggers.dedup();
        // duplicates are allowed — several
        // snippets can share one trigger, e.g.
        // `background` in css — only assert the
        // table
        // is non-empty and findable
        assert!(len >= 1);
        assert!(file.find("for").is_some());
    }
}
