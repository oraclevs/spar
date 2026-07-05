pub fn is_pascal_case(name: &str) -> bool {
    matches!(name.chars().next(), Some(c) if c.is_uppercase())
        && !name.contains('_')
}

pub fn is_camel_case(name: &str) -> bool {
    matches!(name.chars().next(), Some(c) if c.is_lowercase())
        && !name.contains('_')
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
        None        => String::new(),
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

    #[test] fn pascal_accepts_single_uppercase_word()  { assert!(is_pascal_case("Server")); }
    #[test] fn pascal_accepts_multipart()              { assert!(is_pascal_case("MetaData")); }
    #[test] fn pascal_rejects_lowercase_start()        { assert!(!is_pascal_case("metaData")); }
    #[test] fn pascal_rejects_snake_case()             { assert!(!is_pascal_case("meta_data")); }
    #[test] fn pascal_rejects_empty()                  { assert!(!is_pascal_case("")); }

    #[test] fn camel_accepts_single_lowercase_word()   { assert!(is_camel_case("port")); }
    #[test] fn camel_accepts_multipart()               { assert!(is_camel_case("poolSize")); }
    #[test] fn camel_rejects_uppercase_start()         { assert!(!is_camel_case("PoolSize")); }
    #[test] fn camel_rejects_snake_case()              { assert!(!is_camel_case("pool_size")); }
    #[test] fn camel_rejects_empty()                   { assert!(!is_camel_case("")); }

    #[test] fn pascal_converts_camel_start()           { assert_eq!(to_pascal_case("metaData"),  "MetaData"); }
    #[test] fn pascal_converts_snake()                 { assert_eq!(to_pascal_case("meta_data"), "MetaData"); }
    #[test] fn pascal_leaves_correct_unchanged()       { assert_eq!(to_pascal_case("MetaData"),  "MetaData"); }

    #[test] fn camel_converts_pascal()                 { assert_eq!(to_camel_case("MetaData"),   "metaData"); }
    #[test] fn camel_converts_snake()                  { assert_eq!(to_camel_case("meta_data"),  "metaData"); }
    #[test] fn camel_leaves_correct_unchanged()        { assert_eq!(to_camel_case("poolSize"),   "poolSize"); }

    #[test] fn hint_pascal_suggests_renamed_form()     { assert_eq!(pascal_case_hint("metaData"), "rename to 'MetaData'"); }
    #[test] fn hint_camel_suggests_renamed_form()      { assert_eq!(camel_case_hint("MetaData"),  "rename to 'metaData'"); }
}
