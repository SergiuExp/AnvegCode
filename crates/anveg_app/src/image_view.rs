//! An image file in a document tab: PNG, JPEG, GIF (animated), SVG, WebP,
//! BMP, ICO, TIFF and Netpbm.
//!
//! - Fit (shrinks large images to the tab, never enlarges small ones) or a
//!   zoom level: 100% is one image pixel per point; − and + step through
//!   common levels. Larger than the tab, it scrolls.
//! - A light or dark backdrop, to see transparent parts either way.
//! - The info bar: format, pixel size, file size.
//! - Reloads when the file changes on disk.
//! - SVG: Image / Source. Source is the ordinary code editor (autosave
//!   included), and the image follows each save.

use std::path::PathBuf;
use std::sync::Arc;

use anveg_core::fswatch;
use anveg_core::images::{self, Kind};
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Image,
    ImageFormat, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, Window,
    div, img, px,
};
use uuid::Uuid;

use crate::editor_view::EditorView;
use crate::model::model;
use crate::ui::Colors;
use crate::workspace::WorkspaceEvent;

/// The zoom steps of − and +.
const ZOOMS: [f32; 12] = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 8.0];

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    Fit,
    Scale(f32),
}

struct Loaded {
    image: Arc<Image>,
    size: Option<(u32, u32)>,
    bytes: u64,
}

pub struct ImageView {
    pub path: PathBuf,
    kind: Kind,
    project: Uuid,
    roots: Vec<PathBuf>,
    loaded: Option<Loaded>,
    error: Option<String>,
    zoom: Zoom,
    dark: bool,
    /// SVG only: the source editor, made when first shown.
    source: Option<Entity<EditorView>>,
    show_source: bool,
    focus: FocusHandle,
    _watch: Option<fswatch::Subscription>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl EventEmitter<WorkspaceEvent> for ImageView {}

impl Focusable for ImageView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.source {
            Some(editor) if self.show_source => editor.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        }
    }
}

fn format_of(kind: Kind) -> ImageFormat {
    match kind {
        Kind::Png => ImageFormat::Png,
        Kind::Jpeg => ImageFormat::Jpeg,
        Kind::Gif => ImageFormat::Gif,
        Kind::Svg => ImageFormat::Svg,
        Kind::Webp => ImageFormat::Webp,
        Kind::Bmp => ImageFormat::Bmp,
        Kind::Ico => ImageFormat::Ico,
        Kind::Tiff => ImageFormat::Tiff,
        Kind::Pnm => ImageFormat::Pnm,
    }
}

impl ImageView {
    /// `path` should be one `images::kind_for` knows.
    pub fn new(
        path: PathBuf,
        project: Uuid,
        roots: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let kind = images::kind_for(&path).unwrap_or(Kind::Png);
        let dark = cx.theme().is_dark();
        let mut this = Self {
            path,
            kind,
            project,
            roots,
            loaded: None,
            error: None,
            zoom: Zoom::Fit,
            dark,
            source: None,
            show_source: false,
            focus: cx.focus_handle(),
            _watch: None,
            _tasks: Vec::new(),
            _subs: Vec::new(),
        };
        this.load(cx);
        this.watch(window, cx);
        this
    }

    pub fn title(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// The SVG source editor, when it exists (for unsaved-change checks).
    pub fn editor(&self) -> Option<Entity<EditorView>> {
        self.source.clone()
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (path, kind) = (self.path.clone(), self.kind);
        let task = cx.background_spawn(async move {
            let len = std::fs::metadata(&path)?.len();
            if len > images::MAX_BYTES {
                anyhow::bail!(
                    "This image is {}; images over {} are not shown.",
                    images::human_size(len),
                    images::human_size(images::MAX_BYTES)
                );
            }
            let bytes = std::fs::read(&path)?;
            let size = images::dimensions(kind, &bytes);
            Ok(Loaded {
                bytes: bytes.len() as u64,
                size,
                image: Arc::new(Image::from_bytes(format_of(kind), bytes)),
            })
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result: anyhow::Result<Loaded> = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(loaded) => {
                        this.loaded = Some(loaded);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        }));
    }

    fn watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(service) = model(cx).read(cx).fswatch.clone() else {
            return;
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        self._watch = Some(service.subscribe(&self.path, false, move |_| {
            let _ = tx.unbounded_send(());
        }));
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            while rx.next().await.is_some() {
                while rx.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.load(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn set_source(&mut self, show: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.show_source = show;
        if show && self.source.is_none() {
            let (path, project, roots) = (self.path.clone(), self.project, self.roots.clone());
            let editor = cx.new(|cx| EditorView::new(path, project, roots, window, cx));
            // The editor's toasts and links go to the window like any tab's.
            self._subs.push(cx.subscribe(&editor, |_, _, event: &WorkspaceEvent, cx| {
                cx.emit(event.clone());
            }));
            self.source = Some(editor);
        }
        let focus = self.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn step(&mut self, up: bool, cx: &mut Context<Self>) {
        let current = match self.zoom {
            Zoom::Scale(z) => z,
            // From Fit, step from the scale Fit shows (≤ 100%).
            Zoom::Fit => 1.0,
        };
        let next = if up {
            ZOOMS.iter().copied().find(|z| *z > current + 0.001)
        } else {
            ZOOMS.iter().rev().copied().find(|z| *z < current - 0.001)
        };
        if let Some(z) = next {
            self.zoom = Zoom::Scale(z);
            cx.notify();
        }
    }

    fn render_image(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let c = Colors::of(cx);
        let backdrop = if self.dark {
            gpui_kit::rgb(0x1e1e1e)
        } else {
            gpui_kit::rgb(0xf4f4f4)
        };
        let muted = c.muted_foreground;
        let Some(loaded) = &self.loaded else {
            let text = self.error.clone().unwrap_or_else(|| "Loading…".into());
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(if self.error.is_some() { c.destructive } else { muted })
                .child(text)
                .into_any_element();
        };
        let kind = self.kind;
        let fallback = move || {
            div()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(format!("This {} could not be decoded.", kind.label()))
                .into_any_element()
        };
        // The id gives the element state GPUI needs to animate GIFs.
        let picture = img(loaded.image.clone()).id("image").with_fallback(fallback);
        let body = match (self.zoom, loaded.size) {
            (Zoom::Scale(z), Some((w, h))) => div()
                .id("image-scroll")
                .flex_1()
                .min_h_0()
                .overflow_scroll()
                .child(
                    // Auto margins: centred while smaller than the tab,
                    // flush (and fully scrollable) when larger.
                    div().min_w_full().min_h_full().flex().p_4().child(
                        picture
                            .flex_none()
                            .m_auto()
                            .w(px(w as f32 * z))
                            .h(px(h as f32 * z))
                            .object_fit(ObjectFit::Fill),
                    ),
                )
                .into_any_element(),
            // Fit (or a zoom without a known size).
            _ => div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .flex_col()
                .child(
                    picture
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .object_fit(ObjectFit::ScaleDown),
                )
                .into_any_element(),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(backdrop)
            .child(body)
            .into_any_element()
    }
}

impl Render for ImageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let mut info = vec![self.kind.label().to_owned()];
        if let Some(loaded) = &self.loaded {
            if let Some((w, h)) = loaded.size {
                info.push(format!("{w} × {h}"));
            }
            info.push(images::human_size(loaded.bytes));
        }
        let small = |id: &'static str, label: SharedString| {
            Button::new(id).ghost().size(ButtonSize::Sm).label(label)
        };
        let is_svg = self.kind == Kind::Svg;
        let source = self.show_source;
        let mut bar = div()
            .flex_none()
            .h(px(32.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(c.border)
            .text_sm();
        if is_svg {
            bar = bar
                .child(
                    small("img-show-image", "Image".into())
                        .toggled(!source)
                        .on_click(cx.listener(|this, _, window, cx| this.set_source(false, window, cx))),
                )
                .child(
                    small("img-show-source", "Source".into())
                        .toggled(source)
                        .on_click(cx.listener(|this, _, window, cx| this.set_source(true, window, cx))),
                )
                .child(div().w(px(1.)).h(px(16.)).bg(c.border));
        }
        if !source {
            let zoom_label = match self.zoom {
                Zoom::Fit => String::new(),
                Zoom::Scale(z) => format!("{:.0}%", z * 100.0),
            };
            let can_zoom = self.loaded.as_ref().is_some_and(|l| l.size.is_some());
            bar = bar
                .child(
                    small("img-fit", "Fit".into())
                        .toggled(self.zoom == Zoom::Fit)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.zoom = Zoom::Fit;
                            cx.notify();
                        })),
                )
                .child(
                    small("img-100", "100%".into())
                        .toggled(self.zoom == Zoom::Scale(1.0))
                        .disabled(!can_zoom)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.zoom = Zoom::Scale(1.0);
                            cx.notify();
                        })),
                )
                .child(
                    small("img-out", "−".into())
                        .tooltip("Zoom out")
                        .disabled(!can_zoom)
                        .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
                )
                .child(
                    small("img-in", "+".into())
                        .tooltip("Zoom in")
                        .disabled(!can_zoom)
                        .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
                )
                .child(
                    div()
                        .w(px(44.))
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(zoom_label),
                )
                .child(
                    small("img-backdrop", if self.dark { "Dark" } else { "Light" }.into())
                        .tooltip("Backdrop behind transparent parts")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dark = !this.dark;
                            cx.notify();
                        })),
                );
        }
        bar = bar.child(div().flex_1()).child(
            div()
                .text_xs()
                .text_color(c.muted_foreground)
                .child(info.join("  ·  ")),
        );
        let content = match (&self.source, source) {
            (Some(editor), true) => div().flex_1().min_h_0().child(editor.clone()).into_any_element(),
            _ => self.render_image(cx),
        };
        div()
            .id("image-view")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .child(content)
    }
}
