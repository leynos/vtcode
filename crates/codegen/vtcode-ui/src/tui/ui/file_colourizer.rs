//! LS_COLORS-based file colourization system
//!
//! Parses LS_COLORS environment variable and applies system file type colours.
//! This allows vtcode to respect user's file listing colour preferences.

use anstyle::Style as AnsiStyle;
use hashbrown::HashMap;
use std::env;
use std::path::Path;

/// Manages LS_COLORS parsing and file type styling
#[derive(Debug, Clone)]
pub struct FileColourizer {
    /// Parsed LS_COLORS key-value pairs
    ls_colours_map: HashMap<String, AnsiStyle>,
    /// Whether system has valid LS_COLORS
    has_ls_colours: bool,
}

impl FileColourizer {
    /// Create a new FileColourizer by parsing LS_COLORS environment variable
    pub fn new() -> Self {
        let ls_colours = env::var("LS_COLORS").unwrap_or_default();
        let ls_colours_map = if ls_colours.is_empty() {
            HashMap::new()
        } else {
            Self::parse_ls_colours(&ls_colours)
        };

        Self {
            has_ls_colours: !ls_colours_map.is_empty(),
            ls_colours_map,
        }
    }

    /// Parse LS_COLORS environment variable into style mappings
    ///
    /// LS_COLORS format is: `key1=value1:key2=value2:...`
    /// Example: `di=01;34:ln=01;36:ex=01;32:*rs=00;35`
    fn parse_ls_colours(ls_colours: &str) -> HashMap<String, AnsiStyle> {
        let mut map = HashMap::new();

        for pair in ls_colours.split(':') {
            if pair.is_empty() {
                continue;
            }

            if let Some((key, value)) = pair.split_once('=') {
                let parser = crate::tui::utils::CachedStyleParser::default();
                if let Ok(style) = parser.parse_ls_colours(value) {
                    map.insert(key.to_owned(), style);
                }
            }
        }

        map
    }

    /// Get the appropriate style for a file path based on its type and extension
    pub fn style_for_path(&self, path: &Path) -> Option<AnsiStyle> {
        if !self.has_ls_colours {
            return None;
        }

        // First try to match by extension (e.g., "*.rs", "*.toml")
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext_key = format!("*.{ext}");
            if let Some(style) = self.get_style(&ext_key) {
                return Some(style);
            }
        }

        // Then try to determine file type and match
        let file_type_key = self.determine_file_type_key(path);
        if let Some(style) = self.get_style(&file_type_key) {
            return Some(style);
        }

        // Finally try the fallback file type
        self.get_style("fi") // fi = regular file
    }

    /// Get style from the map with fallbacks
    fn get_style(&self, key: &str) -> Option<AnsiStyle> {
        if let Some(style) = self.ls_colours_map.get(key) {
            return Some(*style);
        }

        // For extension patterns, also try general file type
        if key.starts_with("*.")
            && let Some(style) = self.ls_colours_map.get("fi")
        {
            // regular file
            return Some(*style);
        }

        None
    }

    /// Determine the appropriate LS_COLORS key for a file path
    ///
    /// This uses path-based heuristics to determine file type without I/O.
    fn determine_file_type_key(&self, path: &Path) -> String {
        // Check if path ends with a directory separator (indicates directory)
        let path_str = path.to_string_lossy();
        if path_str.ends_with('/') || path_str.ends_with('\\') {
            return "di".to_string(); // directory
        }

        // Check for common executable patterns
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && (name.ends_with(".sh")
                || name.ends_with(".py")
                || name.ends_with(".rb")
                || name.ends_with(".pl")
                || name.ends_with(".php"))
        {
            return "ex".to_string(); // executable
        }

        // Check for special file types based on name
        match path.file_name().and_then(|n| n.to_str()) {
            Some(name) if name.starts_with('.') => "so".to_string(), // socket/file (special)
            _ => "fi".to_string(),                                   // regular file
        }
    }
}

impl Default for FileColourizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_without_ls_colours() {
        // Create a FileColourizer when LS_COLORS is not set
        // We'll test the parsing function directly rather than modifying global env
        let ls_colours_val = env::var("LS_COLORS").unwrap_or_default();
        let colourizer = FileColourizer::new();

        // If LS_COLORS was not set originally, map should be empty
        if ls_colours_val.is_empty() {
            assert!(!colourizer.has_ls_colours);
            assert!(colourizer.ls_colours_map.is_empty());
        }
    }

    #[test]
    fn test_parse_ls_colours() {
        let ls_colours = "di=01;34:ln=01;36:ex=01;32:*rs=00;35";
        let map = FileColourizer::parse_ls_colours(ls_colours);

        assert_eq!(map.len(), 4);
        assert!(map.contains_key("di"));
        assert!(map.contains_key("ln"));
        assert!(map.contains_key("ex"));
        assert!(map.contains_key("*rs"));
    }

    #[test]
    fn test_style_for_path_no_ls_colours() {
        // Test with empty LS_COLORS map
        let colourizer = FileColourizer {
            ls_colours_map: HashMap::new(),
            has_ls_colours: false,
        };

        let path = Path::new("/tmp/test.rs");
        let style = colourizer.style_for_path(path);

        assert!(style.is_none());
    }

    #[test]
    fn test_determine_file_type_key_directory() {
        // Test with a pre-populated colourizer
        let colourizer = FileColourizer {
            ls_colours_map: {
                let mut map = HashMap::new();
                map.insert("di".to_string(), anstyle::Style::new().bold());
                map
            },
            has_ls_colours: true,
        };

        // This test checks the logic in style_for_path which calls determine_file_type_key internally
        // For directory paths, it should try to match with "di" key
        assert_eq!(colourizer.determine_file_type_key(Path::new("/tmp/dir/")), "di");
    }

    #[test]
    fn test_determine_file_type_key_extension() {
        let colourizer = FileColourizer {
            ls_colours_map: {
                let mut map = HashMap::new();
                map.insert("*.rs".to_string(), anstyle::Style::new().bold());
                map.insert("fi".to_string(), anstyle::Style::new().underline());
                map
            },
            has_ls_colours: true,
        };

        let rs_path = Path::new("/tmp/test.rs");
        let rs_style = colourizer.style_for_path(rs_path);
        assert!(rs_style.is_some());

        let txt_path = Path::new("/tmp/test.txt");
        let txt_style = colourizer.style_for_path(txt_path);
        assert!(txt_style.is_some()); // Should fall back to 'fi' style
    }

    #[test]
    fn test_determine_file_type_key_executables() {
        let colourizer = FileColourizer {
            ls_colours_map: {
                let mut map = HashMap::new();
                map.insert("ex".to_string(), anstyle::Style::new().bold());
                map
            },
            has_ls_colours: true,
        };

        assert_eq!(colourizer.determine_file_type_key(Path::new("/tmp/script.sh")), "ex");
        assert_eq!(colourizer.determine_file_type_key(Path::new("/tmp/main.py")), "ex");
        assert_eq!(colourizer.determine_file_type_key(Path::new("/tmp/main.rb")), "ex");
    }
}
