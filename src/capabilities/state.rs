//! `lur.state` — process-wide primitive KV shared by every pooled VM.
//! `update` is optimistic (version-checked retry), so no lock is held across
//! user code. Entries may carry a TTL and may be marked evictable; only
//! evictable entries are ever dropped to make room (see `Limits`).
//!
//! `fetch` is single-flight per key: on a miss one caller (the *leader*) runs
//! the loader while concurrent callers for the same key, on any VM, wait for
//! its outcome (see [`Flight`]). Only Rust data ([`Shared`]) crosses VMs.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, Table, Value};
use tokio::sync::watch;

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// A stored primitive; nil is absence.
#[derive(Debug, Clone, PartialEq)]
enum Prim {
    Bool(bool),
    Num(f64),
    Str(Vec<u8>),
}

impl Prim {
    fn size(&self) -> usize {
        match self {
            Prim::Bool(_) => 1,
            Prim::Num(_) => 8,
            Prim::Str(s) => s.len(),
        }
    }
}

/// Fixed per-entry charge on top of key and value bytes.
const ENTRY_OVERHEAD: usize = 64;
/// How often a write sweeps expired entries even when under the caps.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// A live value and the version it was written at. Versions come from one
/// store-wide counter that only goes up, so a key deleted and recreated always
/// gets a newer version than any a reader could still hold (no ABA), and a
/// deleted key needs no entry. An absent key reads as version 0.
///
/// An entry past `expires` but within `stale_for` of it is *stale*: invisible to
/// every operation except `fetch`, which may fall back to it when its loader
/// fails. A stale entry reads as version 0, like an absent one.
#[derive(Debug, Clone)]
struct Entry {
    value: Prim,
    version: u64,
    /// Fresh until this instant; `None` never expires.
    expires: Option<Instant>,
    /// Kept for `fetch` fallback this long after `expires`.
    stale_for: Option<Duration>,
    /// Only evictable entries sit in `Inner::order` and may be dropped for room.
    evictable: bool,
    size: usize,
    /// Recency stamp; the key of this entry in `Inner::order` iff `evictable`.
    tick: u64,
}

enum Freshness {
    Fresh,
    Stale,
    Dead,
}

impl Entry {
    fn freshness(&self, now: Instant) -> Freshness {
        match self.expires {
            Some(x) if now >= x => match self.stale_for {
                Some(d) if x.checked_add(d).is_none_or(|s| now < s) => Freshness::Stale,
                _ => Freshness::Dead,
            },
            _ => Freshness::Fresh,
        }
    }
}

/// What a write does to the expiry of the entry it replaces.
#[derive(Debug, Clone, Copy, Default)]
enum Expiry {
    /// No expiry (`set`/`add` without `ttl_ms`).
    #[default]
    Clear,
    /// Keep whatever the entry has (`cas`/`update`/`incr` without `ttl_ms`).
    Keep,
    /// Expire this many ms from now.
    After(i64),
    /// Set only if the entry has none (`incr` fixed window).
    AfterIfUnset(i64),
}

#[derive(Debug, Clone, Copy, Default)]
struct Write {
    expiry: Expiry,
    /// `None` keeps the existing flag (`false` for a new key).
    evictable: Option<bool>,
    /// `fetch` only: how long past expiry the value stays usable as a fallback.
    stale_ms: Option<i64>,
}

/// A write that could not fit under the caps: nothing evictable was left.
#[derive(Debug)]
struct Full;

fn add_ms(now: Instant, ms: i64) -> Instant {
    // `ms` is validated positive; an absurd one must not overflow `Instant`.
    now.checked_add(Duration::from_millis(ms.cast_unsigned()))
        .unwrap_or_else(|| now + Duration::from_secs(10 * 365 * 24 * 3600))
}

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<Vec<u8>, Entry>,
    /// Recency index of evictable entries: `tick` -> key, oldest first.
    order: BTreeMap<u64, Vec<u8>>,
    next_tick: u64,
    bytes: usize,
    /// Last version handed out; only ever increases.
    last_version: u64,
    last_sweep: Option<Instant>,
    /// Lower bound on the earliest `expires` of any entry; lets a write at the
    /// cap skip scanning the map when nothing can have expired.
    earliest: Option<Instant>,
    evictions: u64,
    /// `fetch` loaders in flight, one per key. Not cache entries: never
    /// counted against the caps, and removed by the leader's [`LeadGuard`] on
    /// every exit path.
    flights: HashMap<Vec<u8>, Flight>,
    next_flight: u64,
}

/// A leader's outcome, handed to every waiter of its flight. Rust data only:
/// waiters may sit on other VMs, so no Lua value is shared.
#[derive(Debug)]
enum Shared {
    /// The loader returned this (`None` = `nil`, which is not cached).
    Value(Option<Prim>),
    /// The loader raised: waiters fall back to a stale value as the leader
    /// does, else raise this.
    Raised(mlua::Error),
    /// The loader returned a non-primitive: waiters raise this, as the leader
    /// does, without a stale fallback.
    Rejected(mlua::Error),
}

/// What a waiter sees: `None` while the leader runs, then its outcome. A
/// leader that ends with no outcome (timed out, cancelled) drops the sender
/// unsent, which closes the channel: waiters then race to lead (handoff).
type Outcome = Option<Arc<Shared>>;

/// One in-flight `fetch` loader for a key.
#[derive(Debug)]
struct Flight {
    /// Tells this leadership apart from a later one for the same key.
    id: u64,
    tx: watch::Sender<Outcome>,
    /// Keys that callers nested inside this flight's loader are waiting on,
    /// with counts: the edges [`Inner::waits_for`] walks to refuse a wait
    /// that would deadlock.
    waits: HashMap<Vec<u8>, usize>,
    /// The leader's VM, so [`end_run`] can remove what a run left behind.
    owner: Arc<VmLeads>,
}

/// Flights led from one VM; counts them so [`end_run`] costs one atomic load
/// when there are none (the usual case).
#[derive(Debug, Default)]
pub(crate) struct VmLeads(AtomicUsize);

/// The VM's [`VmLeads`] and store, kept in `lua` app data for [`end_run`].
struct VmFlights {
    store: Arc<StateStore>,
    leads: Arc<VmLeads>,
}

/// Call when a run on `lua` ends, however it ended. A VM runs one request or
/// cron job at a time, so any flight it still leads now is orphaned: its
/// leader is parked in a Lua coroutine the script suspended and never
/// resumed (which may stay reachable, e.g. in an app.lua upvalue, so no GC
/// would drop it). Removing the flight hands off to its waiters; the stray
/// `LeadGuard`, if ever dropped or finished, then finds no flight with its id.
pub(crate) fn end_run(lua: &Lua) {
    let Some(vm) = lua.app_data_ref::<VmFlights>() else {
        return;
    };
    if vm.leads.0.load(Ordering::Acquire) == 0 {
        return;
    }
    let mut inner = vm.store.lock_for_drop();
    let orphaned: Vec<Vec<u8>> = inner
        .flights
        .iter()
        .filter(|(_, f)| Arc::ptr_eq(&f.owner, &vm.leads))
        .map(|(k, _)| k.clone())
        .collect();
    let removed: Vec<Flight> = orphaned
        .iter()
        .filter_map(|k| remove_flight(&mut inner, k))
        .collect();
    drop(inner);
    drop(removed); // closes the channels outside the lock
}

impl Inner {
    /// Whether waiting on `key` from inside the loaders in `chain` would wait
    /// on ourselves: `key` is in `chain`, or the leader of `key` (or anything
    /// its loader waits on, transitively) waits on a key in `chain`.
    fn waits_for(&self, key: &[u8], chain: &[(Vec<u8>, u64)]) -> bool {
        let mut stack = vec![key];
        let mut seen: Vec<&[u8]> = Vec::new();
        while let Some(k) = stack.pop() {
            if chain.iter().any(|(c, _)| c.as_slice() == k) {
                return true;
            }
            if seen.contains(&k) {
                continue;
            }
            seen.push(k);
            if let Some(f) = self.flights.get(k) {
                stack.extend(f.waits.keys().map(Vec::as_slice));
            }
        }
        false
    }

    /// Add (`up`) or remove one wait edge to `key` on each flight in `chain`
    /// that is still the same leadership.
    fn edge(&mut self, chain: &[(Vec<u8>, u64)], key: &[u8], up: bool) {
        for (c, id) in chain {
            let Some(f) = self.flights.get_mut(c).filter(|f| f.id == *id) else {
                continue;
            };
            if up {
                *f.waits.entry(key.to_vec()).or_default() += 1;
            } else if let Some(n) = f.waits.get_mut(key) {
                *n -= 1;
                if *n == 0 {
                    f.waits.remove(key);
                }
            }
        }
    }
    fn stamp(&mut self) -> u64 {
        self.next_tick += 1;
        self.next_tick
    }

    fn remove(&mut self, key: &[u8]) {
        if let Some(e) = self.map.remove(key) {
            if e.evictable {
                self.order.remove(&e.tick);
            }
            self.bytes -= e.size;
        }
    }

    fn drop_dead(&mut self, now: Instant) {
        let dead: Vec<Vec<u8>> = self
            .map
            .iter()
            .filter(|(_, e)| matches!(e.freshness(now), Freshness::Dead))
            .map(|(k, _)| k.clone())
            .collect();
        for key in dead {
            self.remove(&key);
        }
    }

    /// Drop every entry past `expires`: dead ones, and stale ones that may be
    /// evicted. These go before any live entry is evicted. A no-op unless
    /// something can have expired, so a full cache does not rescan per write.
    fn drop_expired(&mut self, now: Instant) {
        if self.earliest.is_none_or(|e| e > now) {
            return;
        }
        let gone: Vec<Vec<u8>> = self
            .map
            .iter()
            .filter(|(_, e)| {
                e.expires.is_some_and(|x| x <= now)
                    && (e.evictable || matches!(e.freshness(now), Freshness::Dead))
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in gone {
            self.remove(&key);
        }
        // What remains and is already past `expires` is stale and unevictable;
        // it is reaped by `drop_dead` when its window closes.
        self.earliest = self
            .map
            .values()
            .filter_map(|e| e.expires)
            .filter(|x| *x > now)
            .min();
    }

    /// The fresh entry for `key`, if any. A dead one is removed on the way; a
    /// stale one reads as absent but is kept for `fetch`.
    fn live(&mut self, key: &[u8], now: Instant) -> Option<&Entry> {
        match self.map.get(key)?.freshness(now) {
            Freshness::Fresh => self.map.get(key),
            Freshness::Stale => None,
            Freshness::Dead => {
                self.remove(key);
                None
            }
        }
    }

    /// Mark a fresh evictable entry most recently used.
    fn touch(&mut self, key: &[u8]) {
        let Some(old) = self.map.get(key).filter(|e| e.evictable).map(|e| e.tick) else {
            return;
        };
        let tick = self.stamp();
        self.order.remove(&old);
        self.order.insert(tick, key.to_vec());
        if let Some(e) = self.map.get_mut(key) {
            e.tick = tick;
        }
    }

    fn version(&mut self, key: &[u8], now: Instant) -> u64 {
        self.live(key, now).map_or(0, |e| e.version)
    }

    /// Write `value` (`None` deletes) under a fresh version, evicting evictable
    /// entries (expired first) to stay under `limits`. On `Err(Full)` nothing
    /// was changed.
    fn put(
        &mut self,
        key: Vec<u8>,
        value: Option<Prim>,
        w: Write,
        now: Instant,
        limits: Limits,
    ) -> Result<(), Full> {
        let Some(value) = value else {
            self.remove(&key);
            return Ok(());
        };
        if self
            .last_sweep
            .is_none_or(|t| now.duration_since(t) >= SWEEP_EVERY)
        {
            self.drop_dead(now);
            self.last_sweep = Some(now);
        }
        let (prev_expires, prev_evictable, prev_stale) =
            self.live(&key, now).map_or((None, false, None), |e| {
                (e.expires, e.evictable, e.stale_for)
            });
        let (expires, inherited) = match w.expiry {
            Expiry::Clear => (None, false),
            Expiry::Keep => (prev_expires, true),
            Expiry::After(ms) => (Some(add_ms(now, ms)), false),
            Expiry::AfterIfUnset(ms) => match prev_expires {
                Some(x) => (Some(x), true),
                None => (Some(add_ms(now, ms)), false),
            },
        };
        let evictable = w.evictable.unwrap_or(prev_evictable);
        // A write that keeps the expiry keeps its stale window too.
        let stale_for = match (expires, w.stale_ms) {
            (Some(_), Some(ms)) => Some(Duration::from_millis(ms.cast_unsigned())),
            (Some(_), None) if inherited => prev_stale,
            _ => None,
        };
        let size = key.len() + value.size() + ENTRY_OVERHEAD;
        if limits.max_bytes != 0 && size > limits.max_bytes {
            return Err(Full);
        }
        let over = |this: &Self| {
            let (present, old) = this.map.get(&key).map_or((0, 0), |e| (1, e.size));
            let entries = this.map.len() + 1 - present;
            let bytes = this.bytes - old + size;
            (limits.max_entries != 0 && entries > limits.max_entries)
                || (limits.max_bytes != 0 && bytes > limits.max_bytes)
        };
        if over(self) {
            // Expired entries are free to drop; only then evict live ones.
            self.drop_expired(now);
        }
        // Evict into a scratch plan first so a write that cannot fit changes
        // nothing: collect victims, and only apply them if the write then fits.
        if over(self) {
            let mut victims = Vec::new();
            let (mut entries, mut bytes) = (self.map.len(), self.bytes);
            if let Some(e) = self.map.get(&key) {
                entries -= 1;
                bytes -= e.size;
            }
            entries += 1;
            bytes += size;
            for (tick, k) in &self.order {
                if (limits.max_entries == 0 || entries <= limits.max_entries)
                    && (limits.max_bytes == 0 || bytes <= limits.max_bytes)
                {
                    break;
                }
                if k.as_slice() == key.as_slice() {
                    continue;
                }
                let e = &self.map[k];
                entries -= 1;
                bytes -= e.size;
                victims.push((*tick, k.clone()));
            }
            let fits = (limits.max_entries == 0 || entries <= limits.max_entries)
                && (limits.max_bytes == 0 || bytes <= limits.max_bytes);
            if !fits {
                return Err(Full);
            }
            for (tick, victim) in victims {
                self.order.remove(&tick);
                self.remove(&victim);
                self.evictions += 1;
            }
        }
        self.remove(&key);
        self.last_version += 1;
        let tick = if evictable {
            let t = self.stamp();
            self.order.insert(t, key.clone());
            t
        } else {
            0
        };
        self.bytes += size;
        if let Some(x) = expires {
            self.earliest = Some(self.earliest.map_or(x, |e| e.min(x)));
        }
        self.map.insert(
            key,
            Entry {
                value,
                version: self.last_version,
                expires,
                stale_for,
                evictable,
                size,
                tick,
            },
        );
        Ok(())
    }
}

#[derive(Debug)]
enum IncrError {
    NotInteger,
    Overflow,
    Full,
}

/// Caps on the store; `0` is unlimited. Only evictable entries are dropped to
/// make room, so a write that cannot fit raises instead of losing a counter.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    pub max_entries: usize,
    pub max_bytes: usize,
}

/// What a `fetch` lookup found.
enum Lookup {
    Fresh(Prim),
    Stale(Prim),
    Absent,
}

/// Counters for a future `/metrics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateStats {
    pub hits: u64,
    pub misses: u64,
    pub stale: u64,
    pub evictions: u64,
}

#[derive(Debug, Default)]
pub struct StateStore {
    inner: Mutex<Inner>,
    limits: Limits,
    hits: AtomicU64,
    misses: AtomicU64,
    stale: AtomicU64,
}

impl StateStore {
    #[must_use]
    pub fn with_limits(limits: Limits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn stats(&self) -> StateStats {
        StateStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            stale: self.stale.load(Ordering::Relaxed),
            evictions: self.lock().evictions,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("state mutex poisoned")
    }

    fn get(&self, key: &[u8]) -> Option<Prim> {
        let mut inner = self.lock();
        let value = inner.live(key, Instant::now())?.value.clone();
        inner.touch(key);
        Some(value)
    }

    fn set(&self, key: Vec<u8>, value: Option<Prim>, w: Write) -> Result<(), Full> {
        self.lock().put(key, value, w, Instant::now(), self.limits)
    }

    fn incr(&self, key: Vec<u8>, n: i64, w: Write) -> Result<i64, IncrError> {
        let mut inner = self.lock();
        let now = Instant::now();
        let base: i64 = match inner.live(&key, now).map(|e| &e.value) {
            None => 0,
            Some(Prim::Num(x))
                if x.fract() == 0.0 && *x >= i64::MIN as f64 && *x < i64::MAX as f64 =>
            {
                *x as i64
            }
            Some(_) => return Err(IncrError::NotInteger),
        };
        let new = base.checked_add(n).ok_or(IncrError::Overflow)?;
        inner
            .put(key, Some(Prim::Num(new as f64)), w, now, self.limits)
            .map_err(|Full| IncrError::Full)?;
        Ok(new)
    }

    fn snapshot(&self, key: &[u8]) -> (Option<Prim>, u64) {
        match self.lock().live(key, Instant::now()) {
            Some(e) => (Some(e.value.clone()), e.version),
            None => (None, 0),
        }
    }

    /// Store `value` iff the key's version is still `expected`.
    fn compare_and_set(
        &self,
        key: &[u8],
        expected: u64,
        value: Option<Prim>,
        w: Write,
    ) -> Result<bool, Full> {
        let mut inner = self.lock();
        let now = Instant::now();
        if inner.version(key, now) != expected {
            return Ok(false);
        }
        inner.put(key.to_vec(), value, w, now, self.limits)?;
        Ok(true)
    }

    /// Compare-and-set by value (`None` = absent); numbers compare as f64.
    fn cas_value(
        &self,
        key: &[u8],
        expected: Option<&Prim>,
        new: Option<Prim>,
        w: Write,
    ) -> Result<bool, Full> {
        let (current, version) = self.snapshot(key);
        if current.as_ref() != expected {
            return Ok(false);
        }
        self.compare_and_set(key, version, new, w)
    }

    /// `None` absent; `Some(None)` no expiry; `Some(Some(ms))` ms left (rounded up).
    #[allow(clippy::option_option)] // the three states are exactly `kv.ttl`'s
    fn ttl(&self, key: &[u8]) -> Option<Option<i64>> {
        let now = Instant::now();
        let mut inner = self.lock();
        let e = inner.live(key, now)?;
        Some(e.expires.map(|x| {
            let left = x.saturating_duration_since(now);
            let ms = left.as_millis() + u128::from(left.subsec_nanos() % 1_000_000 != 0);
            i64::try_from(ms).unwrap_or(i64::MAX)
        }))
    }

    fn lookup(&self, key: &[u8]) -> Lookup {
        let mut inner = self.lock();
        let now = Instant::now();
        if let Some(e) = inner.live(key, now) {
            let value = e.value.clone();
            inner.touch(key);
            return Lookup::Fresh(value);
        }
        match inner.map.get(key) {
            Some(e) if matches!(e.freshness(now), Freshness::Stale) => {
                Lookup::Stale(e.value.clone())
            }
            _ => Lookup::Absent,
        }
    }

    /// `fetch` loaders currently in flight (for tests and a future `/metrics`).
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.lock().flights.len()
    }

    /// The miss path of `fetch`. Under one lock: re-check for a fresh value
    /// (a leader may just have stored it), then join the flight for `key` or
    /// start one. `Inline` means waiting would deadlock on ourselves.
    fn join(self: &Arc<Self>, key: &[u8], owner: &Arc<VmLeads>) -> Join {
        let mut inner = self.lock();
        if let Some(e) = inner.live(key, Instant::now()) {
            let value = e.value.clone();
            inner.touch(key);
            return Join::Hit(value);
        }
        if let Some(f) = inner.flights.get(key) {
            let rx = f.tx.subscribe();
            let chain = leading(self);
            if chain.is_empty() {
                return Join::Wait(rx, None);
            }
            if inner.waits_for(key, &chain) {
                return Join::Inline;
            }
            inner.edge(&chain, key, true);
            return Join::Wait(
                rx,
                Some(WaitGuard {
                    store: self.clone(),
                    key: key.to_vec(),
                    chain,
                }),
            );
        }
        inner.next_flight += 1;
        let id = inner.next_flight;
        let (tx, _) = watch::channel(None);
        owner.0.fetch_add(1, Ordering::AcqRel);
        inner.flights.insert(
            key.to_vec(),
            Flight {
                id,
                tx,
                waits: HashMap::new(),
                owner: owner.clone(),
            },
        );
        Join::Lead(LeadGuard {
            store: self.clone(),
            key: key.to_vec(),
            id,
            done: false,
        })
    }

    /// Lock even if a panic poisoned the mutex: guards run during unwinding.
    fn lock_for_drop(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// How a `fetch` miss proceeds.
enum Join {
    /// A leader stored the value between the first lookup and the join.
    Hit(Prim),
    /// Run the loader for everyone; the guard ends the flight.
    Lead(LeadGuard),
    /// Wait for the leader's outcome. The guard (only for a caller inside
    /// another loader) holds the wait edges used for deadlock detection.
    Wait(watch::Receiver<Outcome>, Option<WaitGuard>),
    /// Waiting would deadlock (this key's leader is waiting on us): run the
    /// loader without single-flight, as `fetch` did before it had any.
    Inline,
}

/// Owns a flight. [`LeadGuard::finish`] publishes an outcome; dropping it
/// unfinished (the leader timed out, was cancelled, or panicked) removes the
/// flight without one, which wakes the waiters to hand leadership off.
struct LeadGuard {
    store: Arc<StateStore>,
    key: Vec<u8>,
    id: u64,
    done: bool,
}

impl LeadGuard {
    /// Store a loaded value (unless a write landed meanwhile; one that cannot
    /// fit goes uncached), then publish `shared` and end the flight, all under
    /// one lock: a caller arriving later hits.
    fn finish(mut self, shared: Shared, w: Write) {
        self.done = true;
        let store = self.store.clone();
        let mut inner = store.lock();
        let now = Instant::now();
        if let Shared::Value(Some(p)) = &shared
            && inner.version(&self.key, now) == 0
        {
            // Version 0 = still absent or stale, as when we looked: a write
            // that landed meanwhile (a counter, an inline fetch) wins.
            let _ = inner.put(self.key.clone(), Some(p.clone()), w, now, store.limits);
        }
        if let Some(f) = take_flight(&mut inner, &self.key, self.id) {
            f.tx.send_replace(Some(Arc::new(shared)));
        }
    }
}

impl Drop for LeadGuard {
    fn drop(&mut self) {
        if !self.done {
            // Dropping the sender unsent closes the channel: no outcome.
            let flight = take_flight(&mut self.store.lock_for_drop(), &self.key, self.id);
            drop(flight);
        }
    }
}

fn take_flight(inner: &mut Inner, key: &[u8], id: u64) -> Option<Flight> {
    if inner.flights.get(key).is_some_and(|f| f.id == id) {
        remove_flight(inner, key)
    } else {
        None
    }
}

/// The only place a flight leaves the table, so its owner's count stays exact.
fn remove_flight(inner: &mut Inner, key: &[u8]) -> Option<Flight> {
    let f = inner.flights.remove(key)?;
    f.owner.0.fetch_sub(1, Ordering::AcqRel);
    Some(f)
}

/// Removes a waiter's deadlock-detection edges when it stops waiting.
struct WaitGuard {
    store: Arc<StateStore>,
    key: Vec<u8>,
    chain: Vec<(Vec<u8>, u64)>,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        self.store
            .lock_for_drop()
            .edge(&self.chain, &self.key, false);
    }
}

/// One flight the current task is leading; the chain runs outward through
/// `parent` to the outermost `fetch` whose loader this task is inside.
struct Link {
    /// The `StateStore` the flight belongs to (its address).
    store: usize,
    key: Vec<u8>,
    id: u64,
    parent: Option<Arc<Link>>,
}

tokio::task_local! {
    /// The innermost flight whose loader is running in this task. A task
    /// local, not a thread local: it follows the loader's future, so it is
    /// seen by everything the loader awaits (nested `fetch`es, `lur.async`
    /// tasks it starts) and by nothing else, e.g. a sibling `lur.async` task.
    static LEADING: Option<Arc<Link>>;
}

fn current_link() -> Option<Arc<Link>> {
    LEADING.try_with(Clone::clone).ok().flatten()
}

/// The flights of `store` this task is leading, innermost first.
fn leading(store: &Arc<StateStore>) -> Vec<(Vec<u8>, u64)> {
    let addr = Arc::as_ptr(store).addr();
    let mut chain = Vec::new();
    let head = current_link();
    let mut link = head.as_deref();
    while let Some(l) = link {
        if l.store == addr {
            chain.push((l.key.clone(), l.id));
        }
        link = l.parent.as_deref();
    }
    chain
}

thread_local! {
    /// Set during an `update` transform so re-entrant `lur.state` calls error.
    static IN_UPDATE: Cell<bool> = const { Cell::new(false) };
}

fn reject_reentry() -> mlua::Result<()> {
    if IN_UPDATE.with(Cell::get) {
        return Err(mlua::Error::RuntimeError(
            "lur.state cannot be re-entered from inside update()".into(),
        ));
    }
    Ok(())
}

fn full_err(fname: &str) -> mlua::Error {
    mlua::Error::RuntimeError(format!(
        "{fname}: state is full and nothing is evictable \
         (--state-max-entries / --state-max-bytes; mark cache entries `evictable = true`)"
    ))
}

fn to_lua(lua: &Lua, p: Option<Prim>) -> mlua::Result<Value> {
    Ok(match p {
        None => Value::Nil,
        Some(Prim::Bool(b)) => Value::Boolean(b),
        Some(Prim::Num(n)) => Value::Number(n),
        Some(Prim::Str(s)) => Value::String(lua.create_string(&s)?),
    })
}

/// Lua value → primitive (nil → delete); non-primitives are rejected.
fn from_lua(value: &Value) -> mlua::Result<Option<Prim>> {
    Ok(match value {
        Value::Nil => None,
        Value::Boolean(b) => Some(Prim::Bool(*b)),
        Value::Integer(i) => Some(Prim::Num(*i as f64)),
        Value::Number(n) => Some(Prim::Num(*n)),
        Value::String(s) => Some(Prim::Str(s.as_bytes().to_vec())),
        _ => {
            return Err(mlua::Error::RuntimeError(
                "lur.state stores only nil/boolean/number/string (lur.json.encode tables yourself)"
                    .into(),
            ));
        }
    })
}

/// A positive whole number of milliseconds under `opts.<name>`.
fn ms_field(opts: &Table, name: &str, fname: &str) -> mlua::Result<Option<i64>> {
    let bad =
        || mlua::Error::RuntimeError(format!("{fname}: opts.{name} must be a positive integer"));
    let ms = match opts.get::<Value>(name)? {
        Value::Nil => return Ok(None),
        Value::Integer(i) => i,
        Value::Number(f) => argcheck::whole_f64_to_i64(f).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    if ms <= 0 {
        return Err(bad());
    }
    Ok(Some(ms))
}

fn bool_field(opts: &Table, name: &str, fname: &str) -> mlua::Result<Option<bool>> {
    match opts.get::<Value>(name)? {
        Value::Nil => Ok(None),
        Value::Boolean(b) => Ok(Some(b)),
        _ => Err(mlua::Error::RuntimeError(format!(
            "{fname}: opts.{name} must be a boolean"
        ))),
    }
}

fn opts_table<'a>(value: &'a Value, fname: &str) -> mlua::Result<Option<&'a Table>> {
    match value {
        Value::Nil => Ok(None),
        Value::Table(t) => Ok(Some(t)),
        other => Err(mlua::Error::RuntimeError(format!(
            "{fname}: opts must be a table, got {}",
            other.type_name()
        ))),
    }
}

/// How a call without an explicit `ttl_ms` treats the existing expiry.
#[derive(Clone, Copy)]
enum NoTtl {
    /// `set`/`add`: clear it.
    Clear,
    /// `cas`/`update`/`incr`/`decr`: keep it.
    Keep,
}

/// `{ ttl_ms?, evictable?, renew_ttl? }` as the last argument of a write.
/// `counter` enables `renew_ttl` and the `incr` fixed-window rule.
fn write_opts(value: &Value, fname: &str, no_ttl: NoTtl, counter: bool) -> mlua::Result<Write> {
    let mut w = Write {
        expiry: match no_ttl {
            NoTtl::Clear => Expiry::Clear,
            NoTtl::Keep => Expiry::Keep,
        },
        evictable: matches!(no_ttl, NoTtl::Clear).then_some(false),
        stale_ms: None,
    };
    let Some(opts) = opts_table(value, fname)? else {
        return Ok(w);
    };
    let ttl = ms_field(opts, "ttl_ms", fname)?;
    if let Some(e) = bool_field(opts, "evictable", fname)? {
        w.evictable = Some(e);
    }
    if counter {
        let renew = bool_field(opts, "renew_ttl", fname)?.unwrap_or(false);
        match (ttl, renew) {
            (Some(ms), true) => w.expiry = Expiry::After(ms),
            (Some(ms), false) => w.expiry = Expiry::AfterIfUnset(ms),
            (None, true) => {
                return Err(mlua::Error::RuntimeError(format!(
                    "{fname}: opts.renew_ttl requires opts.ttl_ms"
                )));
            }
            (None, false) => {}
        }
    } else if let Some(ms) = ttl {
        w.expiry = Expiry::After(ms);
    }
    Ok(w)
}

/// Run a `fetch` loader. As the leader (`guard`), publish the outcome to the
/// waiters, unless this VM hit its deadline: then there is none, and dropping
/// the guard hands off. Without a guard (a deadlocking wait was refused), just
/// load and store as a plain cache would.
async fn load(
    lua: &Lua,
    s: &Arc<StateStore>,
    key: &[u8],
    func: &mlua::Function,
    w: Write,
    guard: Option<LeadGuard>,
) -> mlua::Result<(Value, &'static str)> {
    let result = match &guard {
        Some(g) => {
            let link = Link {
                store: Arc::as_ptr(s).addr(),
                key: key.to_vec(),
                id: g.id,
                parent: current_link(),
            };
            LEADING
                .scope(Some(Arc::new(link)), func.call_async::<Value>(()))
                .await
        }
        None => func.call_async::<Value>(()).await,
    };
    let e = match result {
        Ok(v) => {
            s.misses.fetch_add(1, Ordering::Relaxed);
            let p = match from_lua(&v) {
                Ok(p) => p,
                Err(e) => {
                    if let Some(g) = guard {
                        g.finish(Shared::Rejected(e.clone()), w);
                    }
                    return Err(e);
                }
            };
            match guard {
                Some(g) => g.finish(Shared::Value(p), w),
                // A value that cannot fit goes uncached.
                None => {
                    if let Some(p) = p {
                        let _ = s.compare_and_set(key, 0, Some(p), w);
                    }
                }
            }
            return Ok((v, "miss"));
        }
        Err(e) => e,
    };
    if let Some(g) = guard {
        if crate::runtime::deadline_passed(lua) {
            // Our own timeout, not the loader's verdict: no outcome.
            drop(g);
            return Err(e);
        }
        // A leader's OOM is the loader failing, not each waiter's own limit:
        // share it as plain text so waiters are not classified as OOM.
        let shared = if crate::runtime::is_memory_error(&e) {
            mlua::Error::RuntimeError(e.to_string())
        } else {
            e.clone()
        };
        g.finish(Shared::Raised(shared), w);
    }
    fallback(lua, s, key, e)
}

/// A waiter's result from its leader's outcome.
fn share(
    lua: &Lua,
    s: &StateStore,
    key: &[u8],
    shared: &Shared,
) -> mlua::Result<(Value, &'static str)> {
    match shared {
        Shared::Value(p) => {
            s.hits.fetch_add(1, Ordering::Relaxed);
            Ok((to_lua(lua, p.clone())?, "hit"))
        }
        Shared::Raised(e) => fallback(lua, s, key, e.clone()),
        Shared::Rejected(e) => Err(e.clone()),
    }
}

/// The loader raised `e`: serve a value another caller refreshed meanwhile, or
/// a stale one, else raise.
fn fallback(
    lua: &Lua,
    s: &StateStore,
    key: &[u8],
    e: mlua::Error,
) -> mlua::Result<(Value, &'static str)> {
    match s.lookup(key) {
        Lookup::Fresh(p) => {
            s.hits.fetch_add(1, Ordering::Relaxed);
            Ok((to_lua(lua, Some(p))?, "hit"))
        }
        Lookup::Stale(p) => {
            s.stale.fetch_add(1, Ordering::Relaxed);
            Ok((to_lua(lua, Some(p))?, "stale"))
        }
        Lookup::Absent => Err(e),
    }
}

/// Install `lur.state` backed by the shared `store`.
pub fn install(lua: &Lua, lur: &Table, store: Arc<StateStore>) -> Result<(), RunError> {
    let state = lua.create_table().map_err(RunError::Init)?;

    let s = store.clone();
    let get = lua
        .create_function(move |lua, key: Value| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.get", 1, "string")?;
            reject_reentry()?;
            let found = s.get(&key.as_bytes());
            let counter = if found.is_some() { &s.hits } else { &s.misses };
            counter.fetch_add(1, Ordering::Relaxed);
            to_lua(lua, found)
        })
        .map_err(RunError::Init)?;
    state.set("get", get).map_err(RunError::Init)?;

    let s = store.clone();
    let set = lua
        .create_function(move |lua, (key, value, opts): (Value, Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.set", 1, "string")?;
            reject_reentry()?;
            let w = write_opts(&opts, "lur.state.set", NoTtl::Clear, false)?;
            s.set(key.as_bytes().to_vec(), from_lua(&value)?, w)
                .map_err(|Full| full_err("lur.state.set"))
        })
        .map_err(RunError::Init)?;
    state.set("set", set).map_err(RunError::Init)?;

    for (name, negate) in [("incr", false), ("decr", true)] {
        let s = store.clone();
        let fname = format!("lur.state.{name}");
        let f = lua
            .create_function(move |lua, (key, n, opts): (Value, Value, Value)| {
                let key: mlua::LuaString = argcheck::arg(lua, key, &fname, 1, "string")?;
                let n = argcheck::integer_arg(n, &fname, 2)?;
                reject_reentry()?;
                let w = write_opts(&opts, &fname, NoTtl::Keep, true)?;
                let step = n.unwrap_or(1);
                let delta = if negate {
                    step.checked_neg().ok_or_else(|| {
                        mlua::Error::RuntimeError(format!("{fname}: step too large"))
                    })?
                } else {
                    step
                };
                s.incr(key.as_bytes().to_vec(), delta, w)
                    .map_err(|e| match e {
                        IncrError::NotInteger => mlua::Error::RuntimeError(format!(
                            "{fname}: existing value is not an integer"
                        )),
                        IncrError::Overflow => {
                            mlua::Error::RuntimeError(format!("{fname}: counter overflow"))
                        }
                        IncrError::Full => full_err(&fname),
                    })
            })
            .map_err(RunError::Init)?;
        state.set(name, f).map_err(RunError::Init)?;
    }

    let s = store.clone();
    let update = lua
        .create_function(move |lua, (key, func, opts): (Value, Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.update", 1, "string")?;
            let func: mlua::Function = argcheck::arg(lua, func, "lur.state.update", 2, "function")?;
            reject_reentry()?;
            let w = write_opts(&opts, "lur.state.update", NoTtl::Keep, false)?;
            let key = key.as_bytes().to_vec();
            loop {
                let (old, version) = s.snapshot(&key);
                let old_lua = to_lua(lua, old)?;
                // No host lock is held while the transform runs.
                IN_UPDATE.with(|f| f.set(true));
                let result = func.call::<Value>(old_lua);
                IN_UPDATE.with(|f| f.set(false));
                let new_lua = result?;
                let new = from_lua(&new_lua)?;
                if s.compare_and_set(&key, version, new, w)
                    .map_err(|Full| full_err("lur.state.update"))?
                {
                    return Ok(new_lua);
                }
                // Lost a race; retry from a fresh snapshot.
            }
        })
        .map_err(RunError::Init)?;
    state.set("update", update).map_err(RunError::Init)?;

    let s = store.clone();
    let cas = lua
        .create_function(
            move |lua, (key, expected, new, opts): (Value, Value, Value, Value)| {
                let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.cas", 1, "string")?;
                reject_reentry()?;
                let w = write_opts(&opts, "lur.state.cas", NoTtl::Keep, false)?;
                let expected_prim = from_lua(&expected)?;
                let new_prim = from_lua(&new)?;
                s.cas_value(&key.as_bytes(), expected_prim.as_ref(), new_prim, w)
                    .map_err(|Full| full_err("lur.state.cas"))
            },
        )
        .map_err(RunError::Init)?;
    state.set("cas", cas).map_err(RunError::Init)?;

    let s = store.clone();
    let add = lua
        .create_function(move |lua, (key, value, opts): (Value, Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.add", 1, "string")?;
            reject_reentry()?;
            let w = write_opts(&opts, "lur.state.add", NoTtl::Clear, false)?;
            let new_prim = from_lua(&value)?;
            s.cas_value(&key.as_bytes(), None, new_prim, w)
                .map_err(|Full| full_err("lur.state.add"))
        })
        .map_err(RunError::Init)?;
    state.set("add", add).map_err(RunError::Init)?;

    // `ttl(key) -> ms, exists`, the same shape as `lur.kv.ttl`.
    let s = store.clone();
    let ttl = lua
        .create_function(move |lua, key: Value| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.ttl", 1, "string")?;
            reject_reentry()?;
            Ok(match s.ttl(&key.as_bytes()) {
                None => (Value::Nil, false),
                Some(None) => (Value::Nil, true),
                Some(Some(ms)) => (Value::Integer(ms), true),
            })
        })
        .map_err(RunError::Init)?;
    state.set("ttl", ttl).map_err(RunError::Init)?;

    // `fetch(key, { ttl_ms, stale_ms? }, fn) -> value, "hit" | "miss" | "stale"`.
    // `fn` runs with no lock held and may await I/O; it is not retried.
    let s = store.clone();
    let leads = Arc::new(VmLeads::default());
    lua.set_app_data(VmFlights {
        store: store.clone(),
        leads: leads.clone(),
    });
    let fetch = lua
        .create_async_function(move |lua, (key, opts, func): (Value, Value, Value)| {
            let s = s.clone();
            let leads = leads.clone();
            async move {
                const F: &str = "lur.state.fetch";
                let key: mlua::LuaString = argcheck::arg(&lua, key, F, 1, "string")?;
                let key = key.as_bytes().to_vec();
                let Some(opts) = opts_table(&opts, F)? else {
                    return Err(mlua::Error::RuntimeError(format!(
                        "{F}: argument #2 must be a table such as {{ ttl_ms = 60000 }}"
                    )));
                };
                let ttl_ms = ms_field(opts, "ttl_ms", F)?.ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("{F}: opts.ttl_ms is required"))
                })?;
                let stale_ms = ms_field(opts, "stale_ms", F)?;
                let func: mlua::Function = argcheck::arg(&lua, func, F, 3, "function")?;
                reject_reentry()?;

                if let Lookup::Fresh(p) = s.lookup(&key) {
                    s.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok((to_lua(&lua, Some(p))?, "hit"));
                }
                let w = Write {
                    expiry: Expiry::After(ttl_ms),
                    evictable: Some(true),
                    stale_ms,
                };
                // Single-flight: lead, or wait for the leader and share its
                // outcome. A leader that ends without one hands off: every
                // waiter wakes and goes round again, and the first to re-join
                // leads.
                loop {
                    match s.join(&key, &leads) {
                        Join::Hit(p) => {
                            s.hits.fetch_add(1, Ordering::Relaxed);
                            return Ok((to_lua(&lua, Some(p))?, "hit"));
                        }
                        Join::Lead(guard) => {
                            return load(&lua, &s, &key, &func, w, Some(guard)).await;
                        }
                        Join::Inline => return load(&lua, &s, &key, &func, w, None).await,
                        Join::Wait(mut rx, _edges) => {
                            // Bound only by this caller's own timeout.
                            let outcome = rx
                                .wait_for(Option::is_some)
                                .await
                                .ok()
                                .and_then(|o| o.clone());
                            if let Some(shared) = outcome {
                                return share(&lua, &s, &key, &shared);
                            }
                        }
                    }
                }
            }
        })
        .map_err(RunError::Init)?;
    state.set("fetch", fetch).map_err(RunError::Init)?;

    lur.set("state", state).map_err(RunError::Init)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEEP: Write = Write {
        expiry: Expiry::Keep,
        evictable: None,
        stale_ms: None,
    };

    fn cache() -> Write {
        Write {
            expiry: Expiry::After(60_000),
            evictable: Some(true),
            stale_ms: None,
        }
    }

    fn put(store: &StateStore, key: &str, n: f64, w: Write) -> Result<(), Full> {
        store.set(key.as_bytes().to_vec(), Some(Prim::Num(n)), w)
    }

    fn present(store: &StateStore, key: &str) -> bool {
        store.snapshot(key.as_bytes()).0.is_some()
    }

    #[test]
    fn compare_and_set_respects_versions() {
        let store = StateStore::default();
        let (_, v0) = store.snapshot(b"k");
        assert_eq!(v0, 0, "absent key starts at version 0");

        assert!(
            store
                .compare_and_set(b"k", 0, Some(Prim::Num(1.0)), KEEP)
                .unwrap()
        );
        let (_, v1) = store.snapshot(b"k");
        assert!(v1 > 0);

        assert!(
            !store
                .compare_and_set(b"k", 0, Some(Prim::Num(9.0)), KEEP)
                .unwrap()
        );
        assert!(
            store
                .compare_and_set(b"k", v1, Some(Prim::Num(2.0)), KEEP)
                .unwrap()
        );
        assert!(
            store.snapshot(b"k").1 > v1,
            "every write gets a newer version"
        );
    }

    #[test]
    fn recreating_a_deleted_key_never_reuses_an_old_version() {
        let store = StateStore::default();
        put(&store, "k", 5.0, Write::default()).unwrap();
        let (_, held) = store.snapshot(b"k");
        store.set(b"k".to_vec(), None, Write::default()).unwrap(); // delete
        assert!(store.get(b"k").is_none());
        put(&store, "k", 5.0, Write::default()).unwrap(); // same value again
        assert!(
            !store
                .compare_and_set(b"k", held, Some(Prim::Num(6.0)), KEEP)
                .unwrap(),
            "a version held across delete + recreate must be stale"
        );
    }

    #[test]
    fn deleting_frees_the_entry() {
        let store = StateStore::default();
        for i in 0..100u32 {
            let key = i.to_be_bytes().to_vec();
            store
                .set(key.clone(), Some(Prim::Bool(true)), cache())
                .unwrap();
            store.set(key, None, Write::default()).unwrap();
        }
        let inner = store.lock();
        assert!(inner.map.is_empty(), "no tombstones are kept");
        assert!(inner.order.is_empty());
        assert_eq!(inner.bytes, 0);
    }

    #[test]
    fn an_absent_key_stays_at_version_zero() {
        let store = StateStore::default();
        store.set(b"k".to_vec(), None, Write::default()).unwrap(); // no-op
        assert_eq!(store.snapshot(b"k"), (None, 0));
        assert!(
            store
                .compare_and_set(b"k", 0, Some(Prim::Bool(true)), KEEP)
                .unwrap()
        );
    }

    #[test]
    fn incr_rejects_two_pow_63_instead_of_saturating() {
        let store = StateStore::default();
        put(&store, "k", 2f64.powi(63), Write::default()).unwrap();
        assert!(matches!(
            store.incr(b"k".to_vec(), -1, KEEP),
            Err(IncrError::NotInteger)
        ));
    }

    #[test]
    fn an_expired_entry_reads_as_absent_with_version_zero() {
        let store = StateStore::default();
        let w = Write {
            expiry: Expiry::After(5),
            ..Write::default()
        };
        put(&store, "k", 1.0, w).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(store.snapshot(b"k"), (None, 0));
        assert!(store.ttl(b"k").is_none());
        assert!(store.lock().map.is_empty(), "dead entry is reaped on read");
    }

    #[test]
    fn set_clears_expiry_but_cas_and_incr_keep_it() {
        let store = StateStore::default();
        let ttl = Write {
            expiry: Expiry::After(60_000),
            ..Write::default()
        };
        put(&store, "k", 1.0, ttl).unwrap();
        assert!(matches!(store.ttl(b"k"), Some(Some(_))));
        store.incr(b"k".to_vec(), 1, KEEP).unwrap();
        assert!(matches!(store.ttl(b"k"), Some(Some(_))), "incr keeps it");
        let (_, v) = store.snapshot(b"k");
        store
            .compare_and_set(b"k", v, Some(Prim::Num(9.0)), KEEP)
            .unwrap();
        assert!(matches!(store.ttl(b"k"), Some(Some(_))), "cas keeps it");
        put(&store, "k", 3.0, Write::default()).unwrap();
        assert_eq!(store.ttl(b"k"), Some(None), "set clears it");
    }

    #[test]
    fn incr_ttl_applies_only_when_unset_unless_renewed() {
        let store = StateStore::default();
        let nx = Write {
            expiry: Expiry::AfterIfUnset(60_000),
            ..Write::default()
        };
        store.incr(b"c".to_vec(), 1, nx).unwrap();
        let Some(Some(first)) = store.ttl(b"c") else {
            panic!("expected a ttl");
        };
        std::thread::sleep(Duration::from_millis(30));
        let longer = Write {
            expiry: Expiry::AfterIfUnset(600_000),
            ..Write::default()
        };
        store.incr(b"c".to_vec(), 1, longer).unwrap();
        let Some(Some(second)) = store.ttl(b"c") else {
            panic!("expected a ttl");
        };
        assert!(second < first, "fixed window is not extended");
        let renew = Write {
            expiry: Expiry::After(600_000),
            ..Write::default()
        };
        store.incr(b"c".to_vec(), 1, renew).unwrap();
        let Some(Some(third)) = store.ttl(b"c") else {
            panic!("expected a ttl");
        };
        assert!(third > second, "renew_ttl resets the window");
    }

    #[test]
    fn stale_entries_are_invisible_except_to_lookup() {
        let store = StateStore::default();
        let w = Write {
            expiry: Expiry::After(5),
            evictable: Some(true),
            stale_ms: Some(60_000),
        };
        put(&store, "k", 1.0, w).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(store.get(b"k").is_none());
        assert_eq!(store.snapshot(b"k"), (None, 0));
        assert!(matches!(store.lookup(b"k"), Lookup::Stale(Prim::Num(_))));
        // add treats a stale key as absent and replaces it.
        assert!(
            store
                .cas_value(b"k", None, Some(Prim::Num(2.0)), cache())
                .unwrap()
        );
        assert!(matches!(
            store.lookup(b"k"),
            Lookup::Fresh(p) if p == Prim::Num(2.0)
        ));
    }

    #[test]
    fn a_write_that_keeps_the_expiry_keeps_the_stale_window() {
        let store = StateStore::default();
        let w = Write {
            expiry: Expiry::After(5),
            evictable: Some(true),
            stale_ms: Some(60_000),
        };
        put(&store, "k", 1.0, w).unwrap();
        let (_, v) = store.snapshot(b"k");
        assert!(
            store
                .compare_and_set(b"k", v, Some(Prim::Num(2.0)), KEEP)
                .unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            matches!(store.lookup(b"k"), Lookup::Stale(p) if p == Prim::Num(2.0)),
            "update/cas must not drop the fallback window"
        );
    }

    #[test]
    fn stale_entries_are_evicted_before_fresh_ones() {
        let store = StateStore::with_limits(Limits {
            max_entries: 2,
            max_bytes: 0,
        });
        let stale = Write {
            expiry: Expiry::After(5),
            evictable: Some(true),
            stale_ms: Some(60_000),
        };
        put(&store, "fresh", 1.0, cache()).unwrap();
        put(&store, "stale", 1.0, stale).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        put(&store, "new", 1.0, cache()).unwrap();
        assert!(
            present(&store, "fresh"),
            "a live entry outlives a stale one"
        );
        assert!(present(&store, "new"));
        assert!(matches!(store.lookup(b"stale"), Lookup::Absent));
        assert_eq!(store.stats().evictions, 0);
    }

    #[test]
    fn a_full_store_with_nothing_expiring_does_not_rescan() {
        let store = StateStore::with_limits(Limits {
            max_entries: 100,
            max_bytes: 0,
        });
        for i in 0..100 {
            put(&store, &format!("k{i}"), 1.0, cache()).unwrap();
        }
        let before = store.lock().earliest;
        put(&store, "more", 1.0, cache()).unwrap();
        assert_eq!(store.lock().earliest, before, "no sweep, bound untouched");
        assert_eq!(store.stats().evictions, 1);
    }

    #[test]
    fn stale_entry_dies_after_its_window() {
        let store = StateStore::default();
        let w = Write {
            expiry: Expiry::After(5),
            evictable: Some(true),
            stale_ms: Some(10),
        };
        put(&store, "k", 1.0, w).unwrap();
        std::thread::sleep(Duration::from_millis(40));
        assert!(matches!(store.lookup(b"k"), Lookup::Absent));
    }

    #[test]
    fn full_store_evicts_lru_evictable_and_spares_the_rest() {
        let store = StateStore::with_limits(Limits {
            max_entries: 3,
            max_bytes: 0,
        });
        put(&store, "pin", 1.0, Write::default()).unwrap();
        put(&store, "a", 1.0, cache()).unwrap();
        put(&store, "b", 2.0, cache()).unwrap();
        assert!(store.get(b"a").is_some(), "touch a: b is now oldest");
        put(&store, "c", 3.0, cache()).unwrap();
        assert!(present(&store, "pin"));
        assert!(present(&store, "a"));
        assert!(!present(&store, "b"), "least recently used is evicted");
        assert!(present(&store, "c"));
        assert_eq!(store.stats().evictions, 1);
    }

    #[test]
    fn full_of_unevictable_rejects_new_keys_and_changes_nothing() {
        let store = StateStore::with_limits(Limits {
            max_entries: 2,
            max_bytes: 0,
        });
        put(&store, "x", 1.0, Write::default()).unwrap();
        put(&store, "y", 2.0, Write::default()).unwrap();
        assert!(put(&store, "z", 3.0, Write::default()).is_err());
        assert!(put(&store, "z", 3.0, cache()).is_err());
        assert!(present(&store, "x") && present(&store, "y"));
        // Overwriting an existing key is not growth.
        put(&store, "x", 9.0, Write::default()).unwrap();
        assert!(store.incr(b"n".to_vec(), 1, KEEP).is_err());
    }

    #[test]
    fn failed_write_does_not_evict_what_it_could_not_replace() {
        let store = StateStore::with_limits(Limits {
            max_entries: 3,
            max_bytes: 0,
        });
        put(&store, "p1", 1.0, Write::default()).unwrap();
        put(&store, "p2", 1.0, Write::default()).unwrap();
        put(&store, "e", 1.0, cache()).unwrap();
        // Needs one slot; evicting `e` frees it, so this fits.
        put(&store, "p3", 1.0, Write::default()).unwrap();
        assert!(!present(&store, "e"));
        // Now nothing is evictable; the next write fails and nothing is lost.
        assert!(put(&store, "p4", 1.0, Write::default()).is_err());
        assert!(present(&store, "p1") && present(&store, "p2") && present(&store, "p3"));
    }

    #[test]
    fn byte_cap_counts_key_value_and_overhead() {
        let big = Prim::Str(vec![0; 1000]);
        let store = StateStore::with_limits(Limits {
            max_entries: 0,
            max_bytes: 2 * (1000 + ENTRY_OVERHEAD + 1) + 10,
        });
        store
            .set(b"a".to_vec(), Some(big.clone()), cache())
            .unwrap();
        store
            .set(b"b".to_vec(), Some(big.clone()), cache())
            .unwrap();
        store.set(b"c".to_vec(), Some(big), cache()).unwrap();
        assert!(!present(&store, "a"), "oldest dropped to fit the third");
        assert!(present(&store, "b") && present(&store, "c"));
        let huge = Prim::Str(vec![0; 10_000]);
        assert!(store.set(b"d".to_vec(), Some(huge), cache()).is_err());
        assert!(present(&store, "b") && present(&store, "c"));
    }

    #[test]
    fn expired_entries_are_dropped_before_live_ones_are_evicted() {
        let store = StateStore::with_limits(Limits {
            max_entries: 2,
            max_bytes: 0,
        });
        let short = Write {
            expiry: Expiry::After(5),
            evictable: Some(true),
            stale_ms: None,
        };
        put(&store, "live", 1.0, cache()).unwrap();
        put(&store, "dying", 1.0, short).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        put(&store, "new", 1.0, cache()).unwrap();
        assert!(present(&store, "live"), "the live one is spared");
        assert!(present(&store, "new"));
        assert_eq!(store.stats().evictions, 0, "expiry is not an eviction");
    }
}
