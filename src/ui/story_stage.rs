//! The story viewer's layout: the story in the middle, as tall as there's
//! room for, and smaller cards for the people before and after it on either
//! side (as many as fit, at most two each side).

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// Width / height of a story.
const RATIO: f64 = 9.0 / 16.0;
/// A side card's height, as a share of the story's.
const CARD_SCALE: f64 = 0.42;
const GAP: i32 = 28;
const CARD_GAP: i32 = 18;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct StoryStage {
        pub main: RefCell<Option<gtk::Widget>>,
        /// Cards and where they go: -1 is just left of the story, 1 just right.
        pub cards: RefCell<Vec<(gtk::Widget, i32)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StoryStage {
        const NAME: &'static str = "FinstaStoryStage";
        type Type = super::StoryStage;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for StoryStage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
            self.obj().add_css_class("story-stage");
        }

        fn dispose(&self) {
            if let Some(main) = self.main.take() {
                main.unparent();
            }
            for (card, _) in self.cards.take() {
                card.unparent();
            }
        }
    }

    impl WidgetImpl for StoryStage {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (min, _, _, _) = match self.main.borrow().as_ref() {
                Some(m) => m.measure(orientation, -1),
                None => (0, 0, -1, -1),
            };
            for (card, _) in self.cards.borrow().iter() {
                let _ = card.measure(orientation, -1);
            }
            let natural = if orientation == gtk::Orientation::Horizontal { 1400 } else { 900 };
            (min, natural.max(min), -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(main) = self.main.borrow().clone() else { return };
            let (min_w, _, _, _) = main.measure(gtk::Orientation::Horizontal, -1);
            let (min_h, _, _, _) = main.measure(gtk::Orientation::Vertical, -1);
            // Room above and below on tall windows; none on small ones.
            let pad = if height > 640 { 16 } else { 0 };
            let mh = (height - 2 * pad).max(min_h);
            let mw = ((mh as f64 * RATIO).round() as i32).clamp(min_w.min(width), width.max(min_w));
            let (mx, my) = ((width - mw) / 2, pad);
            main.allocate(mw, mh, -1, Some(gtk::gsk::Transform::new().translate(&graphene_point(mx, my))));

            let ch = (mh as f64 * CARD_SCALE).round() as i32;
            let cw = (ch as f64 * RATIO).round() as i32;
            let cy = my + (mh - ch) / 2;
            for (card, slot) in self.cards.borrow().iter() {
                let (cmin_w, _, _, _) = card.measure(gtk::Orientation::Horizontal, -1);
                let (cmin_h, _, _, _) = card.measure(gtk::Orientation::Vertical, -1);
                let (w, h) = (cw.max(cmin_w), ch.max(cmin_h));
                let steps = slot.abs() - 1;
                let x = if *slot < 0 {
                    mx - GAP - w - steps * (w + CARD_GAP)
                } else {
                    mx + mw + GAP + steps * (w + CARD_GAP)
                };
                // Cards that don't fit go out of sight (and out of reach).
                let fits = x >= 8 && x + w <= width - 8 && h <= mh;
                let (x, y) = if fits { (x, cy) } else { (-10 * width.max(1000), cy) };
                card.allocate(w, h, -1, Some(gtk::gsk::Transform::new().translate(&graphene_point(x, y))));
            }
        }
    }
}

fn graphene_point(x: i32, y: i32) -> gtk::graphene::Point {
    gtk::graphene::Point::new(x as f32, y as f32)
}

glib::wrapper! {
    pub struct StoryStage(ObjectSubclass<imp::StoryStage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StoryStage {
    pub fn new(main: &impl IsA<gtk::Widget>) -> Self {
        let stage: Self = glib::Object::new();
        main.set_parent(&stage);
        stage.imp().main.replace(Some(main.clone().upcast()));
        stage
    }

    /// Replace the side cards. `slot` -1 is the card just left of the story,
    /// -2 the one before it; 1 and 2 on the right.
    pub fn set_cards(&self, cards: Vec<(gtk::Widget, i32)>) {
        for (card, _) in self.imp().cards.take() {
            card.unparent();
        }
        let main = self.imp().main.borrow().clone();
        for (card, _) in &cards {
            // After the story, so focus reaches the story first.
            card.insert_after(self, main.as_ref());
        }
        self.imp().cards.replace(cards);
        self.queue_allocate();
    }
}
