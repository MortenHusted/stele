//! Shared destination policy for source links in human reports.

/// Whether a source may be linked: a repository-relative path, a fragment,
/// or an HTTPS URL. Renderers must still escape the destination for their
/// output format. Rejected destinations can be displayed as plain text.
pub fn is_safe_source(source: &str) -> bool {
    if source.is_empty()
        || source
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\')
    {
        return false;
    }

    // Check encoded separators and controls too, so encoded text cannot
    // disguise an absolute path or a parent-directory component.
    let mut decoded = Vec::with_capacity(source.len());
    let mut bytes = source.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let Some(high) = bytes.next().and_then(|byte| (byte as char).to_digit(16)) else {
                return false;
            };
            let Some(low) = bytes.next().and_then(|byte| (byte as char).to_digit(16)) else {
                return false;
            };
            let byte = (high * 16 + low) as u8;
            if byte.is_ascii_control() || byte == b'\\' {
                return false;
            }
            decoded.push(byte);
        } else {
            decoded.push(byte);
        }
    }
    let Ok(decoded) = std::str::from_utf8(&decoded) else {
        return false;
    };
    if decoded.chars().any(char::is_control) {
        return false;
    }
    if source
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        let authority = source[8..]
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        return !authority.is_empty();
    }
    if source.starts_with('#') {
        return true;
    }
    let path = decoded.split(['?', '#']).next().unwrap_or_default();
    !path.is_empty()
        && !path.starts_with('/')
        && !path.split('/').any(|part| part == "..")
        && !path.split('/').next().unwrap_or_default().contains(':')
}

/// Explicit filesystem context for repository-relative report citations.
///
/// Construct this from existing source and report directories. Their absolute
/// lexical locations are retained because browsers resolve relative URLs before
/// following directory symlinks. No repository root is inferred. Renderers
/// without context preserve authored links.
#[derive(Debug, Clone)]
pub struct SourceContext {
    prefix: String,
}

impl SourceContext {
    pub fn new(
        source_root: &std::path::Path,
        output_directory: &std::path::Path,
    ) -> Result<Self, String> {
        let directory = |path: &std::path::Path| {
            let metadata = std::fs::metadata(path).map_err(|error| {
                format!("read source-link directory `{}`: {error}", path.display())
            })?;
            if !metadata.is_dir() {
                return Err(format!(
                    "source-link base `{}` is not a directory",
                    path.display()
                ));
            }
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|error| format!("resolve source-link directory: {error}"))?
                    .join(path)
            };
            let mut normalized = std::path::PathBuf::new();
            for component in absolute.components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        normalized.pop();
                    }
                    component => normalized.push(component.as_os_str()),
                }
            }
            Ok(normalized)
        };
        let source = directory(source_root)?;
        let output = directory(output_directory)?;
        let source = source.components().collect::<Vec<_>>();
        let output = output.components().collect::<Vec<_>>();
        if source.first() != output.first() {
            return Err("source and report directories must share a filesystem root".into());
        }
        let common = source
            .iter()
            .zip(&output)
            .take_while(|(a, b)| a == b)
            .count();
        let mut parts = vec!["..".to_string(); output.len() - common];
        for component in &source[common..] {
            let value = component
                .as_os_str()
                .to_str()
                .ok_or("source-link directory is not UTF-8")?;
            let mut encoded = String::new();
            for byte in value.bytes() {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    encoded.push(byte as char);
                } else {
                    use std::fmt::Write;
                    write!(encoded, "%{byte:02X}").expect("writing to a string succeeds");
                }
            }
            parts.push(encoded);
        }
        let prefix = if parts.is_empty() {
            String::new()
        } else {
            format!("{}/", parts.join("/"))
        };
        Ok(Self { prefix })
    }
}

/// Validate authored metadata before adding any trusted presentation context.
pub(crate) fn source_destination(source: &str, context: Option<&SourceContext>) -> Option<String> {
    if !is_safe_source(source) {
        return None;
    }
    if source.starts_with('#')
        || source
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        return Some(source.to_string());
    }
    Some(match context {
        Some(context) => format!("{}{source}", context.prefix),
        None => source.to_string(),
    })
}
