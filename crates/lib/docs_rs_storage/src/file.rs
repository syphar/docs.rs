use docs_rs_mimes::detect_mime;
use mime::Mime;
use serde_json::Value;
use std::path::PathBuf;

/// represents a file path from our source or documentation builds.
/// Used to return metadata about the file.
#[derive(Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
}

impl FileEntry {
    pub fn mime(&self) -> Mime {
        detect_mime(&self.path)
    }
}

pub fn file_list_to_json(files: impl IntoIterator<Item = FileEntry>) -> Value {
    Value::Array(
        files
            .into_iter()
            .map(|info| {
                Value::Array(vec![
                    Value::String(info.mime().as_ref().to_string()),
                    Value::String(info.path.into_os_string().into_string().unwrap()),
                    Value::Number(info.size.into()),
                ])
            })
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderEntry {
    File(String, Mime),
    Dir(String),
}

impl FolderEntry {
    pub fn name(&self) -> &str {
        match self {
            FolderEntry::File(name, _) => name,
            FolderEntry::Dir(name) => name,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Dir(_))
    }

    pub fn mime(&self) -> Option<&Mime> {
        match self {
            Self::File(_, mime) => Some(mime),
            Self::Dir(_) => None,
        }
    }
}

impl PartialOrd for FolderEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FolderEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (FolderEntry::Dir(a), FolderEntry::Dir(b)) => a.cmp(b),
            (FolderEntry::File(a, _), FolderEntry::File(b, _)) => a.cmp(b),
            (FolderEntry::Dir(_), FolderEntry::File(_, _)) => std::cmp::Ordering::Less,
            (FolderEntry::File(_, _), FolderEntry::Dir(_)) => std::cmp::Ordering::Greater,
        }
    }
}
