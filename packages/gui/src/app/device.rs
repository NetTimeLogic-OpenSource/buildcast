use std::sync::mpsc;

use buildcast_core::disks::DiskInfo;
use egui::{Align, Button, ComboBox, Layout, RichText};

use super::App;
use crate::card;

const MAX_DISK_BYTES: u64 = 256 * 1024 * 1024 * 1024;

impl App {
    pub(super) fn device_card(&mut self, ui: &mut egui::Ui, busy: bool) {
        card::show(ui, |ui| {
            ui.horizontal(|ui| {
                let sel_text = self.selected_disk().map_or_else(
                    || {
                        if self.disks.is_empty() && self.scan.is_some() {
                            "Scanning for drives…".into()
                        } else if self.disks.is_empty() {
                            "No removable drives found. Plug one in.".into()
                        } else {
                            "Select a device…".into()
                        }
                    },
                    DiskInfo::label_bus,
                );
                ui.add_enabled_ui(!busy, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(Button::new(RichText::new("🔄").size(16.0)))
                            .on_hover_text(
                                "Refresh the device list (also refreshes when the window regains \
                                 focus)",
                            )
                            .clicked()
                        {
                            self.refresh_disks();
                        }
                        if self.scan.is_some() {
                            ui.spinner();
                        }
                        ui.add_enabled_ui(!self.disks.is_empty(), |ui| {
                            ComboBox::from_id_salt("device")
                                .width(ui.available_width())
                                .selected_text(sel_text)
                                .show_ui(ui, |ui| {
                                    for (i, d) in self.disks.iter().enumerate() {
                                        ui.selectable_value(&mut self.sel, Some(i), d.label_bus());
                                    }
                                });
                        });
                    });
                });
            });
            let changed = ui
                .add_enabled(
                    !busy,
                    egui::Checkbox::new(
                        &mut self.show_all,
                        RichText::new("Show all drives").size(12.0),
                    ),
                )
                .on_hover_text(
                    "Also list internal and large disks. Writing to the wrong one destroys its \
                     data.",
                )
                .changed();
            if changed {
                self.refresh_disks();
            }
            if self.show_all {
                ui.label(
                    RichText::new("⚠ Internal disks are listed. Double-check your selection.")
                        .color(ui.visuals().warn_fg_color)
                        .size(12.0),
                );
            }
        });
    }

    pub(super) fn poll_disks(&mut self) {
        let Some(rx) = &self.scan else { return };
        let found = match rx.try_recv() {
            Ok(found) => found,
            Err(mpsc::TryRecvError::Empty) => return,
            // the scan thread panicked, keep the old list
            Err(mpsc::TryRecvError::Disconnected) => {
                self.scan = None;
                return;
            }
        };
        self.scan = None;
        // never show a list scanned with a stale "show all" setting
        if std::mem::take(&mut self.rescan) {
            self.refresh_disks();
            return;
        }
        let prev = self.selected_disk().map(|d| d.id.clone());
        self.disks = found;
        self.sel = prev.and_then(|id| self.disks.iter().position(|d| d.id == id));
        if self.sel.is_none() && self.disks.len() == 1 {
            self.sel = Some(0);
        }
    }

    pub(super) fn refresh_disks(&mut self) {
        if self.scan.is_some() {
            self.rescan = true;
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (show_all, repaint) = (self.show_all, self.repaint.clone());
        std::thread::spawn(move || {
            let _ = tx.send(DiskInfo::list(show_all, MAX_DISK_BYTES));
            repaint();
        });
        self.scan = Some(rx);
    }

    pub(super) fn selected_disk(&self) -> Option<&DiskInfo> {
        self.sel.and_then(|i| self.disks.get(i))
    }
}
