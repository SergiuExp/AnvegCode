//! An app window: a row of project tabs across the top and the shown
//! project below it. Every open project is a tab of exactly one app
//! window; there is one window unless the user moves a project to another
//! (Window ▸ Move Project to New Window, or a tab's context menu).
//!
//! The app draws the tabs itself (`window_tabs.rs`); macOS's own window
//! tabs are off, so a window never splits into several.
//!
//! A project's views belong to the window they were made in (their focus
//! listeners are that window's), so moving a project to another window
//! makes it again there from its saved state, and its running sessions
//! resume (`windows::move_project`).

use anveg_core::config::{AppWindowState, SavedBounds, WindowState};
use gpui_cn::MenuState;
use gpui_kit::{
    AppContext as _, Context, Entity, EntityId, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Window, WindowBounds, div,
};
use uuid::Uuid;

use crate::actions::*;
use crate::model::{AppModel, ModelEvent, OpenAppWindow, OpenWindow, model};
use crate::project_window::{ProjectWindow, ProjectWindowEvent};
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt};

/// A project to open as a tab.
pub struct ProjectToOpen {
    pub project_id: Uuid,
    pub saved: Option<WindowState>,
    /// Sessions to resume once Claude Code is found.
    pub resume: Vec<String>,
}

pub struct AppWindow {
    /// Stable for the window's life (menus name windows by it).
    pub id: Uuid,
    /// The name the user gave it (Window ▸ Rename Window…).
    pub name: Option<String>,
    /// The project tabs, in order.
    pub projects: Vec<Entity<ProjectWindow>>,
    /// The shown tab.
    pub active: usize,
    model: Entity<AppModel>,
    bounds: Option<SavedBounds>,
    /// The title last given to the window.
    title: String,
    /// Closing the whole window: each project that agrees is closed, and
    /// one that asks first (running sessions, unsaved files) continues
    /// the close when the user confirms.
    pub closing: bool,
    /// The pointer is over the full-screen window buttons (they show
    /// their glyphs, as macOS's do).
    pub buttons_hovered: bool,
    /// Saved full screen and on its way there (`windows::enter_full_screen`):
    /// saved as full screen meanwhile, so a quit or a failed attempt does
    /// not forget it.
    pub restore_fullscreen: bool,
    pub tab_menu: Entity<MenuState>,
    project_subscriptions: Vec<(EntityId, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl AppWindow {
    pub fn new(
        id: Uuid,
        name: Option<String>,
        projects: Vec<ProjectToOpen>,
        active: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = model(cx);
        let tab_menu = cx.new(MenuState::new);
        let mut subscriptions = vec![cx.observe(&tab_menu, |_, _, cx| cx.notify())];
        subscriptions.push(cx.subscribe_in(&model, window, |this, _, event, window, cx| {
            match event {
                ModelEvent::ProjectsChanged => {
                    this.update_title(window, cx);
                    cx.notify();
                }
                // The tabs show other projects' names and whether they
                // need input.
                ModelEvent::AttentionChanged | ModelEvent::WindowsChanged => cx.notify(),
                _ => {}
            }
        }));
        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.bounds = Some(saved_bounds(window));
            this.save_soon(cx);
        }));
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                let id = this.id;
                this.model.update(cx, |m, _| m.last_active_window = Some(id));
                // Window ▸ Move Project to Window lists the other windows.
                cx.defer(|cx| crate::menus::install(cx));
            }
        }));
        let entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            entity
                .update(cx, |this, cx| this.close_all(window, cx))
                .unwrap_or(true)
        });
        cx.on_release(move |this: &mut Self, cx| {
            let model = this.model.clone();
            cx.defer(move |cx| model.update(cx, |m, cx| m.app_window_closed(id, cx)));
        })
        .detach();
        let me = cx.entity().downgrade();
        model.update(cx, |m, cx| {
            m.register_app_window(
                OpenAppWindow {
                    id,
                    handle: window.window_handle(),
                    view: me,
                },
                cx,
            );
            m.last_active_window = Some(id);
        });
        crate::platform::mark_project_window(window);

        let mut this = Self {
            id,
            name,
            projects: Vec::new(),
            active: 0,
            model,
            bounds: Some(saved_bounds(window)),
            title: String::new(),
            closing: false,
            buttons_hovered: false,
            restore_fullscreen: false,
            tab_menu,
            project_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        let active = active.min(projects.len().saturating_sub(1));
        for (i, p) in projects.into_iter().enumerate() {
            this.add_project(p, i == active, window, cx);
        }
        this.update_title(window, cx);
        this
    }

    // -----------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------

    /// Add a project as the last tab; `show` makes it the shown one.
    pub fn add_project(
        &mut self,
        p: ProjectToOpen,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ProjectWindow> {
        let ProjectToOpen {
            project_id,
            saved,
            resume,
        } = p;
        let view = cx.new(|cx| ProjectWindow::new(project_id, saved, resume, show, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
            ProjectWindowEvent::Close => this.project_closed(view.entity_id(), window, cx),
        });
        self.project_subscriptions.push((view.entity_id(), sub));
        let host = cx.entity().downgrade();
        let handle = window.window_handle();
        self.model.update(cx, |m, cx| {
            m.register_window(
                OpenWindow {
                    project_id,
                    handle,
                    view: view.downgrade(),
                    host,
                },
                cx,
            );
        });
        self.projects.push(view.clone());
        if show {
            let shown = self.projects.len() - 1;
            if shown != self.active
                && let Some(old) = self.projects.get(self.active).cloned()
            {
                old.update(cx, |p, cx| p.set_shown(false, window, cx));
            }
            self.active = shown;
        }
        self.after_tabs_changed(window, cx);
        view
    }

    /// Show tab `index`.
    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.projects.len() {
            return;
        }
        if index != self.active {
            if let Some(old) = self.projects.get(self.active).cloned() {
                old.update(cx, |p, cx| p.set_shown(false, window, cx));
            }
            self.active = index;
        }
        let view = self.projects[index].clone();
        view.update(cx, |p, cx| p.set_shown(true, window, cx));
        self.save_soon(cx);
        cx.notify();
    }

    /// Show a project's tab, if it is in this window.
    pub fn activate_project(&mut self, project_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(project_id, cx) {
            self.activate(i, window, cx);
        }
    }

    pub fn index_of(&self, project_id: Uuid, cx: &gpui_kit::App) -> Option<usize> {
        self.projects
            .iter()
            .position(|p| p.read(cx).project_id == project_id)
    }

    pub fn active_project(&self) -> Option<&Entity<ProjectWindow>> {
        self.projects.get(self.active)
    }

    /// Step through the tabs (wrapping).
    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.projects.len() as isize;
        if n < 2 {
            return;
        }
        let next = (self.active as isize + delta).rem_euclid(n) as usize;
        self.closing = false;
        self.activate(next, window, cx);
    }

    /// Take tab `index` out of the window (closed or moved away). The
    /// window closes when it was the last.
    pub fn remove_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.projects.len() {
            return;
        }
        let removed = self.projects.remove(index);
        let removed_id = removed.entity_id();
        self.project_subscriptions.retain(|(id, _)| *id != removed_id);
        drop(removed);
        if self.projects.is_empty() {
            self.active = 0;
            window.remove_window();
            return;
        }
        if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = index.min(self.projects.len() - 1);
            let view = self.projects[self.active].clone();
            view.update(cx, |p, cx| p.set_shown(true, window, cx));
        }
        self.after_tabs_changed(window, cx);
    }

    fn after_tabs_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_title(window, cx);
        self.save_soon(cx);
        let model = self.model.clone();
        cx.defer(move |cx| {
            model.update(cx, |_, cx| cx.emit(ModelEvent::WindowsChanged));
            crate::menus::install(cx);
        });
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Closing
    // -----------------------------------------------------------------

    /// ⌘W, or × on a tab: close one project (it may ask first).
    pub fn close_project(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.closing = false;
        let Some(view) = self.projects.get(index).cloned() else {
            return;
        };
        if index != self.active {
            // Its questions show in its tab.
            self.activate(index, window, cx);
        }
        view.update(cx, |p, cx| p.close(window, cx));
    }

    /// A project agreed to close (after its own questions).
    fn project_closed(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.projects.iter().position(|p| p.entity_id() == id) else {
            return;
        };
        let closing = self.closing && self.projects.len() > 1;
        self.remove_at(index, window, cx);
        if closing && self.close_all(window, cx) {
            window.remove_window();
        }
    }

    /// Close every project, asking as ⌘W does. True when all closed (the
    /// window can go); otherwise the project that asks is shown.
    pub fn close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.closing = true;
        while let Some(first) = self.projects.first().cloned() {
            if !first.update(cx, |p, cx| p.should_close(window, cx)) {
                self.activate(0, window, cx);
                return false;
            }
            if self.projects.len() == 1 {
                // The window closes with it.
                return true;
            }
            self.remove_at(0, window, cx);
        }
        true
    }

    // -----------------------------------------------------------------
    // Name and state
    // -----------------------------------------------------------------

    /// The project names, in tab order.
    pub fn project_names(&self, cx: &gpui_kit::App) -> Vec<String> {
        self.projects
            .iter()
            .map(|p| p.read(cx).project_name(cx))
            .collect()
    }

    /// The window's name: the user's, or its projects'.
    pub fn display_name(&self, cx: &gpui_kit::App) -> String {
        match &self.name {
            Some(name) => name.clone(),
            None => auto_name(&self.project_names(cx)),
        }
    }

    fn update_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut title = self.display_name(cx);
        if self.model.read(cx).paths.is_sandbox() {
            title.push_str(" (Dev)");
        }
        if title == self.title {
            return;
        }
        window.set_window_title(&title);
        self.title = title;
        // Setting a title puts the window back in macOS's own window list
        // (`changeWindowsItem`); the app lists project windows itself.
        crate::platform::keep_project_windows_out_of_window_list();
    }

    pub fn set_name(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.name = name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty());
        self.after_tabs_changed(window, cx);
    }

    /// Window ▸ Rename Window…: asked in the shown project.
    pub fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.active_project().cloned() else {
            return;
        };
        let weak = cx.entity().downgrade();
        let current = self.name.clone().unwrap_or_default();
        let prompt = Prompt::new(
            "Rename Window",
            "Leave it empty to name the window after its projects.",
        )
        .input(auto_name(&self.project_names(cx)), current)
        .cancel()
        .button("Rename", ButtonStyle::Primary, move |value, window, cx| {
            let _ = weak.update(cx, |this, cx| this.set_name(value, window, cx));
            Ok(())
        });
        view.update(cx, |p, cx| p.show_prompt(prompt, window, cx));
    }

    /// The window's entry in `state.json`.
    pub fn window_state(&self, cx: &gpui_kit::App) -> AppWindowState {
        AppWindowState {
            name: self.name.clone(),
            bounds: self.bounds(),
            projects: self
                .projects
                .iter()
                .map(|p| p.read(cx).project_id)
                .collect(),
            active: self.active,
        }
    }

    pub fn bounds(&self) -> Option<SavedBounds> {
        self.bounds.map(|mut b| {
            b.fullscreen |= self.restore_fullscreen;
            b
        })
    }

    fn save_soon(&self, cx: &mut Context<Self>) {
        let model = self.model.clone();
        cx.defer(move |cx| model.update(cx, |m, cx| m.schedule_save(cx)));
    }

    /// Move the shown project (deferred: moving updates other windows).
    fn move_active(&mut self, to: crate::windows::MoveTarget, cx: &mut Context<Self>) {
        let Some(view) = self.active_project() else {
            return;
        };
        let project_id = view.read(cx).project_id;
        cx.defer(move |cx| crate::windows::move_project(project_id, to, cx));
    }
}

impl Render for AppWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let tabs = self.render_tabs_row(window, cx);
        let shown = self.active_project().cloned();
        div()
            .id("app-window")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .on_action(cx.listener(|this, _: &CloseProject, window, cx| {
                this.close_project(this.active, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                if this.close_all(window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|this, _: &NextProject, window, cx| this.step(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousProject, window, cx| this.step(-1, window, cx)))
            .on_action(cx.listener(|this, _: &MoveProjectToNewWindow, _, cx| {
                this.move_active(crate::windows::MoveTarget::NewWindow, cx)
            }))
            .on_action(cx.listener(|this, action: &MoveProjectToWindow, _, cx| {
                this.move_active(crate::windows::MoveTarget::Window(action.window), cx)
            }))
            .on_action(cx.listener(|this, _: &MergeAllWindows, _, cx| {
                let id = this.id;
                cx.defer(move |cx| crate::windows::merge_all_into(id, cx));
            }))
            .on_action(cx.listener(|this, _: &RenameWindow, window, cx| this.begin_rename(window, cx)))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
            .child(tabs)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .children(shown.map(|view| div().flex_1().min_h_0().child(view))),
            )
    }
}

/// A window named after its projects: the first two, then how many more.
pub fn auto_name(names: &[String]) -> String {
    match names {
        [] => "Anveg Code".to_owned(),
        [one] => one.clone(),
        [a, b] => format!("{a}, {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} +{}", rest.len()),
    }
}

pub fn saved_bounds(window: &Window) -> SavedBounds {
    let (bounds, maximized, fullscreen) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (b, false, false),
        WindowBounds::Maximized(b) => (b, true, false),
        WindowBounds::Fullscreen(b) => (b, false, true),
    };
    SavedBounds {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width).max(400.),
        height: f32::from(bounds.size.height).max(300.),
        maximized,
        fullscreen,
    }
}

#[cfg(test)]
mod tests {
    use super::auto_name;

    #[test]
    fn windows_are_named_after_their_projects() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(auto_name(&[]), "Anveg Code");
        assert_eq!(auto_name(&names(&["aiaiai"])), "aiaiai");
        assert_eq!(auto_name(&names(&["aiaiai", "fairturn"])), "aiaiai, fairturn");
        assert_eq!(
            auto_name(&names(&["aiaiai", "fairturn", "TinyTimer", "go_proj"])),
            "aiaiai, fairturn +2"
        );
    }
}
