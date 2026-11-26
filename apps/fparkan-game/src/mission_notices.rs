use crate::StaticVulkanApp;
use fparkan_runtime::MissionNotice;

impl StaticVulkanApp {
    pub(super) fn present_mission_notice(&mut self, notice: MissionNotice, clock_ms: u32) {
        match notice {
            MissionNotice::BuildingCaptured { .. } => {}
            MissionNotice::ObjectiveComplete(index) => {
                eprintln!(
                    "mission_objective index={index} status=complete queue_time_ms={clock_ms}"
                );
            }
            MissionNotice::Completed | MissionNotice::Failed => {
                let status = if notice == MissionNotice::Completed {
                    "completed"
                } else {
                    "failed"
                };
                eprintln!("mission_outcome status={status} queue_time_ms={clock_ms}");
            }
            MissionNotice::Information(index) => {
                #[cfg(windows)]
                {
                    let queued = self
                        .mission_hints
                        .as_mut()
                        .ok_or_else(|| "mission text resources unavailable".to_string())
                        .and_then(|hints| hints.enqueue(index));
                    if let Err(error) = queued {
                        eprintln!(
                            "sim_coverage mission_text_unavailable=1 index={index} reason={error}"
                        );
                    }
                }
                #[cfg(not(windows))]
                eprintln!("sim_coverage mission_text_unavailable=1 index={index} reason=platform");
            }
        }
        if let Some(audio) = &mut self.audio {
            let voice = match notice {
                MissionNotice::Information(index) => audio.queue_mission_voice(index),
                MissionNotice::ObjectiveComplete(_) => audio.queue_game_voice("VOICE_OBJ_COMPLETE"),
                MissionNotice::BuildingCaptured { neutral } => audio.queue_game_voice(if neutral {
                    "VOICE_NBUILD_CAPTURE"
                } else {
                    "VOICE_EBUILD_CAPTURE"
                }),
                MissionNotice::Completed => audio.queue_game_voice("VOICE_MISSION_COMPLETE"),
                MissionNotice::Failed => audio.queue_game_voice("VOICE_MISSION_FAIL"),
            };
            match voice {
                Ok(resource) => eprintln!(
                    "mission_voice notice={notice:?} resource={resource} queue_time_ms={clock_ms}"
                ),
                Err(error) => eprintln!(
                    "sim_coverage mission_voice_unavailable=1 notice={notice:?} reason={error}"
                ),
            }
        }
    }
}
