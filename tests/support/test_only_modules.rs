//! Conservative external unit-test module resolution for source inventories.
use std::path::{Path, PathBuf};

/// Exclude only an ordinary external module carrying exactly #[cfg(test)].
/// Rust's parser handles visibility; unfamiliar attributes (notably #[path])
/// and unparsable source remain visible to the inventory for review.
pub fn external_test_module_roots(sources: &[(String, String)]) -> Vec<PathBuf> {
    let mut excluded = Vec::new();
    for (relative, source) in sources {
        let Ok(file) = syn::parse_file(source) else {
            continue;
        };
        let path = Path::new(relative);
        let stem = path.file_stem().unwrap().to_str().unwrap();
        let parent = path.parent().unwrap();
        let directory = if matches!(stem, "lib" | "main" | "mod") {
            parent.to_path_buf()
        } else {
            parent.join(stem)
        };
        for item in file.items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            if module.content.is_some() || module.attrs.len() != 1 {
                continue;
            }
            let syn::Meta::List(cfg) = &module.attrs[0].meta else {
                continue;
            };
            if !cfg.path.is_ident("cfg") || cfg.tokens.to_string() != "test" {
                continue;
            }
            let module = directory.join(module.ident.to_string());
            excluded.push(module.with_extension("rs"));
            excluded.push(module);
        }
    }
    excluded
}
