use fparkan_runtime::{loaded_mission_script_bundles, Engine, MissionScriptInitExecution};

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
