use std::collections::BTreeMap;
use std::path::Path;

use crate::error::SparError;

pub fn load(path: &Path) -> Result<BTreeMap<String, String>, SparError> {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error_at_path(path, error.to_string())),
    };
    let mut values = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(error_at_path(
                path,
                format!("line {} is missing '='", index + 1),
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(error_at_path(
                path,
                format!("line {} has an empty key", index + 1),
            ));
        }
        let value = strip_comment(value.trim()).trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}

fn strip_comment(value: &str) -> &str {
    let mut quote = None;
    for (index, character) in value.char_indices() {
        if matches!(character, '\'' | '"') {
            quote = if quote == Some(character) {
                None
            } else {
                quote.or(Some(character))
            };
        } else if character == '#'
            && quote.is_none()
            && value[..index]
                .chars()
                .last()
                .is_some_and(char::is_whitespace)
        {
            return &value[..index];
        }
    }
    value
}

fn error_at_path(path: &Path, message: String) -> SparError {
    SparError::EvalError {
        message: format!("failed to read dotenv file '{}': {message}", path.display()),
        span: crate::Span::dummy(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::load;

    #[test]
    fn parses_dotenv_values_comments_and_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        fs::write(
            &path,
            "\n# comment\nKEY=value\nQUOTED=\"quoted value\"\nLITERAL='literal # value'\nCOMMENT=value # comment\n",
        )
        .unwrap();

        assert_eq!(
            load(&path).unwrap(),
            BTreeMap::from([
                ("KEY".to_string(), "value".to_string()),
                ("QUOTED".to_string(), "quoted value".to_string()),
                ("LITERAL".to_string(), "literal # value".to_string()),
                ("COMMENT".to_string(), "value".to_string()),
            ])
        );
    }

    #[test]
    fn rejects_malformed_lines_and_empty_keys_but_allows_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(".env");
        assert!(load(&missing).unwrap().is_empty());

        for contents in ["MALFORMED", "=value"] {
            let path = dir.path().join("invalid.env");
            fs::write(&path, contents).unwrap();
            assert!(load(&path).is_err(), "must reject {contents}");
        }
    }
}
