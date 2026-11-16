//! A single-child container whose height follows its width at a fixed ratio,
//! so media keeps its shape (and doesn't jump around while loading).

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    pub struct RatioBin {
        pub ratio: Cell<f64>,
        pub natural_width: Cell<i32>,
        /// If set, never grow wider than the natural width. Needed when the
        /// bin sits in a non-filling (start/end aligned) container: GTK
        /// measures height for the container's full width there.
        pub fixed: Cell<bool>,
        pub child: RefCell<Option<gtk::Widget>>,
    }

    impl Default for RatioBin {
        fn default() -> Self {
            Self {
                ratio: Cell::new(1.0),
                natural_width: Cell::new(600),
                fixed: Cell::new(false),
                child: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RatioBin {
        const NAME: &'static str = "FinstaRatioBin";
        type Type = super::RatioBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for RatioBin {
        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RatioBin {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                // GTK requires children to be measured before they are allocated.
                let _ = child.measure(orientation, -1);
            }
            let natural = self.natural_width.get();
            match orientation {
                gtk::Orientation::Horizontal if self.fixed.get() => (natural, natural, -1, -1),
                gtk::Orientation::Horizontal => (48.min(natural), natural, -1, -1),
                _ => {
                    let width = match for_size {
                        s if s < 0 => natural,
                        s if self.fixed.get() => s.min(natural),
                        s => s,
                    };
                    let height = (width as f64 * self.ratio.get()).round() as i32;
                    (height, height, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                let w = if self.fixed.get() { width.min(self.natural_width.get()) } else { width };
                let h = if self.fixed.get() {
                    height.min((w as f64 * self.ratio.get()).round() as i32)
                } else {
                    height
                };
                child.allocate(w, h, baseline, None);
            }
        }
    }
}

glib::wrapper! {
    pub struct RatioBin(ObjectSubclass<imp::RatioBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl RatioBin {
    /// `ratio` is height / width. `natural_width` is the preferred width.
    pub fn new(ratio: f64, natural_width: i32, child: &impl IsA<gtk::Widget>) -> Self {
        let bin: Self = glib::Object::new();
        bin.imp().ratio.set(ratio.max(0.05));
        bin.imp().natural_width.set(natural_width);
        bin.set_child(child);
        bin
    }

    /// Exactly `width` wide; height follows `ratio`.
    pub fn fixed(ratio: f64, width: i32, child: &impl IsA<gtk::Widget>) -> Self {
        let bin = Self::new(ratio, width, child);
        bin.imp().fixed.set(true);
        bin
    }

    pub fn set_child(&self, child: &impl IsA<gtk::Widget>) {
        if let Some(old) = self.imp().child.take() {
            old.unparent();
        }
        child.set_parent(self);
        self.imp().child.replace(Some(child.clone().upcast()));
        self.queue_resize();
    }
}
