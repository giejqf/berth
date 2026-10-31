//! Links between the rail's slots (ADR 0027): a slot made from another's
//! follow-up, or asked for by its agent, and slots whose agents exchanged
//! messages. Drawn in the rail's gutter, left of the slots' numbers: a solid
//! line with an arrow into the slot that was made, a dotted one between
//! slots that talked. Links show quietly; those of the selected slot and
//! the one under the pointer stand out, and are the ones sure to be drawn
//! when the gutter has no room for all.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gpui::{
    Bounds, Hsla, IntoElement, PathBuilder, Pixels, Styled as _, Window, canvas, point, px,
};

use crate::backend::SlotEntry;
use crate::theme::Tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// `a` made `b`: one of its follow-ups, or a slot its agent asked for.
    From,
    /// Their agents exchanged messages.
    Messages,
}

/// A link between two rows of the rail, by their place in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    pub a: usize,
    pub b: usize,
    /// Its line's place in the gutter, 0 the farthest from the numbers.
    pub lane: usize,
}

/// Two rows a line joins.
type Edge = (usize, usize);

/// Lines side by side the gutter has room for. A slot in focus needs at
/// most three: to the slot it came from, to those it made, to those it
/// talked to.
const LANES: usize = 3;
/// From a number's left edge: the farthest lane, the step between lanes,
/// and where a line stops short of the number.
const OUTER: f32 = 13.;
const STEP: f32 = 3.5;
const GAP: f32 = 2.;

/// The links between the rows of `order` (the rail's, top to bottom), each
/// in a lane: a slot and what it made share one line, and lines that
/// overlap take different lanes, the longer farther out. The slots in
/// `focus` (ids, the first first) go before the rest, and each one's
/// messages share one line from it; a line no lane is free for isn't drawn
/// (two lines in one lane would read as one).
pub fn links(order: &[&SlotEntry], focus: &[String]) -> Vec<Link> {
    let row: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.workflow.as_ref().map(|w| (w.id.as_str(), i)))
        .collect();
    let mut made = Vec::new();
    let mut talked = BTreeSet::new();
    for (i, e) in order.iter().enumerate() {
        if let Some(&p) = e.facts.from_workflow.as_deref().and_then(|f| row.get(f))
            && p != i
        {
            made.push((p, i));
        }
        for peer in &e.facts.peers {
            if let Some(&j) = row.get(peer.workflow_id.as_str())
                && j != i
            {
                talked.insert((i.min(j), i.max(j)));
            }
        }
    }
    let focus: Vec<usize> = focus
        .iter()
        .filter_map(|f| order.iter().position(|e| e.slot.id == *f))
        .collect();
    // A slot and every slot it made are one group, on one line from it; so
    // are a slot in focus and the slots it talked to. Any other pair that
    // talked is a group of its own.
    let mut groups: Vec<(LinkKind, usize, Vec<Edge>)> = Vec::new();
    let mut join =
        |kind, hub, edge| match groups.iter_mut().find(|(k, h, _)| *k == kind && *h == hub) {
            Some((_, _, g)) => g.push(edge),
            None => groups.push((kind, hub, vec![edge])),
        };
    for (p, c) in made {
        join(LinkKind::From, p, (p, c));
    }
    for (i, j) in talked {
        match focus.iter().find(|&&f| f == i || f == j) {
            Some(&hub) => join(LinkKind::Messages, hub, (hub, i + j - hub)),
            // Rows are below `order.len()`: a hub of its own.
            None => join(
                LinkKind::Messages,
                order.len() + i * order.len() + j,
                (i, j),
            ),
        }
    }
    let span = |g: &[Edge]| {
        let rows = g.iter().flat_map(|&(a, b)| [a, b]);
        let lo = rows.clone().min().unwrap_or(0);
        (lo, rows.max().unwrap_or(0))
    };
    // The first slot in focus first, then the next, then the rest.
    let rank = |g: &[Edge]| {
        focus
            .iter()
            .position(|f| g.iter().any(|&(a, b)| a == *f || b == *f))
            .unwrap_or(focus.len())
    };
    groups.sort_by_key(|(_, _, g)| {
        let (lo, hi) = span(g);
        (rank(g), std::cmp::Reverse(hi - lo), lo)
    });
    let mut placed: Vec<(Edge, usize)> = Vec::new();
    let mut out = Vec::new();
    for (kind, _, g) in groups {
        let (lo, hi) = span(&g);
        let free = (0..LANES).find(|&l| {
            !placed
                .iter()
                .any(|&((a, b), pl)| pl == l && a <= hi && lo <= b)
        });
        if let Some(lane) = free {
            placed.push(((lo, hi), lane));
            out.extend(g.into_iter().map(|(a, b)| Link { kind, a, b, lane }));
        }
    }
    out
}

/// What links `entry`'s slot to the repo's others (`all`, every window's),
/// one line each: "Follow-up of slot 2", "Follow-ups in slots 3, 4",
/// "3 messages with slot 4".
pub fn describe(entry: &SlotEntry, all: &[SlotEntry]) -> Vec<String> {
    let Some(w) = &entry.workflow else {
        return Vec::new();
    };
    let showing = |id: &str| {
        all.iter()
            .find(|e| e.slot.id != entry.slot.id && e.workflow.as_ref().is_some_and(|w| w.id == id))
    };
    let mut out = Vec::new();
    if let Some(from) = entry.facts.from_workflow.as_deref().and_then(showing) {
        out.push(format!("Follow-up of slot {}", from.slot.number));
    }
    let made: Vec<String> = all
        .iter()
        .filter(|e| {
            e.slot.id != entry.slot.id && e.facts.from_workflow.as_deref() == Some(w.id.as_str())
        })
        .map(|e| e.slot.number.to_string())
        .collect();
    match made.as_slice() {
        [] => {}
        [one] => out.push(format!("Follow-up in slot {one}")),
        many => out.push(format!("Follow-ups in slots {}", many.join(", "))),
    }
    for peer in &entry.facts.peers {
        if let Some(e) = showing(&peer.workflow_id) {
            let n = peer.sent + peer.received;
            out.push(format!(
                "{n} {} with slot {}",
                if n == 1 { "message" } else { "messages" },
                e.slot.number
            ));
        }
    }
    out
}

/// Where each slot's number was laid out, by slot id: [`track`] records it
/// as the frame is laid out, [`overlay`] draws from it once all of it is.
pub type Tiles = Rc<RefCell<HashMap<String, Bounds<Pixels>>>>;

/// Records where the element it is put in (a slot's number) lies.
pub fn track(tiles: &Tiles, slot_id: &str) -> impl IntoElement {
    let (tiles, id) = (tiles.clone(), slot_id.to_owned());
    canvas(
        move |bounds, _, _| {
            tiles.borrow_mut().insert(id, bounds);
        },
        |_, (), _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Draws `links` between the numbers [`track`] recorded, rows named by
/// slot id in `ids`: those of a slot in `focus` last, in a stronger color.
/// Put it after the rows, in the element they scroll in.
pub fn overlay(
    links: Vec<Link>,
    ids: Vec<String>,
    tiles: Tiles,
    focus: Vec<String>,
    t: &Tokens,
) -> impl IntoElement {
    let (quiet, strong) = (t.border_strong, t.text3);
    canvas(
        |_, _, _| {},
        move |_, (), window, _| {
            let tiles = tiles.borrow();
            let lit = |l: &Link| focus.iter().any(|f| *f == ids[l.a] || *f == ids[l.b]);
            for on in [false, true] {
                for l in links.iter().filter(|l| lit(l) == on) {
                    if let (Some(a), Some(b)) = (tiles.get(&ids[l.a]), tiles.get(&ids[l.b])) {
                        paint(window, l, a, b, if on { strong } else { quiet });
                    }
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// One link: a bracket in its lane from `a`'s number to `b`'s, rounded at
/// the turns; solid with an arrow into `b` for [`LinkKind::From`], dotted
/// for [`LinkKind::Messages`].
fn paint(window: &mut Window, l: &Link, a: &Bounds<Pixels>, b: &Bounds<Pixels>, color: Hsla) {
    let left = if a.left() < b.left() {
        a.left()
    } else {
        b.left()
    };
    let x = left - px(OUTER - STEP * l.lane as f32);
    let end = left - px(GAP);
    let (ya, yb) = (a.center().y, b.center().y);
    let (top, bottom) = if ya < yb { (ya, yb) } else { (yb, ya) };
    let r = px(3.);
    let mut line = PathBuilder::stroke(px(1.5));
    if l.kind == LinkKind::Messages {
        line = line.dash_array(&[px(2.), px(2.5)]);
    }
    line.move_to(point(end, top));
    line.line_to(point(x + r, top));
    line.curve_to(point(x, top + r), point(x, top));
    line.line_to(point(x, bottom - r));
    line.curve_to(point(x + r, bottom), point(x, bottom));
    line.line_to(point(end, bottom));
    if let Ok(path) = line.build() {
        window.paint_path(path, color);
    }
    if l.kind == LinkKind::From {
        let y = b.center().y;
        let mut head = PathBuilder::fill();
        head.add_polygon(
            &[
                point(end - px(4.), y - px(3.)),
                point(end + px(0.5), y),
                point(end - px(4.), y + px(3.)),
            ],
            true,
        );
        if let Ok(path) = head.build() {
            window.paint_path(path, color);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use berth_core::{MessagePeer, Slot, SlotState, Workflow, WorkflowFacts, WorkflowState};
    use berth_protocol::{Origin, OriginKind};

    fn entry(n: u32, from: Option<u32>, peers: &[u32]) -> SlotEntry {
        let wf = |n: u32| format!("w{n}");
        SlotEntry {
            slot: Slot {
                id: format!("s{n}"),
                repo_id: "r".into(),
                slot_type: "worktree".into(),
                name: format!("slot-{n}"),
                custom_name: None,
                model: None,
                effort: None,
                state: SlotState::Running,
                state_detail: None,
                checkout_path: None,
                placement: "host".into(),
                branch: format!("berth/{n}"),
                base_ref: "main".into(),
                created_at: 0,
                number: n,
            },
            window_id: None,
            workflow: Some(Workflow {
                id: wf(n),
                slot_id: format!("s{n}"),
                origin: Origin {
                    kind: OriginKind::Thought,
                    r#ref: String::new(),
                    title: format!("Slot {n}"),
                    body: String::new(),
                    custom_prompt: None,
                    url: None,
                },
                state: WorkflowState::Idle,
                claude_session_id: None,
                section_no: 1,
                total_cost_usd: 0.0,
                last_error: None,
                created_at: 0,
                closed_at: None,
            }),
            facts: WorkflowFacts {
                from_workflow: from.map(wf),
                peers: peers
                    .iter()
                    .map(|&p| MessagePeer {
                        workflow_id: wf(p),
                        sent: 1,
                        received: 1,
                    })
                    .collect(),
                ..WorkflowFacts::default()
            },
        }
    }

    fn short(links: &[Link]) -> Vec<(LinkKind, usize, usize, usize)> {
        links.iter().map(|l| (l.kind, l.a, l.b, l.lane)).collect()
    }

    #[test]
    fn a_slot_and_what_it_made_share_a_line() {
        // Rows: 1, 2 (from 1), 3, 4 (from 1).
        let rows = [
            entry(1, None, &[]),
            entry(2, Some(1), &[]),
            entry(3, None, &[]),
            entry(4, Some(1), &[]),
        ];
        let order: Vec<&SlotEntry> = rows.iter().collect();
        assert_eq!(
            short(&links(&order, &[])),
            [(LinkKind::From, 0, 1, 0), (LinkKind::From, 0, 3, 0)]
        );
    }

    #[test]
    fn overlapping_lines_take_lanes_the_longer_farther_out() {
        // 1 made 4; 2 and 3 talked (inside it), both sides listing it; 4
        // and 5 talked, touching the first line's end.
        let rows = [
            entry(1, None, &[]),
            entry(2, None, &[3]),
            entry(3, None, &[2]),
            entry(4, Some(1), &[5]),
            entry(5, None, &[4]),
        ];
        let order: Vec<&SlotEntry> = rows.iter().collect();
        assert_eq!(
            short(&links(&order, &[])),
            [
                (LinkKind::From, 0, 3, 0),
                (LinkKind::Messages, 1, 2, 1),
                (LinkKind::Messages, 3, 4, 1),
            ]
        );
    }

    #[test]
    fn a_crowded_gutter_draws_the_focused_slots_links_and_what_fits() {
        // 1 talked to 2, 3, 4, 5; 2 to 5, 3 to 4; 1 made 6.
        let rows = [
            entry(1, None, &[2, 3, 4, 5]),
            entry(2, None, &[1, 5]),
            entry(3, None, &[1, 4]),
            entry(4, None, &[1, 3]),
            entry(5, None, &[1, 2]),
            entry(6, Some(1), &[]),
        ];
        let order: Vec<&SlotEntry> = rows.iter().collect();
        let never_shared = |links: &[Link]| {
            for x in links {
                for y in links {
                    let (xl, xh) = (x.a.min(x.b), x.a.max(x.b));
                    let (yl, yh) = (y.a.min(y.b), y.a.max(y.b));
                    let same_line = x.kind == y.kind && x.a == y.a;
                    assert!(x.lane != y.lane || same_line || xh < yl || yh < xl);
                }
            }
        };
        // Nothing in focus: the longest lines first, the rest where they fit.
        let quiet = links(&order, &[]);
        never_shared(&quiet);
        assert!(quiet.len() < 7);

        // Slot 1 in focus: its four messages are one line, and all of its
        // links are drawn.
        let focused = links(&order, &["s1".to_owned()]);
        never_shared(&focused);
        let of_1: Vec<_> = focused.iter().filter(|l| l.a == 0 || l.b == 0).collect();
        assert_eq!(of_1.len(), 5);
        let lanes: BTreeSet<_> = of_1.iter().map(|l| l.lane).collect();
        assert_eq!(lanes.len(), 2);
    }

    #[test]
    fn links_to_slots_elsewhere_are_not_drawn_but_described() {
        // Slot 2 is in another window: this rail has only 1 and 3.
        let all = [
            entry(1, None, &[2]),
            entry(2, Some(3), &[1]),
            entry(3, None, &[]),
        ];
        let order: Vec<&SlotEntry> = [&all[0], &all[2]].into();
        assert!(links(&order, &[]).is_empty());
        assert_eq!(
            describe(&all[1], &all),
            ["Follow-up of slot 3", "2 messages with slot 1"]
        );
        assert_eq!(describe(&all[2], &all), ["Follow-up in slot 2"]);
    }
}
