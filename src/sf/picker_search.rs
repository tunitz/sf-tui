//! Debounced, cancellation-aware asynchronous search for the metadata pickers.
//!
//! The app owns one [`PickerSearchEngine`] and feeds it a [`SearchRequest`] whenever a query or
//! the underlying index changes. Ranking runs on Tokio's blocking pool and is capped at one task
//! at a time; results are delivered through a caller-supplied callback so this domain module never
//! needs to know about UI events.
//!
//! Integration in `app.rs`:
//!
//! ```ignore
//! // On a query/index/generation change:
//! self.picker_search.submit(
//!     SearchRequest {
//!         scope: PickerScope::Transfer,
//!         generation,
//!         index_revision,
//!         query,
//!         dataset: Arc::clone(&set),
//!     },
//!     Instant::now(),
//! );
//!
//! // On each UI tick:
//! if self.picker_search.deadline().is_some_and(|deadline| deadline <= Instant::now()) {
//!     let tx = self.event_tx.clone();
//!     self.picker_search.start_due(Instant::now(), move |reply| {
//!         emit_event(tx.clone(), AppEvent::PickerResults(reply));
//!     });
//! }
//!
//! // On mode/org/index changes and shutdown:
//! self.picker_search.invalidate(PickerScope::Transfer);
//! self.picker_search.cancel_all();
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use super::metadata::{SuggestionItem, SuggestionSet};

/// Which picker a search belongs to. Kept separate from `SuggestionKind` so a scope always maps
/// to exactly one UI search state, even when two scopes share a dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickerScope {
    Transfer,
    Tests,
    Create,
}

/// One debounced ranking request. `generation` identifies the edit and `index_revision` identifies
/// the component index the dataset was built from; both are echoed in the reply so a consumer can
/// ignore late work.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub scope: PickerScope,
    pub generation: u64,
    pub index_revision: u64,
    pub query: String,
    pub dataset: Arc<SuggestionSet>,
}

/// Ranked results together with the identity of the request that produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchReply {
    pub scope: PickerScope,
    pub generation: u64,
    pub index_revision: u64,
    pub query: String,
    pub results: Vec<SuggestionItem>,
}

/// The parts of a request that decide whether a submit is a new edit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestKey {
    scope: PickerScope,
    generation: u64,
    index_revision: u64,
    query: String,
}

impl RequestKey {
    fn from_request(request: &SearchRequest) -> Self {
        Self {
            scope: request.scope,
            generation: request.generation,
            index_revision: request.index_revision,
            query: request.query.clone(),
        }
    }
}

#[derive(Debug)]
struct Pending {
    request: SearchRequest,
    deadline: Instant,
}

#[derive(Debug)]
struct InFlight {
    handle: JoinHandle<()>,
    key: RequestKey,
}

/// Owns at most one in-flight ranking task and one replaceable pending request per scope.
///
/// Every submit that changes the effective request bumps a shared validity counter. The worker
/// compares its captured counter before invoking the callback, so replies from an older edit, an
/// invalidated index or a cancelled search are dropped inside the worker rather than reaching the
/// UI. The counter is global on purpose: a mode/org/index change affects every scope's dataset.
#[derive(Debug)]
pub struct PickerSearchEngine {
    debounce: Duration,
    limit: usize,
    pending: HashMap<PickerScope, Pending>,
    in_flight: Option<InFlight>,
    validity: Arc<AtomicU64>,
}

impl PickerSearchEngine {
    pub fn new(debounce: Duration, limit: usize) -> Self {
        Self {
            debounce,
            limit,
            pending: HashMap::new(),
            in_flight: None,
            validity: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Queue a request for its scope. An edit identical to the pending request or to the running
    /// one is ignored; anything else replaces the pending request and restarts the debounce.
    /// Ranking is never performed here.
    pub fn submit(&mut self, request: SearchRequest, now: Instant) {
        let key = RequestKey::from_request(&request);
        let unchanged = self
            .pending
            .get(&request.scope)
            .map(|pending| RequestKey::from_request(&pending.request) == key)
            .or_else(|| self.in_flight.as_ref().map(|entry| entry.key == key))
            .unwrap_or(false);
        if unchanged {
            return;
        }
        self.validity.fetch_add(1, Ordering::SeqCst);
        let deadline = now.checked_add(self.debounce).unwrap_or(now);
        self.pending
            .insert(request.scope, Pending { request, deadline });
    }

    /// The earliest moment pending work becomes eligible to start, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.values().map(|pending| pending.deadline).min()
    }

    /// Start the oldest due request, if no ranking task is running. The callback is invoked at
    /// most once per started request and only while that request is still current.
    pub fn start_due<F>(&mut self, now: Instant, emit: F)
    where
        F: Fn(SearchReply) + Send + 'static,
    {
        self.reap();
        if self.in_flight.is_some() {
            return;
        }
        let Some((&scope, deadline)) = self
            .pending
            .iter()
            .min_by_key(|(_, pending)| pending.deadline)
            .map(|(scope, pending)| (scope, pending.deadline))
        else {
            return;
        };
        if deadline > now {
            return;
        }
        let Some(pending) = self.pending.remove(&scope) else {
            return;
        };
        let epoch = self.validity.load(Ordering::SeqCst);
        let validity = Arc::clone(&self.validity);
        let limit = self.limit;
        let key = RequestKey::from_request(&pending.request);
        let request = pending.request;
        let handle = tokio::spawn(async move {
            let ranked = tokio::task::spawn_blocking(move || rank(&request, limit));
            if let Ok(reply) = ranked.await
                && validity.load(Ordering::SeqCst) == epoch
            {
                emit(reply);
            }
        });
        self.in_flight = Some(InFlight { handle, key });
    }

    /// Reject outstanding work for a scope and abandon its pending request. The shared epoch
    /// also invalidates any running task, including one owned by another scope.
    pub fn invalidate(&mut self, scope: PickerScope) {
        self.validity.fetch_add(1, Ordering::SeqCst);
        self.pending.remove(&scope);
        if self
            .in_flight
            .as_ref()
            .is_some_and(|entry| entry.key.scope == scope)
            && let Some(entry) = self.in_flight.take()
        {
            entry.handle.abort();
        }
    }

    /// Drop everything and abort the running task. Safe to call during shutdown.
    pub fn cancel_all(&mut self) {
        self.validity.fetch_add(1, Ordering::SeqCst);
        self.pending.clear();
        if let Some(entry) = self.in_flight.take() {
            entry.handle.abort();
        }
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight
            .as_ref()
            .is_some_and(|entry| !entry.handle.is_finished())
    }

    pub fn is_busy(&self) -> bool {
        self.is_pending() || self.is_in_flight()
    }

    pub fn pending_scope(&self) -> Option<PickerScope> {
        self.pending.keys().next().copied()
    }

    pub fn pending_query(&self) -> Option<&str> {
        self.pending
            .values()
            .next()
            .map(|pending| pending.request.query.as_str())
    }

    fn reap(&mut self) {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|entry| entry.handle.is_finished())
        {
            self.in_flight = None;
        }
    }
}

impl Drop for PickerSearchEngine {
    fn drop(&mut self) {
        if let Some(entry) = self.in_flight.take() {
            entry.handle.abort();
        }
    }
}

/// Pure ranking step. Kept separate from scheduling so limits and reply identity can be tested
/// without a runtime.
fn rank(request: &SearchRequest, limit: usize) -> SearchReply {
    let mut results = request.dataset.filter(&request.query, limit);
    if results.len() > limit {
        results.truncate(limit);
    }
    SearchReply {
        scope: request.scope,
        generation: request.generation,
        index_revision: request.index_revision,
        query: request.query.clone(),
        results,
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::sf::metadata::SuggestionKind;

    const DEBOUNCE: Duration = Duration::from_millis(90);

    fn dataset() -> Arc<SuggestionSet> {
        Arc::new(SuggestionSet::new(
            SuggestionKind::Transfer,
            vec!["Alpha".to_owned(), "Alpaca".to_owned(), "Beta".to_owned()],
        ))
    }

    fn request(
        scope: PickerScope,
        generation: u64,
        index_revision: u64,
        query: &str,
    ) -> SearchRequest {
        SearchRequest {
            scope,
            generation,
            index_revision,
            query: query.to_owned(),
            dataset: dataset(),
        }
    }

    fn emitter(
        tx: &mpsc::UnboundedSender<SearchReply>,
    ) -> impl Fn(SearchReply) + Send + Clone + 'static {
        let tx = tx.clone();
        move |reply: SearchReply| {
            let _ = tx.send(reply);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn submit_only_queues_work() {
        let mut engine = PickerSearchEngine::new(DEBOUNCE, 20);
        let base = Instant::now();
        let (_tx, mut rx) = mpsc::unbounded_channel::<SearchReply>();

        assert!(!engine.is_pending());
        assert!(!engine.is_in_flight());
        assert_eq!(engine.deadline(), None);

        engine.submit(request(PickerScope::Transfer, 1, 0, "alpha"), base);

        assert!(engine.is_pending());
        assert!(!engine.is_in_flight());
        assert_eq!(engine.deadline(), Some(base + DEBOUNCE));
        assert_eq!(engine.pending_scope(), Some(PickerScope::Transfer));
        assert_eq!(engine.pending_query(), Some("alpha"));
        // No emit callback was given to submit; nothing could have been ranked or delivered.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deadline_controls_when_work_starts() {
        let mut engine = PickerSearchEngine::new(DEBOUNCE, 20);
        let base = Instant::now();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = emitter(&tx);

        engine.submit(request(PickerScope::Transfer, 1, 0, "alpha"), base);
        engine.start_due(base + DEBOUNCE - Duration::from_millis(1), emit.clone());
        assert!(engine.is_pending());
        assert!(!engine.is_in_flight());
        assert_eq!(engine.deadline(), Some(base + DEBOUNCE));

        engine.start_due(base + DEBOUNCE, emit);
        assert!(!engine.is_pending());
        assert_eq!(engine.deadline(), None);
        assert!(engine.is_in_flight());

        let reply = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("search reply")
            .expect("channel open");
        assert_eq!(reply.generation, 1);
        assert_eq!(reply.query, "alpha");
        assert!(!reply.results.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_request_replaces_pending() {
        let mut engine = PickerSearchEngine::new(DEBOUNCE, 20);
        let base = Instant::now();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = emitter(&tx);

        engine.submit(request(PickerScope::Transfer, 1, 0, "alpha"), base);
        engine.submit(
            request(PickerScope::Transfer, 2, 0, "beta"),
            base + Duration::from_millis(10),
        );

        assert_eq!(engine.pending_query(), Some("beta"));
        assert_eq!(
            engine.deadline(),
            Some(base + Duration::from_millis(10) + DEBOUNCE)
        );

        engine.start_due(base + DEBOUNCE, emit.clone());
        assert!(
            engine.is_pending(),
            "the newer request must wait for its deadline"
        );

        let later = base + Duration::from_millis(10) + DEBOUNCE;
        let reply = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                engine.start_due(later, emit.clone());
                if let Ok(reply) = rx.try_recv() {
                    return reply;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("search reply");
        assert_eq!(reply.generation, 2);
        assert_eq!(reply.query, "beta");
        assert!(
            rx.try_recv().is_err(),
            "the replaced request must not be delivered"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_in_flight_reply_is_discarded() {
        let mut engine = PickerSearchEngine::new(DEBOUNCE, 20);
        let base = Instant::now();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = emitter(&tx);

        engine.submit(request(PickerScope::Transfer, 1, 0, "alpha"), base);
        // No `.await` before the next submit: on a current-thread runtime the worker is only
        // queued here, so the validity bump deterministically makes its reply stale.
        engine.start_due(base + DEBOUNCE, emit.clone());
        engine.submit(
            request(PickerScope::Transfer, 2, 0, "beta"),
            base + DEBOUNCE,
        );

        let later = base + DEBOUNCE + DEBOUNCE;
        let reply = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                engine.start_due(later, emit.clone());
                if let Ok(reply) = rx.try_recv() {
                    return reply;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fresh search reply");
        assert_eq!(reply.generation, 2);
        assert_eq!(reply.query, "beta");
        assert!(rx.try_recv().is_err(), "stale generation was delivered");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rapid_edits_coalesce_into_one_reply() {
        let mut engine = PickerSearchEngine::new(DEBOUNCE, 20);
        let base = Instant::now();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = emitter(&tx);

        engine.submit(request(PickerScope::Transfer, 1, 0, "alpha"), base);
        engine.start_due(base + DEBOUNCE, emit.clone());
        engine.submit(
            request(PickerScope::Transfer, 2, 0, "alph"),
            base + DEBOUNCE,
        );
        engine.submit(request(PickerScope::Transfer, 3, 0, "alp"), base + DEBOUNCE);

        let later = base + DEBOUNCE + DEBOUNCE;
        let replies: Vec<SearchReply> = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                engine.start_due(later, emit.clone());
                let received: Vec<SearchReply> =
                    std::iter::from_fn(|| rx.try_recv().ok()).collect();
                if !received.is_empty() && !engine.is_busy() {
                    return received;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("search replies");
        assert_eq!(replies.len(), 1, "in-flight and pending work must coalesce");
        assert_eq!(replies[0].query, "alp");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalidate_drops_pending_and_running_work() {
        let mut engine = PickerSearchEngine::new(DEBOUNCE, 20);
        let base = Instant::now();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = emitter(&tx);

        engine.submit(request(PickerScope::Tests, 7, 0, "alpha"), base);
        engine.start_due(base + DEBOUNCE, emit.clone());
        engine.invalidate(PickerScope::Tests);

        assert!(!engine.is_pending());
        assert!(!engine.is_in_flight());
        assert_eq!(engine.deadline(), None);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(rx.try_recv().is_err(), "invalidated work was delivered");

        engine.cancel_all();
        engine.submit(request(PickerScope::Create, 8, 0, "alpha"), base);
        engine.cancel_all();
        assert!(!engine.is_busy());
        assert_eq!(engine.deadline(), None);
    }

    #[test]
    fn rank_echoes_request_identity_and_bounds_results() {
        let request = request(PickerScope::Create, 9, 3, "a");
        let reply = rank(&request, 2);
        assert_eq!(reply.scope, PickerScope::Create);
        assert_eq!(reply.generation, 9);
        assert_eq!(reply.index_revision, 3);
        assert_eq!(reply.query, "a");
        assert_eq!(reply.results.len(), 2);

        assert!(rank(&request, 0).results.is_empty());
        assert!(rank(&request, 20).results.len() <= 20);
    }
}
