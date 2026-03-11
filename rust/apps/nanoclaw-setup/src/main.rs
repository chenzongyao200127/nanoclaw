use std::env;

const STEPS: &[&str] = &[
    "environment",
    "container",
    "groups",
    "register",
    "mounts",
    "service",
    "verify",
];

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--list-steps") | None => {
            println!("nanoclaw-setup bootstrap");
            println!("planned setup steps:");
            for step in STEPS {
                println!("- {step}");
            }
            println!("status: Rust setup surface is partially implemented.");
        }
        Some("--step") => match args.next() {
            Some(step) if STEPS.contains(&step.as_str()) => {
                println!("requested step: {step}");
                println!("status: partially implemented in Rust");
            }
            Some(step) => {
                eprintln!("unknown step: {step}");
                std::process::exit(1);
            }
            None => {
                eprintln!("missing step name after --step");
                std::process::exit(1);
            }
        },
        Some(flag) => {
            eprintln!("unknown argument: {flag}");
            std::process::exit(1);
        }
    }
}
