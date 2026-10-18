//! Dependency declarations for [`Plugin::DEPENDENCIES`](crate::Plugin::DEPENDENCIES).
//!
//! Everything here is `const`, so the whole list is built at compile time and the loader can
//! read it from the describe export before the plugin runs any code of its own.

use uniloader_api::DependencyKind;

/// One requirement, checked by the loader before the plugin starts.
///
/// ```
/// use uniloader_sdk::Dependency;
///
/// const DEPS: &[Dependency] = &[
///     Dependency::plugin("unreal", ">=0.1"),
///     Dependency::library("openxr_loader.dll"),
///     Dependency::plugin("jump", "").optional(),
/// ];
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Dependency {
    kind: DependencyKind,
    name: &'static str,
    version: &'static str,
    optional: bool,
}

impl Dependency {
    /// Another plugin, by name, with a version requirement (`>=1.2`, `>=1.2, <2`, `=1.4`, or
    /// an empty string for any version). Also forces that plugin to start first.
    #[must_use]
    pub const fn plugin(name: &'static str, version: &'static str) -> Self {
        Dependency {
            kind: DependencyKind::PLUGIN,
            name,
            version,
            optional: false,
        }
    }

    /// A DLL that must be findable, for example `openxr_loader.dll`. The loader only checks
    /// that it can be found; loading it stays the plugin's job.
    #[must_use]
    pub const fn library(file: &'static str) -> Self {
        Dependency {
            kind: DependencyKind::LIBRARY,
            name: file,
            version: "",
            optional: false,
        }
    }

    /// A file that must exist, relative to the game directory or absolute.
    #[must_use]
    pub const fn file(path: &'static str) -> Self {
        Dependency {
            kind: DependencyKind::FILE,
            name: path,
            version: "",
            optional: false,
        }
    }

    /// Carry on without it. A plugin dependency then only affects load order.
    #[must_use]
    pub const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// The ABI form handed to the loader.
    pub(crate) fn to_api(self) -> uniloader_api::Dependency {
        uniloader_api::Dependency {
            struct_size: core::mem::size_of::<uniloader_api::Dependency>(),
            kind: self.kind,
            name: uniloader_api::Str::new(self.name),
            version: uniloader_api::Str::new(self.version),
            optional: u32::from(self.optional),
        }
    }
}
