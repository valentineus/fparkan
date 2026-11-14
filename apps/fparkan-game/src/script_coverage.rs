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
