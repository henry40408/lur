use lur::runtime::{Runtime, RuntimeConfig};

#[test]
fn set_get_incr_update_round_trip() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "assert(lur.state.get('x') == nil, 'absent is nil')\n\
         lur.state.set('x', 'hello')\n\
         assert(lur.state.get('x') == 'hello', 'string round-trip')\n\
         lur.state.set('b', true)\n\
         assert(lur.state.get('b') == true, 'boolean round-trip')\n\
         lur.state.set('n', 41)\n\
         assert(lur.state.incr('n') == 42, 'incr default +1')\n\
         assert(lur.state.incr('n', 8) == 50, 'incr +8')\n\
         assert(lur.state.incr('fresh') == 1, 'absent starts at 0')\n\
         lur.state.set('x', nil)\n\
         assert(lur.state.get('x') == nil, 'set nil deletes')\n\
         local v = lur.state.update('n', function(old) return old + 100 end)\n\
         assert(v == 150, 'update returns the new value, got ' .. tostring(v))\n\
         assert(lur.state.get('n') == 150, 'update persisted')",
    )
    .expect("state ops work");
}

#[test]
fn non_primitive_value_is_an_error() {
    let rt = Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.state.set('t', {})").is_err(),
        "storing a table must error"
    );
}

#[test]
fn incr_on_a_non_number_is_an_error() {
    let rt = Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.state.set('s', 'text')\nlur.state.incr('s')")
            .is_err(),
        "incrementing a non-number must error"
    );
}

#[test]
fn update_fn_cannot_reenter_state() {
    let rt = Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.state.update('k', function(old) return lur.state.get('k') end)")
            .is_err(),
        "re-entering lur.state from an update fn must error"
    );
}

#[test]
fn state_incr_is_integer_and_has_decr() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    rt.run(
        "assert(lur.state.incr('n') == 1, 'first incr -> 1')\n\
         assert(lur.state.incr('n', 4) == 5, 'incr by 4')\n\
         assert(lur.state.decr('n', 2) == 3, 'decr by 2')\n\
         -- fractional step is rejected\n\
         local ok = pcall(function() return lur.state.incr('n', 0.5) end)\n\
         assert(ok == false, 'fractional step rejected')\n\
         -- non-integer existing value is rejected\n\
         lur.state.set('s', 'text')\n\
         local ok2, err = pcall(function() return lur.state.incr('s') end)\n\
         assert(ok2 == false and tostring(err):find('not an integer'), 'msg: ' .. tostring(err))",
    )
    .expect("state integer incr/decr");
}

#[test]
fn state_is_shared_across_vms_from_the_same_config() {
    // Cross-VM sharing is the whole point (§6): the store is host-side. Two VMs
    // built from the same config share the same store Arc.
    let config = RuntimeConfig::default();
    let writer = Runtime::with_config(config.clone()).expect("runtime builds");
    let reader = Runtime::with_config(config).expect("runtime builds");
    writer.run("lur.state.set('shared', 7)").expect("write");
    reader
        .run("assert(lur.state.get('shared') == 7, 'reader sees writer')")
        .expect("read sees the shared write");
}
