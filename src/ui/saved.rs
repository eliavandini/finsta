//! Your saved posts: a grid, 12 or so at a time, with a "Show More" button.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::Media;
use crate::rt;
use crate::ui::profile::thumbnail;
use crate::ui::util;
use crate::ui::window::Main;

struct Saved {
    main: Weak<Main>,
    grid: gtk::FlowBox,
    status: gtk::Box,
    more: gtk::Button,
    posts: RefCell<Vec<Media>>,
    next: RefCell<Option<String>>,
}

pub fn page(main: &Rc<Main>) -> adw::NavigationPage {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(
        &gtk::Label::builder()
            .label("Saved")
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    let status = gtk::Box::new(gtk::Orientation::Vertical, 0);
    status.append(&util::spinner_box());
    content.append(&status);
    let grid = gtk::FlowBox::builder()
        .min_children_per_line(3)
        .max_children_per_line(3)
        .homogeneous(true)
        .column_spacing(4)
        .row_spacing(4)
        .selection_mode(gtk::SelectionMode::None)
        .build();
    content.append(&grid);
    let more = gtk::Button::builder()
        .label("Show More Saved Posts")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .visible(false)
        .build();
    content.append(&more);
    let clamp = adw::Clamp::builder().maximum_size(760).child(&content).build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    let page = adw::NavigationPage::builder().title("Saved").tag("saved").child(&scroller).build();

    let saved = Rc::new(Saved {
        main: Rc::downgrade(main),
        grid,
        status,
        more,
        posts: RefCell::new(Vec::new()),
        next: RefCell::new(None),
    });
    let weak = Rc::downgrade(&saved);
    saved.grid.connect_child_activated(move |_, child| {
        let Some(s) = weak.upgrade() else { return };
        let media = s.posts.borrow().get(child.index() as usize).cloned();
        if let (Some(media), Some(main)) = (media, s.main.upgrade()) {
            main.open_post(media);
        }
    });
    let weak = Rc::downgrade(&saved);
    saved.more.connect_clicked(move |_| {
        if let Some(s) = weak.upgrade() {
            s.load();
        }
    });
    saved.load();
    util::attach(&page, saved);
    page
}

impl Saved {
    fn load(self: &Rc<Self>) {
        let Some(main) = self.main.upgrade() else { return };
        self.more.set_sensitive(false);
        let client = main.client.clone();
        let next = self.next.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.saved_posts(next).await }).await;
            let Some(s) = weak.upgrade() else { return };
            s.more.set_sensitive(true);
            util::clear_box(&s.status);
            match res {
                Ok(page) => {
                    for m in &page.items {
                        s.grid.append(&thumbnail(m));
                    }
                    s.posts.borrow_mut().extend(page.items);
                    let next = page.next_max_id.filter(|_| page.more_available);
                    s.more.set_visible(next.is_some());
                    s.next.replace(next);
                    if s.posts.borrow().is_empty() {
                        s.status.append(&message("Nothing saved yet. Posts you save on Instagram show up here."));
                    }
                }
                Err(e) => {
                    s.status.append(&message(&format!("Couldn't load your saved posts: {e}")));
                    if let Some(main) = s.main.upgrade()
                        && crate::api::is_login_required(&e)
                    {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }
}

fn message(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .margin_top(24)
        .build()
}
