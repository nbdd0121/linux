// SPDX-License-Identifier: GPL-2.0

//! Nova Core GPU Driver

use kernel::{
    debugfs,
    driver::Registration,
    pci,
    prelude::*,
    InPlaceModule, //
};

#[macro_use]
mod bitfield;

mod driver;
mod falcon;
mod fb;
mod firmware;
mod gfw;
mod gpu;
mod gsp;
#[macro_use]
mod num;
mod regs;
mod sbuffer;
mod vbios;

pub(crate) const MODULE_NAME: &core::ffi::CStr = <LocalModule as kernel::ModuleMetadata>::NAME;

#[pin_data]
struct NovaCoreModule {
    // Fields are dropped in declaration order, so `_driver` is dropped first,
    // then `_debugfs_guard` clears `DEBUGFS_ROOT`.
    #[pin]
    _driver: Registration<pci::AdapterNew<driver::NovaCoreDriver<'debugfs>>>,
    debugfs: debugfs::Dir,
}

impl InPlaceModule for NovaCoreModule {
    fn init(module: &'static kernel::ThisModule) -> impl PinInit<Self, Error> {
        let debugfs = debugfs::Dir::new(kernel::c_str!("nova_core"));

        try_pin_init!(Self {
            debugfs,
            // SAFETY: `Registration` is not forgotten.
            _driver <- unsafe {
                Registration::with_data_lt(MODULE_NAME, module, driver::NovaCoreDriver {
                    debugfs,
                })
            },
        })
    }
}

module! {
    type: NovaCoreModule,
    name: "NovaCore",
    authors: ["Danilo Krummrich"],
    description: "Nova Core GPU driver",
    license: "GPL v2",
    firmware: [],
}

kernel::module_firmware!(firmware::ModInfoBuilder);
