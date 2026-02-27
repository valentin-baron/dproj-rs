use dproj_rs::dproj::Dproj;
use std::time::{Duration, Instant};

fn bench<T>(label: &str, iterations: u32, mut f: impl FnMut() -> T) -> (Duration, T) {
    // Warmup
    for _ in 0..5 {
        std::hint::black_box(f());
    }

    let mut total = Duration::ZERO;
    let mut last = None;
    for _ in 0..iterations {
        let start = Instant::now();
        let result = f();
        total += start.elapsed();
        last = Some(result);
    }

    let avg = total / iterations;
    println!("{label:<45} {iterations:>6} iterations   avg {avg:>12.3?}   total {total:>12.3?}");
    (avg, last.unwrap())
}

fn main() {
    let source = std::fs::read_to_string("example.dproj")
        .expect("example.dproj not found — run from repo root");

    let iterations = 1000;

    println!("─── Performance: example.dproj ({} bytes) ───", source.len());
    println!();

    // 1. Full parse (XML + type mapping)
    bench("Dproj::parse (full)", iterations, || {
        Dproj::parse(source.clone()).unwrap()
    });

    // 2. roxmltree XML parse only (baseline)
    bench("roxmltree::Document::parse (XML only)", iterations, || {
        roxmltree::Document::parse(&source).unwrap()
    });

    // 3. Active PG resolution: Debug/Win32
    let dproj = Dproj::parse(source.clone()).unwrap();
    bench("active_property_group (default)", iterations, || {
        dproj.active_property_group().unwrap()
    });

    // 4. Active PG resolution: Release/Win32
    bench("active_property_group_for(Release, Win32)", iterations, || {
        dproj.active_property_group_for("Release", "Win32").unwrap()
    });

    // 5. Condition parsing only (all conditions from the file)
    let conditions: Vec<String> = dproj
        .project
        .property_groups
        .iter()
        .filter_map(|pg| pg.condition.clone())
        .collect();
    let cond_count = conditions.len();
    bench(
        &format!("parse all {cond_count} conditions"),
        iterations,
        || {
            for cond in &conditions {
                std::hint::black_box(dproj_rs::condition::parse_condition(cond).unwrap());
            }
        },
    );

    // 6. Mutation round-trip (set + reparse)
    bench("set_property_value + reparse", iterations, || {
        let mut d = Dproj::parse(source.clone()).unwrap();
        d.set_property_value(0, "ProjectVersion", "99.9").unwrap();
    });

    println!();
    println!("Done.");
}
