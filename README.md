# dproj-rs

Parse Embarcadero Delphi/RAD Studio `.dproj` project files and `rsvars.bat` environment variable files.

## Features

- **`.dproj` parsing**: Read and parse Delphi project files with proper handling of conditions, properties, and references
- **`rsvars.bat` parsing**: Extract environment variables from RAD Studio's `rsvars.bat` with full `%VAR%` expansion against the live system environment
- **Variable resolution**: Expand MSBuild-style `$(Var)` references and `%VAR%` environment variables
- **Condition evaluation**: Evaluate conditional expressions in project configurations
- **Non-destructive mutation**: Change property values while preserving original XML whitespace, comments, and attribute ordering

## Usage

```rust
use dproj_rs::{Dproj, DprojBuilder, rsvars};

// Simple – load without rsvars
let dproj = Dproj::from_file("MyProject.dproj")?;

// With rsvars – system env is included automatically
let dproj = DprojBuilder::new()
    .rsvars_file(r"C:\Program Files (x86)\Embarcadero\Studio\23.0\bin\rsvars.bat")?
    .from_file("MyProject.dproj")?;
```

See the [examples](examples/) directory for more detailed usage.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for a full history of changes.

## License

Licensed under the [LICENSE](LICENSE) file.
