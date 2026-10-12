//! Entry point for the D2.3 egress scenario driver.
//!
//! `plaudon-egress-scenario <scenario>` runs one scenario under Local-only
//! Mode, writes its JSON decision summary to `PLAUDON_EGRESS_DECISIONS` and
//! exits 0 only when the observed outcome is the one the spec requires.

use std::process::ExitCode;

use plaudon_egress_scenario::{run, Scenario, ScenarioContext};

fn main() -> ExitCode {
    let Some(raw) = std::env::args().nth(1) else {
        eprintln!("usage: plaudon-egress-scenario <scenario>");
        eprintln!("scenarios: {}", scenario_list());
        return ExitCode::from(2);
    };
    if raw == "--list" {
        println!("{}", scenario_list());
        return ExitCode::SUCCESS;
    }
    let Some(scenario) = Scenario::parse(&raw) else {
        eprintln!("unknown scenario: {raw}");
        eprintln!("scenarios: {}", scenario_list());
        return ExitCode::from(2);
    };

    let ctx = match ScenarioContext::from_env() {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("could not prepare the scenario working directory: {err}");
            return ExitCode::from(3);
        }
    };

    let summary = run(scenario, &ctx);
    match plaudon_egress_scenario::write_summary(&summary) {
        Ok(Some(path)) => eprintln!("decisions written to {}", path.display()),
        Ok(None) => eprintln!("PLAUDON_EGRESS_DECISIONS not set; summary not persisted"),
        Err(err) => {
            eprintln!("could not write the decision summary: {err}");
            return ExitCode::from(4);
        }
    }

    println!("{}: {}", summary.scenario, summary.outcome);
    if summary.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn scenario_list() -> String {
    Scenario::ALL
        .iter()
        .map(|s| s.name())
        .collect::<Vec<_>>()
        .join(", ")
}
