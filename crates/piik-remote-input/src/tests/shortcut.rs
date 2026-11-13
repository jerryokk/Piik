use std::collections::BTreeSet;

use super::*;

#[test]
fn stop_shortcut_collision_and_replacement() {
    let registered = Arc::new(Mutex::new(BTreeSet::from([10])));
    let held = registered.clone();
    let register: RegisterStop = Box::new(move |key| {
        let mut set = lock(&held);
        if !set.insert(key) {
            return Err(Error::input("occupied"));
        }
        let held = held.clone();
        Ok(Box::new(move || {
            lock(&held).remove(&key);
        }))
    });
    let fixture = Fixture::with(
        RecordedInput::focused(),
        Options {
            register_stop: Some(register),
            ..Options::default()
        },
    );
    let has = |key| lock(&registered).contains(&key);
    let session = fixture.session();
    assert!(
        set_policy(&session, "ask", "").is_err() && session.state().mode == "off",
        "occupied shortcut enabled control"
    );
    let state = fixture.manager.read_stop_shortcut().unwrap();
    assert!(
        !state.available && state.key == 10,
        "collision was hidden: {state:?}"
    );
    fixture.manager.set_stop_shortcut(9).unwrap();
    assert!(!has(9), "idle configuration reserved the shortcut");
    assert!(
        set_policy(&session, "ask", "").is_ok() && session.state().mode == "ask" && has(9),
        "changing the shortcut did not recover the same backend"
    );
    assert!(
        fixture.manager.set_stop_shortcut(10).is_err()
            && has(9)
            && fixture.inner(|i| i.stop_key) == 9,
        "a collision retired the working shortcut"
    );
    assert!(
        fixture.manager.set_stop_shortcut(8).is_ok()
            && has(8)
            && !has(9)
            && fixture.inner(|i| i.stop_key) == 8,
        "replacing the shortcut did not retire the old binding"
    );
    let other = fixture.manager.new_session::<FakeChannel>().unwrap();
    other.set_target(display("2"));
    set_policy(&other, "ask", "").unwrap();
    assert!(
        set_policy(&session, "off", "").is_ok() && has(8),
        "disabling one session released another session's shortcut"
    );
    other.close();
    assert!(
        !has(8),
        "closing the last enabled session retained the shortcut"
    );
    assert!(
        set_policy(&session, "ask", "").is_ok() && has(8),
        "a later control session did not reacquire the shortcut"
    );
    fixture.manager.close();
    assert!(
        !has(8) && has(10),
        "shutdown leaked or released another owner's shortcut"
    );
    assert!(crate::valid_stop_key(11) && !crate::valid_stop_key(12) && !crate::valid_stop_key(0));
}
