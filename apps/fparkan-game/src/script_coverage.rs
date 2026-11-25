use fparkan_runtime::{
    loaded_mission_script_bundles, Engine, MissionScriptInitExecution, SimulationReport,
};

pub(super) fn log_script_init(engine: &Engine, mission: &str) {
    for bundle in loaded_mission_script_bundles(engine).unwrap_or_default() {
        let clan = bundle.clan_index;
        let script = &bundle.script_path;
        for &(instruction, handler) in &bundle.init_stubs {
            eprintln!("sim_coverage script_init_handler_stub=1 mission={mission} clan={clan} script={script} instruction={instruction} handler={handler}");
        }
        if let MissionScriptInitExecution::Unsupported {
            event_index,
            instruction_index,
            error,
        } = &bundle.init_execution
        {
            eprintln!("sim_coverage script_init_unsupported=1 mission={mission} clan={clan} script={script} event={event_index} instruction={instruction_index} reason={error}");
        }
    }
}

pub(super) fn log_clan_ai_stops(report: &SimulationReport) {
    for &(clan, count) in &report.script_capture_queries {
        eprintln!("sim_coverage script_capture_target_approx={count} clan={clan}");
    }
    for &(clan, count) in &report.script_attack_queries {
        eprintln!("sim_coverage script_attack_target_approx={count} clan={clan}");
    }
    for &(clan, target, count) in &report.script_factory_defence_queries {
        eprintln!("sim_coverage script_factory_defence_approx={count} clan={clan} target={target}");
    }
    for &(clan, target, count) in &report.script_building_defence_queries {
        eprintln!(
            "sim_coverage script_building_defence_approx={count} clan={clan} target={target}"
        );
    }
    for &(clan, target, count) in &report.script_base_threat_queries {
        eprintln!("sim_coverage script_base_threat_approx={count} clan={clan} target={target}");
    }
    for &(clan, target, count) in &report.script_group_orders {
        eprintln!("sim_coverage script_group_order_approx={count} clan={clan} target={target}");
    }
    for &(clan, problem, count) in &report.script_problem_waits {
        eprintln!("sim_coverage script_problem_wait_approx={count} clan={clan} problem={problem}");
    }
    for &(clan, target, count) in &report.script_factory_patrol_continues {
        eprintln!("sim_coverage script_factory_patrol_continue_approx={count} clan={clan} target={target}");
    }
    for &(clan, unit, count) in &report.script_ore_transport_orders {
        eprintln!("sim_coverage script_ore_transport_approx={count} clan={clan} unit={unit}");
    }
    for &(clan, class, count) in &report.script_building_site_queries {
        eprintln!(
            "sim_coverage script_building_site_approx={count} clan={clan} class={class:#010x}"
        );
    }
    for &(clan, builder, class, accepted, rejected) in &report.script_construction_orders {
        if accepted != 0 {
            eprintln!("sim_coverage script_building_order_approx={accepted} clan={clan} builder={builder} class={class:#010x}");
        }
        if rejected != 0 {
            eprintln!("sim_coverage script_building_rejected={rejected} clan={clan} builder={builder} class={class:#010x}");
        }
    }
    for &(clan, count) in &report.script_capturer_queries {
        eprintln!("sim_coverage script_nearest_capturer_approx={count} clan={clan}");
    }
    for &(unit, target, count) in &report.script_capture_approaches {
        eprintln!("sim_coverage script_capture_approach_stub={count} unit={unit} target={target}");
    }
    for &(clan, target, count) in &report.script_strength_queries {
        eprintln!("sim_coverage script_combat_strength_approx={count} clan={clan} target={target}");
    }
    for &(clan, plant, count) in &report.script_factory_orders {
        eprintln!("sim_coverage script_factory_orders_approx={count} clan={clan} plant={plant}");
    }
    for &(clan, plant, count) in &report.script_factory_rejections {
        eprintln!("sim_coverage script_factory_rejections={count} clan={clan} plant={plant}");
    }
    for (clan, first_ms) in &report.first_factory_completion_ms {
        eprintln!("script_factory_completion clan={clan} first_ms={first_ms}");
    }
    for &(clan, kind, count) in &report.clan_problem_starts {
        eprintln!("sim_coverage script_problem_starts_approx={count} clan={clan} problem={kind}");
    }
    for (&(clan, kind), count) in &report.clan_problem_dispatch_stubs {
        eprintln!("sim_coverage script_problem_dispatch_stubs={count} clan={clan} problem={kind}");
    }
    for &(clan, handler, count) in &report.clan_research_status_stubs {
        eprintln!(
            "sim_coverage script_research_status_stubs={count} clan={clan} handler={handler}"
        );
    }
    for (stop, count) in &report.clan_ai_event_stops {
        eprintln!(
            "sim_coverage script_clan_ai_stops={count} clan={} script={} event={} instruction={} reason={}",
            stop.clan_index, stop.script_path, stop.event_index, stop.instruction_index, stop.reason,
        );
    }
}
