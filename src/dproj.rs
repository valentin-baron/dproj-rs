#![allow(dead_code)]

use std::collections::HashMap;

use crate::condition;

// ═══════════════════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Expand `$(Var)` references in a raw string value using the given variable
/// map.  Unknown variables expand to the empty string.
fn expand_msbuild_vars(s: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'(') {
            chars.next(); // consume '('
            let var_name: String = chars.by_ref().take_while(|&ch| ch != ')').collect();
            if let Some(val) = vars.get(&var_name) {
                result.push_str(val);
            }
        } else {
            result.push(c);
        }
    }

    result
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Error
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub struct DprojError {
    pub message: String,
}

impl DprojError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl std::fmt::Display for DprojError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for DprojError {}

impl From<roxmltree::Error> for DprojError {
    fn from(error: roxmltree::Error) -> Self {
        Self::new(format!("XML Error: {error}"))
    }
}

impl From<std::io::Error> for DprojError {
    fn from(error: std::io::Error) -> Self {
        Self::new(format!("IO Error: {error}"))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Dproj – top-level handle
// ═══════════════════════════════════════════════════════════════════════════════

/// Handle for reading and mutating a `.dproj` file while preserving its
/// original formatting.
///
/// Reading is done by parsing into fully owned types via `roxmltree`.
/// Mutations splice the raw source string using byte-accurate positions from
/// `roxmltree::Node::range()`, so whitespace, CDATA sections, comments, and
/// attribute ordering are never touched.
#[derive(Debug, Clone)]
pub struct Dproj {
    source: String,
    /// Parent directory of the `.dproj` file, used for resolving relative
    /// paths (e.g. `MainSource`, `DCC_ExeOutput`).  `None` when created via
    /// [`Dproj::parse`] without a file path.
    directory: Option<std::path::PathBuf>,
    /// External environment variables (e.g. from `rsvars.bat` or the system
    /// environment) that are seeded into the `$(Var)` expansion map before
    /// property group evaluation.
    env: HashMap<String, String>,
    pub project: DprojProject,
}

impl Dproj {
    /// Parse a `.dproj` file from its XML source string.
    pub fn parse(source: impl Into<String>) -> Result<Self, DprojError> {
        let source = source.into();
        let project = {
            let doc = roxmltree::Document::parse(&source)?;
            DprojProject::parse(doc.root_element())?
        };
        Ok(Self { source, directory: None, env: HashMap::new(), project })
    }

    /// Load a `.dproj` file from disk.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, DprojError> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path)
            .map_err(|e| DprojError::new(format!("{}: {e}", path.display())))?;
        let mut dproj = Self::parse(source)?;
        dproj.directory = path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        Ok(dproj)
    }

    /// The current raw XML source (reflects any mutations).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Write the (potentially mutated) source back to disk.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), DprojError> {
        std::fs::write(path, &self.source)?;
        Ok(())
    }

    /// Change the text content of an existing element inside the `pg_index`-th
    /// `<PropertyGroup>` (0-based). Returns an error if the PropertyGroup or
    /// element is not found. The in-memory typed struct is updated directly
    /// (no full reparse).
    pub fn set_property_value(
        &mut self,
        pg_index: usize,
        tag: &str,
        value: &str,
    ) -> Result<(), DprojError> {
        let doc = roxmltree::Document::parse(&self.source)?;

        let pg_node = doc
            .root_element()
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "PropertyGroup")
            .nth(pg_index)
            .ok_or_else(|| {
                DprojError::new(format!(
                    "PropertyGroup index {pg_index} out of bounds"
                ))
            })?;

        let element = pg_node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == tag)
            .ok_or_else(|| {
                DprojError::new(format!(
                    "Element <{tag}> not found in PropertyGroup[{pg_index}]"
                ))
            })?;

        if let Some(text_node) = element.children().find(|n| n.is_text()) {
            // Element has text content – replace just the text span.
            let range = text_node.range();
            self.source.replace_range(range, value);
        } else {
            // Self-closing or empty element – rewrite the whole element tag.
            let range = element.range();
            let name = element.tag_name().name();
            let attrs: String = element
                .attributes()
                .map(|a| format!(" {}=\"{}\"", a.name(), a.value()))
                .collect();
            self.source
                .replace_range(range, &format!("<{name}{attrs}>{value}</{name}>"));
        }

        // Targeted in-memory update — no full reparse needed.
        let pg = &mut self.project.property_groups[pg_index];
        if set_project_property(tag, value, &mut pg.project_properties) { return Ok(()); }
        if set_dcc_option(tag, value, &mut pg.dcc_options) { return Ok(()); }
        if set_brcc_option(tag, value, &mut pg.brcc_options) { return Ok(()); }
        if set_build_event(tag, value, &mut pg.build_events) { return Ok(()); }
        if set_ver_info(tag, value, &mut pg.ver_info) { return Ok(()); }
        if set_platform_packaging(tag, value, &mut pg.platform_packaging) { return Ok(()); }
        if set_debugger_option(tag, value, &mut pg.debugger_options) { return Ok(()); }
        pg.other.insert(tag.to_string(), value.to_string());

        Ok(())
    }

    // ─── Listing helpers ─────────────────────────────────────────────────

    /// Return the names of all build configurations defined in the project
    /// (e.g. `["Debug", "Release"]`), in document order.
    pub fn configurations(&self) -> Vec<&str> {
        self.project
            .item_groups
            .iter()
            .flat_map(|ig| &ig.build_configurations)
            .map(|bc| bc.name.as_str())
            .collect()
    }

    /// Return all platforms listed in `<BorlandProject><Platforms>`,
    /// together with their active flag (e.g. `[("Win32", true), ("Win64", false)]`).
    ///
    /// Falls back to the `<TargetedPlatforms>` bitmask or the unconditional
    /// `<Platform>` element when the `<Platforms>` section is absent.
    pub fn platforms(&self) -> Vec<(&str, bool)> {
        // Primary source: ProjectExtensions > BorlandProject > Platforms
        if let Some(ext) = &self.project.project_extensions {
            if let Some(bp) = &ext.borland_project {
                if !bp.platforms.is_empty() {
                    return bp
                        .platforms
                        .iter()
                        .map(|p| (p.value.as_str(), p.active))
                        .collect();
                }
            }
        }

        // Fallback: unconditional <Platform> element.
        for pg in &self.project.property_groups {
            if pg.condition.is_none() {
                if let Some(p) = &pg.project_properties.platform {
                    return vec![(p.as_str(), true)];
                }
            }
        }

        Vec::new()
    }

    /// The project's active configuration name (e.g. `"Debug"`).
    pub fn active_configuration(&self) -> Result<String, DprojError> {
        self.active_config_platform().map(|(c, _)| c)
    }

    /// The project's active platform name (e.g. `"Win32"`).
    pub fn active_platform(&self) -> Result<String, DprojError> {
        self.active_config_platform().map(|(_, p)| p)
    }

    // ─── Path accessors ──────────────────────────────────────────────────

    /// The parent directory of the `.dproj` file (set by [`from_file`](Self::from_file)).
    ///
    /// Returns `None` when the `Dproj` was created via [`parse`](Self::parse)
    /// without a file path.
    pub fn directory(&self) -> Option<&std::path::Path> {
        self.directory.as_deref()
    }

    /// Resolve the project's main source file (`.dpr` or `.dpk`).
    ///
    /// Reads `<MainSource>` from the first unconditional `<PropertyGroup>` and
    /// resolves it relative to the `.dproj` file's directory.
    pub fn get_main_source(&self) -> Result<std::path::PathBuf, DprojError> {
        let dir = self.directory.as_deref().ok_or_else(|| {
            DprojError::new(
                "Cannot resolve main source: no directory (use Dproj::from_file)",
            )
        })?;

        for pg in &self.project.property_groups {
            if pg.condition.is_some() {
                continue;
            }
            if let Some(main) = &pg.project_properties.main_source {
                return Ok(dir.join(main));
            }
        }

        Err(DprojError::new(
            "No <MainSource> element found in any unconditional PropertyGroup",
        ))
    }

    /// Resolve the project's output executable / library path.
    ///
    /// Consults the **active** (merged) property group so the result
    /// respects the current `Config` / `Platform` selection.
    ///
    /// Resolution order:
    /// 1. `<DCC_ExeOutput>` directory + project stem + `.exe`
    /// 2. `<DCC_DependencyCheckOutputName>` (absolute or relative path)
    ///
    /// `$(Variable)` references (e.g. `$(Platform)`, `$(Config)`) inside the
    /// path values are expanded using the active build variables.
    ///
    /// All paths are resolved relative to the `.dproj` file's directory.
    pub fn get_exe_path(&self) -> Result<std::path::PathBuf, DprojError> {
        let (config, platform) = self.active_config_platform()?;
        self.get_exe_path_for(&config, &platform)
    }

    /// Resolve the output executable / library path for an explicitly chosen
    /// configuration and platform.
    ///
    /// Same resolution as [`get_exe_path`](Self::get_exe_path) but uses the
    /// merged property group for the given config/platform pair.
    pub fn get_exe_path_for(
        &self,
        config: &str,
        platform: &str,
    ) -> Result<std::path::PathBuf, DprojError> {
        let dir = self.directory.as_deref().ok_or_else(|| {
            DprojError::new(
                "Cannot resolve exe path: no directory (use Dproj::from_file)",
            )
        })?;

        // active_property_group_for already expands $(Var) references.
        let pg = self.active_property_group_for(config, platform)?;

        if let Some(exe_output) = &pg.dcc_options.exe_output {
            if let Some(stem) = self.project_stem() {
                let exe = dir.join(exe_output).join(&stem).with_extension("exe");
                return Ok(exe);
            }
        }

        if let Some(dep_name) = &pg.dcc_options.dependency_check_output_name {
            return Ok(dir.join(dep_name));
        }

        Err(DprojError::new(format!(
            "No <DCC_ExeOutput> or <DCC_DependencyCheckOutputName> found for {config}/{platform}",
        )))
    }

    /// Derive the project stem (filename without extension) from
    /// `<ProjectName>` or `<MainSource>`.
    fn project_stem(&self) -> Option<String> {
        // Try ProjectName first.
        for pg in &self.project.property_groups {
            if pg.condition.is_some() {
                continue;
            }
            if let Some(name) = &pg.project_properties.project_name {
                return Some(name.clone());
            }
        }
        // Fall back to MainSource stem.
        for pg in &self.project.property_groups {
            if pg.condition.is_some() {
                continue;
            }
            if let Some(main) = &pg.project_properties.main_source {
                return std::path::Path::new(main)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
            }
        }
        None
    }

    // ─── Setters for default config / platform ───────────────────────────

    /// Change the project's default configuration (the text inside the
    /// `<Config>` or `<Configuration>` element in the first unconditional
    /// `<PropertyGroup>`).  Both the raw XML source and the in-memory
    /// struct are updated.
    pub fn set_configuration(&mut self, value: &str) -> Result<(), DprojError> {
        self.set_default_element(&["Config", "Configuration"], value, |pp, v| {
            // Update whichever field was present.
            if pp.config.is_some() {
                pp.config = Some(v.to_string());
            }
            if pp.configuration.is_some() {
                pp.configuration = Some(v.to_string());
            }
        })
    }

    /// Change the project's default platform (the text inside the
    /// `<Platform>` element in the first unconditional `<PropertyGroup>`).
    /// Both the raw XML source and the in-memory struct are updated.
    pub fn set_platform(&mut self, value: &str) -> Result<(), DprojError> {
        self.set_default_element(&["Platform"], value, |pp, v| {
            pp.platform = Some(v.to_string());
        })
    }

    /// Shared implementation for [`set_configuration`](Self::set_configuration)
    /// and [`set_platform`](Self::set_platform).
    ///
    /// Searches unconditional `<PropertyGroup>`s for the first element
    /// whose tag matches one of `candidates`, then byte-splices the new
    /// value into the raw source and calls `update_fn` on the in-memory
    /// [`ProjectProperties`].
    fn set_default_element<F>(
        &mut self,
        candidates: &[&str],
        value: &str,
        update_fn: F,
    ) -> Result<(), DprojError>
    where
        F: Fn(&mut ProjectProperties, &str),
    {
        let doc = roxmltree::Document::parse(&self.source)?;

        // Walk PropertyGroup nodes in document order, looking for the first
        // unconditional one that contains a matching child element.
        let mut pg_index = 0usize;
        let mut found = None;

        for pg_node in doc
            .root_element()
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "PropertyGroup")
        {
            if pg_node.attribute("Condition").is_some() {
                pg_index += 1;
                continue;
            }

            for &tag in candidates {
                if let Some(element) = pg_node
                    .children()
                    .find(|n| n.is_element() && n.tag_name().name() == tag)
                {
                    found = Some((pg_index, element.range(), element.children().find(|n| n.is_text()).map(|t| t.range()), tag));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
            pg_index += 1;
        }

        let (pg_idx, elem_range, text_range, tag) = found.ok_or_else(|| {
            DprojError::new(format!(
                "No <{}> element found in any unconditional PropertyGroup",
                candidates.join("> or <")
            ))
        })?;

        // Byte-splice the raw source.
        if let Some(range) = text_range {
            self.source.replace_range(range, value);
        } else {
            // Self-closing / empty element — rewrite entire element, preserving attributes.
            let doc2 = roxmltree::Document::parse(&self.source)?;
            let element = doc2
                .root_element()
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "PropertyGroup")
                .nth(pg_idx)
                .and_then(|pg| pg.children().find(|n| n.is_element() && n.tag_name().name() == tag))
                .unwrap();
            let attrs: String = element
                .attributes()
                .map(|a| format!(" {}=\"{}\"", a.name(), a.value()))
                .collect();
            self.source
                .replace_range(elem_range, &format!("<{tag}{attrs}>{value}</{tag}>"));
        }

        // Update in-memory struct.
        update_fn(
            &mut self.project.property_groups[pg_idx].project_properties,
            value,
        );

        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  DprojBuilder – ergonomic construction with environment variables
// ═══════════════════════════════════════════════════════════════════════════════

/// Builder for constructing a [`Dproj`] with custom environment variables.
///
/// Environment variables are seeded into the `$(Var)` expansion map before
/// property group evaluation, so references like `$(BDS)`, `$(BDSCOMMONDIR)`,
/// etc. are resolved correctly.
///
/// # Example
/// ```no_run
/// use dproj_rs::{DprojBuilder, rsvars};
///
/// let rsvars = rsvars::parse_rsvars_file(r"C:\Delphi\bin\rsvars.bat").unwrap();
/// let dproj = DprojBuilder::new()
///     .env(rsvars)
///     .system_env()
///     .from_file("MyProject.dproj")
///     .unwrap();
/// ```
#[derive(Debug, Clone, Default)]
pub struct DprojBuilder {
    env: HashMap<String, String>,
}

impl DprojBuilder {
    /// Create a new builder with an empty environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge an entire variable map into the environment.
    ///
    /// Typically used with the result of [`crate::rsvars::parse_rsvars`] or
    /// [`crate::rsvars::parse_rsvars_file`].
    ///
    /// Later calls override earlier values for the same key.
    pub fn env(mut self, vars: HashMap<String, String>) -> Self {
        for (k, v) in vars {
            self.env.insert(k, v);
        }
        self
    }

    /// Set a single environment variable.
    pub fn env_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Parse an `rsvars.bat` file from its contents and merge the resulting
    /// variables into the environment.
    pub fn rsvars(self, content: &str) -> Self {
        let vars = crate::rsvars::parse_rsvars(content);
        self.env(vars)
    }

    /// Parse an `rsvars.bat` file from disk and merge the resulting variables
    /// into the environment.
    pub fn rsvars_file(
        self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, DprojError> {
        let vars = crate::rsvars::parse_rsvars_file(path)
            .map_err(|e| DprojError::new(format!("rsvars: {e}")))?;
        Ok(self.env(vars))
    }

    /// Pull all current process environment variables into the map.
    ///
    /// Useful as a fallback layer: call this *after* [`rsvars`](Self::rsvars)
    /// so that rsvars values take precedence over any stale system env vars.
    /// Or call it *before* to provide a base that rsvars then overrides.
    pub fn system_env(mut self) -> Self {
        for (k, v) in std::env::vars() {
            self.env.insert(k, v);
        }
        self
    }

    /// Parse a `.dproj` file from its XML source string.
    pub fn parse(self, source: impl Into<String>) -> Result<Dproj, DprojError> {
        let mut dproj = Dproj::parse(source)?;
        dproj.env = self.env;
        Ok(dproj)
    }

    /// Load a `.dproj` file from disk.
    pub fn from_file(
        self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<Dproj, DprojError> {
        let mut dproj = Dproj::from_file(path)?;
        dproj.env = self.env;
        Ok(dproj)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Type definitions
// ═══════════════════════════════════════════════════════════════════════════════

// All value fields are `Option<String>` regardless of their logical type
// (bool, integer, path …) because the same XML key can carry different
// representations across Delphi / RAD Studio versions.  Interpretation of
// the raw strings is left to the consumer.

// ─── DprojProject ────────────────────────────────────────────────────────────

/// Root representation of a `.dproj` file (`<Project>`).
#[derive(Debug, Clone, Default)]
pub struct DprojProject {
    pub property_groups: Vec<PropertyGroup>,
    pub item_groups: Vec<ItemGroup>,
    pub project_extensions: Option<ProjectExtensions>,
    pub imports: Vec<Import>,
}

// ─── PropertyGroup ───────────────────────────────────────────────────────────

/// A `<PropertyGroup>` element, optionally gated by a `Condition`.
#[derive(Debug, Clone, Default)]
pub struct PropertyGroup {
    pub condition: Option<String>,
    pub project_properties: ProjectProperties,
    pub dcc_options: DccOptions,
    pub brcc_options: BrccOptions,
    pub build_events: BuildEvents,
    pub ver_info: VerInfo,
    pub platform_packaging: PlatformPackaging,
    pub debugger_options: DebuggerOptions,
    /// XML child elements not captured by the typed fields above.
    pub other: HashMap<String, String>,
}

// ─── Project-level properties ────────────────────────────────────────────────

/// Core project metadata that can appear in any `<PropertyGroup>`.
#[derive(Debug, Clone, Default)]
pub struct ProjectProperties {
    pub project_guid: Option<String>,
    /// `<ProjectVersion>` – MSBuild project-file format version (e.g. "19.5", "20.2").
    pub project_version: Option<String>,
    /// `<Version>` – older format version tag (e.g. "7.0").
    pub version: Option<String>,
    pub framework_type: Option<String>,
    /// `<Config>` – newer condition-style config selector.
    pub config: Option<String>,
    /// `<Configuration>` – older condition-style config selector.
    pub configuration: Option<String>,
    pub platform: Option<String>,
    pub project_name: Option<String>,
    pub targeted_platforms: Option<String>,
    pub app_type: Option<String>,
    pub main_source: Option<String>,
    pub base: Option<String>,
    pub cfg_parent: Option<String>,
    pub sanitized_project_name: Option<String>,
    pub custom_styles: Option<String>,
    pub gen_package: Option<String>,
    pub gen_dll: Option<String>,
    pub use_packages: Option<String>,
    /// `<Icon_MainIcon>`.
    pub icon_main_icon: Option<String>,
    /// `<Icns_MainIcns>` (macOS).
    pub icns_main_icns: Option<String>,
}

// ─── Delphi Compiler (DCC) options ───────────────────────────────────────────

/// All `DCC_*` properties from a `<PropertyGroup>`.
#[derive(Debug, Clone, Default)]
pub struct DccOptions {
    // ── Compiler identity (older format) ──
    pub dcc_compiler: Option<String>,
    pub dependency_check_output_name: Option<String>,

    // ── Output paths ──
    pub dcu_output: Option<String>,
    pub exe_output: Option<String>,
    pub dcp_output: Option<String>,
    pub bpl_output: Option<String>,
    pub obj_output: Option<String>,
    pub hpp_output: Option<String>,
    pub bpi_output: Option<String>,
    pub cbuilder_output: Option<String>,

    // ── Search paths ──
    pub unit_search_path: Option<String>,
    pub resource_path: Option<String>,
    pub include_path: Option<String>,
    pub obj_path: Option<String>,
    pub framework_path: Option<String>,
    pub sys_lib_root: Option<String>,

    // ── Defines & aliases ──
    pub define: Option<String>,
    pub namespace: Option<String>,
    pub unit_alias: Option<String>,
    pub use_package: Option<String>,

    // ── Code generation ──
    pub optimize: Option<String>,
    pub alignment: Option<String>,
    pub minimum_enum_size: Option<String>,
    pub code_page: Option<String>,
    pub inlining: Option<String>,
    pub generate_stack_frames: Option<String>,
    pub generate_pic_code: Option<String>,
    pub generate_android_app_bundle_file: Option<String>,
    pub generate_osx_universal_binary_file: Option<String>,

    // ── Compiler switches ──
    pub e: Option<String>,
    pub n: Option<String>,
    pub s: Option<String>,
    pub f: Option<String>,
    pub k: Option<String>,
    pub extended_syntax: Option<String>,
    pub long_strings: Option<String>,
    pub open_string_params: Option<String>,
    pub strict_var_strings: Option<String>,
    pub typed_at_parameter: Option<String>,
    pub full_boolean_evaluations: Option<String>,
    pub writeable_constants: Option<String>,
    pub run_time_type_info: Option<String>,
    pub pentium_safe_divide: Option<String>,

    // ── Runtime checks ──
    pub io_checking: Option<String>,
    pub integer_overflow_check: Option<String>,
    pub range_checking: Option<String>,
    pub assertions_at_runtime: Option<String>,
    pub imported_data_references: Option<String>,

    // ── Debug ──
    pub debug_information: Option<String>,
    pub local_debug_symbols: Option<String>,
    pub symbol_reference_info: Option<String>,
    pub debug_dcus: Option<String>,
    pub debug_info_in_exe: Option<String>,
    pub debug_info_in_tds: Option<String>,
    pub debug_vn: Option<String>,
    pub remote_debug: Option<String>,

    // ── Warnings & hints ──
    pub hints: Option<String>,
    pub warnings: Option<String>,
    pub show_general_messages: Option<String>,

    // ── Individual warning / hint directives ──
    // Stored by XML tag name (e.g. "DCC_UNSAFE_TYPE" → "False").
    // This catch-all avoids hard-coding ~70 keys that change between versions.
    pub warning_directives: HashMap<String, String>,

    // ── Linker / PE ──
    pub console_target: Option<String>,
    pub description: Option<String>,
    pub additional_switches: Option<String>,
    pub linker_options: Option<String>,
    pub image_base: Option<String>,
    pub map_file: Option<String>,
    pub map_file_arm: Option<String>,
    /// Older combined "min,max" format.
    pub stack_size: Option<String>,
    pub max_stack_size: Option<String>,
    pub min_stack_size: Option<String>,
    pub base_address: Option<String>,
    pub pe_flags: Option<String>,
    pub pe_opt_flags: Option<String>,
    pub pe_os_version: Option<String>,
    pub pe_sub_sys_version: Option<String>,
    pub pe_user_version: Option<String>,
    pub nx_compat: Option<String>,
    pub dynamic_base: Option<String>,
    pub high_entropy_va: Option<String>,
    pub ts_aware: Option<String>,
    pub large_address_aware: Option<String>,
    pub allow_undefined: Option<String>,

    // ── Output control ──
    pub output_xml_documentation: Option<String>,
    pub output_dependencies: Option<String>,
    pub output_drc_file: Option<String>,
    pub old_dos_file_names: Option<String>,
    pub xml_output: Option<String>,
    pub remove_tmp_lnk_file: Option<String>,
    pub include_dcus_in_uses_completion: Option<String>,
    pub use_msbuild_externally: Option<String>,
    pub legacy_ifend: Option<String>,
    pub hpp_output_arm: Option<String>,

    // ── Platform-specific minimum versions ──
    pub ios_minimum_version: Option<String>,
    pub macos_arm_minimum_version: Option<String>,
    pub macos_minimum_version: Option<String>,
}

// ─── BRCC options ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct BrccOptions {
    pub user_supplied_options: Option<String>,
    pub code_page: Option<String>,
    pub language: Option<String>,
    pub delete_include_path: Option<String>,
    pub enable_multi_byte: Option<String>,
    pub compiler_to_use: Option<String>,
    pub response_filename: Option<String>,
    pub verbose: Option<String>,
    pub defines: Option<String>,
    pub include_path: Option<String>,
    pub output_dir: Option<String>,
}

// ─── Build events ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct BuildEvents {
    pub pre_build_event: Option<String>,
    pub pre_build_event_cancel_on_error: Option<String>,
    pub pre_build_event_ignore_exit_code: Option<String>,
    pub pre_link_event: Option<String>,
    pub pre_link_event_cancel_on_error: Option<String>,
    pub pre_link_event_ignore_exit_code: Option<String>,
    pub post_build_event: Option<String>,
    pub post_build_event_cancel_on_error: Option<String>,
    pub post_build_event_ignore_exit_code: Option<String>,
    pub post_build_event_execute_when: Option<String>,
}

// ─── Version info ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct VerInfo {
    pub include_ver_info: Option<String>,
    pub major_ver: Option<String>,
    pub minor_ver: Option<String>,
    pub release: Option<String>,
    pub build: Option<String>,
    pub debug: Option<String>,
    pub pre_release: Option<String>,
    pub special: Option<String>,
    pub private: Option<String>,
    pub dll: Option<String>,
    pub auto_gen_version: Option<String>,
    pub locale: Option<String>,
    pub keys: Option<String>,
}

// ─── Platform / packaging ────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct PlatformPackaging {
    pub app_dpi_awareness_mode: Option<String>,
    pub app_enable_runtime_themes: Option<String>,
    pub app_execution_level: Option<String>,
    pub app_execution_level_ui_access: Option<String>,
    pub manifest_file: Option<String>,
    pub output_ext: Option<String>,
    pub bt_build_type: Option<String>,
    pub pf_uwp_publisher: Option<String>,
    pub pf_uwp_package_name: Option<String>,
    pub pf_uwp_package_display_name: Option<String>,
    pub pf_uwp_publisher_display_name: Option<String>,
    pub pf_uwp_distribution_type: Option<String>,
    pub uwp_delphi_logo44: Option<String>,
    pub uwp_delphi_logo150: Option<String>,
}

// ─── Debugger ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct DebuggerOptions {
    pub include_system_vars: Option<String>,
    pub env_vars: Option<String>,
    pub symbol_source_path: Option<String>,
    pub run_params: Option<String>,
}

// ─── ItemGroup ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ItemGroup {
    pub delphi_compile: Option<DelphiCompile>,
    pub dcc_references: Vec<DccReference>,
    pub build_configurations: Vec<BuildConfiguration>,
}

#[derive(Debug, Clone, Default)]
pub struct DelphiCompile {
    pub include: String,
    pub main_source: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DccReference {
    pub include: String,
    pub form: Option<String>,
    pub form_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BuildConfiguration {
    pub name: String,
    pub key: String,
    pub cfg_parent: Option<String>,
}

// ─── ProjectExtensions ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ProjectExtensions {
    pub borland_personality: Option<String>,
    pub borland_project_type: Option<String>,
    pub borland_project: Option<BorlandProject>,
    pub project_file_version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BorlandProject {
    pub delphi_personality: Option<DelphiPersonality>,
    pub deployment: Option<Deployment>,
    pub platforms: Vec<Platform>,
    pub model_support: Option<String>,
    pub active_x_project_info: Option<ActiveXProjectInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct DelphiPersonality {
    pub parameters: Vec<NameValuePair>,
    pub version_info: Vec<NameValuePair>,
    pub version_info_keys: Vec<NameValuePair>,
    pub type_lib_options: Vec<NameValuePair>,
    pub excluded_packages: Vec<ExcludedPackage>,
    pub sources: Vec<NameValuePair>,
}

#[derive(Debug, Clone, Default)]
pub struct NameValuePair {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExcludedPackage {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub struct ActiveXProjectInfo {
    pub version: Option<String>,
}

// ─── Deployment ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Deployment {
    pub version: Option<String>,
    pub deploy_files: Vec<DeployFile>,
    pub deploy_classes: Vec<DeployClass>,
    pub project_roots: Vec<ProjectRoot>,
}

#[derive(Debug, Clone, Default)]
pub struct DeployFile {
    pub local_name: String,
    pub configuration: Option<String>,
    pub class: Option<String>,
    pub platforms: Vec<DeployFilePlatform>,
}

#[derive(Debug, Clone, Default)]
pub struct DeployFilePlatform {
    pub name: String,
    pub remote_name: Option<String>,
    pub overwrite: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DeployClass {
    pub name: String,
    pub required: Option<String>,
    pub platforms: Vec<DeployClassPlatform>,
}

#[derive(Debug, Clone, Default)]
pub struct DeployClassPlatform {
    pub name: String,
    pub remote_dir: Option<String>,
    pub operation: Option<String>,
    pub extensions: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectRoot {
    pub platform: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Platform {
    pub value: String,
    pub active: bool,
}

// ─── Import ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Import {
    pub project: String,
    pub condition: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Merging – combine PropertyGroups by overriding non-None fields
// ═══════════════════════════════════════════════════════════════════════════════

/// Override `self.$field` with `other.$field` when the latter is `Some`.
macro_rules! merge_options {
    ($self:expr, $other:expr, $($field:ident),* $(,)?) => {
        $(
            if $other.$field.is_some() {
                $self.$field = $other.$field.clone();
            }
        )*
    };
}

/// Expand `$(Var)` references in every `Some` string field using the given
/// variable map.
macro_rules! expand_options {
    ($self:expr, $vars:expr, $($field:ident),* $(,)?) => {
        $(
            if let Some(ref mut v) = $self.$field {
                if v.contains("$(") {
                    *v = expand_msbuild_vars(v, $vars);
                }
            }
        )*
    };
}

/// Collect all `Some` string fields into a `HashMap<tag_name, value>`.
macro_rules! collect_tag_values {
    ($self:expr, $map:expr, $($tag:literal => $field:ident),* $(,)?) => {
        $(
            if let Some(v) = &$self.$field {
                $map.insert($tag.to_string(), v.clone());
            }
        )*
    };
}

impl PropertyGroup {
    /// Merge `other` into `self`: any field that is `Some` in `other`
    /// overwrites the corresponding field in `self`.
    pub fn merge_from(&mut self, other: &Self) {
        self.project_properties.merge_from(&other.project_properties);
        self.dcc_options.merge_from(&other.dcc_options);
        self.brcc_options.merge_from(&other.brcc_options);
        self.build_events.merge_from(&other.build_events);
        self.ver_info.merge_from(&other.ver_info);
        self.platform_packaging.merge_from(&other.platform_packaging);
        self.debugger_options.merge_from(&other.debugger_options);
        for (k, v) in &other.other {
            self.other.insert(k.clone(), v.clone());
        }
    }

    /// Expand `$(Var)` references in every `Some` field using the given
    /// variable map.  Used during MSBuild-style incremental property
    /// evaluation so that self-referencing list properties (e.g.
    /// `"src;$(DCC_UnitSearchPath)"`) are resolved correctly.
    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        self.project_properties.expand_vars(vars);
        self.dcc_options.expand_vars(vars);
        self.brcc_options.expand_vars(vars);
        self.build_events.expand_vars(vars);
        self.ver_info.expand_vars(vars);
        self.platform_packaging.expand_vars(vars);
        self.debugger_options.expand_vars(vars);
        for v in self.other.values_mut() {
            if v.contains("$(") {
                *v = expand_msbuild_vars(v, vars);
            }
        }
    }

    /// Push all currently-set property values into the variable map so
    /// that later property groups can reference them with `$(TagName)`.
    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        self.project_properties.collect_into_vars(vars);
        self.dcc_options.collect_into_vars(vars);
        self.brcc_options.collect_into_vars(vars);
        self.build_events.collect_into_vars(vars);
        self.ver_info.collect_into_vars(vars);
        self.platform_packaging.collect_into_vars(vars);
        self.debugger_options.collect_into_vars(vars);
        for (k, v) in &self.other {
            vars.insert(k.clone(), v.clone());
        }
    }
}

impl ProjectProperties {
    fn merge_from(&mut self, o: &Self) {
        merge_options!(self, o,
            project_guid, project_version, version, framework_type,
            config, configuration, platform, project_name,
            targeted_platforms, app_type, main_source, base,
            cfg_parent, sanitized_project_name, custom_styles,
            gen_package, gen_dll, use_packages,
            icon_main_icon, icns_main_icns,
        );
    }

    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        expand_options!(self, vars,
            project_guid, project_version, version, framework_type,
            config, configuration, platform, project_name,
            targeted_platforms, app_type, main_source, base,
            cfg_parent, sanitized_project_name, custom_styles,
            gen_package, gen_dll, use_packages,
            icon_main_icon, icns_main_icns,
        );
    }

    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        collect_tag_values!(self, vars,
            "ProjectGuid" => project_guid,
            "ProjectVersion" => project_version,
            "Version" => version,
            "FrameworkType" => framework_type,
            "Config" => config,
            "Configuration" => configuration,
            "Platform" => platform,
            "ProjectName" => project_name,
            "TargetedPlatforms" => targeted_platforms,
            "AppType" => app_type,
            "MainSource" => main_source,
            "Base" => base,
            "CfgParent" => cfg_parent,
            "SanitizedProjectName" => sanitized_project_name,
            "Custom_Styles" => custom_styles,
            "GenPackage" => gen_package,
            "GenDll" => gen_dll,
            "UsePackages" => use_packages,
            "Icon_MainIcon" => icon_main_icon,
            "Icns_MainIcns" => icns_main_icns,
        );
    }
}

impl DccOptions {
    fn merge_from(&mut self, o: &Self) {
        merge_options!(self, o,
            dcc_compiler, dependency_check_output_name,
            dcu_output, exe_output, dcp_output, bpl_output,
            obj_output, hpp_output, bpi_output, cbuilder_output,
            unit_search_path, resource_path, include_path, obj_path,
            framework_path, sys_lib_root,
            define, namespace, unit_alias, use_package,
            optimize, alignment, minimum_enum_size, code_page,
            inlining, generate_stack_frames, generate_pic_code,
            generate_android_app_bundle_file, generate_osx_universal_binary_file,
            e, n, s, f, k,
            extended_syntax, long_strings, open_string_params,
            strict_var_strings, typed_at_parameter,
            full_boolean_evaluations, writeable_constants,
            run_time_type_info, pentium_safe_divide,
            io_checking, integer_overflow_check, range_checking,
            assertions_at_runtime, imported_data_references,
            debug_information, local_debug_symbols, symbol_reference_info,
            debug_dcus, debug_info_in_exe, debug_info_in_tds,
            debug_vn, remote_debug,
            hints, warnings, show_general_messages,
            console_target, description, additional_switches,
            linker_options, image_base, map_file, map_file_arm,
            stack_size, max_stack_size, min_stack_size,
            base_address, pe_flags, pe_opt_flags,
            pe_os_version, pe_sub_sys_version, pe_user_version,
            nx_compat, dynamic_base, high_entropy_va, ts_aware,
            large_address_aware, allow_undefined,
            output_xml_documentation, output_dependencies, output_drc_file,
            old_dos_file_names, xml_output, remove_tmp_lnk_file,
            include_dcus_in_uses_completion, use_msbuild_externally,
            legacy_ifend, hpp_output_arm,
            ios_minimum_version, macos_arm_minimum_version, macos_minimum_version,
        );
        for (k, v) in &o.warning_directives {
            self.warning_directives.insert(k.clone(), v.clone());
        }
    }

    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        expand_options!(self, vars,
            dcc_compiler, dependency_check_output_name,
            dcu_output, exe_output, dcp_output, bpl_output,
            obj_output, hpp_output, bpi_output, cbuilder_output,
            unit_search_path, resource_path, include_path, obj_path,
            framework_path, sys_lib_root,
            define, namespace, unit_alias, use_package,
            optimize, alignment, minimum_enum_size, code_page,
            inlining, generate_stack_frames, generate_pic_code,
            generate_android_app_bundle_file, generate_osx_universal_binary_file,
            e, n, s, f, k,
            extended_syntax, long_strings, open_string_params,
            strict_var_strings, typed_at_parameter,
            full_boolean_evaluations, writeable_constants,
            run_time_type_info, pentium_safe_divide,
            io_checking, integer_overflow_check, range_checking,
            assertions_at_runtime, imported_data_references,
            debug_information, local_debug_symbols, symbol_reference_info,
            debug_dcus, debug_info_in_exe, debug_info_in_tds,
            debug_vn, remote_debug,
            hints, warnings, show_general_messages,
            console_target, description, additional_switches,
            linker_options, image_base, map_file, map_file_arm,
            stack_size, max_stack_size, min_stack_size,
            base_address, pe_flags, pe_opt_flags,
            pe_os_version, pe_sub_sys_version, pe_user_version,
            nx_compat, dynamic_base, high_entropy_va, ts_aware,
            large_address_aware, allow_undefined,
            output_xml_documentation, output_dependencies, output_drc_file,
            old_dos_file_names, xml_output, remove_tmp_lnk_file,
            include_dcus_in_uses_completion, use_msbuild_externally,
            legacy_ifend, hpp_output_arm,
            ios_minimum_version, macos_arm_minimum_version, macos_minimum_version,
        );
        for v in self.warning_directives.values_mut() {
            if v.contains("$(") {
                *v = expand_msbuild_vars(v, vars);
            }
        }
    }

    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        collect_tag_values!(self, vars,
            "DCC_DCCCompiler" => dcc_compiler,
            "DCC_DependencyCheckOutputName" => dependency_check_output_name,
            "DCC_DcuOutput" => dcu_output,
            "DCC_ExeOutput" => exe_output,
            "DCC_DcpOutput" => dcp_output,
            "DCC_BplOutput" => bpl_output,
            "DCC_ObjOutput" => obj_output,
            "DCC_HppOutput" => hpp_output,
            "DCC_BpiOutput" => bpi_output,
            "DCC_CBuilderOutput" => cbuilder_output,
            "DCC_UnitSearchPath" => unit_search_path,
            "DCC_ResourcePath" => resource_path,
            "DCC_IncludePath" => include_path,
            "DCC_ObjPath" => obj_path,
            "DCC_FrameworkPath" => framework_path,
            "DCC_SysLibRoot" => sys_lib_root,
            "DCC_Define" => define,
            "DCC_Namespace" => namespace,
            "DCC_UnitAlias" => unit_alias,
            "DCC_UsePackage" => use_package,
            "DCC_Optimize" => optimize,
            "DCC_Alignment" => alignment,
            "DCC_MinimumEnumSize" => minimum_enum_size,
            "DCC_CodePage" => code_page,
            "DCC_Inlining" => inlining,
            "DCC_GenerateStackFrames" => generate_stack_frames,
            "DCC_GeneratePICCode" => generate_pic_code,
            "DCC_GenerateAndroidAppBundleFile" => generate_android_app_bundle_file,
            "DCC_GenerateOSXUniversalBinaryFile" => generate_osx_universal_binary_file,
            "DCC_E" => e,
            "DCC_N" => n,
            "DCC_S" => s,
            "DCC_F" => f,
            "DCC_K" => k,
            "DCC_ExtendedSyntax" => extended_syntax,
            "DCC_LongStrings" => long_strings,
            "DCC_OpenStringParams" => open_string_params,
            "DCC_StrictVarStrings" => strict_var_strings,
            "DCC_TypedAtParameter" => typed_at_parameter,
            "DCC_FullBooleanEvaluations" => full_boolean_evaluations,
            "DCC_WriteableConstants" => writeable_constants,
            "DCC_RunTimeTypeInfo" => run_time_type_info,
            "DCC_PentiumSafeDivide" => pentium_safe_divide,
            "DCC_IOChecking" => io_checking,
            "DCC_IntegerOverflowCheck" => integer_overflow_check,
            "DCC_RangeChecking" => range_checking,
            "DCC_AssertionsAtRuntime" => assertions_at_runtime,
            "DCC_ImportedDataReferences" => imported_data_references,
            "DCC_DebugInformation" => debug_information,
            "DCC_LocalDebugSymbols" => local_debug_symbols,
            "DCC_SymbolReferenceInfo" => symbol_reference_info,
            "DCC_DebugDCUs" => debug_dcus,
            "DCC_DebugInfoInExe" => debug_info_in_exe,
            "DCC_DebugInfoInTds" => debug_info_in_tds,
            "DCC_DebugVN" => debug_vn,
            "DCC_RemoteDebug" => remote_debug,
            "DCC_Hints" => hints,
            "DCC_Warnings" => warnings,
            "DCC_ShowGeneralMessages" => show_general_messages,
            "DCC_ConsoleTarget" => console_target,
            "DCC_Description" => description,
            "DCC_AdditionalSwitches" => additional_switches,
            "DCC_LinkerOptions" => linker_options,
            "DCC_ImageBase" => image_base,
            "DCC_MapFile" => map_file,
            "DCC_MapFileARM" => map_file_arm,
            "DCC_StackSize" => stack_size,
            "DCC_MaxStackSize" => max_stack_size,
            "DCC_MinStackSize" => min_stack_size,
            "DCC_BaseAddress" => base_address,
            "DCC_PEFlags" => pe_flags,
            "DCC_PEOptFlags" => pe_opt_flags,
            "DCC_PEOSVersion" => pe_os_version,
            "DCC_PESubSysVersion" => pe_sub_sys_version,
            "DCC_PEUserVersion" => pe_user_version,
            "DCC_NXCompat" => nx_compat,
            "DCC_DynamicBase" => dynamic_base,
            "DCC_HighEntropyVa" => high_entropy_va,
            "DCC_TSAware" => ts_aware,
            "DCC_LargeAddressAware" => large_address_aware,
            "DCC_AllowUndefined" => allow_undefined,
            "DCC_OutputXMLDocumentation" => output_xml_documentation,
            "DCC_OutputDependencies" => output_dependencies,
            "DCC_OutputDRCFile" => output_drc_file,
            "DCC_OldDosFileNames" => old_dos_file_names,
            "DCC_XmlOutput" => xml_output,
            "DCC_RemoveTmpLnkFile" => remove_tmp_lnk_file,
            "DCC_IncludeDCUsInUsesCompletion" => include_dcus_in_uses_completion,
            "DCC_UseMSBuildExternally" => use_msbuild_externally,
            "DCC_LegacyIFEND" => legacy_ifend,
            "DCC_HppOutputARM" => hpp_output_arm,
            "DCC_iOSMinimumVersion" => ios_minimum_version,
            "DCC_macOSArmMinimumVersion" => macos_arm_minimum_version,
            "DCC_macOSMinimumVersion" => macos_minimum_version,
        );
        for (k, v) in &self.warning_directives {
            vars.insert(k.clone(), v.clone());
        }
    }
}

impl BrccOptions {
    fn merge_from(&mut self, o: &Self) {
        merge_options!(self, o,
            user_supplied_options, code_page, language,
            delete_include_path, enable_multi_byte, compiler_to_use,
            response_filename, verbose, defines, include_path, output_dir,
        );
    }

    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        expand_options!(self, vars,
            user_supplied_options, code_page, language,
            delete_include_path, enable_multi_byte, compiler_to_use,
            response_filename, verbose, defines, include_path, output_dir,
        );
    }

    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        collect_tag_values!(self, vars,
            "BRCC_UserSuppliedOptions" => user_supplied_options,
            "BRCC_CodePage" => code_page,
            "BRCC_Language" => language,
            "BRCC_DeleteIncludePath" => delete_include_path,
            "BRCC_EnableMultiByte" => enable_multi_byte,
            "BRCC_CompilerToUse" => compiler_to_use,
            "BRCC_ResponseFilename" => response_filename,
            "BRCC_Verbose" => verbose,
            "BRCC_Defines" => defines,
            "BRCC_IncludePath" => include_path,
            "BRCC_OutputDir" => output_dir,
        );
    }
}

impl BuildEvents {
    fn merge_from(&mut self, o: &Self) {
        merge_options!(self, o,
            pre_build_event, pre_build_event_cancel_on_error,
            pre_build_event_ignore_exit_code,
            pre_link_event, pre_link_event_cancel_on_error,
            pre_link_event_ignore_exit_code,
            post_build_event, post_build_event_cancel_on_error,
            post_build_event_ignore_exit_code, post_build_event_execute_when,
        );
    }

    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        expand_options!(self, vars,
            pre_build_event, pre_build_event_cancel_on_error,
            pre_build_event_ignore_exit_code,
            pre_link_event, pre_link_event_cancel_on_error,
            pre_link_event_ignore_exit_code,
            post_build_event, post_build_event_cancel_on_error,
            post_build_event_ignore_exit_code, post_build_event_execute_when,
        );
    }

    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        collect_tag_values!(self, vars,
            "PreBuildEvent" => pre_build_event,
            "PreBuildEventCancelOnError" => pre_build_event_cancel_on_error,
            "PreBuildEventIgnoreExitCode" => pre_build_event_ignore_exit_code,
            "PreLinkEvent" => pre_link_event,
            "PreLinkEventCancelOnError" => pre_link_event_cancel_on_error,
            "PreLinkEventIgnoreExitCode" => pre_link_event_ignore_exit_code,
            "PostBuildEvent" => post_build_event,
            "PostBuildEventCancelOnError" => post_build_event_cancel_on_error,
            "PostBuildEventIgnoreExitCode" => post_build_event_ignore_exit_code,
            "PostBuildEventExecuteWhen" => post_build_event_execute_when,
        );
    }
}

impl VerInfo {
    fn merge_from(&mut self, o: &Self) {
        merge_options!(self, o,
            include_ver_info, major_ver, minor_ver, release, build,
            debug, pre_release, special, private, dll,
            auto_gen_version, locale, keys,
        );
    }

    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        expand_options!(self, vars,
            include_ver_info, major_ver, minor_ver, release, build,
            debug, pre_release, special, private, dll,
            auto_gen_version, locale, keys,
        );
    }

    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        collect_tag_values!(self, vars,
            "VerInfo_IncludeVerInfo" => include_ver_info,
            "VerInfo_MajorVer" => major_ver,
            "VerInfo_MinorVer" => minor_ver,
            "VerInfo_Release" => release,
            "VerInfo_Build" => build,
            "VerInfo_Debug" => debug,
            "VerInfo_PreRelease" => pre_release,
            "VerInfo_Special" => special,
            "VerInfo_Private" => private,
            "VerInfo_DLL" => dll,
            "VerInfo_AutoGenVersion" => auto_gen_version,
            "VerInfo_Locale" => locale,
            "VerInfo_Keys" => keys,
        );
    }
}

impl PlatformPackaging {
    fn merge_from(&mut self, o: &Self) {
        merge_options!(self, o,
            app_dpi_awareness_mode, app_enable_runtime_themes,
            app_execution_level, app_execution_level_ui_access,
            manifest_file, output_ext, bt_build_type,
            pf_uwp_publisher, pf_uwp_package_name,
            pf_uwp_package_display_name, pf_uwp_publisher_display_name,
            pf_uwp_distribution_type, uwp_delphi_logo44, uwp_delphi_logo150,
        );
    }

    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        expand_options!(self, vars,
            app_dpi_awareness_mode, app_enable_runtime_themes,
            app_execution_level, app_execution_level_ui_access,
            manifest_file, output_ext, bt_build_type,
            pf_uwp_publisher, pf_uwp_package_name,
            pf_uwp_package_display_name, pf_uwp_publisher_display_name,
            pf_uwp_distribution_type, uwp_delphi_logo44, uwp_delphi_logo150,
        );
    }

    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        collect_tag_values!(self, vars,
            "AppDPIAwarenessMode" => app_dpi_awareness_mode,
            "AppEnableRuntimeThemes" => app_enable_runtime_themes,
            "AppExecutionLevel" => app_execution_level,
            "AppExecutionLevelUIAccess" => app_execution_level_ui_access,
            "Manifest_File" => manifest_file,
            "OutputExt" => output_ext,
            "BT_BuildType" => bt_build_type,
            "PF_UWPPublisher" => pf_uwp_publisher,
            "PF_UWPPackageName" => pf_uwp_package_name,
            "PF_UWPPackageDisplayName" => pf_uwp_package_display_name,
            "PF_UWPPublisherDisplayName" => pf_uwp_publisher_display_name,
            "PF_UWPDistributionType" => pf_uwp_distribution_type,
            "UWP_DelphiLogo44" => uwp_delphi_logo44,
            "UWP_DelphiLogo150" => uwp_delphi_logo150,
        );
    }
}

impl DebuggerOptions {
    fn merge_from(&mut self, o: &Self) {
        merge_options!(self, o,
            include_system_vars, env_vars, symbol_source_path, run_params,
        );
    }

    fn expand_vars(&mut self, vars: &HashMap<String, String>) {
        expand_options!(self, vars,
            include_system_vars, env_vars, symbol_source_path, run_params,
        );
    }

    fn collect_into_vars(&self, vars: &mut HashMap<String, String>) {
        collect_tag_values!(self, vars,
            "Debugger_IncludeSystemVars" => include_system_vars,
            "Debugger_EnvVars" => env_vars,
            "Debugger_SymbolSourcePath" => symbol_source_path,
            "Debugger_RunParams" => run_params,
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Active property group resolution
// ═══════════════════════════════════════════════════════════════════════════════

impl Dproj {
    /// Build the MSBuild variable map that drives PropertyGroup condition
    /// evaluation for a given *configuration* (e.g. `"Debug"`) and
    /// *platform* (e.g. `"Win32"`).
    ///
    /// The map is derived from the `<BuildConfiguration>` items in the
    /// project's `<ItemGroup>`s.  Walking from the selected configuration
    /// up through `CfgParent` links, every key encountered is set to
    /// `"true"`, and a `"{key}_{platform}"` combo variable is set as well.
    fn resolve_build_variables(
        &self,
        config: &str,
        platform: &str,
    ) -> Result<HashMap<String, String>, DprojError> {
        // Start with external environment variables (rsvars, system env, etc.)
        let mut vars = self.env.clone();

        // Built-in MSBuild-style variables derived from the project stem.
        if let Some(stem) = self.project_stem() {
            vars.insert("MSBuildProjectName".to_string(), stem);
        }

        // Config / Platform override anything from the environment.
        vars.insert("Config".to_string(), config.to_string());
        vars.insert("Configuration".to_string(), config.to_string());
        vars.insert("Platform".to_string(), platform.to_string());

        // Collect all BuildConfiguration entries.
        let build_configs: Vec<&BuildConfiguration> = self
            .project
            .item_groups
            .iter()
            .flat_map(|ig| &ig.build_configurations)
            .collect();

        // Verify the selected config exists.
        if !build_configs.iter().any(|bc| bc.name == config) {
            return Err(DprojError::new(format!(
                "Build configuration '{config}' not found"
            )));
        }

        // Walk parent chain, setting each key → "true".
        let mut current_name = config.to_string();
        let mut visited = Vec::new();
        loop {
            if visited.contains(&current_name) {
                break; // prevent cycles
            }
            visited.push(current_name.clone());

            let Some(bc) = build_configs.iter().find(|bc| bc.name == current_name) else {
                break;
            };

            vars.insert(bc.key.clone(), "true".to_string());
            vars.insert(format!("{}_{}", bc.key, platform), "true".to_string());

            let Some(parent) = &bc.cfg_parent else {
                break;
            };
            current_name = parent.clone();
        }

        Ok(vars)
    }

    /// Compute the **effective** (merged) [`PropertyGroup`] for the
    /// project's current configuration and platform.
    ///
    /// The active configuration and platform are read from the unconditional
    /// `<PropertyGroup>` elements in the file (the `<Config>`/`<Configuration>`
    /// and `<Platform>` defaults).
    ///
    /// Every `<PropertyGroup>` in the project is evaluated in document order:
    /// groups without a `Condition` always contribute, and conditional groups
    /// contribute when their condition is satisfied by the resolved variable
    /// map.  Later values override earlier ones.
    pub fn active_property_group(&self) -> Result<PropertyGroup, DprojError> {
        let (config, platform) = self.active_config_platform()?;
        self.active_property_group_for(&config, &platform)
    }

    /// Same as [`active_property_group`](Self::active_property_group) but for
    /// an explicitly chosen configuration and platform instead of the file
    /// defaults.
    pub fn active_property_group_for(
        &self,
        config: &str,
        platform: &str,
    ) -> Result<PropertyGroup, DprojError> {
        let build_vars = self.resolve_build_variables(config, platform)?;
        let mut vars = build_vars.clone();
        let mut result = PropertyGroup::default();

        for pg in &self.project.property_groups {
            let matches = if let Some(cond) = &pg.condition {
                let expr = condition::parse_condition(cond)
                    .map_err(DprojError::new)?;
                condition::evaluate(&expr, &vars)
            } else {
                true
            };

            if matches {
                // Clone and expand $(Var) references using the accumulated
                // property map so that self-referencing list properties
                // (e.g. "src;$(DCC_UnitSearchPath)") resolve correctly.
                let mut expanded = pg.clone();
                expanded.expand_vars(&vars);
                result.merge_from(&expanded);
                // Feed the newly-merged values back into the variable map
                // so subsequent PGs can reference them.
                result.collect_into_vars(&mut vars);
                // Re-assert build variables: the requested config/platform
                // must always take priority over project-level defaults
                // (e.g. an unconditional PG may set <Config>Debug</Config>).
                for (k, v) in &build_vars {
                    vars.insert(k.clone(), v.clone());
                }
            }
        }

        Ok(result)
    }

    /// Extract the active `(Config, Platform)` from the project's
    /// unconditional property groups.
    fn active_config_platform(&self) -> Result<(String, String), DprojError> {
        let mut config: Option<String> = None;
        let mut platform: Option<String> = None;

        for pg in &self.project.property_groups {
            // Only look at unconditional groups for the defaults.
            if pg.condition.is_some() {
                continue;
            }
            if config.is_none() {
                config = pg.project_properties.config.clone()
                    .or_else(|| pg.project_properties.configuration.clone());
            }
            if platform.is_none() {
                platform = pg.project_properties.platform.clone();
            }
            if config.is_some() && platform.is_some() {
                break;
            }
        }

        let config = config.ok_or_else(|| {
            DprojError::new("No default Config/Configuration found in unconditional PropertyGroups")
        })?;
        let platform = platform.ok_or_else(|| {
            DprojError::new("No default Platform found in unconditional PropertyGroups")
        })?;

        Ok((config, platform))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Parsing – roxmltree → owned types
// ═══════════════════════════════════════════════════════════════════════════════

impl DprojProject {
    fn parse(root: roxmltree::Node) -> Result<Self, DprojError> {
        let mut project = Self::default();

        for child in root.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "PropertyGroup" => {
                    project.property_groups.push(PropertyGroup::parse(&child));
                }
                "ItemGroup" => {
                    project.item_groups.push(ItemGroup::parse(&child));
                }
                "ProjectExtensions" => {
                    project.project_extensions = Some(ProjectExtensions::parse(&child));
                }
                "Import" => {
                    project.imports.push(Import {
                        project: child.attribute("Project").unwrap_or("").to_string(),
                        condition: child.attribute("Condition").map(String::from),
                    });
                }
                _ => {}
            }
        }

        Ok(project)
    }
}

// ─── PropertyGroup dispatch ──────────────────────────────────────────────────

impl PropertyGroup {
    fn parse(node: &roxmltree::Node) -> Self {
        let mut pg = Self {
            condition: node.attribute("Condition").map(String::from),
            ..Default::default()
        };

        for child in node.children().filter(|n| n.is_element()) {
            let tag = child.tag_name().name();
            let text = child.text().unwrap_or("").to_string();

            if set_project_property(tag, &text, &mut pg.project_properties) { continue; }
            if set_dcc_option(tag, &text, &mut pg.dcc_options) { continue; }
            if set_brcc_option(tag, &text, &mut pg.brcc_options) { continue; }
            if set_build_event(tag, &text, &mut pg.build_events) { continue; }
            if set_ver_info(tag, &text, &mut pg.ver_info) { continue; }
            if set_platform_packaging(tag, &text, &mut pg.platform_packaging) { continue; }
            if set_debugger_option(tag, &text, &mut pg.debugger_options) { continue; }

            // Unrecognised element → stash in `other`.
            pg.other.insert(tag.to_string(), text);
        }

        pg
    }
}

fn set_project_property(tag: &str, text: &str, p: &mut ProjectProperties) -> bool {
    let s = || Some(text.to_string());
    match tag {
        "ProjectGuid"          => p.project_guid = s(),
        "ProjectVersion"       => p.project_version = s(),
        "Version"              => p.version = s(),
        "FrameworkType"        => p.framework_type = s(),
        "Config"               => p.config = s(),
        "Configuration"        => p.configuration = s(),
        "Platform"             => p.platform = s(),
        "ProjectName"          => p.project_name = s(),
        "TargetedPlatforms"    => p.targeted_platforms = s(),
        "AppType"              => p.app_type = s(),
        "MainSource"           => p.main_source = s(),
        "Base"                 => p.base = s(),
        "CfgParent"            => p.cfg_parent = s(),
        "SanitizedProjectName" => p.sanitized_project_name = s(),
        "Custom_Styles"        => p.custom_styles = s(),
        "GenPackage"           => p.gen_package = s(),
        "GenDll"               => p.gen_dll = s(),
        "UsePackages"          => p.use_packages = s(),
        "Icon_MainIcon"        => p.icon_main_icon = s(),
        "Icns_MainIcns"        => p.icns_main_icns = s(),
        _ => return false,
    }
    true
}

fn set_dcc_option(tag: &str, text: &str, o: &mut DccOptions) -> bool {
    let s = || Some(text.to_string());
    match tag {
        // Compiler identity
        "DCC_DCCCompiler"                  => o.dcc_compiler = s(),
        "DCC_DependencyCheckOutputName"    => o.dependency_check_output_name = s(),
        // Output paths
        "DCC_DcuOutput"                    => o.dcu_output = s(),
        "DCC_ExeOutput"                    => o.exe_output = s(),
        "DCC_DcpOutput"                    => o.dcp_output = s(),
        "DCC_BplOutput"                    => o.bpl_output = s(),
        "DCC_ObjOutput"                    => o.obj_output = s(),
        "DCC_HppOutput"                    => o.hpp_output = s(),
        "DCC_BpiOutput"                    => o.bpi_output = s(),
        "DCC_CBuilderOutput"              => o.cbuilder_output = s(),
        // Search paths
        "DCC_UnitSearchPath"               => o.unit_search_path = s(),
        "DCC_ResourcePath"                 => o.resource_path = s(),
        "DCC_IncludePath"                  => o.include_path = s(),
        "DCC_ObjPath"                      => o.obj_path = s(),
        "DCC_FrameworkPath"                => o.framework_path = s(),
        "DCC_SysLibRoot"                   => o.sys_lib_root = s(),
        // Defines & aliases
        "DCC_Define"                       => o.define = s(),
        "DCC_Namespace"                    => o.namespace = s(),
        "DCC_UnitAlias"                    => o.unit_alias = s(),
        "DCC_UsePackage"                   => o.use_package = s(),
        // Code generation
        "DCC_Optimize"                     => o.optimize = s(),
        "DCC_Alignment"                    => o.alignment = s(),
        "DCC_MinimumEnumSize"              => o.minimum_enum_size = s(),
        "DCC_CodePage"                     => o.code_page = s(),
        "DCC_Inlining"                     => o.inlining = s(),
        "DCC_GenerateStackFrames"          => o.generate_stack_frames = s(),
        "DCC_GeneratePICCode"              => o.generate_pic_code = s(),
        "DCC_GenerateAndroidAppBundleFile" => o.generate_android_app_bundle_file = s(),
        "DCC_GenerateOSXUniversalBinaryFile" => o.generate_osx_universal_binary_file = s(),
        // Compiler switches
        "DCC_E"                            => o.e = s(),
        "DCC_N"                            => o.n = s(),
        "DCC_S"                            => o.s = s(),
        "DCC_F"                            => o.f = s(),
        "DCC_K"                            => o.k = s(),
        "DCC_ExtendedSyntax"               => o.extended_syntax = s(),
        "DCC_LongStrings"                  => o.long_strings = s(),
        "DCC_OpenStringParams"             => o.open_string_params = s(),
        "DCC_StrictVarStrings"             => o.strict_var_strings = s(),
        "DCC_TypedAtParameter"             => o.typed_at_parameter = s(),
        "DCC_FullBooleanEvaluations"       => o.full_boolean_evaluations = s(),
        "DCC_WriteableConstants"           => o.writeable_constants = s(),
        "DCC_RunTimeTypeInfo"              => o.run_time_type_info = s(),
        "DCC_PentiumSafeDivide"            => o.pentium_safe_divide = s(),
        // Runtime checks
        "DCC_IOChecking"                   => o.io_checking = s(),
        "DCC_IntegerOverflowCheck"         => o.integer_overflow_check = s(),
        "DCC_RangeChecking"                => o.range_checking = s(),
        "DCC_AssertionsAtRuntime"          => o.assertions_at_runtime = s(),
        "DCC_ImportedDataReferences"       => o.imported_data_references = s(),
        // Debug
        "DCC_DebugInformation"             => o.debug_information = s(),
        "DCC_LocalDebugSymbols"            => o.local_debug_symbols = s(),
        "DCC_SymbolReferenceInfo"          => o.symbol_reference_info = s(),
        "DCC_DebugDCUs"                    => o.debug_dcus = s(),
        "DCC_DebugInfoInExe"               => o.debug_info_in_exe = s(),
        "DCC_DebugInfoInTds"               => o.debug_info_in_tds = s(),
        "DCC_DebugVN"                      => o.debug_vn = s(),
        "DCC_RemoteDebug"                  => o.remote_debug = s(),
        // Warnings & hints
        "DCC_Hints"                        => o.hints = s(),
        "DCC_Warnings"                     => o.warnings = s(),
        "DCC_ShowGeneralMessages"          => o.show_general_messages = s(),
        // Linker / PE
        "DCC_ConsoleTarget"                => o.console_target = s(),
        "DCC_Description"                  => o.description = s(),
        "DCC_AdditionalSwitches"           => o.additional_switches = s(),
        "DCC_LinkerOptions"                => o.linker_options = s(),
        "DCC_ImageBase"                    => o.image_base = s(),
        "DCC_MapFile"                      => o.map_file = s(),
        "DCC_MapFileARM"                   => o.map_file_arm = s(),
        "DCC_StackSize"                    => o.stack_size = s(),
        "DCC_MaxStackSize"                 => o.max_stack_size = s(),
        "DCC_MinStackSize"                 => o.min_stack_size = s(),
        "DCC_BaseAddress"                  => o.base_address = s(),
        "DCC_PEFlags"                      => o.pe_flags = s(),
        "DCC_PEOptFlags"                   => o.pe_opt_flags = s(),
        "DCC_PEOSVersion"                  => o.pe_os_version = s(),
        "DCC_PESubSysVersion"              => o.pe_sub_sys_version = s(),
        "DCC_PEUserVersion"                => o.pe_user_version = s(),
        "DCC_NXCompat"                     => o.nx_compat = s(),
        "DCC_DynamicBase"                  => o.dynamic_base = s(),
        "DCC_HighEntropyVa"                => o.high_entropy_va = s(),
        "DCC_TSAware"                      => o.ts_aware = s(),
        "DCC_LargeAddressAware"            => o.large_address_aware = s(),
        "DCC_AllowUndefined"               => o.allow_undefined = s(),
        // Output control
        "DCC_OutputXMLDocumentation"       => o.output_xml_documentation = s(),
        "DCC_OutputDependencies"           => o.output_dependencies = s(),
        "DCC_OutputDRCFile"                => o.output_drc_file = s(),
        "DCC_OldDosFileNames"              => o.old_dos_file_names = s(),
        "DCC_XmlOutput"                    => o.xml_output = s(),
        "DCC_RemoveTmpLnkFile"             => o.remove_tmp_lnk_file = s(),
        "DCC_IncludeDCUsInUsesCompletion"  => o.include_dcus_in_uses_completion = s(),
        "DCC_UseMSBuildExternally"         => o.use_msbuild_externally = s(),
        "DCC_LegacyIFEND"                 => o.legacy_ifend = s(),
        "DCC_HppOutputARM"                => o.hpp_output_arm = s(),
        // Platform minimum versions
        "DCC_iOSMinimumVersion"            => o.ios_minimum_version = s(),
        "DCC_macOSArmMinimumVersion"       => o.macos_arm_minimum_version = s(),
        "DCC_macOSMinimumVersion"          => o.macos_minimum_version = s(),
        // Any other DCC_ tag → warning / hint directive.
        _ if tag.starts_with("DCC_") => {
            o.warning_directives.insert(tag.to_string(), text.to_string());
        }
        _ => return false,
    }
    true
}

fn set_brcc_option(tag: &str, text: &str, o: &mut BrccOptions) -> bool {
    let s = || Some(text.to_string());
    match tag {
        "BRCC_UserSuppliedOptions" => o.user_supplied_options = s(),
        "BRCC_CodePage"            => o.code_page = s(),
        "BRCC_Language"            => o.language = s(),
        "BRCC_DeleteIncludePath"   => o.delete_include_path = s(),
        "BRCC_EnableMultiByte"     => o.enable_multi_byte = s(),
        "BRCC_CompilerToUse"       => o.compiler_to_use = s(),
        "BRCC_ResponseFilename"    => o.response_filename = s(),
        "BRCC_Verbose"             => o.verbose = s(),
        "BRCC_Defines"             => o.defines = s(),
        "BRCC_IncludePath"         => o.include_path = s(),
        "BRCC_OutputDir"           => o.output_dir = s(),
        _ => return false,
    }
    true
}

fn set_build_event(tag: &str, text: &str, e: &mut BuildEvents) -> bool {
    let s = || Some(text.to_string());
    match tag {
        "PreBuildEvent"                 => e.pre_build_event = s(),
        "PreBuildEventCancelOnError"    => e.pre_build_event_cancel_on_error = s(),
        "PreBuildEventIgnoreExitCode"   => e.pre_build_event_ignore_exit_code = s(),
        "PreLinkEvent"                  => e.pre_link_event = s(),
        "PreLinkEventCancelOnError"     => e.pre_link_event_cancel_on_error = s(),
        "PreLinkEventIgnoreExitCode"    => e.pre_link_event_ignore_exit_code = s(),
        "PostBuildEvent"                => e.post_build_event = s(),
        "PostBuildEventCancelOnError"   => e.post_build_event_cancel_on_error = s(),
        "PostBuildEventIgnoreExitCode"  => e.post_build_event_ignore_exit_code = s(),
        "PostBuildEventExecuteWhen"     => e.post_build_event_execute_when = s(),
        _ => return false,
    }
    true
}

fn set_ver_info(tag: &str, text: &str, v: &mut VerInfo) -> bool {
    let s = || Some(text.to_string());
    match tag {
        "VerInfo_IncludeVerInfo"  => v.include_ver_info = s(),
        "VerInfo_MajorVer"       => v.major_ver = s(),
        "VerInfo_MinorVer"       => v.minor_ver = s(),
        "VerInfo_Release"        => v.release = s(),
        "VerInfo_Build"          => v.build = s(),
        "VerInfo_Debug"          => v.debug = s(),
        "VerInfo_PreRelease"     => v.pre_release = s(),
        "VerInfo_Special"        => v.special = s(),
        "VerInfo_Private"        => v.private = s(),
        "VerInfo_DLL"            => v.dll = s(),
        "VerInfo_AutoGenVersion" => v.auto_gen_version = s(),
        "VerInfo_Locale"         => v.locale = s(),
        "VerInfo_Keys"           => v.keys = s(),
        _ => return false,
    }
    true
}

fn set_platform_packaging(tag: &str, text: &str, p: &mut PlatformPackaging) -> bool {
    let s = || Some(text.to_string());
    match tag {
        "AppDPIAwarenessMode"         => p.app_dpi_awareness_mode = s(),
        "AppEnableRuntimeThemes"      => p.app_enable_runtime_themes = s(),
        "AppExecutionLevel"           => p.app_execution_level = s(),
        "AppExecutionLevelUIAccess"   => p.app_execution_level_ui_access = s(),
        "Manifest_File"               => p.manifest_file = s(),
        "OutputExt"                   => p.output_ext = s(),
        "BT_BuildType"                => p.bt_build_type = s(),
        "PF_UWPPublisher"             => p.pf_uwp_publisher = s(),
        "PF_UWPPackageName"           => p.pf_uwp_package_name = s(),
        "PF_UWPPackageDisplayName"    => p.pf_uwp_package_display_name = s(),
        "PF_UWPPublisherDisplayName"  => p.pf_uwp_publisher_display_name = s(),
        "PF_UWPDistributionType"      => p.pf_uwp_distribution_type = s(),
        "UWP_DelphiLogo44"            => p.uwp_delphi_logo44 = s(),
        "UWP_DelphiLogo150"           => p.uwp_delphi_logo150 = s(),
        _ => return false,
    }
    true
}

fn set_debugger_option(tag: &str, text: &str, d: &mut DebuggerOptions) -> bool {
    let s = || Some(text.to_string());
    match tag {
        "Debugger_IncludeSystemVars" => d.include_system_vars = s(),
        "Debugger_EnvVars"           => d.env_vars = s(),
        "Debugger_SymbolSourcePath"  => d.symbol_source_path = s(),
        "Debugger_RunParams"         => d.run_params = s(),
        _ => return false,
    }
    true
}

// ─── ItemGroup ───────────────────────────────────────────────────────────────

impl ItemGroup {
    fn parse(node: &roxmltree::Node) -> Self {
        let mut ig = Self::default();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "DelphiCompile" => {
                    ig.delphi_compile = Some(DelphiCompile {
                        include: child.attribute("Include").unwrap_or("").to_string(),
                        main_source: find_child_text(&child, "MainSource"),
                    });
                }
                "DCCReference" => {
                    ig.dcc_references.push(DccReference {
                        include: child.attribute("Include").unwrap_or("").to_string(),
                        form: find_child_text(&child, "Form"),
                        form_type: find_child_text(&child, "FormType"),
                    });
                }
                "BuildConfiguration" => {
                    ig.build_configurations.push(BuildConfiguration {
                        name: child.attribute("Include").unwrap_or("").to_string(),
                        key: find_child_text(&child, "Key").unwrap_or_default(),
                        cfg_parent: find_child_text(&child, "CfgParent"),
                    });
                }
                _ => {}
            }
        }

        ig
    }
}

// ─── ProjectExtensions ───────────────────────────────────────────────────────

impl ProjectExtensions {
    fn parse(node: &roxmltree::Node) -> Self {
        let mut ext = Self::default();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Borland.Personality" => {
                    ext.borland_personality = child.text().map(String::from);
                }
                "Borland.ProjectType" => {
                    ext.borland_project_type = child.text().map(String::from);
                }
                "BorlandProject" => {
                    ext.borland_project = Some(BorlandProject::parse(&child));
                }
                "ProjectFileVersion" => {
                    ext.project_file_version = child.text().map(String::from);
                }
                _ => {}
            }
        }

        ext
    }
}

impl BorlandProject {
    fn parse(node: &roxmltree::Node) -> Self {
        let mut bp = Self::default();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Delphi.Personality" => {
                    bp.delphi_personality = Some(DelphiPersonality::parse(&child));
                }
                "Deployment" => {
                    bp.deployment = Some(Deployment::parse(&child));
                }
                "Platforms" => {
                    bp.platforms = child
                        .children()
                        .filter(|n| n.is_element() && n.tag_name().name() == "Platform")
                        .map(|p| Platform {
                            value: p.attribute("value").unwrap_or("").to_string(),
                            active: p.text().unwrap_or("").eq_ignore_ascii_case("true"),
                        })
                        .collect();
                }
                "ModelSupport" => {
                    bp.model_support = child.text().map(String::from);
                }
                "ActiveXProjectInfo" => {
                    bp.active_x_project_info = Some(ActiveXProjectInfo {
                        version: find_child_text(&child, "version"),
                    });
                }
                _ => {}
            }
        }

        bp
    }
}

impl DelphiPersonality {
    fn parse(node: &roxmltree::Node) -> Self {
        let mut dp = Self::default();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Parameters" => {
                    dp.parameters = parse_name_value_pairs(&child, "Parameters");
                }
                "VersionInfo" => {
                    dp.version_info = parse_name_value_pairs(&child, "VersionInfo");
                }
                "VersionInfoKeys" => {
                    dp.version_info_keys =
                        parse_name_value_pairs(&child, "VersionInfoKeys");
                }
                "TypeLibOptions" => {
                    dp.type_lib_options =
                        parse_name_value_pairs(&child, "TypeLibOptions");
                }
                "Excluded_Packages" => {
                    dp.excluded_packages = child
                        .children()
                        .filter(|c| {
                            c.is_element() && c.tag_name().name() == "Excluded_Packages"
                        })
                        .map(|c| ExcludedPackage {
                            name: c.attribute("Name").unwrap_or("").to_string(),
                            description: c.text().unwrap_or("").to_string(),
                        })
                        .collect();
                }
                "Source" => {
                    dp.sources = parse_name_value_pairs(&child, "Source");
                }
                _ => {}
            }
        }

        dp
    }
}

/// Parse `<Parent><Child Name="key">value</Child> …</Parent>` lists.
fn parse_name_value_pairs(parent: &roxmltree::Node, child_tag: &str) -> Vec<NameValuePair> {
    parent
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == child_tag)
        .map(|c| NameValuePair {
            name: c.attribute("Name").unwrap_or("").to_string(),
            value: c.text().unwrap_or("").to_string(),
        })
        .collect()
}

// ─── Deployment ──────────────────────────────────────────────────────────────

impl Deployment {
    fn parse(node: &roxmltree::Node) -> Self {
        let mut dep = Self {
            version: node.attribute("Version").map(String::from),
            ..Default::default()
        };

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "DeployFile" => {
                    dep.deploy_files.push(DeployFile {
                        local_name: child
                            .attribute("LocalName")
                            .unwrap_or("")
                            .to_string(),
                        configuration: child
                            .attribute("Configuration")
                            .map(String::from),
                        class: child.attribute("Class").map(String::from),
                        platforms: parse_deploy_platforms(&child, |p| {
                            DeployFilePlatform {
                                name: p.attribute("Name").unwrap_or("").to_string(),
                                remote_name: find_child_text(&p, "RemoteName"),
                                overwrite: find_child_text(&p, "Overwrite"),
                            }
                        }),
                    });
                }
                "DeployClass" => {
                    dep.deploy_classes.push(DeployClass {
                        name: child.attribute("Name").unwrap_or("").to_string(),
                        required: child.attribute("Required").map(String::from),
                        platforms: parse_deploy_platforms(&child, |p| {
                            DeployClassPlatform {
                                name: p.attribute("Name").unwrap_or("").to_string(),
                                remote_dir: find_child_text(&p, "RemoteDir"),
                                operation: find_child_text(&p, "Operation"),
                                extensions: find_child_text(&p, "Extensions"),
                            }
                        }),
                    });
                }
                "ProjectRoot" => {
                    dep.project_roots.push(ProjectRoot {
                        platform: child
                            .attribute("Platform")
                            .unwrap_or("")
                            .to_string(),
                        name: child.attribute("Name").unwrap_or("").to_string(),
                    });
                }
                _ => {}
            }
        }

        dep
    }
}

fn parse_deploy_platforms<T>(
    parent: &roxmltree::Node,
    map_fn: impl Fn(roxmltree::Node) -> T,
) -> Vec<T> {
    parent
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "Platform")
        .map(map_fn)
        .collect()
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Return the text content of the first child element with the given tag name.
fn find_child_text(parent: &roxmltree::Node, tag: &str) -> Option<String> {
    parent
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == tag)
        .and_then(|c| c.text())
        .map(String::from)
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test: every .dproj in the repo root must parse without error.
    #[test]
    fn parse_all_dproj_files() {
        let files = [
            "example.dproj",
        ];
        for file in &files {
            let result = Dproj::from_file(file);
            assert!(result.is_ok(), "Failed to parse {file}: {}", result.unwrap_err());
        }
    }

    #[test]
    fn example_dproj_basic_properties() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let pg0 = &dproj.project.property_groups[0];
        assert_eq!(pg0.project_properties.project_version.as_deref(), Some("20.1"));
        assert_eq!(pg0.project_properties.framework_type.as_deref(), Some("VCL"));
        assert!(pg0.project_properties.project_guid.is_some());
    }

    #[test]
    fn set_property_value_round_trip() {
        let source = std::fs::read_to_string("example.dproj").unwrap();
        let mut dproj = Dproj::parse(source).unwrap();

        let old = dproj.project.property_groups[0]
            .project_properties
            .project_version
            .clone();
        assert_eq!(old.as_deref(), Some("20.1"));

        dproj.set_property_value(0, "ProjectVersion", "99.9").unwrap();
        assert_eq!(
            dproj.project.property_groups[0]
                .project_properties
                .project_version
                .as_deref(),
            Some("99.9")
        );

        // The raw source should reflect the change too.
        assert!(dproj.source().contains("<ProjectVersion>99.9</ProjectVersion>"));
    }

    // ── Active property group resolution ─────────────────────────────────

    #[test]
    fn active_pg_all_files() {
        // Every dproj must resolve an active PG for its default config/platform.
        let files = [
            "example.dproj",
        ];
        for file in files {
            let dproj = Dproj::from_file(file).unwrap();
            let result = dproj.active_property_group();
            assert!(
                result.is_ok(),
                "active_property_group failed for {file}: {}",
                result.unwrap_err()
            );
        }
    }

    #[test]
    fn active_pg_example_debug_win32() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        // example.dproj defaults to Debug/Win32.
        let pg = dproj.active_property_group().unwrap();

        // Core project properties should be populated from the unconditional PG.
        assert_eq!(pg.project_properties.project_version.as_deref(), Some("20.1"));
        assert_eq!(pg.project_properties.framework_type.as_deref(), Some("VCL"));

        // Debug-specific DCC options should be merged in.
        // In example.dproj, Debug has DCC_Optimize = "false" and
        // DCC_DebugInformation = "2".
        assert!(
            pg.dcc_options.optimize.is_some() || pg.dcc_options.debug_information.is_some(),
            "Expected some DCC options from the Debug config PG"
        );
    }

    #[test]
    fn active_pg_example_release_win32() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let pg = dproj.active_property_group_for("Release", "Win32").unwrap();

        assert_eq!(pg.project_properties.project_version.as_deref(), Some("20.1"));
        // Release should have its own DCC options (optimization, etc.)
        assert!(pg.dcc_options.optimize.is_some() || pg.dcc_options.debug_information.is_some());
    }

    #[test]
    fn active_pg_nonexistent_config() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let result = dproj.active_property_group_for("DoesNotExist", "Win32");
        assert!(result.is_err());
    }

    // ── Listing helpers ──────────────────────────────────────────────────

    #[test]
    fn configurations_lists_all() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let configs = dproj.configurations();
        assert!(configs.contains(&"Debug"));
        assert!(configs.contains(&"Release"));
        assert!(configs.len() >= 2);
    }

    #[test]
    fn platforms_lists_all() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let platforms = dproj.platforms();
        // example.dproj should have at least Win32
        let names: Vec<&str> = platforms.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"Win32"), "expected Win32 in {names:?}");
    }

    #[test]
    fn active_configuration_and_platform() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        assert_eq!(dproj.active_configuration().unwrap(), "Debug");
        assert_eq!(dproj.active_platform().unwrap(), "Win32");
    }

    #[test]
    fn set_configuration_round_trip() {
        let source = std::fs::read_to_string("example.dproj").unwrap();
        let mut dproj = Dproj::parse(source).unwrap();

        assert_eq!(dproj.active_configuration().unwrap(), "Debug");

        dproj.set_configuration("Release").unwrap();

        // In-memory struct updated
        assert_eq!(dproj.active_configuration().unwrap(), "Release");

        // Raw source updated
        assert!(dproj.source().contains(">Release</Config>"));

        // active_property_group now resolves Release
        let pg = dproj.active_property_group().unwrap();
        assert_eq!(pg.project_properties.project_version.as_deref(), Some("20.1"));
    }

    #[test]
    fn set_platform_round_trip() {
        let source = std::fs::read_to_string("example.dproj").unwrap();
        let mut dproj = Dproj::parse(source).unwrap();

        assert_eq!(dproj.active_platform().unwrap(), "Win32");

        dproj.set_platform("Win64").unwrap();

        // In-memory struct updated
        assert_eq!(dproj.active_platform().unwrap(), "Win64");

        // Raw source updated
        assert!(dproj.source().contains(">Win64</Platform>"));
    }

    // ── Merging ──────────────────────────────────────────────────────────

    #[test]
    fn merge_overrides_values() {
        let mut base = PropertyGroup::default();
        base.project_properties.project_version = Some("1.0".into());
        base.project_properties.framework_type = Some("VCL".into());

        let mut overlay = PropertyGroup::default();
        overlay.project_properties.project_version = Some("2.0".into());

        base.merge_from(&overlay);

        assert_eq!(base.project_properties.project_version.as_deref(), Some("2.0"));
        // framework_type was not overridden so it stays.
        assert_eq!(base.project_properties.framework_type.as_deref(), Some("VCL"));
    }

    // ── Path accessors ───────────────────────────────────────────────────

    #[test]
    fn directory_is_set_by_from_file() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        assert!(dproj.directory().is_some());
    }

    #[test]
    fn directory_is_none_for_parse() {
        let source = std::fs::read_to_string("example.dproj").unwrap();
        let dproj = Dproj::parse(source).unwrap();
        assert!(dproj.directory().is_none());
    }

    #[test]
    fn get_main_source_resolves() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let main = dproj.get_main_source().unwrap();
        // example.dproj has <MainSource>Project1.dpr</MainSource>
        assert_eq!(main.file_name().unwrap().to_str().unwrap(), "Project1.dpr");
        assert!(main.is_absolute(), "should be absolute: {main:?}");
    }

    #[test]
    fn get_main_source_fails_without_directory() {
        let source = std::fs::read_to_string("example.dproj").unwrap();
        let dproj = Dproj::parse(source).unwrap();
        assert!(dproj.get_main_source().is_err());
    }

    #[test]
    fn get_exe_path_resolves_with_variables() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        // example.dproj: DCC_ExeOutput = .\$(Platform)\$(Config)\DDD
        // default config = Debug, platform = Win32, stem = Project1
        let exe = dproj.get_exe_path().unwrap();
        let path_str = exe.to_string_lossy();
        // Should contain the expanded variables, not raw $(…) references
        assert!(
            !path_str.contains("$("),
            "expected expanded path, got: {path_str}"
        );
        assert!(
            path_str.contains("Win32") && path_str.contains("Debug"),
            "expected Win32/Debug in path: {path_str}"
        );
        assert!(
            path_str.ends_with("Project1.exe"),
            "expected Project1.exe, got: {path_str}"
        );
    }

    #[test]
    fn get_exe_path_for_release() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let exe = dproj.get_exe_path_for("Release", "Win64").unwrap();
        let path_str = exe.to_string_lossy();
        assert!(
            path_str.contains("Win64") && path_str.contains("Release"),
            "expected Win64/Release in path: {path_str}"
        );
    }

    #[test]
    fn expand_msbuild_vars_works() {
        let mut vars = HashMap::new();
        vars.insert("Config".into(), "Debug".into());
        vars.insert("Platform".into(), "Win32".into());
        assert_eq!(
            super::expand_msbuild_vars(".\\$(Platform)\\$(Config)\\out", &vars),
            ".\\Win32\\Debug\\out"
        );
    }

    // ── List-property accumulation ───────────────────────────────────────

    #[test]
    fn active_pg_accumulates_list_properties() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        // Default: Debug/Win32
        let pg = dproj.active_property_group().unwrap();

        // DCC_Define should accumulate across PGs:
        //   Base PG:  "AAA;$(DCC_Define)"      → "AAA;"
        //   Cfg_1:    "DEBUG;$(DCC_Define)"     → "DEBUG;AAA;"
        let define = pg.dcc_options.define.as_deref().unwrap();
        assert!(define.contains("DEBUG"), "expected DEBUG in defines: {define}");
        assert!(define.contains("AAA"), "expected AAA in defines: {define}");
        assert!(
            !define.contains("$(DCC_Define)"),
            "expected expanded defines, got: {define}"
        );

        // DCC_Namespace should accumulate from Base + Base_Win32:
        //   Base:      "System;Xml;...;JJJ;$(DCC_Namespace)"
        //   Base_Win32: "Winapi;System.Win;...;Bde;$(DCC_Namespace)"
        let ns = pg.dcc_options.namespace.as_deref().unwrap();
        assert!(ns.contains("Winapi"), "expected Winapi in namespace: {ns}");
        assert!(ns.contains("JJJ"), "expected JJJ in namespace: {ns}");
        assert!(
            !ns.contains("$(DCC_Namespace)"),
            "expected expanded namespace, got: {ns}"
        );

        // DCC_UnitSearchPath should accumulate:
        //   Base: "EEE;$(DCC_UnitSearchPath)" → "EEE;"
        let usp = pg.dcc_options.unit_search_path.as_deref().unwrap();
        assert!(usp.contains("EEE"), "expected EEE in search path: {usp}");
        assert!(
            !usp.contains("$(DCC_UnitSearchPath)"),
            "expected expanded search path, got: {usp}"
        );
    }

    #[test]
    fn active_pg_release_accumulates_defines() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let pg = dproj.active_property_group_for("Release", "Win32").unwrap();

        // Release: "RELEASE;$(DCC_Define)" should pick up AAA from Base.
        let define = pg.dcc_options.define.as_deref().unwrap();
        assert!(define.contains("RELEASE"), "expected RELEASE: {define}");
        assert!(define.contains("AAA"), "expected AAA from base: {define}");
        assert!(
            !define.contains("DEBUG"),
            "Release should NOT contain DEBUG: {define}"
        );
    }

    // ── DprojBuilder & env expansion ─────────────────────────────────────

    #[test]
    fn builder_from_file_basic() {
        let dproj = DprojBuilder::new()
            .from_file("example.dproj")
            .unwrap();
        assert_eq!(dproj.active_configuration().unwrap(), "Debug");
    }

    #[test]
    fn builder_with_env_var() {
        let dproj = DprojBuilder::new()
            .env_var("BDS", r"C:\TestDelphi")
            .from_file("example.dproj")
            .unwrap();
        let pg = dproj.active_property_group().unwrap();
        // Icon_MainIcon = $(BDS)\bin\delphi_PROJECTICON.ico
        let icon = pg.project_properties.icon_main_icon.as_deref().unwrap();
        assert!(
            icon.contains(r"C:\TestDelphi"),
            "expected expanded BDS in icon path: {icon}"
        );
        assert!(
            !icon.contains("$(BDS)"),
            "expected no raw $(BDS) in icon path: {icon}"
        );
    }

    #[test]
    fn builder_with_rsvars_content() {
        let rsvars_content = std::fs::read_to_string("rsvars.bat").unwrap();
        let dproj = DprojBuilder::new()
            .rsvars(&rsvars_content)
            .from_file("example.dproj")
            .unwrap();
        let pg = dproj.active_property_group().unwrap();
        // Icon_MainIcon should have the real BDS path expanded
        let icon = pg.project_properties.icon_main_icon.as_deref().unwrap();
        assert!(
            icon.contains("Embarcadero"),
            "expected Embarcadero in expanded icon path: {icon}"
        );
        // Custom_Styles should have $(BDSCOMMONDIR) expanded
        let styles = pg.project_properties.custom_styles.as_deref().unwrap();
        assert!(
            !styles.contains("$(BDSCOMMONDIR)"),
            "expected expanded BDSCOMMONDIR: {styles}"
        );
    }

    #[test]
    fn builder_with_rsvars_file() {
        let dproj = DprojBuilder::new()
            .rsvars_file("rsvars.bat")
            .unwrap()
            .from_file("example.dproj")
            .unwrap();
        let pg = dproj.active_property_group().unwrap();
        let icon = pg.project_properties.icon_main_icon.as_deref().unwrap();
        assert!(
            !icon.contains("$(BDS)"),
            "expected expanded BDS: {icon}"
        );
    }

    #[test]
    fn builder_env_map() {
        let mut env = HashMap::new();
        env.insert("BDS".into(), r"D:\Studio".into());
        env.insert("BDSCOMMONDIR".into(), r"D:\Common".into());
        let dproj = DprojBuilder::new()
            .env(env)
            .from_file("example.dproj")
            .unwrap();
        let pg = dproj.active_property_group().unwrap();
        let styles = pg.project_properties.custom_styles.as_deref().unwrap();
        assert!(
            styles.contains(r"D:\Common"),
            "expected D:\\Common in styles: {styles}"
        );
    }

    #[test]
    fn builder_parse_string() {
        let source = std::fs::read_to_string("example.dproj").unwrap();
        let dproj = DprojBuilder::new()
            .env_var("BDS", r"C:\MyBDS")
            .parse(source)
            .unwrap();
        let pg = dproj.active_property_group().unwrap();
        let icon = pg.project_properties.icon_main_icon.as_deref().unwrap();
        assert!(
            icon.contains(r"C:\MyBDS"),
            "expected C:\\MyBDS in icon: {icon}"
        );
    }

    #[test]
    fn msbuild_project_name_expanded() {
        let dproj = Dproj::from_file("example.dproj").unwrap();
        let pg = dproj.active_property_group().unwrap();
        // VerInfo_Keys contains $(MSBuildProjectName) which should resolve
        // to the project stem "Project1".
        let keys = pg.ver_info.keys.as_deref().unwrap();
        assert!(
            keys.contains("Project1"),
            "expected Project1 in VerInfo_Keys: {keys}"
        );
        assert!(
            !keys.contains("$(MSBuildProjectName)"),
            "expected expanded MSBuildProjectName: {keys}"
        );
    }

}
