# dproj-rs

Parse Embarcadero Delphi/RAD Studio `.dproj` project files and `rsvars.bat` environment variable files.

## Features

- **`.dproj` parsing**: Read and parse Delphi project files with proper handling of conditions, properties, and references
- **`rsvars.bat` parsing**: Extract environment variables from RAD Studio's rsvars.bat with variable expansion
- **Variable resolution**: Expand MSBuild-style `$(Var)` references and `%VAR%` environment variables
- **Condition evaluation**: Evaluate conditional expressions in project configurations

## Usage

```rust
use dproj_rs::Dproj;

let dproj = Dproj::from_file("MyProject.dproj")?;
```

See the [examples](examples/) directory for more detailed usage.

## License

Licensed under the [LICENSE](LICENSE) file.
