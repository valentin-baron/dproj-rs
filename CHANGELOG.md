# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] – 2026-02-28

### Changed

- **`rsvars::parse_rsvars` / `parse_rsvars_file`** now seed the returned map
  with all current process environment variables before parsing the file.
  File entries override any duplicate key from the environment.  All keys are
  stored and looked up in upper-case to match Windows' case-insensitive
  environment semantics (`Path` and `PATH` are treated as the same variable).
  `%VAR%` expansions inside the file therefore resolve against the live system
  environment without any extra steps from the caller.

### Removed

- **`DprojBuilder::system_env`** has been removed.  The method was a manual
  way to merge the process environment into the builder's variable map; that
  behaviour is now automatic inside `parse_rsvars` / `parse_rsvars_file`, so
  the method is no longer needed.  If you were calling `.system_env()` after
  `.rsvars(…)` or `.rsvars_file(…)`, simply remove that call — the result is
  identical.

## [0.1.0] – 2026-02-27

Initial release.

- Parse `.dproj` project files into typed Rust structs.
- Read and mutate property values while preserving original XML formatting.
- Evaluate MSBuild-style `Condition` attributes.
- Expand `$(Var)` references across `<PropertyGroup>` elements.
- Parse `rsvars.bat` files and expand `%VAR%` references.
- `DprojBuilder` for constructing a `Dproj` with custom environment variables.
- `Dproj::from_file` / `Dproj::parse` convenience constructors.
- `Dproj::active_property_group` / `active_property_group_for` for merged
  property resolution.
- Path helpers: `get_main_source`, `get_exe_path`, `get_exe_path_for`.
- Configuration / platform setters: `set_configuration`, `set_platform`,
  `set_property_value`.
