use crate::{
    apply_recovered_script_host_callback, mission_events::MissionEvents, MissionMode,
    MissionNotice, MissionScriptHostEffect, MissionScriptOutcome,
};
use fparkan_script::VmHostCallbackCommand;

pub(super) fn apply(
    mode: MissionMode,
    status: &mut u32,
    outcome: &mut MissionScriptOutcome,
    events: &mut MissionEvents,
    command: VmHostCallbackCommand,
    notices: &mut Vec<MissionNotice>,
) -> Result<(), String> {
    if mode == MissionMode::AutoDemo {
        return Ok(());
    }
    let outcome_command = match command.first {
        0 => Some(command),
        1 => {
            // shortcut: present the first message until the native repeated-message UI is wired.
            if events.voiced_messages.insert(command.second) {
                notices.push(MissionNotice::Information(command.second));
            }
            None
        }
        3..=5 => events
            .objectives
            .apply(command.first, command.second, notices)?
            .then_some(VmHostCallbackCommand {
                mode: 0,
                first: 0,
                second: 1,
            }),
        _ => return Err(format!("Handler(30) command {}", command.first)),
    };
    if let Some(command) = outcome_command {
        match apply_recovered_script_host_callback(mode, status, outcome, command) {
            MissionScriptHostEffect::MissionCompleted => notices.push(MissionNotice::Completed),
            MissionScriptHostEffect::MissionFailed => notices.push(MissionNotice::Failed),
            MissionScriptHostEffect::NoChange => {}
            MissionScriptHostEffect::Unhandled => {
                return Err(format!("Handler(30) outcome {}", command.second));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn information_is_presented_once_across_script_callers() {
        let mut events = MissionEvents::default();
        let mut status = 5;
        let mut outcome = MissionScriptOutcome::Active;
        let mut notices = Vec::new();
        for index in [0, 0, 1, 0, 1] {
            apply(
                MissionMode::SingleMission,
                &mut status,
                &mut outcome,
                &mut events,
                VmHostCallbackCommand {
                    mode: 0,
                    first: 1,
                    second: index,
                },
                &mut notices,
            )
            .unwrap();
        }
        assert_eq!(
            notices,
            [MissionNotice::Information(0), MissionNotice::Information(1)]
        );
        assert_eq!(outcome, MissionScriptOutcome::Active);
        assert_eq!(status, 5);
    }

    #[test]
    fn autodemo_does_not_consume_an_information_message() {
        let mut events = MissionEvents::default();
        let mut status = 5;
        let mut outcome = MissionScriptOutcome::Active;
        let mut notices = Vec::new();
        let command = VmHostCallbackCommand {
            mode: 0,
            first: 1,
            second: 0,
        };
        apply(
            MissionMode::AutoDemo,
            &mut status,
            &mut outcome,
            &mut events,
            command,
            &mut notices,
        )
        .unwrap();
        assert!(notices.is_empty());
        apply(
            MissionMode::SingleMission,
            &mut status,
            &mut outcome,
            &mut events,
            command,
            &mut notices,
        )
        .unwrap();
        assert_eq!(notices, [MissionNotice::Information(0)]);
    }
}
