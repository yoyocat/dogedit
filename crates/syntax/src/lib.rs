pub mod dynlang;
pub mod folding;
pub mod highlight;
pub mod install;
pub mod manifest;
pub mod registry;
pub mod remote;

pub use dynlang::{PluginError, load_parser};
pub use folding::folding_spans;
pub use highlight::{
    runs_in_range, Highlighter, HighlightRun, ThemeConfig, WindowPaint, STYLE_BOLD, STYLE_BOX,
    STYLE_DOTTED, STYLE_DOUBLE, STYLE_ITALIC, STYLE_STRIKE, STYLE_UNDERLINE,
    STYLE_UNDERLINE_FAMILY, STYLE_WAVY,
};
pub use manifest::PluginManifest;
pub use registry::{
    LanguageRegistry, PluginDiag, PluginInfo, PluginState, default_extension, query_inherits,
};
pub use remote::{RegistryDoc, RemotePackage};
