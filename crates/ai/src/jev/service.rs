//! JevDecisionService: turns decision items into batched `/v1/systemone`
//! calls, reuses cached decisions, and gates every answer.
//!
//! A batch shares one `state = {"items": [...]}`; every question is scoped
//! to its item by rewriting `{item}` in its instructions to `items[j]` and
//! prefixing its id with `i<j>.`. The API answers questions independently,
//! so decisions are cached per item, not per batch (research §7.3).

use super::decided::{decide_gate, probs_of, summarize, Decided, ESCAPE_VALUES};
use crate::cost::{estimate_tokens, Pricing};
use crate::providers::jev::{JevAnswer, JevQuestion, JevResponse, JevTransport};
use crate::providers::AiError;
use kadr_core::{CancelToken, DecisionId};
use kadr_project::{now_ms, DecisionKind, Gate, StoredDecision};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// Per-call overhead the API bills regardless of content (research §3).
const CALL_OVERHEAD_TOKENS: u64 = 260;
const MAX_ITEMS: usize = 40;
/// Below the API's 32k (state + longest question) and 64k (request) limits,
/// leaving room for our token estimate being low.
const MAX_STATE_TOKENS: u64 = 27_000;
const MAX_TOTAL_TOKENS: u64 = 54_000;
/// Probability moves smaller than this keep the previous gate.
const HYSTERESIS: f32 = 0.05;

/// One thing to decide.
#[derive(Clone, Debug)]
pub struct JevItem {
    pub kind: DecisionKind,
    pub subject: String,
    pub prompt_version: &'static str,
    /// Item-local state; questions refer to it as `{item}`.
    pub state: serde_json::Value,
    /// Local question ids ("usability", "best_cam", …).
    pub questions: BTreeMap<String, JevQuestion>,
    /// The question whose answer becomes the decision's value.
    pub primary: String,
    /// Bucketed features kept with the decision (precedents, audits).
    pub features: serde_json::Value,
    /// A local detector agrees with a destructive verdict.
    pub extra_ok: bool,
}

impl JevItem {
    fn tokens(&self) -> (u64, u64) {
        let state = estimate_tokens(&self.state.to_string());
        let q = self.questions.values().map(|q| estimate_tokens(&serde_json::to_string(q).unwrap_or_default())).sum();
        (state, q)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JevEstimate {
    pub calls: usize,
    pub input_tokens: u64,
    pub usd: f64,
    pub cached: usize,
    pub total: usize,
}

pub struct JevDecisionService {
    transport: Arc<dyn JevTransport>,
    model: String,
    pricing: Pricing,
}

impl JevDecisionService {
    pub fn new(transport: Arc<dyn JevTransport>, model: String, pricing: Pricing) -> Self {
        JevDecisionService { transport, model, pricing }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Same model + template + item content ⇒ same key ⇒ no new call.
    pub fn cache_key(&self, item: &JevItem) -> String {
        let canon = json!({"m": self.model, "v": item.prompt_version, "s": item.state, "q": item.questions});
        blake3::hash(canon.to_string().as_bytes()).to_hex().to_string()
    }

    fn cached<'c>(&self, item: &JevItem, cache: &'c [StoredDecision]) -> Option<&'c StoredDecision> {
        let key = self.cache_key(item);
        cache.iter().rev().find(|c| c.key == key)
    }

    /// Greedy packing of `todo` (indices into `items`) into batches.
    fn pack(items: &[JevItem], todo: &[usize]) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = vec![];
        let (mut state, mut total) = (0u64, 0u64);
        for &i in todo {
            let (s, q) = items[i].tokens();
            let fits = out.last().is_some_and(|b| b.len() < MAX_ITEMS && state + s <= MAX_STATE_TOKENS && total + s + q <= MAX_TOTAL_TOKENS);
            if !fits {
                out.push(vec![]);
                (state, total) = (0, 0);
            }
            out.last_mut().unwrap().push(i);
            state += s;
            total += s + q;
        }
        out
    }

    pub fn estimate(&self, items: &[JevItem], cache: &[StoredDecision]) -> JevEstimate {
        let todo: Vec<usize> = (0..items.len()).filter(|&i| self.cached(&items[i], cache).is_none()).collect();
        let batches = Self::pack(items, &todo);
        let input: u64 = batches
            .iter()
            .map(|b| CALL_OVERHEAD_TOKENS + b.iter().map(|&i| { let (s, q) = items[i].tokens(); s + q }).sum::<u64>())
            .sum();
        JevEstimate {
            calls: batches.len(),
            input_tokens: input,
            usd: self.pricing.cost(input, 0),
            cached: items.len() - todo.len(),
            total: items.len(),
        }
    }

    /// Decides every item (cached ones without a call). Returns decisions in
    /// item order plus the billed input / output tokens.
    pub async fn run(&self, items: Vec<JevItem>, cache: &[StoredDecision], cancel: CancelToken) -> Result<(Vec<Decided>, u64, u64), AiError> {
        let mut out: Vec<Option<Decided>> = vec![None; items.len()];
        let mut todo = vec![];
        for (i, it) in items.iter().enumerate() {
            match self.cached(it, cache) {
                Some(c) => out[i] = Some(Decided { stored: c.clone(), from_cache: true, answers: BTreeMap::new() }),
                None => todo.push(i),
            }
        }
        let (mut used_in, mut used_out) = (0u64, 0u64);
        let mut stack: Vec<Vec<usize>> = Self::pack(&items, &todo);
        stack.reverse();
        while let Some(batch) = stack.pop() {
            if cancel.is_cancelled() {
                return Err(AiError::Cancelled);
            }
            let (state, questions) = batch_request(&items, &batch);
            match self.call(&state, &questions, &cancel).await {
                Ok(resp) => {
                    used_in += resp.input_tokens;
                    used_out += resp.output_tokens;
                    for (j, &i) in batch.iter().enumerate() {
                        let mut d = self.decided(&items[i], j, &resp);
                        let old = cache.iter().rev().find(|c| c.subject == d.stored.subject && c.kind == d.stored.kind);
                        apply_hysteresis(old, &mut d.stored);
                        out[i] = Some(d);
                    }
                }
                Err(AiError::TooLarge) if batch.len() > 1 => {
                    let (a, b) = batch.split_at(batch.len() / 2);
                    stack.push(b.to_vec());
                    stack.push(a.to_vec());
                }
                Err(AiError::TooLarge) => {
                    tracing::warn!(subject = %items[batch[0]].subject, "Jev item too large even alone");
                    out[batch[0]] = Some(self.review(&items[batch[0]], "too_large"));
                }
                Err(e) => return Err(e),
            }
        }
        Ok((out.into_iter().map(|d| d.expect("every item decided")).collect(), used_in, used_out))
    }

    async fn call(&self, state: &serde_json::Value, q: &BTreeMap<String, JevQuestion>, cancel: &CancelToken) -> Result<JevResponse, AiError> {
        let fut = self.transport.decide(&self.model, state, q);
        let cancelled = async {
            while !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::select! {
            r = fut => r,
            _ = cancelled => Err(AiError::Cancelled),
        }
    }

    fn stored(&self, item: &JevItem, value: String, probs: BTreeMap<String, f32>, p_max: f32, margin: f32, gate: Gate, model: &str) -> StoredDecision {
        StoredDecision {
            id: DecisionId::new(),
            kind: item.kind,
            key: self.cache_key(item),
            subject: item.subject.clone(),
            value,
            probs,
            p_max,
            margin,
            gate,
            model: model.to_string(),
            prompt_version: item.prompt_version.to_string(),
            at_ms: now_ms(),
            features: item.features.clone(),
            human: None,
        }
    }

    fn review(&self, item: &JevItem, why: &str) -> Decided {
        tracing::debug!(subject = %item.subject, why, "Jev item → review");
        Decided { stored: self.stored(item, "REVIEW".into(), BTreeMap::new(), 0.0, 0.0, Gate::Review, &self.model), from_cache: false, answers: BTreeMap::new() }
    }

    fn decided(&self, item: &JevItem, j: usize, resp: &JevResponse) -> Decided {
        let prefix = format!("i{j}.");
        let answers: BTreeMap<String, JevAnswer> =
            resp.answers.iter().filter_map(|(k, a)| Some((k.strip_prefix(&prefix)?.to_string(), a.clone()))).collect();
        let (Some(q), Some(a)) = (item.questions.get(&item.primary), answers.get(&item.primary)) else {
            return self.review(item, "missing answer");
        };
        let probs = probs_of(q, a);
        let known: f32 = probs.values().sum();
        let foreign_choice = a.choice.as_ref().is_some_and(|c| !probs.contains_key(c));
        let Some((value, p_max, margin)) = summarize(&probs).filter(|_| known >= 0.5 && !foreign_choice) else {
            return self.review(item, "unknown option");
        };
        let gate = if ESCAPE_VALUES.contains(&value.as_str()) { Gate::Review } else { decide_gate(item.kind, &value, p_max, margin, item.extra_ok) };
        Decided { stored: self.stored(item, value, probs, p_max, margin, gate, &resp.model), from_cache: false, answers }
    }
}

fn batch_request(items: &[JevItem], batch: &[usize]) -> (serde_json::Value, BTreeMap<String, JevQuestion>) {
    let mut states = vec![];
    let mut questions = BTreeMap::new();
    for (j, &i) in batch.iter().enumerate() {
        states.push(items[i].state.clone());
        let slot = format!("items[{j}]");
        for (id, q) in &items[i].questions {
            questions.insert(format!("i{j}.{id}"), scoped(q, &slot));
        }
    }
    (json!({ "items": states }), questions)
}

fn scoped(q: &JevQuestion, slot: &str) -> JevQuestion {
    let r = |s: &str| s.replace("{item}", slot);
    match q {
        JevQuestion::Noul { instructions } => JevQuestion::Noul { instructions: r(instructions) },
        JevQuestion::Choice { instructions, criteria } => JevQuestion::Choice { instructions: r(instructions), criteria: criteria.clone() },
        JevQuestion::Score { instructions, criteria } => JevQuestion::Score { instructions: r(instructions), criteria: criteria.clone() },
    }
}

/// Jev isn't deterministic (±0.01–0.03): the same verdict with a nearly
/// equal probability keeps its previous gate so badges don't blink.
pub fn apply_hysteresis(old: Option<&StoredDecision>, new: &mut StoredDecision) {
    if let Some(o) = old {
        if o.value == new.value && (new.p_max - o.p_max).abs() <= HYSTERESIS {
            new.gate = o.gate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::Pricing;
    use crate::jev::decide_gate;
    use crate::providers::jev::{JevAnswer, JevQuestion, JevResponse, JevTransport};
    use crate::providers::{AiError, BoxFuture};
    use kadr_core::CancelToken;
    use kadr_project::{DecisionKind, Gate};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    type Reply = dyn Fn(&BTreeMap<String, JevQuestion>) -> Result<BTreeMap<String, JevAnswer>, AiError> + Send + Sync;

    struct Fake {
        calls: AtomicUsize,
        reply: Box<Reply>,
    }

    impl Fake {
        fn new(
            reply: impl Fn(&BTreeMap<String, JevQuestion>) -> Result<BTreeMap<String, JevAnswer>, AiError> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Fake { calls: AtomicUsize::new(0), reply: Box::new(reply) })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl JevTransport for Fake {
        fn decide<'a>(
            &'a self,
            _model: &'a str,
            _state: &'a serde_json::Value,
            q: &'a BTreeMap<String, JevQuestion>,
        ) -> BoxFuture<'a, Result<JevResponse, AiError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let r = (self.reply)(q);
            Box::pin(async move { Ok(JevResponse { model: "jev-1.13.0".into(), answers: r?, input_tokens: 100, output_tokens: 10 }) })
        }
    }

    fn choice(value: &str, probs: &[(&str, f64)]) -> JevAnswer {
        JevAnswer {
            kind: "choice".into(),
            noul: None,
            choice: Some(value.into()),
            score: None,
            confidence: None,
            probabilities: probs.iter().map(|(k, p)| (k.to_string(), *p)).collect(),
        }
    }

    fn keep_all(q: &BTreeMap<String, JevQuestion>) -> Result<BTreeMap<String, JevAnswer>, AiError> {
        Ok(q.keys().map(|k| (k.clone(), choice("KEEP", &[("KEEP", 0.95), ("DISCARD", 0.02), ("REVIEW", 0.03)]))).collect())
    }

    fn usability_item(n: usize) -> JevItem {
        let criteria = ["KEEP", "DISCARD", "REVIEW"].iter().map(|k| (k.to_string(), json!(k.to_lowercase()))).collect();
        JevItem {
            kind: DecisionKind::ShotUsability,
            subject: format!("s{n}"),
            prompt_version: "test/v1",
            state: json!({"shot": n, "sharpness": "sharp"}),
            questions: BTreeMap::from([(
                "usability".to_string(),
                JevQuestion::Choice { instructions: "Is `{item}` usable?".into(), criteria },
            )]),
            primary: "usability".into(),
            features: json!({"sharpness": "sharp"}),
            extra_ok: false,
        }
    }

    fn service(t: Arc<Fake>) -> JevDecisionService {
        JevDecisionService::new(t, "jev-1.13.0".into(), Pricing { input_per_mtok: 0.042, output_per_mtok: 0.0 })
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn batches_and_caches() {
        let fake = Fake::new(keep_all);
        let svc = service(fake.clone());
        let (d, input, _) = rt().block_on(svc.run((0..50).map(usability_item).collect(), &[], CancelToken::new())).unwrap();
        assert_eq!(d.len(), 50);
        assert_eq!(fake.calls(), 2, "40 + 10 items");
        assert_eq!(input, 200);
        assert!(d.iter().all(|x| x.stored.value == "KEEP" && x.stored.gate == Gate::AutoApply && !x.from_cache));
        assert_eq!(d[7].stored.subject, "s7");
        let cache: Vec<_> = d.into_iter().map(|x| x.stored).collect();
        let est = svc.estimate(&(0..50).map(usability_item).collect::<Vec<_>>(), &cache);
        assert_eq!((est.cached, est.calls), (50, 0));
        let (d2, _, _) = rt().block_on(svc.run((0..50).map(usability_item).collect(), &cache, CancelToken::new())).unwrap();
        assert_eq!(fake.calls(), 2, "everything came from the cache");
        assert!(d2.iter().all(|x| x.from_cache));
    }

    #[test]
    fn questions_are_scoped_to_their_item() {
        let seen = Arc::new(std::sync::Mutex::new(vec![]));
        let s2 = seen.clone();
        let fake = Fake::new(move |q| {
            s2.lock().unwrap().extend(q.iter().map(|(k, v)| (k.clone(), v.instructions().to_string())));
            keep_all(q)
        });
        rt().block_on(service(fake).run((0..3).map(usability_item).collect(), &[], CancelToken::new())).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&("i2.usability".to_string(), "Is `items[2]` usable?".to_string())), "{seen:?}");
    }

    #[test]
    fn missing_or_unknown_answer_becomes_review() {
        let fake = Fake::new(|q| {
            Ok(q.keys().filter(|k| !k.starts_with("i1.")).map(|k| (k.clone(), choice("MAYBE", &[("MAYBE", 0.9), ("KEEP", 0.1)]))).collect())
        });
        let (d, _, _) = rt().block_on(service(fake).run((0..3).map(usability_item).collect(), &[], CancelToken::new())).unwrap();
        assert!(d.iter().all(|x| x.stored.gate == Gate::Review && x.stored.value == "REVIEW"), "{d:?}");
    }

    #[test]
    fn too_large_halves_the_batch() {
        let fake = Fake::new(|q| if q.len() > 10 { Err(AiError::TooLarge) } else { keep_all(q) });
        let (d, _, _) = rt().block_on(service(fake.clone()).run((0..40).map(usability_item).collect(), &[], CancelToken::new())).unwrap();
        assert_eq!(d.len(), 40);
        assert!(d.iter().all(|x| x.stored.value == "KEEP"));
        assert!(fake.calls() >= 4);
    }

    #[test]
    fn cancelled_run_makes_no_calls() {
        let fake = Fake::new(keep_all);
        let cancel = CancelToken::new();
        cancel.cancel();
        let r = rt().block_on(service(fake.clone()).run((0..5).map(usability_item).collect(), &[], cancel));
        assert_eq!(r.unwrap_err(), AiError::Cancelled);
        assert_eq!(fake.calls(), 0);
    }

    #[test]
    fn hysteresis_keeps_gate() {
        let fake = Fake::new(keep_all);
        let (d, _, _) = rt().block_on(service(fake).run(vec![usability_item(0)], &[], CancelToken::new())).unwrap();
        let mut old = d[0].stored.clone();
        old.p_max = 0.91;
        old.gate = Gate::AutoApply;
        let mut new = old.clone();
        new.key = "other".into();
        new.p_max = 0.89;
        new.gate = Gate::Suggest;
        apply_hysteresis(Some(&old), &mut new);
        assert_eq!(new.gate, Gate::AutoApply, "a 0.02 wobble must not flip the badge");
        new.p_max = 0.80;
        new.gate = Gate::Suggest;
        apply_hysteresis(Some(&old), &mut new);
        assert_eq!(new.gate, Gate::Suggest);
    }

    #[test]
    fn gate_thresholds() {
        use DecisionKind::*;
        assert_eq!(decide_gate(ShotUsability, "KEEP", 0.93, 0.6, false), Gate::AutoApply);
        assert_eq!(decide_gate(ShotUsability, "KEEP", 0.75, 0.6, false), Gate::Suggest);
        assert_eq!(decide_gate(ShotUsability, "DISCARD", 0.97, 0.9, false), Gate::Suggest, "local detector disagrees");
        assert_eq!(decide_gate(ShotUsability, "DISCARD", 0.97, 0.9, true), Gate::AutoApply);
        assert_eq!(decide_gate(ShotUsability, "REVIEW", 0.99, 0.9, true), Gate::Review);
        assert_eq!(decide_gate(CameraPick, "CAM2", 0.8, 0.4, false), Gate::AutoApply);
        assert_eq!(decide_gate(CameraPick, "CAM2", 0.6, 0.25, false), Gate::Suggest);
        assert_eq!(decide_gate(CameraPick, "CAM2", 0.4, 0.1, false), Gate::Review);
        assert_eq!(decide_gate(CameraPick, "UNSURE", 0.9, 0.8, false), Gate::Review);
    }
}
