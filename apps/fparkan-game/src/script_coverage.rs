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
    for ((object, component), (count, reason)) in &report.weapon_poses.fallbacks {
        eprintln!("sim_coverage unit_weapon_pose_fallback={count} object={object} component={component} reason={reason}");
    }
    for (object, count) in &report.weapon_poses.missing {
        eprintln!("sim_coverage unit_weapon_pose_stub={count} object={object}");
    }
    for &(clan, result, count) in &report.clan_research_scan_queries {
        eprintln!("sim_coverage script_research_scan_status={count} clan={clan} result={result}");
    }
    for &(clan, count, registered) in &report.clan_ai_clock_ticks {
        eprintln!("sim_coverage script_clan_clock_approx={count} script_clan_registration={registered} clan={clan}");
    }
    for &(clan, entries, selected, count) in &report.clan_research_catalogs {
        eprintln!("sim_coverage script_research_catalog_approx={count} clan={clan} entries={entries} selected={selected}");
    }
    for &(clan, lab, accepted, rejected) in &report.laboratory.orders {
        eprintln!("sim_coverage script_lab_research_approx={accepted} script_lab_research_rejections={rejected} clan={clan} lab={lab}");
    }
    for &(clan, lab, row, count) in &report.laboratory.completed {
        eprintln!(
            "sim_coverage script_lab_technology_researched={count} clan={clan} lab={lab} row={row}"
        );
    }
    for &(clan, row, count) in &report.laboratory.available {
        eprintln!("sim_coverage script_research_available={count} clan={clan} row={row}");
    }
    for &(clan, lab, count) in &report.laboratory.stopped {
        eprintln!("sim_coverage script_lab_research_stopped={count} clan={clan} lab={lab}");
    }
    for &(clan, lab, count) in &report.laboratory.clock_ticks {
        eprintln!("sim_coverage script_lab_clock_approx={count} clan={clan} lab={lab}");
    }
    for &(clan, target, x, y, count) in &report.script_mine_queries {
        eprintln!("sim_coverage script_mine_target_approx={count} clan={clan} target={target} x={x} y={y}");
    }
    for &(clan, class, blocked, count) in &report.script_class_limit_queries {
        eprintln!("sim_coverage script_class_limit_approx={count} clan={clan} class={class:#010x} blocked={blocked}");
    }
    for &(unit, order, count) in &report.script_unit_order_counts {
        eprintln!("sim_coverage script_unit_queue_approx={count} unit={unit} order={order}");
    }
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
    for &(clan, event, instruction, count) in &report.script_area_strength_queries {
        eprintln!("sim_coverage script_area_strength_approx={count} clan={clan} event={event} instruction={instruction}");
    }
    for &(clan, target, count) in &report.script_base_threat_queries {
        eprintln!("sim_coverage script_base_threat_approx={count} clan={clan} target={target}");
    }
    for &(clan, target, count) in &report.script_group_orders {
        eprintln!("sim_coverage script_group_order_approx={count} clan={clan} target={target}");
    }
    for &(clan, x, y, count) in &report.script_point_group_orders {
        eprintln!("sim_coverage script_point_group_order_approx={count} clan={clan} x={x} y={y}");
    }
    for &(clan, x, y, count) in &report.script_point_patrol_continues {
        eprintln!(
            "sim_coverage script_point_patrol_continue_approx={count} clan={clan} x={x} y={y}"
        );
    }
    for &(clan, class, unit, count) in &report.script_free_unit_queries {
        eprintln!(
            "sim_coverage script_free_unit_approx={count} clan={clan} class={class} unit={unit}"
        );
    }
    for &(clan, mode, count) in &report.script_group_recruitments {
        eprintln!("sim_coverage script_group_recruitment_approx={count} clan={clan} mode={mode}");
    }
    for &(clan, capacity, free, reserved, count) in &report.script_free_mind_queries {
        eprintln!("sim_coverage script_free_minds_approx={count} clan={clan} capacity={capacity} free={free} reserved={reserved}");
    }
    for &(clan, class, target, count) in &report.script_class_target_queries {
        eprintln!("sim_coverage script_class_target_approx={count} clan={clan} class={class} target={target}");
    }
    for &(clan, unit, count) in &report.script_size_class_queries {
        eprintln!("sim_coverage script_size_class_approx={count} clan={clan} unit={unit}");
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
    for &(clan, builder, source, count) in &report.script_builder_supplies {
        eprintln!("sim_coverage script_builder_supply_approx={count} clan={clan} builder={builder} source={source}");
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
    for &(clan, plant, count) in &report.factory_clock_ticks {
        eprintln!("sim_coverage script_factory_clock_approx={count} clan={clan} plant={plant}");
    }
    for &(unit, count) in &report.control_resource_ticks {
        eprintln!("sim_coverage control_resource_clock_approx={count} unit={unit}");
    }
    for (unit, count, reason) in &report.control_resource_stops {
        eprintln!("sim_coverage control_resource_budget_stub={count} unit={unit} reason={reason}");
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
