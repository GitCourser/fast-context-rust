use std::fs;
use std::io;
use std::path::Path;

pub const PROJECT_PATH_REQUIRED_MESSAGE: &str =
    "project_path is required. Pass the absolute path to the project root directory.";

pub fn validate_project_path(project_path: Option<&str>) -> Option<String> {
    let Some(project_path) = project_path else {
        return Some(format!("Error: {PROJECT_PATH_REQUIRED_MESSAGE}"));
    };

    if project_path.trim().is_empty() {
        return Some(format!("Error: {PROJECT_PATH_REQUIRED_MESSAGE}"));
    }

    let path = Path::new(project_path);
    if !path.is_absolute() {
        return Some(format!(
            "Error: project_path must be an absolute path, got: {project_path}"
        ));
    }

    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => None,
        Ok(_) => Some(format!(
            "Error: project_path is not a directory: {project_path}"
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(format!(
            "Error: project_path does not exist: {project_path}"
        )),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Some(format!(
            "Error: cannot access project_path (EACCES): {project_path}"
        )),
        Err(error) => Some(format!(
            "Error: failed to validate project_path: UNKNOWN: {error}"
        )),
    }
}

pub fn validate_project_path_required(project_path: &str) -> Result<(), String> {
    match validate_project_path(Some(project_path)) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
