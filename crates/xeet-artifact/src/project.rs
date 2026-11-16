//! Consumer projections: one artifact, served to consumers with different
//! sensory capabilities.
//!
//! Vision is an optional consumer capability, not a prerequisite for
//! understanding an artifact. A **text** consumer (any model or program that
//! reads text) gets every element's meaning as text, its place in the
//! hierarchy, its position and row in the layout, the state, provenance,
//! capabilities and limitations — and an explicit list of what exists *only*
//! in pixels. A **vision** consumer gets the same, plus the view to attach,
//! with the rule that anything it reads from pixels is an inference.
//!
//! Every projection is a pure, deterministic function of the artifact and the
//! [`Request`]: same inputs, same bytes. Element ids are the artifact's ids. A
//! partial projection (a focus element, a depth, a character budget) always
//! keeps the provenance header and says exactly what it left out.
//!
//! No model is involved in producing a projection, and none is needed to read
//! one.

use crate::model::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write;

pub const PROJECTION_FORMAT: &str = "xeet-artifact.projection";
pub const PROJECTION_VERSION: &str = "0.1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Consumer {
    /// Reads text only. Gets no pixels; is told what only pixels hold.
    #[default]
    Text,
    /// Also reads images. Gets view references to attach alongside.
    Vision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Section {
    Elements,
    Layout,
    Relations,
    State,
    Capabilities,
    Limitations,
    Consumers,
}

pub const ALL_SECTIONS: &[Section] = &[
    Section::Elements,
    Section::Layout,
    Section::Relations,
    Section::State,
    Section::Capabilities,
    Section::Limitations,
    Section::Consumers,
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Request {
    pub consumer: Consumer,
    /// Sections to include; empty means all. The context header (subject,
    /// observation, provenance) is always included.
    pub sections: Vec<Section>,
    /// Project only this element's subtree, with its ancestors as context.
    pub focus: Option<String>,
    /// Levels below the root (or the focus) to include.
    pub depth: Option<usize>,
    /// Truncate any single string to this many characters, saying so.
    pub max_text: Option<usize>,
    /// Total size budget in characters of the serialized projection. Elements
    /// are kept in breadth-first order until the budget is met; the rest are
    /// listed as omitted. The context header is never dropped.
    pub max_chars: Option<usize>,
    /// Capabilities without input schemas, preconditions and verification:
    /// id, kind, target, executor and required authority only. A budget
    /// switches to this before it drops capabilities altogether.
    pub compact_capabilities: bool,
}

impl Request {
    fn wants(&self, s: Section) -> bool {
        self.sections.is_empty() || self.sections.contains(&s)
    }
}

/// How an element's meaning reaches a consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    /// Its meaning is in its text or value.
    Text,
    /// It organizes other elements; its meaning is its kind, role, attributes
    /// and children.
    Structural,
    /// Its content exists only as pixels (an image without alt text, a chart
    /// without data). A text consumer cannot know it; a vision consumer can
    /// only infer it.
    VisualOnly,
}

pub fn availability(e: &Element) -> Availability {
    let has_text = e.text.as_deref().is_some_and(|t| !t.trim().is_empty());
    let has_value = e.value.as_ref().is_some_and(|v| !v.is_null());
    if has_text || has_value {
        Availability::Text
    } else if e.kind == "media" {
        Availability::VisualOnly
    } else {
        Availability::Structural
    }
}

/// What each kind of consumer can understand from the whole artifact.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerCoverage {
    pub elements: usize,
    pub meaning_in_text: usize,
    pub structural: usize,
    /// Content only in pixels: unknown to text consumers; inferable (not
    /// verifiable) by vision consumers.
    pub visual_only: Vec<String>,
    /// Elements whose content was inferred (by a model, OCR, …), for every
    /// consumer alike.
    pub inferred: Vec<String>,
    /// For each consumer, what it can rely on.
    pub text_consumer: String,
    pub vision_consumer: String,
}

pub fn consumer_coverage(a: &Artifact) -> ConsumerCoverage {
    let mut c = ConsumerCoverage {
        elements: a.semantic.elements.len(),
        meaning_in_text: 0,
        structural: 0,
        visual_only: Vec::new(),
        inferred: Vec::new(),
        text_consumer: String::new(),
        vision_consumer: String::new(),
    };
    for e in &a.semantic.elements {
        match availability(e) {
            Availability::Text => c.meaning_in_text += 1,
            Availability::Structural => c.structural += 1,
            Availability::VisualOnly => c.visual_only.push(e.id.clone()),
        }
        if e.basis == Basis::Inferred {
            c.inferred.push(e.id.clone());
        }
    }
    let only = match c.visual_only.len() {
        0 => "none exists only as pixels: nothing semantic requires vision".to_string(),
        1 => "1 exists only as pixels and is unknown without vision".to_string(),
        n => format!("{n} exist only as pixels and are unknown without vision"),
    };
    c.text_consumer = format!(
        "{} of {} elements carry their meaning as text and {} are structural; {only}",
        c.meaning_in_text, c.elements, c.structural
    );
    c.vision_consumer = if c.visual_only.is_empty() {
        "the image adds appearance only; every element's meaning is already in the text".to_string()
    } else {
        format!(
            "the image adds appearance, and {} pixel-only element(s) whose content it can read only as inference",
            c.visual_only.len()
        )
    };
    c
}

// ---------------------------------------------------------------------------
// The structured projection
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Projection {
    pub format: String,
    pub version: String,
    /// The artifact this projection was made from.
    pub artifact: String,
    pub consumer: Consumer,
    pub request: Request,
    pub context: Context,
    /// View references. A vision consumer attaches these images; a text
    /// consumer receives them as references only.
    pub views: Vec<ViewRef>,
    pub elements: Vec<PElement>,
    pub layout: Vec<Row>,
    pub relations: Vec<Relation>,
    pub state: Vec<Fact>,
    pub capabilities: Vec<PCapability>,
    pub limitations: Vec<PNote>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumers: Option<ConsumerCoverage>,
    /// What this projection left out of the artifact, and why.
    pub omitted: Omitted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub subject_type: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub captured_at: String,
    pub method: String,
    pub adapter: String,
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,
    pub authenticity: Authenticity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    pub temporal: String,
    pub capability_policy: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ViewRef {
    pub id: String,
    pub asset: String,
    pub media_type: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub scale: f32,
    pub layout_width: f32,
    pub layout_height: f32,
    pub fidelity: FidelityClaim,
    pub attached: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PElement {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub depth: usize,
    pub kind: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Characters cut from `text` by `maxText`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_cut: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub attrs: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub bounds: Vec<Bounds>,
    pub visible: bool,
    pub basis: Basis,
    pub availability: Availability,
    /// Included only as an ancestor of the focus.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub context_only: bool,
}

/// One visual row: elements whose vertical extents overlap, left to right.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub view: String,
    pub top: f32,
    pub bottom: f32,
    pub items: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PCapability {
    pub id: String,
    pub kind: CapabilityKind,
    pub title: String,
    pub target: String,
    pub executor: ExecutorKind,
    pub reference: String,
    pub availability: crate::model::Availability,
    pub authority: Vec<String>,
    /// Absent in compact form; request the capabilities section to get it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub input_schema: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub preconditions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verification: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PNote {
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub what: String,
    pub why: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Omitted {
    /// Elements of the artifact not in this projection.
    pub elements: Vec<String>,
    pub sections: Vec<Section>,
    /// Why: `focus`, `depth`, `budget`, or `sections`.
    pub reasons: Vec<String>,
    /// The budget could not be met even by the minimal projection.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub budget_exceeded: bool,
}

/// Output forms of a projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    /// Line-oriented text for any model or person.
    Text,
    /// The [`Projection`] as JSON, keys sorted.
    Json,
}

/// Under a budget, sections are kept in this order of importance, after the
/// context header and the top-level elements and before deeper elements:
/// what was true, what can be done, what is missing, then structure.
pub const BUDGET_PRIORITY: &[Section] = &[
    Section::State,
    Section::Capabilities,
    Section::Limitations,
    Section::Relations,
    Section::Layout,
    Section::Consumers,
];

/// Projects and serializes in one step, honoring the character budget.
///
/// Under a budget the fill is deterministic and by priority: the context
/// header (always), the top-level elements, then the sections of
/// [`BUDGET_PRIORITY`] in order, stopping at the first that does not fit, then as many deeper elements,
/// breadth-first, as fit. Everything left out is listed under `omitted`, so
/// a consumer can ask for it by id or section.
pub fn render(a: &Artifact, req: &Request, form: Form) -> Result<String, String> {
    let order = candidate_order(a, req)?;
    let serialize = |p: &Projection| match form {
        Form::Text => text(p),
        Form::Json => {
            crate::canon::pretty(&serde_json::to_value(p).expect("projection serializes"))
        }
    };
    let build = |r: &Request, k: usize| {
        let mut p = assemble(a, r, &order, k);
        p.request = req.clone();
        if req.sections.is_empty() {
            // Sections missing here were dropped by the budget, not requested.
            p.omitted.reasons.retain(|x| x != "sections");
        }
        if k < order.len()
            || r.sections != req.sections
            || r.compact_capabilities != req.compact_capabilities
        {
            p.omitted.reasons.push("budget".into());
        }
        let s = serialize(&p);
        (p, s)
    };
    let Some(budget) = req.max_chars else {
        return Ok(build(req, order.len()).1);
    };
    if build(req, order.len()).1.chars().count() <= budget {
        return Ok(build(req, order.len()).1);
    }
    let fits = |r: &Request, k: usize| build(r, k).1.chars().count() <= budget;

    // The top level: the focus's ancestors (context), the start element and
    // its children.
    let ancestors = order.iter().take_while(|(_, ctx)| *ctx).count();
    let children = order
        .get(ancestors)
        .and_then(|(id, _)| a.semantic.elements.iter().find(|e| &e.id == id))
        .map_or(0, |e| e.children.len());
    let top = (ancestors + 1 + children).min(order.len());

    let wanted: Vec<Section> = if req.sections.is_empty() {
        ALL_SECTIONS.to_vec()
    } else {
        req.sections.clone()
    };
    let mut r = req.clone();
    // Start from elements alone (if wanted), largest top-level prefix that fits.
    r.sections = wanted
        .iter()
        .copied()
        .filter(|s| *s == Section::Elements)
        .collect();
    let Some(base) = largest_prefix(top, |k| fits(&r, k)) else {
        let (mut p, _) = build(&r, 1);
        p.omitted.budget_exceeded = true;
        return Ok(serialize(&p));
    };
    // A strict prefix of the priority order: a larger budget never keeps
    // fewer sections, and a less important section never displaces a more
    // important one.
    for s in BUDGET_PRIORITY.iter().filter(|s| wanted.contains(s)) {
        r.sections.push(*s);
        if fits(&r, base) {
            continue;
        }
        if *s == Section::Capabilities && !r.compact_capabilities {
            r.compact_capabilities = true;
            if fits(&r, base) {
                continue;
            }
            r.compact_capabilities = req.compact_capabilities;
        }
        r.sections.pop();
        break;
    }
    r.sections.sort();
    let k = largest_prefix(order.len(), |k| k <= base || fits(&r, k))
        .unwrap_or(base)
        .max(base);
    Ok(build(&r, k).1)
}

/// The largest `k` in `1..=n` with `fits(k)`, assuming fit is monotone.
fn largest_prefix(n: usize, fits: impl Fn(usize) -> bool) -> Option<usize> {
    if n == 0 || !fits(1) {
        return None;
    }
    let (mut lo, mut hi) = (1usize, n);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(lo)
}

/// The structured projection without a budget.
pub fn project(a: &Artifact, req: &Request) -> Result<Projection, String> {
    let order = candidate_order(a, req)?;
    Ok(assemble(a, req, &order, order.len()))
}

/// Elements eligible for this request, breadth-first: ancestors of the focus
/// first (as context), then the focus subtree, limited by depth.
fn candidate_order(a: &Artifact, req: &Request) -> Result<Vec<(String, bool)>, String> {
    let by_id: BTreeMap<&str, &Element> = a
        .semantic
        .elements
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();
    let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
    for e in &a.semantic.elements {
        for c in &e.children {
            parent.insert(c, &e.id);
        }
    }
    let start = req.focus.as_deref().unwrap_or(&a.semantic.root);
    if !by_id.contains_key(start) {
        return Err(format!("no element {start}"));
    }
    let mut out = Vec::new();
    let mut chain = Vec::new();
    let mut cur = start;
    while let Some(p) = parent.get(cur) {
        chain.push(*p);
        cur = p;
        if chain.len() > by_id.len() {
            break;
        }
    }
    for id in chain.iter().rev() {
        out.push((id.to_string(), true));
    }
    let mut queue = VecDeque::from([(start, 0usize)]);
    let mut seen = BTreeSet::new();
    while let Some((id, d)) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        out.push((id.to_string(), false));
        if req.depth.is_some_and(|max| d >= max) {
            continue;
        }
        if let Some(e) = by_id.get(id) {
            for c in &e.children {
                if by_id.contains_key(c.as_str()) {
                    queue.push_back((c, d + 1));
                }
            }
        }
    }
    Ok(out)
}

fn cut(s: &str, max: Option<usize>) -> (String, Option<usize>) {
    match max {
        Some(m) if s.chars().count() > m => {
            let kept: String = s.chars().take(m).collect();
            (kept, Some(s.chars().count() - m))
        }
        _ => (s.to_string(), None),
    }
}

fn cut_value(v: &Value, max: Option<usize>) -> Value {
    match v {
        Value::String(s) => {
            let (kept, lost) = cut(s, max);
            match lost {
                Some(n) => Value::String(format!("{kept}… (+{n} chars)")),
                None => Value::String(kept),
            }
        }
        Value::Array(items) => Value::Array(items.iter().map(|x| cut_value(x, max)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| (k.clone(), cut_value(x, max)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn assemble(a: &Artifact, req: &Request, order: &[(String, bool)], k: usize) -> Projection {
    let keep: Vec<&(String, bool)> = order.iter().take(k.max(1).min(order.len())).collect();
    let included: BTreeSet<&str> = keep.iter().map(|(id, _)| id.as_str()).collect();
    let by_id: BTreeMap<&str, &Element> = a
        .semantic
        .elements
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();
    let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
    for e in &a.semantic.elements {
        for c in &e.children {
            parent.insert(c, &e.id);
        }
    }
    let depth_of = |id: &str| {
        let mut d = 0;
        let mut cur = id;
        while let Some(p) = parent.get(cur) {
            d += 1;
            cur = p;
            if d > by_id.len() {
                break;
            }
        }
        d
    };

    // Elements in tree (document) order, so a reader sees the hierarchy.
    let mut elements = Vec::new();
    let mut stack = vec![a.semantic.root.as_str()];
    let mut visited = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Some(e) = by_id.get(id) else { continue };
        if included.contains(id) {
            let context_only = keep.iter().any(|(k, ctx)| k == id && *ctx);
            let (text, text_cut) = match &e.text {
                Some(t) => {
                    let (t, lost) = cut(t, req.max_text);
                    (Some(t), lost)
                }
                None => (None, None),
            };
            elements.push(PElement {
                id: e.id.clone(),
                parent: parent.get(id).map(|p| p.to_string()),
                depth: depth_of(id),
                kind: e.kind.clone(),
                role: e.role.clone(),
                text,
                text_cut,
                value: e.value.as_ref().map(|v| cut_value(v, req.max_text)),
                unit: e.unit.clone(),
                attrs: e
                    .attrs
                    .iter()
                    .map(|(k, v)| (k.clone(), cut_value(v, req.max_text)))
                    .collect(),
                bounds: e.bounds.clone(),
                visible: e.visible,
                basis: e.basis,
                availability: availability(e),
                context_only,
            });
        }
        for c in e.children.iter().rev() {
            stack.push(c);
        }
    }

    let views = a
        .views
        .iter()
        .map(|v| ViewRef {
            id: v.id.clone(),
            asset: v.asset.clone(),
            media_type: v.media_type.clone(),
            pixel_width: v.pixel_width,
            pixel_height: v.pixel_height,
            scale: v.scale,
            layout_width: v.layout_width,
            layout_height: v.layout_height,
            fidelity: v.fidelity.claim,
            attached: req.consumer == Consumer::Vision,
        })
        .collect();

    let wants = |s| req.wants(s);
    let layout = if wants(Section::Layout) {
        rows(a, &elements)
    } else {
        Vec::new()
    };
    let relations = if wants(Section::Relations) {
        a.semantic
            .relations
            .iter()
            .filter(|r| included.contains(r.from.as_str()) || included.contains(r.to.as_str()))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let state = if wants(Section::State) {
        a.state
            .facts
            .iter()
            .filter(|f| included.contains(f.subject.as_str()))
            .map(|f| Fact {
                value: cut_value(&f.value, req.max_text),
                ..f.clone()
            })
            .collect()
    } else {
        Vec::new()
    };
    let capabilities = if wants(Section::Capabilities) {
        a.capabilities
            .iter()
            .filter(|c| included.contains(c.target.as_str()))
            .map(|c| PCapability {
                id: c.id.clone(),
                kind: c.kind,
                title: c.title.clone(),
                target: c.target.clone(),
                executor: c.executor.kind,
                reference: c.executor.reference.clone(),
                availability: c.availability,
                authority: c.authority.required.clone(),
                input_schema: (!req.compact_capabilities).then(|| c.input_schema.clone()),
                preconditions: if req.compact_capabilities {
                    Vec::new()
                } else {
                    c.preconditions.clone()
                },
                verification: (!req.compact_capabilities).then(|| c.verification.clone()),
            })
            .collect()
    } else {
        Vec::new()
    };
    let limitations = if wants(Section::Limitations) {
        let l = &a.limitations;
        [
            ("omitted", &l.omitted),
            ("approximated", &l.approximated),
            ("derived", &l.derived),
            ("inferred", &l.inferred),
            ("unverified", &l.unverified),
        ]
        .into_iter()
        .flat_map(|(cat, notes)| {
            notes
                .iter()
                .filter(|n| match n.subject.as_deref() {
                    None => true,
                    Some(s) => !by_id.contains_key(s) || included.contains(s),
                })
                .map(move |n| PNote {
                    category: cat.to_string(),
                    subject: n.subject.clone(),
                    what: n.what.clone(),
                    why: n.why.clone(),
                })
        })
        .collect()
    } else {
        Vec::new()
    };

    let mut omitted = Omitted::default();
    let mut all: Vec<&str> = a.semantic.elements.iter().map(|e| e.id.as_str()).collect();
    all.sort();
    omitted.elements = all
        .into_iter()
        .filter(|id| !included.contains(id))
        .map(str::to_string)
        .collect();
    if req.focus.is_some() && !omitted.elements.is_empty() {
        omitted.reasons.push("focus".into());
    }
    if req.depth.is_some() {
        omitted.reasons.push("depth".into());
    }
    omitted.sections = ALL_SECTIONS
        .iter()
        .copied()
        .filter(|s| !req.wants(*s))
        .collect();
    if !omitted.sections.is_empty() {
        omitted.reasons.push("sections".into());
    }

    let p = &a.provenance;
    Projection {
        format: PROJECTION_FORMAT.into(),
        version: PROJECTION_VERSION.into(),
        artifact: a.id.clone(),
        consumer: req.consumer,
        request: req.clone(),
        context: Context {
            subject_type: a.subject.kind.clone(),
            title: a.subject.title.clone(),
            reference: a.subject.reference.clone(),
            captured_at: p.capture.captured_at.clone(),
            method: p.capture.method.clone(),
            adapter: format!("{} {}", p.adapter.name, p.adapter.version),
            source_type: p.source.kind.clone(),
            source_version: p.source.version.clone(),
            authenticity: p.authenticity,
            derived_from: p.derived_from.clone(),
            temporal: a.state.temporal.clone(),
            capability_policy: a.capability_policy.clone(),
        },
        views,
        elements: if wants(Section::Elements) {
            elements
        } else {
            Vec::new()
        },
        layout,
        relations,
        state,
        capabilities,
        limitations,
        consumers: wants(Section::Consumers).then(|| consumer_coverage(a)),
        omitted,
    }
}

/// Groups the painted leaf elements of the first view into rows: an element
/// joins a row when their vertical extents overlap by at least half of the
/// shorter one. Rows top to bottom, items left to right. Deterministic.
fn rows(a: &Artifact, elements: &[PElement]) -> Vec<Row> {
    let Some(view) = a.views.first() else {
        return Vec::new();
    };
    let in_view = |e: &PElement| e.bounds.iter().find(|b| b.view == view.id).cloned();
    let has_bounded_child: BTreeSet<&str> = elements
        .iter()
        .filter(|e| in_view(e).is_some())
        .filter_map(|e| e.parent.as_deref())
        .collect();
    let mut leaves: Vec<(Bounds, &str)> = elements
        .iter()
        .filter(|e| e.visible && !has_bounded_child.contains(e.id.as_str()))
        .filter_map(|e| in_view(e).map(|b| (b, e.id.as_str())))
        .collect();
    leaves.sort_by(|(a, ai), (b, bi)| {
        a.y.total_cmp(&b.y)
            .then(a.x.total_cmp(&b.x))
            .then(ai.cmp(bi))
    });
    // (top, bottom, items as (x, id))
    type Open<'a> = (f32, f32, Vec<(f32, &'a str)>);
    let mut rows: Vec<Open> = Vec::new();
    for (b, id) in leaves {
        let (top, bottom) = (b.y, b.y + b.h);
        let joined = rows.last_mut().filter(|(t, bt, _)| {
            let overlap = bottom.min(*bt) - top.max(*t);
            let shorter = (bottom - top).min(*bt - *t).max(0.01);
            overlap >= shorter / 2.0
        });
        match joined {
            Some(row) => {
                row.0 = row.0.min(top);
                row.1 = row.1.max(bottom);
                row.2.push((b.x, id));
            }
            None => rows.push((top, bottom, vec![(b.x, id)])),
        }
    }
    rows.into_iter()
        .map(|(top, bottom, mut items)| {
            items.sort_by(|(a, ai), (b, bi)| a.total_cmp(b).then(ai.cmp(bi)));
            Row {
                view: view.id.clone(),
                top,
                bottom,
                items: items.into_iter().map(|(_, id)| id.to_string()).collect(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The text form
// ---------------------------------------------------------------------------

fn n(v: f32) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

fn q(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}

fn mark(b: Basis) -> &'static str {
    match b {
        Basis::Source => "",
        Basis::Observed => " ^observed",
        Basis::Derived => " ~derived",
        Basis::Inferred => " !inferred",
        Basis::Unknown => " ?unknown",
    }
}

/// The text form: line-oriented, every string JSON-quoted, numbers fixed to
/// two decimals at most. Readable by people and by any model that reads text.
pub fn text(p: &Projection) -> String {
    let mut s = String::new();
    let c = &p.context;
    let _ = writeln!(
        s,
        "ARTIFACT {} ({} {}) for a {} consumer",
        p.artifact,
        p.format,
        p.version,
        match p.consumer {
            Consumer::Text => "text-only",
            Consumer::Vision => "vision-capable",
        }
    );
    let _ = writeln!(
        s,
        "SUBJECT {} {}{}",
        c.subject_type,
        q(&c.title),
        c.reference
            .as_deref()
            .map(|r| format!(" <{r}>"))
            .unwrap_or_default()
    );
    let _ = writeln!(
        s,
        "OBSERVED {} via {} by {}; source {}{}; authenticity {}",
        c.captured_at,
        c.method,
        c.adapter,
        c.source_type,
        c.source_version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default(),
        serde_json::to_value(c.authenticity)
            .unwrap()
            .as_str()
            .unwrap_or("")
    );
    if let Some(d) = &c.derived_from {
        let _ = writeln!(s, "DERIVED FROM {d}");
    }
    let _ = writeln!(s, "NOTE a {}: every fact is as of OBSERVED, not now. Capabilities are {}: descriptions, never grants.", c.temporal.replace('-', " "), c.capability_policy);
    let _ = writeln!(s, "LEGEND basis: unmarked=source ~derived ^observed !inferred ?unknown; @view(x,y wxh) in layout units; [visual-only]=content exists only as pixels");
    for v in &p.views {
        let how = if v.attached {
            "image attached; anything read from its pixels is an inference, not a source fact"
        } else {
            "image not included; nothing below requires it unless marked [visual-only]"
        };
        let _ = writeln!(
            s,
            "VIEW {} {} {}x{} layout units at scale {} ({}x{} px), fidelity {}; {how}",
            v.id,
            v.media_type,
            n(v.layout_width),
            n(v.layout_height),
            n(v.scale),
            v.pixel_width,
            v.pixel_height,
            serde_json::to_value(v.fidelity)
                .unwrap()
                .as_str()
                .unwrap_or("")
        );
    }

    if !p.elements.is_empty() {
        let _ = writeln!(s, "\nELEMENTS");
        let base = p.elements.iter().map(|e| e.depth).min().unwrap_or(0);
        for e in &p.elements {
            let mut line = format!(
                "{}{} {} {}",
                "  ".repeat(e.depth - base + 1),
                e.id,
                e.kind,
                e.role
            );
            if let Some(t) = &e.text {
                let _ = write!(line, " {}", q(t));
                if let Some(lost) = e.text_cut {
                    let _ = write!(line, "…(+{lost} chars)");
                }
            }
            if let Some(v) = &e.value {
                let _ = write!(line, " = {}", crate::canon::canonical(v));
            }
            if let Some(u) = &e.unit {
                let _ = write!(line, " {u}");
            }
            line.push_str(mark(e.basis));
            if !e.visible {
                line.push_str(" [not in view]");
            }
            if e.availability == Availability::VisualOnly {
                line.push_str(" [visual-only]");
            }
            if e.context_only {
                line.push_str(" [context]");
            }
            for b in &e.bounds {
                let _ = write!(
                    line,
                    " @{}({},{} {}x{})",
                    b.view,
                    n(b.x),
                    n(b.y),
                    n(b.w),
                    n(b.h)
                );
            }
            if !e.attrs.is_empty() {
                let attrs = serde_json::to_value(&e.attrs).unwrap();
                let _ = write!(line, " {}", crate::canon::canonical(&attrs));
            }
            let _ = writeln!(s, "{line}");
        }
    }
    if !p.layout.is_empty() {
        let _ = writeln!(s, "\nLAYOUT rows top to bottom, items left to right");
        for r in &p.layout {
            let _ = writeln!(
                s,
                "  {} y {}-{}: {}",
                r.view,
                n(r.top),
                n(r.bottom),
                r.items.join(" | ")
            );
        }
    }
    if !p.relations.is_empty() {
        let _ = writeln!(s, "\nRELATIONS");
        for r in &p.relations {
            let _ = writeln!(s, "  {} {} {}{}", r.from, r.kind, r.to, mark(r.basis));
        }
    }
    if !p.state.is_empty() {
        let _ = writeln!(s, "\nSTATE as of {}", c.captured_at);
        for f in &p.state {
            let _ = writeln!(
                s,
                "  {}.{} = {}{}{}{}",
                f.subject,
                f.key,
                crate::canon::canonical(&f.value),
                mark(f.basis),
                if f.volatile {
                    " (may have changed)"
                } else {
                    ""
                },
                f.source_time
                    .as_deref()
                    .map(|t| format!(" (source time {t})"))
                    .unwrap_or_default()
            );
        }
    }
    if !p.capabilities.is_empty() {
        let compact = p.capabilities.iter().all(|c| c.input_schema.is_none());
        let _ = writeln!(
            s,
            "\nCAPABILITIES (descriptions, not grants){}",
            if compact {
                "; compact: inputs, preconditions and verification omitted"
            } else {
                ""
            }
        );
        for cap in &p.capabilities {
            let auth = if cap.authority.is_empty() {
                "no authority".to_string()
            } else {
                format!("needs {}", cap.authority.join("; "))
            };
            let _ = writeln!(
                s,
                "  {} {} on {}: {} via {} {}; {}; {auth}",
                cap.id,
                serde_json::to_value(cap.kind)
                    .unwrap()
                    .as_str()
                    .unwrap_or(""),
                cap.target,
                q(&cap.title),
                serde_json::to_value(cap.executor)
                    .unwrap()
                    .as_str()
                    .unwrap_or(""),
                cap.reference,
                serde_json::to_value(cap.availability)
                    .unwrap()
                    .as_str()
                    .unwrap_or("")
            );
            if let Some(schema) = &cap.input_schema {
                let _ = writeln!(s, "    input {}", crate::canon::canonical(schema));
            }
            for pre in &cap.preconditions {
                let _ = writeln!(s, "    requires {}", q(pre));
            }
            if let Some(v) = &cap.verification {
                let _ = writeln!(s, "    verify {}", q(v));
            }
        }
    }
    if !p.limitations.is_empty() {
        let _ = writeln!(s, "\nLIMITATIONS");
        for l in &p.limitations {
            let subject = l
                .subject
                .as_deref()
                .map(|x| format!(" [{x}]"))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "  {}{subject} {} — {}",
                l.category,
                q(&l.what),
                q(&l.why)
            );
        }
    }
    if let Some(cov) = &p.consumers {
        let _ = writeln!(s, "\nCONSUMERS");
        let _ = writeln!(s, "  text-only: {}", cov.text_consumer);
        let _ = writeln!(s, "  vision: {}", cov.vision_consumer);
        if !cov.visual_only.is_empty() {
            let _ = writeln!(s, "  visual-only: {}", cov.visual_only.join(", "));
        }
        if !cov.inferred.is_empty() {
            let _ = writeln!(s, "  inferred: {}", cov.inferred.join(", "));
        }
    }
    let o = &p.omitted;
    if !o.elements.is_empty() || !o.sections.is_empty() || o.budget_exceeded {
        let _ = writeln!(
            s,
            "\nOMITTED FROM THIS PROJECTION ({}); request them by id with a focus",
            o.reasons.join(", ")
        );
        if !o.elements.is_empty() {
            let _ = writeln!(
                s,
                "  {} elements: {}",
                o.elements.len(),
                o.elements.join(", ")
            );
        }
        if !o.sections.is_empty() {
            let names: Vec<String> = o
                .sections
                .iter()
                .map(|x| {
                    serde_json::to_value(x)
                        .unwrap()
                        .as_str()
                        .unwrap_or("")
                        .to_string()
                })
                .collect();
            let _ = writeln!(s, "  sections: {}", names.join(", "));
        }
        if o.budget_exceeded {
            let _ = writeln!(s, "  even the minimal projection (context header, start element, this list) exceeds the requested budget");
        }
    }
    s
}
