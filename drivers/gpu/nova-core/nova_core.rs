// SPDX-License-Identifier: GPL-2.0

//! Nova Core GPU Driver

use kernel::{
    debugfs,
    driver::Registration,
    pci,
    prelude::*,
    revocable::{LazyRevocable, RevokeHandle},
    InPlaceModule, //
};

#[macro_use]
mod bitfield;

mod dma;
mod driver;
mod falcon;
mod fb;
mod firmware;
mod gfw;
mod gpu;
mod gsp;
mod num;
mod regs;
mod sbuffer;
mod vbios;

pub(crate) const MODULE_NAME: &kernel::str::CStr = <LocalModule as kernel::ModuleMetadata>::NAME;

static DEBUGFS_ROOT: LazyRevocable<debugfs::Dir> = LazyRevocable::new();

#[pin_data]
struct NovaCoreModule {
    // Fields are dropped in declaration order, so _driver is dropped first,
    // then _debugfs_guard clears DEBUGFS_ROOT.
    #[pin]
    _driver: Registration<pci::Adapter<driver::NovaCore>>,
    _debugfs_root: RevokeHandle<'static, debugfs::Dir>,
}

impl InPlaceModule for NovaCoreModule {
    fn init(module: &'static kernel::ThisModule) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            _driver <- Registration::new(MODULE_NAME, module),
            _debugfs_root: Pin::static_ref(&DEBUGFS_ROOT).init(
                debugfs::Dir::new(kernel::c_str!("nova_core"))
            )?,
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
