//! Process-wide registry of imported source files, so a `Span` can say which
//! file it belongs to. Id `0` is reserved for "the source being compiled".

use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: String,
    pub text: Arc<str>,
}

static FILES: RwLock<Vec<SourceFile>> = RwLock::new(Vec::new());

/// Drops `.` components so `/dir/./lib.spar` and `/dir/lib.spar` are one file.
fn tidy(path: &str) -> String {
    use std::path::Component;
    std::path::Path::new(path)
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect::<std::path::PathBuf>()
        .display()
        .to_string()
}

pub fn register(path: &str, text: &str) -> u32 {
    let path = tidy(path);
    let path = path.as_str();
    let mut files = FILES.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = files
        .iter()
        .position(|file| file.path == path && &*file.text == text)
    {
        return index as u32 + 1;
    }
    files.push(SourceFile {
        path: path.to_string(),
        text: Arc::from(text),
    });
    files.len() as u32
}

pub fn lookup(id: u32) -> Option<SourceFile> {
    if id == 0 {
        return None;
    }
    let files = FILES.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    files.get(id as usize - 1).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_and_text_reuse_the_id() {
        let a = register("/tmp/sm-a.spar", "var x: int = 1;");
        let b = register("/tmp/sm-a.spar", "var x: int = 1;");
        assert_eq!(a, b);
        assert!(a >= 1);
    }

    #[test]
    fn edited_file_gets_a_new_id_and_old_text_stays_readable() {
        let old = register("/tmp/sm-b.spar", "old text");
        let new = register("/tmp/sm-b.spar", "new text");
        assert_ne!(old, new);
        assert_eq!(&*lookup(old).unwrap().text, "old text");
        assert_eq!(&*lookup(new).unwrap().text, "new text");
        assert_eq!(lookup(new).unwrap().path, "/tmp/sm-b.spar");
    }

    #[test]
    fn dot_components_do_not_split_a_file() {
        let a = register("/tmp/./sm-c.spar", "t");
        let b = register("/tmp/sm-c.spar", "t");
        assert_eq!(a, b);
        assert_eq!(lookup(a).unwrap().path, "/tmp/sm-c.spar");
    }

    #[test]
    fn zero_and_unknown_ids_have_no_file() {
        assert!(lookup(0).is_none());
        assert!(lookup(u32::MAX).is_none());
    }
}
