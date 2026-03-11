use nanoclaw_core::{MigrationPhase, default_porting_targets};
use nanoclaw_db::planned_tables;
use nanoclaw_runtime::critical_boundaries;

fn main() {
    println!("nanoclaw-host bootstrap");
    println!("phase: {:?}", MigrationPhase::Scaffolded);
    println!("{}", MigrationPhase::Scaffolded.description());
    println!();
    println!("porting targets:");
    for target in default_porting_targets() {
        println!("- {} -> {}: {}", target.ts_area, target.rust_area, target.summary);
    }
    println!();
    println!("planned tables:");
    for table in planned_tables() {
        println!("- {}", table.name());
    }
    println!();
    println!("runtime boundaries:");
    for boundary in critical_boundaries() {
        println!("- {} ({})", boundary.name, boundary.goal);
    }
}
