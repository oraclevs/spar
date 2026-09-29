pub fn is_pascal_case(name: &str) -> bool {
    matches!(name.chars().next(), Some(c) if c.is_uppercase()) && !name.contains('_')
}

pub fn is_camel_case(name: &str) -> bool {
    matches!(name.chars().next(), Some(c) if c.is_lowercase()) && !name.contains('_')
}

/// `MAX_RETRIES`-style names, accepted for `const` declarations alongside camelCase.
pub fn is_screaming_snake_case(name: &str) -> bool {
    matches!(name.chars().next(), Some(c) if c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub fn to_pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut cap = true;
    for ch in name.chars() {
        if ch == '_' {
            cap = true;
        } else if cap {
            out.extend(ch.to_uppercase());
            cap = false;
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn to_camel_case(name: &str) -> String {
    let pascal = to_pascal_case(name);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
    }
}

pub fn pascal_case_hint(name: &str) -> String {
    format!("rename to '{}'", to_pascal_case(name))
}

pub fn camel_case_hint(name: &str) -> String {
    format!("rename to '{}'", to_camel_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_accepts_single_uppercase_word() {
        assert!(is_pascal_case("Server"));
    }
    #[test]
    fn pascal_accepts_multipart() {
        assert!(is_pascal_case("MetaData"));
    }
    #[test]
    fn pascal_rejects_lowercase_start() {
        assert!(!is_pascal_case("metaData"));
    }
    #[test]
    fn pascal_rejects_snake_case() {
        assert!(!is_pascal_case("meta_data"));
    }
    #[test]
    fn pascal_rejects_empty() {
        assert!(!is_pascal_case(""));
    }

    #[test]
    fn camel_accepts_single_lowercase_word() {
        assert!(is_camel_case("port"));
    }
    #[test]
    fn camel_accepts_multipart() {
        assert!(is_camel_case("poolSize"));
    }
    #[test]
    fn camel_rejects_uppercase_start() {
        assert!(!is_camel_case("PoolSize"));
    }
    #[test]
    fn camel_rejects_snake_case() {
        assert!(!is_camel_case("pool_size"));
    }
    #[test]
    fn camel_rejects_empty() {
        assert!(!is_camel_case(""));
    }

    #[test]
    fn pascal_converts_camel_start() {
        assert_eq!(to_pascal_case("metaData"), "MetaData");
    }
    #[test]
    fn pascal_converts_snake() {
        assert_eq!(to_pascal_case("meta_data"), "MetaData");
    }
    #[test]
    fn pascal_leaves_correct_unchanged() {
        assert_eq!(to_pascal_case("MetaData"), "MetaData");
    }

    #[test]
    fn camel_converts_pascal() {
        assert_eq!(to_camel_case("MetaData"), "metaData");
    }
    #[test]
    fn camel_converts_snake() {
        assert_eq!(to_camel_case("meta_data"), "metaData");
    }
    #[test]
    fn camel_leaves_correct_unchanged() {
        assert_eq!(to_camel_case("poolSize"), "poolSize");
    }

    #[test]
    fn hint_pascal_suggests_renamed_form() {
        assert_eq!(pascal_case_hint("metaData"), "rename to 'MetaData'");
    }
    #[test]
    fn hint_camel_suggests_renamed_form() {
        assert_eq!(camel_case_hint("MetaData"), "rename to 'metaData'");
    }
}

/// Reverses `loader_scope::isolate`'s mangling for display: `SparModule<hex>Name` -> `Name`,
/// `sparModule<hex>Name` -> `name`. The mangled spelling is compiler plumbing and must never reach
/// diagnostics, hovers or completions. Text without mangled names is returned unchanged.
pub fn demangle(text: &str) -> String {
    if !text.contains("arModule") {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        let lower = rest.starts_with("sparModule");
        let upper = rest.starts_with("SparModule");
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if (lower || upper) && boundary {
            let hex_start = i + "sparModule".len();
            let mut j = hex_start;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || (b'a'..=b'f').contains(&bytes[j])) {
                j += 1;
            }
            // The mangled name always continues with the (upper-cased) first letter of the original.
            if j > hex_start && j < bytes.len() && bytes[j].is_ascii_uppercase() {
                if lower {
                    out.push(bytes[j].to_ascii_lowercase() as char);
                } else {
                    out.push(bytes[j] as char);
                }
                i = j + 1;
                continue;
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod demangle_tests {
    use super::demangle;

    #[test]
    fn strips_mangled_prefixes() {
        assert_eq!(demangle("List<SparModulec5a597130d9698afShelve>"), "List<Shelve>");
        assert_eq!(demangle("sparModule1f2Request"), "request");
        assert_eq!(demangle("fn(a: SparModuleabcHttpClient) -> SparModuleabcHttpClient"), "fn(a: HttpClient) -> HttpClient");
        assert_eq!(demangle("plain SparModule and MySparModule1aFoo"), "plain SparModule and MySparModule1aFoo");
        assert_eq!(demangle("héllo"), "héllo");
    }
}
