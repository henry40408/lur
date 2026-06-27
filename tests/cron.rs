use std::path::PathBuf;

use lur::runtime::RuntimeConfig;
use lur::serve::Server;

/// A single-VM server with a `SQLite` db (so cron handlers can use lur.kv as an
/// observable side effect).
fn cron_server(db: PathBuf, src: &str) -> Server {
    Server::load(
        src,
        RuntimeConfig {
            db_path: Some(db),
            pool_size: 1,
            ..Default::default()
        },
    )
    .expect("app loads")
}

#[test]
fn cron_handler_runs_when_fired() {
    let dir = tempfile::tempdir().unwrap();
    let s = cron_server(
        dir.path().join("c.db"),
        "lur.serve.cron('* * * * * *', function()\n\
         \tlocal n = (tonumber(lur.kv.get('c')) or 0) + 1\n\
         \tlur.kv.set('c', tostring(n))\n\
         end, { name = 'tick' })\n\
         lur.serve.http('GET', '/c', function() return { body = lur.kv.get('c') or '0' } end)",
    );

    assert!(s.fire_cron("tick").expect("fire ok"), "job 'tick' exists");
    assert_eq!(s.dispatch("GET", "/c", b"").unwrap().body, b"1");
    s.fire_cron("tick").unwrap();
    assert_eq!(s.dispatch("GET", "/c", b"").unwrap().body, b"2");
}

#[test]
fn cron_in_one_shot_is_a_registration_error() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.serve.cron('* * * * * *', function() end)")
            .is_err(),
        "lur.serve.cron must error outside server mode"
    );
}

#[test]
fn invalid_cron_spec_fails_at_load() {
    // 5-field crontab is not valid — must be 6-field (sec min hour dom mon dow).
    let err = Server::load(
        "lur.serve.cron('0 * * * *', function() end)",
        RuntimeConfig::default(),
    );
    assert!(err.is_err(), "a 5-field spec must fail at load");
}
