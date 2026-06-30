use lur::runtime::{Runtime, RuntimeConfig};

#[test]
fn cas_set_if_absent() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "-- add: succeeds when absent, returns true\n\
         assert(lur.state.add('k', 'hello') == true, 'add absent -> true')\n\
         assert(lur.state.get('k') == 'hello', 'value was set')\n\
         -- add: fails when present, returns false\n\
         assert(lur.state.add('k', 'world') == false, 'add present -> false')\n\
         assert(lur.state.get('k') == 'hello', 'original value unchanged')",
    )
    .expect("cas_set_if_absent");
}

#[test]
fn cas_update_if_equal() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "lur.state.set('n', 7)\n\
         -- cas: matching value swaps in the new one\n\
         assert(lur.state.cas('n', 7, 8) == true, 'cas matching -> true')\n\
         assert(lur.state.get('n') == 8, 'new value stored')\n\
         -- cas: non-matching expected leaves value intact\n\
         assert(lur.state.cas('n', 7, 99) == false, 'cas stale -> false')\n\
         assert(lur.state.get('n') == 8, 'value unchanged after failed cas')",
    )
    .expect("cas_update_if_equal");
}

#[test]
fn cas_delete_if_equal() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "lur.state.set('d', 'bye')\n\
         -- cas with nil new-value deletes the key\n\
         assert(lur.state.cas('d', 'bye', nil) == true, 'delete via cas')\n\
         assert(lur.state.get('d') == nil, 'key gone')",
    )
    .expect("cas_delete_if_equal");
}

#[test]
fn cas_ensure_absent() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "-- cas(key, nil, nil): true when absent (ensures absent -> absent)\n\
         assert(lur.state.cas('absent', nil, nil) == true, 'nil->nil on absent')\n\
         -- cas(key, nil, nil): false when present (key was there)\n\
         lur.state.set('present', 'yes')\n\
         assert(lur.state.cas('present', nil, nil) == false, 'nil->nil on present -> false')",
    )
    .expect("cas_ensure_absent");
}

#[test]
fn cas_wrong_key_type_errors() {
    // Tables and functions don't coerce to string, so they trigger the type guard.
    let rt = Runtime::new().expect("runtime builds");
    let err = rt
        .run("lur.state.cas({}, nil, nil)")
        .expect_err("table key for cas must error");
    assert!(
        err.to_string().contains("lur.state.cas"),
        "error voiced correctly: {err}"
    );
    let err2 = rt
        .run("lur.state.add({}, 'v')")
        .expect_err("table key for add must error");
    assert!(
        err2.to_string().contains("lur.state.add"),
        "error voiced correctly: {err2}"
    );
}

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
