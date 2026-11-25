use super::{Instant, StaticVulkanApp};

impl StaticVulkanApp {
    pub(super) fn set_mission_paused_at(&mut self, paused: bool, now: Instant) {
        if paused {
            self.mission_pause_started.get_or_insert(now);
        } else if let Some(start) = self.mission_pause_started.take() {
            if let Some(engine) = &mut self.mission_engine {
                fparkan_runtime::note_mission_pause_elapsed(
                    engine,
                    now.saturating_duration_since(start).as_millis() as u32,
                );
            }
        }
        self.mission_paused = paused;
        self.player_mouse = [0.0; 2];
        self.player_move_pressed = Default::default();
        self.player_capture_pressed = false;
        self.player_select_friend_pressed = false;
        self.player_build_pressed = false;
        self.player_research_pressed = false;
        self.player_mine_pressed = false;
        self.player_disembark_pressed = false;
        if self.cockpit.is_some() && self.demo_camera.is_none() {
            self.set_mouse_look(self.window_focused && !paused);
        }
        self.simulation_last_tick = now;
        eprintln!(
            "mission_pause paused={paused} queue_time_ms={}",
            self.simulation_time_ms
        );
        self.update_mission_title();
        if let Some(audio) = self.audio.as_mut() {
            audio.set_paused(paused || !self.window_focused);
        }
    }
}
