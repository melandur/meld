//! Requests to open a comparison tab, from the command line, the new
//! comparison page, recent comparisons or another tab.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum Comparison {
    /// Two or three files; `None` is a blank pane.
    Files {
        paths: Vec<Option<PathBuf>>,
        labels: Vec<String>,
        merge_output: Option<PathBuf>,
        auto_merge: bool,
        extras: FileExtras,
    },
    /// Two or three folders.
    Folders {
        paths: Vec<PathBuf>,
        labels: Vec<String>,
        auto_compare: bool,
    },
    /// A version-controlled folder or file.
    VersionControl {
        location: PathBuf,
        auto_compare: bool,
    },
    /// A single version-controlled file, compared with its base revision.
    VersionControlFile(PathBuf),
    /// Two revisions of a Git repository, with AI Lens explanations.
    Git {
        location: PathBuf,
        base: Option<String>,
        target: Option<String>,
    },
    /// Two or three images.
    Images { paths: Vec<PathBuf> },
    /// A saved `.meldcmp` comparison file.
    SavedFile(PathBuf),
}

/// Extra details of file comparisons opened from a version control view.
#[derive(Debug, Clone, Default)]
pub struct FileExtras {
    /// Tab label instead of the file names.
    pub tab_label: Option<String>,
    /// Offer to mark the merged file resolved after saving it.
    pub prompt_resolve: bool,
    /// Old/new roles of the two panes when they differ from left/right.
    pub diff_sides: Option<[meld_config::DiffSide; 2]>,
    /// Temporary files (base revisions and the like) to delete when the
    /// comparison closes.
    pub temp_files: Vec<PathBuf>,
}

impl Comparison {
    /// Picks the comparison type for paths given without a type, the way
    /// Meld's `open_files` does: one path is a version control comparison,
    /// several folders are a folder comparison, anything else is files.
    pub fn from_paths(paths: Vec<Option<PathBuf>>, auto_compare: bool) -> Result<Self, String> {
        match paths.as_slice() {
            [] => Err("no paths given".into()),
            [Some(path)] if !path.exists() => Err("Cannot compare a non-existent file".into()),
            [Some(path)] if path.is_dir() => Ok(Comparison::VersionControl {
                location: path.clone(),
                auto_compare,
            }),
            [Some(path)] => Ok(Comparison::VersionControlFile(path.clone())),
            [None] => Err("a blank pane needs a second file to compare".into()),
            _ if paths.len() > 3 => Err(format!(
                "too many arguments (wanted 0-3, got {})",
                paths.len()
            )),
            _ => {
                // Archives are compared as folders (`open_paths` mounts
                // them first).
                let dirs = paths
                    .iter()
                    .filter(|p| p.as_ref().is_some_and(|p| p.is_dir() || meld_files::is_archive(p)))
                    .count();
                if dirs == paths.len() {
                    Ok(Comparison::Folders {
                        paths: paths.into_iter().flatten().collect(),
                        labels: Vec::new(),
                        auto_compare,
                    })
                } else if dirs > 0 {
                    Err("Cannot compare a mixture of files and directories".into())
                } else if paths
                    .iter()
                    .all(|p| p.as_ref().is_some_and(|p| is_image_path(p)))
                {
                    Ok(Comparison::Images {
                        paths: paths.into_iter().flatten().collect(),
                    })
                } else {
                    Ok(Comparison::files(paths))
                }
            }
        }
    }

    pub fn files(paths: Vec<Option<PathBuf>>) -> Self {
        Comparison::Files {
            paths,
            labels: Vec::new(),
            merge_output: None,
            auto_merge: false,
            extras: FileExtras::default(),
        }
    }

    pub fn set_labels(&mut self, new_labels: &[String]) {
        match self {
            Comparison::Files { labels, .. } | Comparison::Folders { labels, .. } => {
                *labels = new_labels.to_vec();
            }
            _ => {}
        }
    }

    pub fn set_merge_output(&mut self, path: PathBuf) {
        if let Comparison::Files { merge_output, .. } = self {
            *merge_output = Some(path);
        }
    }

    pub fn set_auto_merge(&mut self) {
        if let Comparison::Files { auto_merge, .. } = self {
            *auto_merge = true;
        }
    }
}

pub fn is_image_path(path: &std::path::Path) -> bool {
    const IMAGE_EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "ico", "tif", "tiff", "webp", "pnm", "tga",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}
