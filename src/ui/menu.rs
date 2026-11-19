//! Right-click (or long-press) menus with the common actions for a post, a
//! story, a conversation, a message or a person.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::api::User;
use crate::ui::util;
use crate::ui::window::Main;

type Run = Rc<dyn Fn()>;

/// A menu: sections of items, and optionally a widget at the top (the
/// reactions row on messages).
#[derive(Default)]
pub struct Menu {
    sections: Vec<Vec<(String, Run)>>,
    top: Option<gtk::Widget>,
    closed: Option<Run>,
}

impl Menu {
    pub fn new() -> Self {
        Menu { sections: vec![Vec::new()], top: None, closed: None }
    }

    /// Add an item to the current section. It runs after the menu closes.
    pub fn item(&mut self, label: impl AsRef<str>, run: impl Fn() + 'static) -> &mut Self {
        // Menu labels treat "_" as a keyboard hint; usernames have them.
        let label = label.as_ref().replace('_', "__");
        if let Some(s) = self.sections.last_mut() {
            s.push((label, Rc::new(run)));
        }
        self
    }

    /// Start a new section (drawn with a line above it).
    pub fn section(&mut self) -> &mut Self {
        if self.sections.last().is_some_and(|s| !s.is_empty()) {
            self.sections.push(Vec::new());
        }
        self
    }

    pub fn top(&mut self, widget: &impl IsA<gtk::Widget>) -> &mut Self {
        self.top = Some(widget.as_ref().clone());
        self
    }

    /// Run when the menu closes (before the chosen item runs).
    pub fn on_closed(&mut self, run: impl Fn() + 'static) -> &mut Self {
        self.closed = Some(Rc::new(run));
        self
    }

    fn is_empty(&self) -> bool {
        self.top.is_none() && self.sections.iter().all(Vec::is_empty)
    }
}

/// Close the menu `widget` is in (for buttons in a menu's top widget).
pub fn close_from(widget: &impl IsA<gtk::Widget>) {
    if let Some(p) = widget.as_ref().ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
        p.popdown();
    }
}

fn popup(widget: &gtk::Widget, menu: Menu, x: f64, y: f64) {
    let model = gio::Menu::new();
    let group = gio::SimpleActionGroup::new();
    if menu.top.is_some() {
        let section = gio::Menu::new();
        let item = gio::MenuItem::new(None, None);
        item.set_attribute_value("custom", Some(&"top".to_variant()));
        section.append_item(&item);
        model.append_section(None, &section);
    }
    let mut n = 0;
    for items in menu.sections.into_iter().filter(|s| !s.is_empty()) {
        let section = gio::Menu::new();
        for (label, run) in items {
            let name = format!("a{n}");
            n += 1;
            let action = gio::SimpleAction::new(&name, None);
            action.connect_activate(move |_, _| {
                // Once the menu is gone: the action may rebuild what it's on.
                let run = run.clone();
                glib::idle_add_local_once(move || run());
            });
            group.add_action(&action);
            section.append(Some(&label), Some(&format!("ctx.{name}")));
        }
        model.append_section(None, &section);
    }
    let popover = gtk::PopoverMenu::from_model(Some(&model));
    popover.insert_action_group("ctx", Some(&group));
    if let Some(top) = &menu.top {
        popover.add_child(top, "top");
    }
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_parent(widget);
    let closed = menu.closed.clone();
    popover.connect_closed(move |p| {
        if let Some(run) = &closed {
            run();
        }
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    popover.popup();
}

/// Open `build`'s menu where `widget` is right-clicked or long-pressed
/// (`build` returns None when there's nothing to offer). `capture`: before
/// the widgets inside see the click (they may have menus of their own).
pub fn attach_with(widget: &impl IsA<gtk::Widget>, capture: bool, build: impl Fn() -> Option<Menu> + 'static) {
    let build = Rc::new(build);
    let right = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(if capture { gtk::PropagationPhase::Capture } else { gtk::PropagationPhase::Bubble })
        .build();
    let b = build.clone();
    right.connect_pressed(move |g, _, x, y| {
        let Some(w) = g.widget() else { return };
        let Some(menu) = b().filter(|m| !m.is_empty()) else { return };
        g.set_state(gtk::EventSequenceState::Claimed);
        popup(&w, menu, x, y);
    });
    widget.add_controller(right);
    let long = gtk::GestureLongPress::builder().touch_only(true).build();
    long.connect_pressed(move |g, x, y| {
        let Some(w) = g.widget() else { return };
        let Some(menu) = build().filter(|m| !m.is_empty()) else { return };
        g.set_state(gtk::EventSequenceState::Claimed);
        popup(&w, menu, x, y);
    });
    widget.add_controller(long);
}

pub fn attach(widget: &impl IsA<gtk::Widget>, build: impl Fn() -> Option<Menu> + 'static) {
    attach_with(widget, false, build);
}

/// A person's address on instagram.com.
pub fn profile_url(user: &User) -> String {
    format!("https://www.instagram.com/{}/", user.username)
}

/// The usual actions for a person: profile, message, link.
pub fn add_person(menu: &mut Menu, main: &Rc<Main>, user: &User, profile: bool, message: bool) {
    if user.username.is_empty() && user.pk.is_empty() {
        return;
    }
    let name = if user.username.is_empty() { "This Account".to_string() } else { format!("@{}", user.username) };
    if profile {
        let (weak, u) = (Rc::downgrade(main), user.clone());
        menu.item(format!("View {name}'s Profile"), move || {
            if let Some(main) = weak.upgrade() {
                main.open_profile(u.clone());
            }
        });
    }
    if message && user.pk != main.client.user_id() && !user.pk.is_empty() {
        let (weak, u) = (Rc::downgrade(main), user.clone());
        menu.item(format!("Message {name}"), move || {
            if let Some(main) = weak.upgrade() {
                main.open_chat_with_user(u.clone());
            }
        });
    }
    if !user.username.is_empty() {
        let url = profile_url(user);
        let weak = Rc::downgrade(main);
        menu.item("Copy Profile Link", move || {
            if let Some(main) = weak.upgrade() {
                main.copy(&url, "Link copied");
            }
        });
    }
}

/// "Copy Link" and "Open on instagram.com" for an address.
pub fn add_link(menu: &mut Menu, main: &Rc<Main>, url: &str) {
    let (weak, u) = (Rc::downgrade(main), url.to_string());
    menu.item("Copy Link", move || {
        if let Some(main) = weak.upgrade() {
            main.copy(&u, "Link copied");
        }
    });
    let (weak, u) = (Rc::downgrade(main), url.to_string());
    menu.item("Open on instagram.com", move || {
        if let Some(main) = weak.upgrade() {
            util::open_uri(&main.window, &u);
        }
    });
}

/// A menu for a person (search results, the people in a conversation).
pub fn person(main: &Rc<Main>, user: &User) -> Menu {
    let mut m = Menu::new();
    add_person(&mut m, main, user, true, true);
    if !user.username.is_empty() {
        let (weak, url) = (Rc::downgrade(main), profile_url(user));
        m.item("Open on instagram.com", move || {
            if let Some(main) = weak.upgrade() {
                util::open_uri(&main.window, &url);
            }
        });
    }
    m
}
