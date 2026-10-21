//! "New workflow": pick an Origin (thought, GitHub issue or PR, a connector's
//! items), optional instructions for every round, and where it runs: a new slot
//! of some type, or an existing slot reused as it is (§6.3).

use berth_protocol::{Origin, OriginKind};
use gpui::{
    App, AppContext as _, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::scroll::ScrollableElement as _;

use super::RepoState;
use super::kit::{Btn, Size, TypeExt as _, field_frame, icon, radio, spinner};
use super::modal;
use super::sign_in::SignInDialog;
use crate::backend::{AgentFix, Command, NewWorkflow, OriginChoice, OriginSource};
use crate::keymap::{Confirm, OriginTab};
use crate::theme::{MONO, Tokens, ty};

#[derive(Clone, PartialEq, Eq)]
enum Source {
    Thought,
    List(OriginSource),
}

pub struct NewWorkflowForm {
    state: Entity<RepoState>,
    source: Source,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    custom: Entity<TextareaState>,
    search: Entity<InputState>,
    selected: Option<OriginChoice>,
    slot_type: Option<String>,
    /// `Some` to reuse a slot (`Some(None)`: none picked yet) instead of
    /// creating one.
    reuse: Option<Option<String>>,
    /// Sent already: ⌘↩ in a one-line field reaches both its own handler and
    /// the dialog's Confirm (the input passes the key on).
    submitted: bool,
    focus: FocusHandle,
    /// Signing Claude Code in, over the form (ADR 0017).
    sign_in: Option<Entity<SignInDialog>>,
}

impl EventEmitter<DismissEvent> for NewWorkflowForm {}

impl NewWorkflowForm {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("What should happen?"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("Links, acceptance criteria, things to avoid…")
        });
        let custom = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("e.g. Don't run the test suite; ask before adding dependencies.")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        for input in [&title, &search] {
            cx.subscribe_in(
                input,
                window,
                |this, _, e: &InputEvent, window, cx| match e {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.submit(window, cx),
                    InputEvent::Change | InputEvent::Focus | InputEvent::Blur => cx.notify(),
                    _ => {}
                },
            )
            .detach();
        }
        for input in [&body, &custom] {
            cx.subscribe_in(
                input,
                window,
                |this, _, e: &InputEvent, window, cx| match e {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.submit(window, cx),
                    InputEvent::Focus | InputEvent::Blur => cx.notify(),
                    _ => {}
                },
            )
            .detach();
        }
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let slot_type = {
            let s = state.read(cx);
            s.snapshot
                .default_slot
                .clone()
                .or_else(|| s.snapshot.slot_types.first().map(|t| t.name.clone()))
        };
        Self {
            state,
            source: Source::Thought,
            title,
            body,
            custom,
            search,
            selected: None,
            slot_type,
            reuse: None,
            submitted: false,
            focus: cx.focus_handle(),
            sign_in: None,
        }
    }

    /// Why the workflow's agent couldn't start, and the fix: it would run on
    /// this Mac, where Claude Code is signed out (or missing) or Node.js is.
    fn agent_blocked(&self, cx: &App) -> Option<(&'static str, AgentFix)> {
        let s = self.state.read(cx);
        let placement = match &self.reuse {
            Some(picked) => s
                .slots
                .iter()
                .find(|e| Some(&e.slot.id) == picked.as_ref())
                .map(|e| e.slot.placement.clone()),
            None => s
                .snapshot
                .slot_types
                .iter()
                .find(|t| self.slot_type.as_deref() == Some(t.name.as_str()))
                .map(|t| t.placement.clone()),
        };
        let host = placement.is_none_or(|p| p != berth_slot::Placement::Inside.as_str());
        let why = cx.try_global::<super::EnvGlobal>()?.0.agents_blocked();
        why.filter(|_| host)
    }

    fn open_sign_in(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let backend = self.state.read(cx).backend.clone();
        let dialog = cx.new(|cx| SignInDialog::new(backend, cx));
        cx.subscribe_in(&dialog, window, |this, _, _: &DismissEvent, window, cx| {
            this.sign_in = None;
            window.focus(&this.focus, cx);
            cx.notify();
        })
        .detach();
        window.focus(&dialog.read(cx).focus_handle(cx), cx);
        self.sign_in = Some(dialog);
        cx.notify();
    }

    /// Start in an existing slot (`None`: let the user pick one).
    pub fn reuse_slot(&mut self, slot_id: Option<String>, cx: &mut Context<'_, Self>) {
        let slot_id = slot_id.or_else(|| self.first_reusable(cx));
        self.reuse = Some(slot_id);
        cx.notify();
    }

    /// The selected slot if it can be reused, else the first one that can.
    fn first_reusable(&self, cx: &App) -> Option<String> {
        let s = self.state.read(cx);
        let ok = |e: &crate::backend::SlotEntry| e.slot.reuse(e.workflow.as_ref()).is_ok();
        let order = s.rail_order();
        order
            .iter()
            .find(|e| s.selected_slot.as_deref() == Some(e.slot.id.as_str()) && ok(e))
            .or_else(|| order.iter().find(|e| ok(e)))
            .map(|e| e.slot.id.clone())
    }

    /// The slot to reuse, if one is picked and still can be.
    fn reuse_target(&self, cx: &App) -> Option<String> {
        let id = self.reuse.clone().flatten()?;
        let s = self.state.read(cx);
        s.slots
            .iter()
            .find(|e| e.slot.id == id)
            .filter(|e| e.slot.reuse(e.workflow.as_ref()).is_ok())
            .map(|e| e.slot.id.clone())
    }

    /// Focus the first field of the current tab.
    pub fn focus_title(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match self.source {
            Source::Thought => self.title.update(cx, |t, cx| t.focus(window, cx)),
            Source::List(_) => self.search.update(cx, |t, cx| t.focus(window, cx)),
        }
    }

    fn sources(&self, cx: &App) -> Vec<(Source, String, &'static str)> {
        let state = self.state.read(cx);
        let mut sources = vec![
            (Source::Thought, "Thought".to_owned(), "lightbulb"),
            (
                Source::List(OriginSource::GhIssues),
                "GitHub issue".to_owned(),
                "issue",
            ),
            (
                Source::List(OriginSource::GhPrs),
                "Pull request".to_owned(),
                "pr",
            ),
        ];
        for c in state.connectors.iter().filter(|c| c.active) {
            let icon = match c.origin_kind.as_deref() {
                Some("linear") => "linear",
                Some("posthog") => "alert",
                // Other kinds start a Thought (`slots.rs`, `list_origins`).
                Some(_) => "lightbulb",
                None => continue,
            };
            sources.push((
                Source::List(OriginSource::Connector(c.name.clone())),
                c.title.clone(),
                icon,
            ));
        }
        sources
    }

    pub fn set_tab(&mut self, n: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some((s, ..)) = self.sources(cx).into_iter().nth(n) {
            self.choose(s, cx);
            self.focus_title(window, cx);
        }
    }

    fn choose(&mut self, source: Source, cx: &mut Context<'_, Self>) {
        if let Source::List(src) = &source {
            self.state.update(cx, |s, cx| s.list_origins(src, cx));
        }
        self.source = source;
        self.selected = None;
        cx.notify();
    }

    fn origin(&self, cx: &App) -> Option<Origin> {
        let custom = self.custom.read(cx).value().trim().to_owned();
        let custom = (!custom.is_empty()).then_some(custom);
        match &self.source {
            Source::Thought => {
                let title = self.title.read(cx).value().trim().to_owned();
                if title.is_empty() {
                    return None;
                }
                Some(Origin {
                    kind: OriginKind::Thought,
                    r#ref: String::new(),
                    title,
                    body: self.body.read(cx).value().trim().to_owned(),
                    custom_prompt: custom,
                    url: None,
                })
            }
            Source::List(_) => self.selected.clone().map(|c| c.into_origin(custom)),
        }
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(origin) = self.origin(cx) else {
            return;
        };
        let slot_id = self.reuse_target(cx);
        if self.submitted
            || self.reuse.is_some() && slot_id.is_none()
            || self.agent_blocked(cx).is_some()
        {
            return;
        }
        self.submitted = true;
        let s = self.state.read(cx);
        s.backend.send(Command::NewWorkflow(NewWorkflow {
            repo_id: s.repo_id().to_owned(),
            slot_type: self.slot_type.clone().filter(|_| slot_id.is_none()),
            slot_id,
            // A new slot shows in the window it was made from.
            window_id: Some(s.window_id.clone()),
            origin,
        }));
        cx.emit(DismissEvent);
    }
}

impl Focusable for NewWorkflowForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A labelled field: 12px 500 label (with an optional muted note) above it.
fn labelled(t: &Tokens, label: &str, note: Option<&str>, field: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .ty(ty::META)
                .medium()
                .text_color(t.text2)
                .child(label.to_owned())
                .children(note.map(|n| {
                    div()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(t.text3)
                        .child(format!(" · {n}"))
                })),
        )
        .child(field)
}

fn title_case(name: &str) -> String {
    match name {
        n if n.len() <= 3 => n.to_uppercase(),
        "docker" => "Container".to_owned(),
        n => {
            let mut c = n.chars();
            c.next().map_or_else(String::new, |f| {
                f.to_uppercase().collect::<String>() + c.as_str()
            })
        }
    }
}

impl Render for NewWorkflowForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if let Some(d) = &self.sign_in {
            return d.clone().into_any_element();
        }
        let t = Tokens::get(cx);
        let sources = self.sources(cx);
        let state = self.state.read(cx);
        let gh_ok = state.snapshot.gh.logged_in;
        let focused = |s: &Entity<InputState>| s.read(cx).focus_handle(cx).is_focused(window);
        let focused_area =
            |s: &Entity<TextareaState>| s.read(cx).focus_handle(cx).is_focused(window);

        let mut tabs = div()
            .flex()
            .items_end()
            .gap(px(4.))
            .px(px(20.))
            .pt(px(12.))
            .border_b_1()
            .border_color(t.border);
        for (source, label, ico) in sources.iter().cloned() {
            let active = source == self.source;
            let id = SharedString::from(format!("src-{label}"));
            tabs = tabs.child(
                div()
                    .id(id)
                    .h(px(34.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_b_2()
                    .border_color(if active {
                        t.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .text_size(px(13.))
                    .cursor_pointer()
                    .when(active, |d| d.text_color(t.text).semibold())
                    .when(!active, |d| d.text_color(t.text2))
                    .child(icon(ico, 14., if active { t.text } else { t.text2 }))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose(source.clone(), cx);
                        this.focus_title(window, cx);
                    })),
            );
        }
        tabs = tabs.child(
            div()
                .ml_auto()
                .self_center()
                .ty(ty::META)
                .text_color(t.text3)
                .font_family(MONO)
                .child(format!("⌘1–{}", sources.len())),
        );

        let what: gpui::AnyElement = match &self.source {
            Source::Thought => div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(labelled(
                    &t,
                    "What should happen?",
                    None,
                    field_frame(&t, focused(&self.title), false)
                        .h(px(38.))
                        .px(px(12.))
                        .items_center()
                        .child(super::kit::bare_input(&self.title)),
                ))
                .child(labelled(
                    &t,
                    "Details, context, constraints",
                    Some("optional"),
                    field_frame(&t, focused_area(&self.body), false)
                        .min_h(px(82.))
                        .px(px(2.))
                        .child(super::kit::textarea_sized(
                            &self.body,
                            gpui_component::Size::Medium,
                        )),
                ))
                .into_any_element(),
            Source::List(src) if !gh_ok && !matches!(src, OriginSource::Connector(_)) => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .ty(ty::META)
                .text_color(t.danger)
                .child(icon("alert-bold", 11., t.danger))
                .child("GitHub CLI isn't signed in. Run gh auth login, then try again.")
                .into_any_element(),
            Source::List(src) => {
                let query = self.search.read(cx).value().to_lowercase();
                let updating =
                    state.origins_loading.contains(src) && state.origins.contains_key(src);
                let list: gpui::AnyElement = match state.origins.get(src) {
                    None => div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .p(px(12.))
                        .ty(ty::META)
                        .text_color(t.text3)
                        .child(spinner("origins-spin", 12, t.accent))
                        .child(format!("Loading {}…", src.label()))
                        .into_any_element(),
                    Some(Err(e)) => div()
                        .p(px(12.))
                        .ty(ty::META)
                        .text_color(t.danger)
                        .child(e.clone())
                        .into_any_element(),
                    Some(Ok(items)) => {
                        let mut col = div()
                            .id("origin-list")
                            .flex()
                            .flex_col()
                            .max_h(px(220.))
                            .overflow_y_scrollbar()
                            .gap(px(2.));
                        let filtered: Vec<&OriginChoice> = items
                            .iter()
                            .filter(|i| {
                                // What the row shows: a PostHog row shows the message, not the id.
                                let shown = if i.kind == OriginKind::Posthog {
                                    i.body.lines().next().unwrap_or_default()
                                } else {
                                    &i.r#ref
                                };
                                query.is_empty()
                                    || i.title.to_lowercase().contains(&query)
                                    || shown.to_lowercase().contains(&query)
                            })
                            .collect();
                        if filtered.is_empty() {
                            col = col.child(
                                div()
                                    .p(px(12.))
                                    .ty(ty::META)
                                    .text_color(t.text3)
                                    .child("Nothing here."),
                            );
                        }
                        for (i, item) in filtered.into_iter().enumerate() {
                            let selected = self
                                .selected
                                .as_ref()
                                .is_some_and(|s| s.r#ref == item.r#ref);
                            let choice = item.clone();
                            let hover = t.raised;
                            let group = SharedString::from(format!("origin-{i}"));
                            // PostHog refs are issue ids: show the error's message instead.
                            let error = item.kind == OriginKind::Posthog;
                            let preview = error
                                .then(|| item.body.lines().next().unwrap_or_default().to_owned())
                                .filter(|p| !p.trim().is_empty());
                            col = col.child(
                                div()
                                    .id(group.clone())
                                    .group(group.clone())
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(px(10.))
                                    .min_h(px(32.))
                                    .px(px(10.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .cursor_pointer()
                                    .when(selected, |d| d.border_color(t.accent).bg(t.accent_soft))
                                    .when(!selected, |d| {
                                        d.border_color(gpui::transparent_black())
                                            .hover(move |s| s.bg(hover))
                                    })
                                    .on_click(cx.listener(
                                        move |this, e: &ClickEvent, window, cx| {
                                            this.selected = Some(choice.clone());
                                            if e.click_count() >= 2 {
                                                this.submit(window, cx);
                                            }
                                            cx.notify();
                                        },
                                    ))
                                    .when(!error, |d| {
                                        d.child(
                                            div()
                                                .min_w(px(56.))
                                                .flex_none()
                                                .whitespace_nowrap()
                                                .font_family(MONO)
                                                .ty(ty::META)
                                                .text_color(t.text3)
                                                .child(item.r#ref.clone()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .items_baseline()
                                            .gap(px(8.))
                                            .whitespace_nowrap()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .child(item.title.clone()),
                                            )
                                            .when_some(preview, |d, p| {
                                                d.child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .ty(ty::META)
                                                        .text_color(t.text3)
                                                        .child(p),
                                                )
                                            }),
                                    )
                                    .when_some(item.url.clone(), |d, url| {
                                        // Shown on hover and on the selected row.
                                        d.child(
                                            div()
                                                .flex_none()
                                                .when(!selected, |d| {
                                                    d.opacity(0.).group_hover(group.clone(), |s| {
                                                        s.opacity(1.)
                                                    })
                                                })
                                                .child(
                                                    Btn::new(SharedString::from(format!(
                                                        "origin-open-{i}"
                                                    )))
                                                    .ghost()
                                                    .size(Size::S22)
                                                    .icon("external-link", 14.)
                                                    .tooltip(super::nodes::open_origin_label(
                                                        item.kind,
                                                    ))
                                                    .on_click(move |_, _, cx| {
                                                        cx.stop_propagation();
                                                        cx.open_url(&url);
                                                    }),
                                                ),
                                        )
                                    }),
                            );
                        }
                        col.into_any_element()
                    }
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        field_frame(&t, focused(&self.search), false)
                            .h(px(34.))
                            .px(px(10.))
                            .gap(px(8.))
                            .items_center()
                            .child(super::kit::bare_input(&self.search))
                            // The last list stays while a newer one loads.
                            .when(updating, |d| {
                                d.child(spinner("origins-updating", 10, t.text3))
                            }),
                    )
                    .child(list)
                    .into_any_element()
            }
        };

        let mut slots = div().grid().grid_cols(3).gap(px(8.));
        let default = state
            .snapshot
            .default_slot
            .clone()
            .or_else(|| state.snapshot.slot_types.first().map(|t| t.name.clone()));
        for st in &state.snapshot.slot_types {
            let active = self.slot_type.as_deref() == Some(st.name.as_str());
            let name = st.name.clone();
            let hover = t.border_strong;
            slots = slots.child(
                div()
                    .id(SharedString::from(format!("slot-type-{}", st.name)))
                    .tooltip({
                        let tip: SharedString = format!(
                            "{} · agent runs {}{}",
                            st.name,
                            if st.placement == "inside" {
                                "inside the slot"
                            } else {
                                "on this Mac"
                            },
                            if st.from_repo {
                                " · defined by this repository"
                            } else {
                                ""
                            }
                        )
                        .into();
                        move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                    })
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .px(px(12.))
                    .py(px(10.))
                    .min_h(px(44.))
                    .rounded(px(6.))
                    .border_1()
                    .cursor_pointer()
                    .when(active, |d| d.border_color(t.accent).bg(t.accent_soft))
                    .when(!active, |d| {
                        d.border_color(t.border)
                            .hover(move |s| s.border_color(hover))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.slot_type = Some(name.clone());
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(radio(&t, active, 14.))
                            .child(div().semibold().child(title_case(&st.name)))
                            .when(default.as_deref() == Some(st.name.as_str()), |d| {
                                d.child(
                                    div()
                                        .ml_auto()
                                        .text_size(px(11.))
                                        .text_color(t.text3)
                                        .child("default"),
                                )
                            }),
                    )
                    .when(!st.description.is_empty(), |d| {
                        d.child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text2)
                                .child(st.description.clone()),
                        )
                    }),
            );
        }
        let no_slot_types = state.snapshot.slot_types.is_empty();

        // Reuse: the repo's slots, each kept exactly as it is.
        let reusing = self.reuse.is_some();
        let picked = self.reuse.clone().flatten();
        let mut closes: Option<String> = None;
        let mut reuse_list = div()
            .id("reuse-list")
            .flex()
            .flex_col()
            .gap(px(6.))
            .max_h(px(196.))
            .overflow_y_scroll();
        for entry in state.rail_order() {
            let reuse = entry.slot.reuse(entry.workflow.as_ref());
            let on = picked.as_deref() == Some(entry.slot.id.as_str()) && reuse.is_ok();
            if on && reuse == Ok(true) {
                closes = entry.workflow.as_ref().map(|w| w.origin.title.clone());
            }
            let (title, sub) = super::slot_rail::rail_lines(entry, state);
            let meta = match reuse {
                Err(blocked) => blocked.reason().to_owned(),
                Ok(_) => format!("{} · {} · {sub}", entry.slot.slot_type, entry.slot.branch),
            };
            let id = entry.slot.id.clone();
            let hover = t.border_strong;
            reuse_list = reuse_list.child(
                div()
                    .id(SharedString::from(format!("reuse-{id}")))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .when(on, |d| d.border_color(t.accent).bg(t.accent_soft))
                    .when(!on, |d| d.border_color(t.border))
                    .map(|d| match reuse {
                        Ok(_) => d
                            .cursor_pointer()
                            .when(!on, |d| d.hover(move |s| s.border_color(hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.reuse = Some(Some(id.clone()));
                                cx.notify();
                            })),
                        Err(_) => d.opacity(0.55),
                    })
                    .child(radio(&t, on, 14.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .semibold()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .ty(ty::META)
                                    .text_color(t.text2)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(meta),
                            ),
                    ),
            );
        }
        let mode_tab = |id: &'static str, label: &'static str, on: bool| {
            let hover = t.text;
            div()
                .id(id)
                .h(px(22.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .ty(ty::META)
                .cursor_pointer()
                .when(on, |d| d.bg(t.win).text_color(t.text).medium())
                .when(!on, |d| {
                    d.text_color(t.text2).hover(move |s| s.text_color(hover))
                })
                .child(label)
        };
        let has_slots = !state.slots.is_empty();
        let blocked = self.agent_blocked(cx);
        let ready = self.origin(cx).is_some()
            && blocked.is_none()
            && if reusing {
                self.reuse_target(cx).is_some()
            } else {
                !no_slot_types
            };
        let this = cx.entity().downgrade();
        let close = this.clone();

        modal::frame(&t, 602.)
            .id("new-workflow")
            .track_focus(&self.focus)
            .key_context("Dialog NewWorkflow")
            .on_action(cx.listener(|this, a: &OriginTab, window, cx| this.set_tab(a.0, window, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.submit(window, cx)))
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                }
            }))
            .child(div().px(px(20.)).pt(px(16.)).child(modal::header(&t, "New workflow", None, move |_, cx| {
                let _ = close.update(cx, |_, cx| cx.emit(DismissEvent));
            })))
            .child(tabs)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .px(px(20.))
                    .py(px(16.))
                    .child(what)
                    .child(labelled(
                        &t,
                        "Instructions for the agent",
                        Some("optional, applies to every round"),
                        field_frame(&t, focused_area(&self.custom), false)
                            .min_h(px(62.))
                            .px(px(2.))
                                .child(super::kit::textarea_sized(&self.custom, gpui_component::Size::Medium)),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .pt(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().ty(ty::META).medium().text_color(t.text2).child("Slot"))
                                    .child(div().ty(ty::META).text_color(t.text3).child(if reusing {
                                        "its files stay as they are; the agent starts fresh"
                                    } else {
                                        "where this agent works"
                                    }))
                                    .when(has_slots, |d| {
                                        d.child(
                                            div()
                                                .ml_auto()
                                                .flex()
                                                .gap(px(2.))
                                                .p(px(2.))
                                                .bg(t.raised)
                                                .rounded(px(6.))
                                                .child(mode_tab("slot-new", "New slot", !reusing).on_click(
                                                    cx.listener(|this, _, _, cx| {
                                                        this.reuse = None;
                                                        cx.notify();
                                                    }),
                                                ))
                                                .child(mode_tab("slot-reuse", "Reuse a slot", reusing).on_click(
                                                    cx.listener(|this, _, _, cx| {
                                                        if this.reuse.is_none() {
                                                            this.reuse_slot(None, cx);
                                                        }
                                                    }),
                                                )),
                                        )
                                    }),
                            )
                            .child(if reusing {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
                                    .child(reuse_list)
                                    .when_some(closes, |d, title| {
                                        d.child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(6.))
                                                .ty(ty::META)
                                                .text_color(t.text2)
                                                .child(super::kit::dot(6., t.attention))
                                                .child(format!("Its open workflow, “{title}”, will be closed.")),
                                        )
                                    })
                                    .into_any_element()
                            } else if no_slot_types {
                                div()
                                    .ty(ty::META)
                                    .text_color(t.danger)
                                    .child("No slot types installed. Add one under ~/.config/berth/slots/<type>/.")
                                    .into_any_element()
                            } else {
                                slots.into_any_element()
                            }),
                    ),
            )
            .child(
                modal::footer(
                    &t,
                    match blocked {
                        Some((why, _)) => div().text_color(t.attention).child(why),
                        None => div().child(if reusing {
                            "The agent takes the first round in the slot's files as they are."
                        } else {
                            "The agent takes the first round as soon as the slot exists."
                        }),
                    },
                )
                    .when_some(blocked, |d, (_, fix)| {
                        d.child(
                            Btn::new("nw-fix")
                                .size(Size::S32)
                                .label(super::sign_in::fix_label(fix))
                                .on_click(cx.listener(move |this, _, window, cx| match fix {
                                    AgentFix::SignIn => this.open_sign_in(window, cx),
                                    AgentFix::GetNode => cx.open_url(super::sign_in::NODE_URL),
                                })),
                        )
                    })
                    .child(
                        Btn::new("nw-cancel")
                            .size(Size::S32)
                            .label("Cancel")
                            .hint("esc")
                            .on_click(move |_, _, cx| {
                                let _ = this.update(cx, |_, cx| cx.emit(DismissEvent));
                            }),
                    )
                    .child(
                        Btn::new("create-workflow")
                            .primary()
                            .size(Size::S32)
                            .label("Create workflow")
                            .hint("⌘↩")
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .into_any_element()
    }
}
