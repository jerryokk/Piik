//! The Windows XInput slots — the single truth for how many controllers the
//! Host's machine can carry, whether they are physical or virtual.
//!
//! ViGEm's plug request names no slot: the driver places a new virtual pad in
//! whatever XInput index is free next. Counting our own devices therefore said
//! nothing about what the games could see, and a Host with two physical pads
//! could end up with four "successful" virtual ones of which two never appeared.
//! Observing XInput instead makes both kinds share one budget.

use windows::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};

use crate::gamepad::MAX_WINDOWS_GAMEPADS;

/// Which XInput slots currently answer. A pad that has been plugged in but not
/// yet enumerated by the driver reads as free, so callers re-read after
/// plugging rather than treating one sample as final.
pub(crate) fn occupied() -> [bool; MAX_WINDOWS_GAMEPADS] {
    let mut slots = [false; MAX_WINDOWS_GAMEPADS];
    for (index, slot) in slots.iter_mut().enumerate() {
        let mut state = XINPUT_STATE::default();
        let index = index as u32;
        // SAFETY: `state` outlives the call; a missing pad returns an error.
        *slot = unsafe { XInputGetState(index, &mut state) } == 0;
    }
    slots
}

pub(crate) fn free_count() -> usize {
    occupied().iter().filter(|used| !**used).count()
}

/// A slot that was free before a plug and answers after it. `None` means the
/// driver accepted the plug but XInput never showed the pad: the caller must
/// retire it instead of reporting success.
pub(crate) fn newly_occupied(
    before: &[bool; MAX_WINDOWS_GAMEPADS],
    after: &[bool; MAX_WINDOWS_GAMEPADS],
) -> Option<usize> {
    (0..MAX_WINDOWS_GAMEPADS).find(|index| !before[*index] && after[*index])
}

#[cfg(test)]
mod tests {
    use super::newly_occupied;

    #[test]
    fn a_plug_lands_in_the_slot_that_changed() {
        let before = [true, false, false, true];
        let after = [true, false, true, true];
        assert_eq!(newly_occupied(&before, &after), Some(2));
    }

    #[test]
    fn a_pad_that_never_appeared_is_not_a_success() {
        let slots = [true, false, false, false];
        assert_eq!(newly_occupied(&slots, &slots), None);
    }

    #[test]
    fn another_connection_taking_the_slot_is_not_ours() {
        // Both slots were free, but the new pad appeared in one we did not
        // expect while ours is still missing: there is no proof ours exists.
        let before = [false, false, false, false];
        let after = [false, false, true, false];
        assert_eq!(newly_occupied(&before, &after), Some(2));
        let occupied_before = [true, false, false, false];
        let occupied_after = [true, false, false, false];
        assert_eq!(newly_occupied(&occupied_before, &occupied_after), None);
    }
}
